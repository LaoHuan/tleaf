#[cfg(feature = "inbound-trojan")]
pub mod inbound;
#[cfg(feature = "outbound-trojan")]
pub mod outbound;

pub static NAME: &str = "trojan";
