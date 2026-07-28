pub mod serial;
pub mod tcp;
pub mod udp;

use std::future::Future;
use std::net::SocketAddr;

use crate::error::TransportError;

/// A chunk of received data, with the peer it came from when the transport
/// exposes one (`None` for serial links).
pub struct Received {
    pub bytes: Vec<u8>,
    pub source: Option<SocketAddr>,
}

pub trait Transport: Send {
    fn send(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn recv(&mut self) -> impl Future<Output = Result<Received, TransportError>> + Send;

    /// Whether a failed send should tear the connection down.
    ///
    /// Connectionless transports say `false`: a datagram that could not be sent
    /// tells us nothing about the socket's ability to keep receiving.
    fn send_error_is_fatal(&self) -> bool {
        true
    }
}
