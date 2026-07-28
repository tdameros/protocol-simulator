#![deny(clippy::all)]
#![warn(clippy::pedantic)]

pub mod connection;
pub mod engine;
pub mod error;
pub mod frame;
pub mod transport;

pub use connection::{ConnectionId, ConnectionStatus, TcpMode, TransportConfig};
pub use engine::{Command, Engine, Event};
pub use error::{EngineError, TransportError};
