use std::io;

use crate::connection::ConnectionId;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    #[error("failed to open serial port {port}: {source}")]
    SerialOpen {
        port: String,
        #[source]
        source: tokio_serial::Error,
    },

    #[error("connection closed by peer")]
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("connection {0} not found")]
    UnknownConnection(ConnectionId),

    #[error("connection {0} already exists")]
    DuplicateConnection(ConnectionId),

    #[error("connection {0} is down")]
    ConnectionDown(ConnectionId),

    #[error("connection {id}: {source}")]
    Transport {
        id: ConnectionId,
        #[source]
        source: TransportError,
    },
}
