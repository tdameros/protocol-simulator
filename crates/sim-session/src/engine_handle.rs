use crate::state::{Direction, LogEntry, ScenarioRun, Session};
use sim_core::frame::FrameDef;
use sim_core::scenario::Scenario;
use sim_core::{Command, ConnectionId, Engine, Event, Outcome, RetryPolicy, TransportConfig};

use std::cell::Cell;

use tokio::sync::mpsc;

pub struct EngineHandle {
    command_tx: mpsc::Sender<Command>,
    event_rx: mpsc::Receiver<Event>,
    /// Commands the engine could not be given, counted rather than handed back.
    ///
    /// The channel is bounded, so a busy engine can refuse one, and a refused
    /// Send is a frame that never went out. Ten call sites would each have to
    /// remember to look at a returned error, and the eleventh would not;
    /// counting them here means the one place that reports it cannot be
    /// bypassed by a new caller.
    dropped: Cell<usize>,
}

impl EngineHandle {
    #[must_use]
    pub fn new() -> Self {
        let (command_tx, event_rx) = Engine::spawn();
        Self {
            command_tx,
            event_rx,
            dropped: Cell::new(0),
        }
    }

    pub fn connect(&self, id: ConnectionId, config: TransportConfig, retry: Option<RetryPolicy>) {
        self.send(Command::Connect { id, config, retry });
    }

    pub fn disconnect(&self, id: ConnectionId) {
        self.send(Command::Disconnect { id });
    }

    pub fn send_raw(&self, id: ConnectionId, bytes: Vec<u8>) {
        self.send(Command::SendRaw { id, bytes });
    }

    pub fn start_scenario(&self, scenario: Scenario, frames: Vec<FrameDef>) {
        self.send(Command::StartScenario {
            scenario: Box::new(scenario),
            frames,
        });
    }

    pub fn stop_scenario(&self, name: String) {
        self.send(Command::StopScenario { name });
    }

    /// Drains every event currently queued from the engine.
    ///
    /// Called once per frame; never blocks the UI thread.
    pub fn drain_events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// How many commands never reached the engine since this was last asked.
    pub fn take_dropped(&self) -> usize {
        self.dropped.replace(0)
    }

    fn send(&self, command: Command) {
        if self.command_tx.try_send(command).is_err() {
            self.dropped.set(self.dropped.get() + 1);
        }
    }

    /// Takes everything the engine has said since the last look.
    ///
    /// Both front ends want the same answer to the same event, so the
    /// reading of one lives here rather than beside a window.
    pub fn drain_into(&mut self, session: &mut Session) {
        // Said once per pass rather than at each of the ten places a command is
        // given: a refused Send is a frame that never went out, and silence
        // there reads as a test that passed.
        let dropped = self.take_dropped();
        if dropped > 0 {
            session.last_error = Some(format!(
                "the engine is too busy: {dropped} command(s) were not carried out"
            ));
        }

        for event in self.drain_events() {
            match event {
                Event::ConnectionStatus { id, status } => {
                    if let Some(entry) = session.connection_mut(&id) {
                        entry.status = status;
                    }
                }
                Event::FrameSent {
                    id,
                    bytes,
                    timestamp,
                } => {
                    session.push_log(LogEntry {
                        seq: 0,
                        id,
                        direction: Direction::Sent,
                        bytes,
                        source: None,
                        timestamp,
                    });
                }
                Event::FrameReceived {
                    id,
                    bytes,
                    source,
                    timestamp,
                } => {
                    session.push_log(LogEntry {
                        seq: 0,
                        id,
                        direction: Direction::Received,
                        bytes,
                        source,
                        timestamp,
                    });
                }
                Event::Error { id, error } => {
                    session.record_error(id, &error);
                }
                Event::ScenarioStep { name, step, pass } => {
                    session.running.insert(name, ScenarioRun { step, pass });
                }
                Event::ScenarioFinished { name, outcome } => {
                    session.running.remove(&name);
                    // A scenario that gave up says why, where a scenario that
                    // simply ran out has nothing to report.
                    if let Outcome::Failed(reason) = outcome {
                        session.last_error = Some(format!("[{name}] {reason}"));
                    }
                }
            }
        }
    }
}

impl Default for EngineHandle {
    fn default() -> Self {
        Self::new()
    }
}
