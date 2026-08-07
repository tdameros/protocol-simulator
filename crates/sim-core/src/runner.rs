//! Running one scenario.
//!
//! A scenario is a client of the engine like any other: it does not reach into
//! the connection map, it issues `SendRaw` back through the command channel.
//! That keeps a single owner for the connections, and gets the scenario the
//! same answers as a person clicking Send, down to the error when a link is
//! down.
//!
//! It holds a weak handle on that channel on purpose. A strong one would keep
//! the engine's own loop alive after the last real caller has gone, since the
//! loop stops when its command channel closes.

use std::collections::BTreeMap;

use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;

use crate::connection::ConnectionId;
use crate::engine::{Command, Event};
use crate::frame::value::{seed_values, Value};
use crate::frame::{codec, FrameDef};
use crate::scenario::{Action, Counter, Scenario, Step};

/// A frame as it arrived, republished for whoever is waiting for one.
///
/// Named apart from the transport's own `Received`, which carries a peer
/// address this side has no use for.
pub type Heard = (ConnectionId, Vec<u8>);

/// How a scenario ended.
///
/// Reported once the last step has been *issued*, not once the last byte has
/// left the socket: a send is posted to the engine and travels on from there,
/// so the frame it produces can still be announced after this is. Anything
/// caring about both has to look at what happened, not at the order it was
/// told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Ran every pass it was asked for.
    Completed,
    /// Told to stop, or left with nowhere to send.
    Stopped,
    Failed(String),
}

pub(crate) struct Context {
    pub scenario: Scenario,
    pub frames: Vec<FrameDef>,
    pub commands: mpsc::WeakSender<Command>,
    pub events: mpsc::Sender<Event>,
    pub received: broadcast::Receiver<Heard>,
}

pub(crate) async fn run(mut context: Context) -> Outcome {
    let scenario = context.scenario.clone();

    // The first tick completes at once, so pass zero starts without waiting a
    // period first.
    let mut ticker = scenario.repeat.map(|repeat| {
        let mut ticker = tokio::time::interval(repeat.every);
        // Skip rather than Burst: a pass that overran must not be followed by a
        // volley of catch-up passes, which on a link at 100 Hz would arrive as
        // a burst nothing on the other end expects. Skip also keeps the cadence
        // pinned to the original grid, so the stream does not drift.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker
    });

    let mut pass: u32 = 0;
    loop {
        if let Some(repeat) = scenario.repeat {
            if repeat.times.is_some_and(|times| pass >= times) {
                return Outcome::Completed;
            }
        }
        if let Some(ticker) = &mut ticker {
            ticker.tick().await;
        }

        for (index, step) in scenario.steps.iter().enumerate() {
            let _ = context
                .events
                .send(Event::ScenarioStep {
                    name: scenario.name.clone(),
                    // Counted from one, as the file numbers them.
                    step: index + 1,
                    pass,
                })
                .await;

            match execute(
                step,
                pass,
                &context.frames,
                &context.commands,
                &mut context.received,
            )
            .await
            {
                StepResult::Done => {}
                StepResult::Stopped => return Outcome::Stopped,
                StepResult::Failed(reason) => {
                    return Outcome::Failed(format!("step {}: {reason}", index + 1))
                }
            }
        }

        if scenario.repeat.is_none() {
            return Outcome::Completed;
        }
        pass = pass.saturating_add(1);
    }
}

enum StepResult {
    Done,
    Stopped,
    Failed(String),
}

async fn execute(
    step: &Step,
    pass: u32,
    frames: &[FrameDef],
    commands: &mpsc::WeakSender<Command>,
    received: &mut broadcast::Receiver<Heard>,
) -> StepResult {
    match &step.action {
        Action::Wait { delay } => {
            tokio::time::sleep(*delay).await;
            StepResult::Done
        }
        Action::Raw { bytes } => send(commands, &step.connection, bytes.clone()).await,
        Action::Send {
            frame,
            with,
            counters,
        } => match encode(frame, with, counters, pass, frames) {
            Ok(bytes) => send(commands, &step.connection, bytes).await,
            Err(reason) => StepResult::Failed(reason),
        },
        Action::WaitFor {
            pattern,
            anchor,
            timeout,
        } => {
            let waiting = async {
                loop {
                    match received.recv().await {
                        Ok((id, bytes)) => {
                            if id == step.connection && pattern.found_in(&bytes, *anchor) {
                                return true;
                            }
                        }
                        // Frames arrived faster than this step could look at
                        // them, so the one being waited for may be among those
                        // dropped. Waiting on is the lesser evil: giving up
                        // would report a timeout that never happened.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return false,
                    }
                }
            };

            match timeout {
                Some(limit) => match tokio::time::timeout(*limit, waiting).await {
                    Ok(true) => StepResult::Done,
                    Ok(false) => StepResult::Stopped,
                    Err(_) => StepResult::Failed(format!(
                        "no matching frame on {} within {} ms",
                        step.connection,
                        limit.as_millis()
                    )),
                },
                None if waiting.await => StepResult::Done,
                None => StepResult::Stopped,
            }
        }
    }
}

async fn send(
    commands: &mpsc::WeakSender<Command>,
    id: &ConnectionId,
    bytes: Vec<u8>,
) -> StepResult {
    // Gone means the engine is shutting down, which is not a failure to report.
    let Some(commands) = commands.upgrade() else {
        return StepResult::Stopped;
    };
    match commands
        .send(Command::SendRaw {
            id: id.clone(),
            bytes,
        })
        .await
    {
        Ok(()) => StepResult::Done,
        Err(_) => StepResult::Stopped,
    }
}

/// The frame's own defaults, overlaid with what the step overrides and what its
/// counters have reached.
fn encode(
    name: &str,
    with: &BTreeMap<String, Value>,
    counters: &BTreeMap<String, Counter>,
    pass: u32,
    frames: &[FrameDef],
) -> Result<Vec<u8>, String> {
    let frame = frames
        .iter()
        .find(|frame| frame.name == name)
        .ok_or_else(|| format!("no frame named {name}"))?;

    let mut values = seed_values(frame);
    for (field, value) in with {
        // Overrides come from a file, so they arrive in whatever shape TOML
        // suggested rather than the one the field declares.
        let kind = field_kind(frame, field)?;
        let coerced = value
            .clone()
            .coerced_to(kind)
            .ok_or_else(|| format!("{name}.{field} cannot hold {}", value.type_name()))?;
        values.insert(field.clone(), coerced);
    }
    for (field, counter) in counters {
        let kind = field_kind(frame, field)?;
        let value = Value::Uint(counter.at(u64::from(pass)))
            .coerced_to(kind)
            .ok_or_else(|| format!("{name}.{field} cannot hold a counter"))?;
        values.insert(field.clone(), value);
    }

    codec::encode(frame, &values).map_err(|error| error.to_string())
}

fn field_kind<'a>(frame: &'a FrameDef, field: &str) -> Result<&'a crate::frame::FieldKind, String> {
    frame
        .fields
        .iter()
        .find(|declared| declared.name == field)
        .map(|declared| &declared.kind)
        .ok_or_else(|| format!("{} has no field named {field}", frame.name))
}
