#![deny(clippy::all)]
#![warn(clippy::pedantic)]

pub mod config;
pub mod connection;
pub mod document;
pub mod engine;
pub mod error;
pub mod frame;
pub mod pattern;
pub mod runner;
pub mod scenario;
pub mod transport;

pub use connection::{ConnectionId, ConnectionStatus, RetryPolicy, TcpMode, TransportConfig};
pub use engine::{Command, Engine, Event};
pub use error::{EngineError, TransportError};
pub use pattern::{Anchor, HexPattern, PatternSpec};
pub use runner::Outcome;
pub use scenario::{Scenario, ScenarioError};
