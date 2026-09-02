//! Everything a front end needs that is not drawing.
//!
//! The frame and scenario libraries as they are loaded, the traffic buffer and
//! its filters, the drafts nobody has saved yet, and the one door to the
//! engine. A window and a terminal want all of it and disagree only about how
//! to show it.
//!
//! The split from `sim-core` is the error strategy as much as the subject.
//! Below is a library with typed errors a caller can branch on. Here is
//! application state, where a failure ends up in front of a person, so
//! `anyhow` carries the context instead.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]

pub mod engine_handle;
pub mod frames;
pub mod hex;
pub mod kinds;
pub mod layout;
pub mod scenarios;
pub mod state;

pub use engine_handle::EngineHandle;
pub use state::Session;
