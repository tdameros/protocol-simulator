use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use super::{Received, Transport};
use crate::error::TransportError;

const RECV_BUFFER_SIZE: usize = 64 * 1024;

pub struct UdpTransport {
    socket: UdpSocket,
    destination: SocketAddr,
    buf: Vec<u8>,
}

impl UdpTransport {
    /// Binds locally and sends to `remote`.
    ///
    /// The socket is deliberately left unconnected: a connected UDP socket only
    /// accepts datagrams coming from `remote`, which silently hides traffic sent
    /// from an unexpected source port — usually the very thing worth seeing.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be bound to `bind`.
    pub async fn bind(bind: SocketAddr, remote: SocketAddr) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(bind).await?;
        Ok(Self::new(socket, remote))
    }

    /// Joins `group` on `interface`, then sends to and receives from that group.
    ///
    /// Async only to pin the call to a tokio runtime: registering the socket with
    /// the reactor panics outside one.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created or bound, or if joining
    /// the group on `interface` fails.
    #[expect(
        clippy::unused_async,
        reason = "async is what forces callers into a runtime, which from_std requires"
    )]
    pub async fn join_multicast(
        group: SocketAddrV4,
        interface: Ipv4Addr,
    ) -> Result<Self, TransportError> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        // Lets several listeners (including other tools on this machine) share the
        // group port. On BSD/macOS SO_REUSEADDR alone is not enough for that —
        // sharing a UDP port there requires SO_REUSEPORT — whereas on Windows
        // SO_REUSEADDR already carries those semantics and SO_REUSEPORT is absent.
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, group.port());
        socket.bind(&SocketAddr::V4(bind_addr).into())?;

        socket.join_multicast_v4(group.ip(), &interface)?;
        socket.set_multicast_if_v4(&interface)?;
        socket.set_nonblocking(true)?;

        let socket = UdpSocket::from_std(socket.into())?;
        Ok(Self::new(socket, SocketAddr::V4(group)))
    }

    fn new(socket: UdpSocket, destination: SocketAddr) -> Self {
        Self {
            socket,
            destination,
            buf: vec![0u8; RECV_BUFFER_SIZE],
        }
    }
}

impl Transport for UdpTransport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.socket.send_to(bytes, self.destination).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Received, TransportError> {
        let (n, source) = self.socket.recv_from(&mut self.buf).await?;
        Ok(Received {
            bytes: self.buf[..n].to_vec(),
            source: Some(source),
        })
    }

    fn send_error_is_fatal(&self) -> bool {
        false
    }
}
