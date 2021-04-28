use std::io;
use std::sync::Arc;

use async_trait::async_trait;

use super::InboundHandler;
use super::{InboundDatagram, InboundTransport, Tag, TcpInboundHandler, UdpInboundHandler};

/// An inbound handler groups a TCP inbound handler and a UDP inbound
/// handler.
pub struct Handler {
    tag: String,
    tcp_handler: Option<Arc<dyn TcpInboundHandler>>,
    udp_handler: Option<Arc<dyn UdpInboundHandler>>,
}

impl Handler {
    pub fn new(
        tag: String,
        tcp: Option<Arc<dyn TcpInboundHandler>>,
        udp: Option<Arc<dyn UdpInboundHandler>>,
    ) -> Self {
        Handler {
            tag,
            tcp_handler: tcp,
            udp_handler: udp,
        }
    }
}

impl Tag for Handler {
    fn tag(&self) -> &String {
        &self.tag
    }
}

impl InboundHandler for Handler {
    fn has_tcp(&self) -> bool {
        self.tcp_handler.is_some()
    }

    fn has_udp(&self) -> bool {
        self.udp_handler.is_some()
    }
}

#[async_trait]
impl TcpInboundHandler for Handler {
    async fn handle_tcp<'a>(
        &'a self,
        transport: InboundTransport,
    ) -> std::io::Result<InboundTransport> {
        if let Some(handler) = &self.tcp_handler {
            handler.handle_tcp(transport).await
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "unimplemented"))
        }
    }
}

#[async_trait]
impl UdpInboundHandler for Handler {
    async fn handle_udp<'a>(
        &'a self,
        socket: Option<Box<dyn InboundDatagram>>,
    ) -> io::Result<Box<dyn InboundDatagram>> {
        if let Some(handler) = &self.udp_handler {
            handler.handle_udp(socket).await
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "unimplemented"))
        }
    }
}
