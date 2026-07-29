use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Received, Transport};
use crate::error::TransportError;

const RECV_BUFFER_SIZE: usize = 64 * 1024;
const LISTEN_BACKLOG: i32 = 128;

/// Binds a listener with `SO_REUSEADDR`.
///
/// `tokio::net::TcpListener::bind` goes through mio, which unlike
/// `std::net::TcpListener::bind` does not set that option. Without it, sockets
/// left in `TIME_WAIT` by the previous peers keep the port busy, so stopping a
/// server and restarting it on the same port fails for about a minute.
fn bind_listener(addr: SocketAddr) -> Result<TcpListener, TransportError> {
    let domain = Domain::for_address(addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(LISTEN_BACKLOG)?;
    Ok(TcpListener::from_std(socket.into())?)
}

pub struct TcpTransport {
    /// `None` between peers: a bound server exists before anyone connects to it
    /// and outlives the client that hangs up.
    stream: Option<TcpStream>,
    peer: Option<SocketAddr>,
    /// Kept by servers so a peer hanging up sends them back to accepting rather
    /// than ending the connection. `None` for clients, which have nothing to
    /// accept on.
    listener: Option<TcpListener>,
    buf: Vec<u8>,
}

impl TcpTransport {
    /// # Errors
    ///
    /// Returns an error if the connection to `addr` cannot be established.
    pub async fn connect(addr: SocketAddr) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self {
            peer: stream.peer_addr().ok(),
            stream: Some(stream),
            listener: None,
            buf: vec![0u8; RECV_BUFFER_SIZE],
        })
    }

    /// Binds the listening socket, without waiting for a peer.
    ///
    /// Accepting is left to [`Transport::relisten`] so that binding the port and
    /// getting a client are two observable steps: a server is up the moment it
    /// holds the port, whether or not anyone has called yet.
    ///
    /// # Errors
    ///
    /// Returns an error if `listen` cannot be bound.
    #[expect(
        clippy::unused_async,
        reason = "async is what forces callers into a runtime, which from_std requires"
    )]
    pub async fn listen(listen: SocketAddr) -> Result<Self, TransportError> {
        Ok(Self {
            stream: None,
            peer: None,
            listener: Some(bind_listener(listen)?),
            buf: vec![0u8; RECV_BUFFER_SIZE],
        })
    }

    fn stream_mut(&mut self) -> Result<&mut TcpStream, TransportError> {
        self.stream.as_mut().ok_or(TransportError::Closed)
    }
}

impl Transport for TcpTransport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.stream_mut()?.write_all(bytes).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Received, TransportError> {
        let Self { stream, buf, .. } = self;
        let stream = stream.as_mut().ok_or(TransportError::Closed)?;
        let n = match stream.read(buf).await {
            Ok(0) => {
                // The peer hung up. Dropping the stream is what puts a server
                // back to waiting instead of reading a dead socket forever.
                self.stream = None;
                self.peer = None;
                return Err(TransportError::Closed);
            }
            Ok(n) => n,
            Err(source) => {
                self.stream = None;
                self.peer = None;
                return Err(source.into());
            }
        };
        Ok(Received {
            bytes: self.buf[..n].to_vec(),
            source: self.peer,
        })
    }

    fn is_ready(&self) -> bool {
        self.stream.is_some()
    }

    async fn relisten(&mut self) -> Result<bool, TransportError> {
        let Some(listener) = self.listener.as_ref() else {
            return Ok(false);
        };
        let (stream, _) = listener.accept().await?;
        self.peer = stream.peer_addr().ok();
        self.stream = Some(stream);
        Ok(true)
    }
}
