use std::convert::TryFrom;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UdpSocket;

use crate::{
    app::dns_client::DnsClient,
    proxy::{
        stream::SimpleProxyStream, OutboundDatagram, OutboundDatagramRecvHalf,
        OutboundDatagramSendHalf, OutboundTransport, OutboundHandler, SimpleOutboundDatagram,
        UdpOutboundHandler, UdpTransportType,
    },
    session::{Session, SocksAddr},
};

struct DatagramToStream {
    recv: Box<dyn OutboundDatagramRecvHalf>,
    send: Box<dyn OutboundDatagramSendHalf>,
    target: SocketAddr,
}

impl AsyncRead for DatagramToStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // FIXME use an internal buf for overflow data
        let mut recv = self.get_mut().recv.recv_from(buf);
        match recv.as_mut().poll(cx) {
            Poll::Ready(res) => match res {
                Ok((n, _)) => Poll::Ready(Ok(n)),
                Err(e) => Poll::Ready(Err(e)),
            },

            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for DatagramToStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let mut send = me.send.send_to(buf, &me.target);
        match send.as_mut().poll(cx) {
            Poll::Ready(res) => match res {
                Ok(n) => Poll::Ready(Ok(n)),
                Err(e) => Poll::Ready(Err(e)),
            },
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct Handler {
    pub actors: Vec<Arc<dyn OutboundHandler>>,
    pub dns_client: Arc<DnsClient>,
}

#[async_trait]
impl UdpOutboundHandler for Handler {
    fn name(&self) -> &str {
        super::NAME
    }

    fn udp_connect_addr(&self) -> Option<(String, u16, SocketAddr)> {
        for a in self.actors.iter() {
            if let Some(addr) = a.udp_connect_addr() {
                return Some(addr);
            }
        }
        None
    }

    fn udp_transport_type(&self) -> UdpTransportType {
        for a in self.actors.iter() {
            if a.udp_transport_type() == UdpTransportType::Stream {
                return UdpTransportType::Stream;
            }
        }
        UdpTransportType::Packet
    }

    async fn handle_udp<'a>(
        &'a self,
        sess: &'a Session,
        transport: Option<OutboundTransport>,
    ) -> io::Result<Box<dyn OutboundDatagram>> {
        if let Some(OutboundTransport::Stream(mut stream)) = transport {
            for (i, a) in self.actors.iter().enumerate() {
                let mut new_sess = sess.clone();
                for j in (i + 1)..self.actors.len() {
                    if let Some((connect_addr, port, _)) = self.actors[j].udp_connect_addr() {
                        if let Ok(addr) = SocksAddr::try_from(format!("{}:{}", connect_addr, port))
                        {
                            new_sess.destination = addr;
                            break;
                        }
                    }
                }

                if i == self.actors.len() - 1 {
                    let dgram = a
                        .handle_udp(&new_sess, Some(OutboundTransport::Stream(stream)))
                        .await?;
                    return Ok(dgram);
                } else {
                    let mut transport_type: UdpTransportType = UdpTransportType::Packet;
                    for k in i..self.actors.len() {
                        if self.actors[k].udp_transport_type() == UdpTransportType::Stream {
                            transport_type = UdpTransportType::Stream;
                        }
                    }
                    match transport_type {
                        UdpTransportType::Stream => {
                            stream = a.handle_tcp(&new_sess, Some(stream)).await?;
                        }
                        UdpTransportType::Packet => {
                            let dgram = a
                                .handle_udp(&new_sess, Some(OutboundTransport::Stream(stream)))
                                .await?;
                            let (r, s) = dgram.split();
                            stream = Box::new(SimpleProxyStream(DatagramToStream {
                                recv: r,
                                send: s,
                                target: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                            }));
                        }
                        UdpTransportType::Unknown => {
                            return Err(io::Error::new(
                                io::ErrorKind::Other,
                                "unknown transport type",
                            ));
                        }
                    }
                }
            }
        }

        let mut transport_type: UdpTransportType = UdpTransportType::Packet;
        for a in self.actors.iter() {
            if a.udp_transport_type() == UdpTransportType::Stream {
                transport_type = UdpTransportType::Stream;
            }
        }
        // if all actors are Packet transports, simply chaining the datagrams.
        if let UdpTransportType::Packet = transport_type {
            let mut bind_addr: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);
            for a in self.actors.iter() {
                if let Some((_, _, baddr)) = a.udp_connect_addr() {
                    bind_addr = baddr;
                }
            }
            let socket = UdpSocket::bind(bind_addr).await?;
            let mut dgram: Box<dyn OutboundDatagram> = Box::new(SimpleOutboundDatagram(socket));

            for (i, a) in self.actors.iter().enumerate() {
                let mut new_sess = sess.clone();
                for j in (i + 1)..self.actors.len() {
                    if let Some((connect_addr, port, _)) = self.actors[j].udp_connect_addr() {
                        if let Ok(addr) = SocksAddr::try_from(format!("{}:{}", connect_addr, port))
                        {
                            new_sess.destination = addr;
                            break;
                        }
                    }
                }
                dgram = a
                    .handle_udp(&new_sess, Some(OutboundTransport::Datagram(dgram)))
                    .await?;
            }
            return Ok(dgram);
        }

        for a in self.actors.iter() {
            if let Some((connect_addr, port, bind_addr)) = a.udp_connect_addr() {
                let mut stream = self
                    .dial_tcp_stream(self.dns_client.clone(), &bind_addr, &connect_addr, &port)
                    .await?;
                let mut datagram: Option<Box<dyn OutboundDatagram>> = None;

                let mut last_index = 0;

                for (i, a) in self.actors.iter().enumerate() {
                    last_index = i;
                    let mut new_sess = sess.clone();
                    for j in (i + 1)..self.actors.len() {
                        if let Some((connect_addr, port, _)) = self.actors[j].udp_connect_addr() {
                            if let Ok(addr) =
                                SocksAddr::try_from(format!("{}:{}", connect_addr, port))
                            {
                                new_sess.destination = addr;
                                break;
                            }
                        }
                    }

                    if i == self.actors.len() - 1 {
                        if let Some(d) = datagram {
                            return a
                                .handle_udp(&new_sess, Some(OutboundTransport::Datagram(d)))
                                .await;
                        } else {
                            return a
                                .handle_udp(&new_sess, Some(OutboundTransport::Stream(stream)))
                                .await;
                        }
                    } else {
                        let mut transport_type: UdpTransportType = UdpTransportType::Packet;

                        // can only transport unreliable upon reliable, not the
                        // reverse, if any following actor requires reliable transport,
                        // we must also use reliable transport here.
                        for k in i..self.actors.len() {
                            if self.actors[k].udp_transport_type() == UdpTransportType::Stream {
                                transport_type = UdpTransportType::Stream;
                            }
                        }

                        match transport_type {
                            UdpTransportType::Stream => {
                                stream = a.handle_tcp(&new_sess, Some(stream)).await?;
                            }
                            UdpTransportType::Packet => {
                                // once a Packet type is encountered, it's guaranteed all
                                // following actors are Packet type.
                                if let Some(d) = datagram {
                                    datagram = Some(
                                        a.handle_udp(
                                            &new_sess,
                                            Some(OutboundTransport::Datagram(d)),
                                        )
                                        .await?,
                                    );
                                } else {
                                    datagram = Some(
                                        a.handle_udp(
                                            &new_sess,
                                            Some(OutboundTransport::Stream(stream)),
                                        )
                                        .await?,
                                    );
                                }
                                break;
                            }
                            UdpTransportType::Unknown => {
                                return Err(io::Error::new(
                                    io::ErrorKind::Other,
                                    "unknown transport type",
                                ));
                            }
                        }
                    }
                }

                // catch up the last actor index, and treats all the remaining
                // actors are Packet transports, and they should be
                for i in (last_index + 1)..self.actors.len() {
                    let mut new_sess = sess.clone();
                    for j in (i + 1)..self.actors.len() {
                        if let Some((connect_addr, port, _)) = self.actors[j].udp_connect_addr() {
                            if let Ok(addr) =
                                SocksAddr::try_from(format!("{}:{}", connect_addr, port))
                            {
                                new_sess.destination = addr;
                                break;
                            }
                        }
                    }

                    if i == self.actors.len() - 1 {
                        return self.actors[i]
                            .handle_udp(
                                &new_sess,
                                Some(OutboundTransport::Datagram(datagram.unwrap())),
                            )
                            .await;
                    } else {
                        datagram = Some(
                            self.actors[i]
                                .handle_udp(
                                    &new_sess,
                                    Some(OutboundTransport::Datagram(datagram.unwrap())),
                                )
                                .await?,
                        );
                    }
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid chain"))
    }
}
