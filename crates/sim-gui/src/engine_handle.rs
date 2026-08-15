use sim_core::frame::FrameDef;
use sim_core::scenario::Scenario;
use sim_core::{Command, ConnectionId, Engine, Event, RetryPolicy, TransportConfig};

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
}

impl Default for EngineHandle {
    fn default() -> Self {
        Self::new()
    }
}
