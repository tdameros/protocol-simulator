use sim_core::{Command, ConnectionId, Engine, Event, TransportConfig};

use tokio::sync::mpsc;

pub struct EngineHandle {
    command_tx: mpsc::Sender<Command>,
    event_rx: mpsc::Receiver<Event>,
}

impl EngineHandle {
    pub fn new() -> Self {
        let (command_tx, event_rx) = Engine::spawn();
        Self {
            command_tx,
            event_rx,
        }
    }

    pub fn connect(&self, id: ConnectionId, config: TransportConfig) {
        self.send(Command::Connect { id, config });
    }

    pub fn disconnect(&self, id: ConnectionId) {
        self.send(Command::Disconnect { id });
    }

    pub fn send_raw(&self, id: ConnectionId, bytes: Vec<u8>) {
        self.send(Command::SendRaw { id, bytes });
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

    fn send(&self, command: Command) {
        let _ = self.command_tx.try_send(command);
    }
}

impl Default for EngineHandle {
    fn default() -> Self {
        Self::new()
    }
}
