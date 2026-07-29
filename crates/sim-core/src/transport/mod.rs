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

    /// Whether the transport can carry data right now.
    ///
    /// False for a server that holds its port but has no peer: it is open, yet
    /// [`Transport::send`] and [`Transport::recv`] have nowhere to go until
    /// [`Transport::relisten`] finds one.
    fn is_ready(&self) -> bool {
        true
    }

    /// Re-establishes the link after the peer went away, when the transport can.
    ///
    /// A listening TCP server goes back to accepting; everything else reports
    /// `false` and lets the connection end.
    fn relisten(&mut self) -> impl Future<Output = Result<bool, TransportError>> + Send {
        async { Ok(false) }
    }
}
