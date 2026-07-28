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
/// left in `TIME_WAIT` by previous peers keep the port busy, so stopping a
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
    stream: TcpStream,
    peer: Option<SocketAddr>,
    buf: Vec<u8>,
}

impl TcpTransport {
    /// # Errors
    ///
    /// Returns an error if the connection to `addr` cannot be established.
    pub async fn connect(addr: SocketAddr) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::from_stream(stream))
    }

    /// # Errors
    ///
    /// Returns an error if `listen` cannot be bound or no peer connects.
    pub async fn listen(listen: SocketAddr) -> Result<Self, TransportError> {
        let listener = bind_listener(listen)?;
        let (stream, _peer) = listener.accept().await?;
        Ok(Self::from_stream(stream))
    }

    fn from_stream(stream: TcpStream) -> Self {
        Self {
            peer: stream.peer_addr().ok(),
            stream,
            buf: vec![0u8; RECV_BUFFER_SIZE],
        }
    }
}

impl Transport for TcpTransport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.stream.write_all(bytes).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Received, TransportError> {
        let n = self.stream.read(&mut self.buf).await?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        Ok(Received {
            bytes: self.buf[..n].to_vec(),
            source: self.peer,
        })
    }
}
