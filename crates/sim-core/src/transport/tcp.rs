use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Received, Transport};
use crate::error::TransportError;

const RECV_BUFFER_SIZE: usize = 64 * 1024;

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
        let listener = TcpListener::bind(listen).await?;
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
