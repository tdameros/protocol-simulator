use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::connection::{ConnectionId, ConnectionStatus, RetryPolicy, TcpMode, TransportConfig};
use crate::error::{EngineError, TransportError};
use crate::frame::FrameDef;
use crate::runner::{self, Heard, Outcome};
use crate::scenario::Scenario;
use crate::transport::serial::SerialTransport;
use crate::transport::tcp::TcpTransport;
use crate::transport::udp::UdpTransport;
use crate::transport::{Received, Transport};

const COMMAND_CHANNEL_CAPACITY: usize = 256;
const EVENT_CHANNEL_CAPACITY: usize = 1024;
const OUTGOING_CHANNEL_CAPACITY: usize = 256;
/// Received frames are republished here for scenarios waiting on one. Deep
/// enough that a step looking for a reply is not outrun by a busy link.
const RECEIVED_CHANNEL_CAPACITY: usize = 1024;

pub enum Command {
    Connect {
        id: ConnectionId,
        config: TransportConfig,
        /// `None` gives up the moment the link fails, which is the default.
        retry: Option<RetryPolicy>,
    },
    Disconnect {
        id: ConnectionId,
    },
    SendRaw {
        id: ConnectionId,
        bytes: Vec<u8>,
    },
    /// Runs a scenario until it ends or is stopped.
    ///
    /// It carries the definitions it encodes against rather than reading a
    /// shared library, so a scenario keeps running against the frames it
    /// started with however they are edited meanwhile.
    StartScenario {
        scenario: Box<Scenario>,
        frames: Vec<FrameDef>,
    },
    StopScenario {
        name: String,
    },
}

pub enum Event {
    ConnectionStatus {
        id: ConnectionId,
        status: ConnectionStatus,
    },
    FrameSent {
        id: ConnectionId,
        bytes: Vec<u8>,
        timestamp: SystemTime,
    },
    FrameReceived {
        id: ConnectionId,
        bytes: Vec<u8>,
        source: Option<SocketAddr>,
        timestamp: SystemTime,
    },
    Error {
        id: Option<ConnectionId>,
        error: EngineError,
    },
    /// A scenario is about to run `step`, counted from one, on pass `pass`,
    /// counted from zero.
    ScenarioStep {
        name: String,
        step: usize,
        pass: u32,
    },
    ScenarioFinished {
        name: String,
        outcome: Outcome,
    },
}

pub struct Engine;

impl Engine {
    /// Starts the engine on a dedicated thread with its own tokio runtime and
    /// returns the channels used to drive it and observe its output.
    ///
    /// # Panics
    ///
    /// Panics if the engine thread or its tokio runtime fail to start.
    #[must_use]
    pub fn spawn() -> (mpsc::Sender<Command>, mpsc::Receiver<Event>) {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        // Weak on purpose: a scenario issues commands back through here, and a
        // strong handle held by the loop itself would keep the channel open
        // after every real caller had gone, so the engine would never stop.
        let issue = command_tx.downgrade();

        std::thread::Builder::new()
            .name("sim-engine".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build engine tokio runtime");
                runtime.block_on(run(command_rx, issue, event_tx));
            })
            .expect("failed to spawn engine thread");

        (command_tx, event_rx)
    }
}

struct ConnectionHandle {
    task: JoinHandle<()>,
    outgoing_tx: mpsc::Sender<Vec<u8>>,
    /// Set by the task while a transport is actually open. A retrying
    /// connection keeps its channel alive between attempts, so this is the only
    /// thing that can tell a send whether it has anywhere to go.
    connected: Arc<AtomicBool>,
    generation: u64,
}

/// A scenario the engine is running, tagged like a connection so a notice from
/// one that has already been replaced cannot evict its successor.
struct ScenarioHandle {
    task: JoinHandle<()>,
    generation: u64,
}

/// Every channel a command handler may need, kept together so that adding one
/// does not widen every signature.
struct Wiring {
    events: mpsc::Sender<Event>,
    /// Where a scenario posts the sends it wants performed.
    issue: mpsc::WeakSender<Command>,
    finished: mpsc::Sender<(ConnectionId, u64)>,
    ended: mpsc::Sender<(String, u64, Outcome)>,
    /// Received frames, republished for scenarios waiting on one.
    heard: broadcast::Sender<Heard>,
}

async fn run(
    mut commands: mpsc::Receiver<Command>,
    issue: mpsc::WeakSender<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut connections: HashMap<ConnectionId, ConnectionHandle> = HashMap::new();
    let mut scenarios: HashMap<String, ScenarioHandle> = HashMap::new();
    let (finished_tx, mut finished_rx) =
        mpsc::channel::<(ConnectionId, u64)>(EVENT_CHANNEL_CAPACITY);
    let (ended_tx, mut ended_rx) = mpsc::channel::<(String, u64, Outcome)>(EVENT_CHANNEL_CAPACITY);
    let (heard_tx, _) = broadcast::channel::<Heard>(RECEIVED_CHANNEL_CAPACITY);
    let wiring = Wiring {
        events,
        issue,
        finished: finished_tx,
        ended: ended_tx,
        heard: heard_tx,
    };
    let mut next_generation: u64 = 0;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                handle_command(
                    command,
                    &mut connections,
                    &mut scenarios,
                    &wiring,
                    &mut next_generation,
                )
                .await;
            }
            // A task that ended on its own frees its name here, and the
            // disconnection is announced from here too, in that order. That
            // ordering is the whole point: this loop is single-threaded, so a
            // caller reacting the instant it sees `Disconnected` cannot have its
            // `Connect` handled before the slot was freed, and cannot be told the
            // name is still taken.
            //
            // The generation guards against a stale notice evicting the newer
            // connection that has since taken the name.
            Some((id, generation)) = finished_rx.recv() => {
                if connections.get(&id).is_some_and(|handle| handle.generation == generation) {
                    connections.remove(&id);
                    let _ = wiring.events
                        .send(Event::ConnectionStatus {
                            id,
                            status: ConnectionStatus::Disconnected,
                        })
                        .await;
                }
            }
            // Same discipline for a scenario that ran itself out: the name is
            // freed here, and only then is the ending announced, so a caller
            // restarting it the instant it hears the news finds the slot empty.
            Some((name, generation, outcome)) = ended_rx.recv() => {
                if scenarios.get(&name).is_some_and(|handle| handle.generation == generation) {
                    scenarios.remove(&name);
                    let _ = wiring.events
                        .send(Event::ScenarioFinished { name, outcome })
                        .await;
                }
            }
        }
    }
}

async fn handle_command(
    command: Command,
    connections: &mut HashMap<ConnectionId, ConnectionHandle>,
    scenarios: &mut HashMap<String, ScenarioHandle>,
    wiring: &Wiring,
    next_generation: &mut u64,
) {
    let events = &wiring.events;
    match command {
        Command::Connect { id, config, retry } => {
            if connections.contains_key(&id) {
                report_error(
                    events,
                    Some(id.clone()),
                    EngineError::DuplicateConnection(id),
                )
                .await;
                return;
            }
            let generation = *next_generation;
            *next_generation += 1;
            let handle = start_connection(
                id.clone(),
                config,
                retry,
                events.clone(),
                wiring.finished.clone(),
                wiring.heard.clone(),
                generation,
            );
            connections.insert(id, handle);
        }
        Command::Disconnect { id } => match connections.remove(&id) {
            Some(handle) => {
                handle.task.abort();
                let _ = events
                    .send(Event::ConnectionStatus {
                        id,
                        status: ConnectionStatus::Disconnected,
                    })
                    .await;
            }
            None => {
                report_error(events, Some(id.clone()), EngineError::UnknownConnection(id)).await;
            }
        },
        Command::SendRaw { id, bytes } => {
            let Some(handle) = connections.get(&id) else {
                report_error(events, Some(id.clone()), EngineError::UnknownConnection(id)).await;
                return;
            };
            // Between attempts the task is alive and its channel open, so a
            // send would sit in the queue and go out much later against a peer
            // that has since changed. Refusing it reports the truth instead.
            if !handle.connected.load(Ordering::Relaxed) {
                report_error(events, Some(id.clone()), EngineError::ConnectionDown(id)).await;
                return;
            }
            // A closed channel means the connection task already exited on its
            // own (transport error, peer hang-up). Its notice is on its way to
            // the loop above, which owns removing the entry and announcing it;
            // dropping the entry here would swallow that announcement.
            if handle.outgoing_tx.send(bytes).await.is_err() {
                report_error(events, Some(id.clone()), EngineError::ConnectionDown(id)).await;
            }
        }
        Command::StartScenario { scenario, frames } => {
            let name = scenario.name.clone();
            if scenarios.contains_key(&name) {
                // Restarting means stopping first, so that a scenario cannot be
                // running twice under one name and emit everything in duplicate.
                report_error(events, None, EngineError::DuplicateScenario(name)).await;
                return;
            }
            let generation = *next_generation;
            *next_generation += 1;
            let handle = start_scenario(*scenario, frames, wiring, generation);
            scenarios.insert(name, handle);
        }
        Command::StopScenario { name } => match scenarios.remove(&name) {
            Some(handle) => {
                handle.task.abort();
                let _ = events
                    .send(Event::ScenarioFinished {
                        name,
                        outcome: Outcome::Stopped,
                    })
                    .await;
            }
            None => report_error(events, None, EngineError::UnknownScenario(name)).await,
        },
    }
}

fn start_scenario(
    scenario: Scenario,
    frames: Vec<FrameDef>,
    wiring: &Wiring,
    generation: u64,
) -> ScenarioHandle {
    let name = scenario.name.clone();
    let context = runner::Context {
        scenario,
        frames,
        commands: wiring.issue.clone(),
        events: wiring.events.clone(),
        // Subscribed before the first step runs, so a reply arriving between
        // the send and the wait is still seen.
        received: wiring.heard.subscribe(),
    };
    let ended = wiring.ended.clone();

    let task = tokio::spawn(async move {
        let outcome = runner::run(context).await;
        // The loop announces the ending once the name is free again, for the
        // same reason a connection does.
        let _ = ended.send((name, generation, outcome)).await;
    });

    ScenarioHandle { task, generation }
}

async fn report_error(events: &mpsc::Sender<Event>, id: Option<ConnectionId>, error: EngineError) {
    let _ = events.send(Event::Error { id, error }).await;
}

/// Why a connection attempt stopped running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The transport failed, or the peer went away. Worth another attempt.
    Link,
    /// The engine dropped the handle; there is nothing left to reconnect for.
    Locally,
}

fn start_connection(
    id: ConnectionId,
    config: TransportConfig,
    retry: Option<RetryPolicy>,
    events: mpsc::Sender<Event>,
    finished_tx: mpsc::Sender<(ConnectionId, u64)>,
    heard_tx: broadcast::Sender<Heard>,
    generation: u64,
) -> ConnectionHandle {
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Vec<u8>>(OUTGOING_CHANNEL_CAPACITY);
    let connected = Arc::new(AtomicBool::new(false));
    let task_connected = Arc::clone(&connected);

    let task = tokio::spawn(async move {
        // Counts consecutive failures, so it drives the backoff and the attempt
        // budget alike. A link that comes up clears it.
        let mut failures: u32 = 0;

        loop {
            // Emitted before every attempt, including a retry: from the outside
            // a connection that is backing off is a connection being opened.
            let _ = events
                .send(Event::ConnectionStatus {
                    id: id.clone(),
                    status: ConnectionStatus::Connecting,
                })
                .await;

            let outcome = match open_transport(config.clone()).await {
                Ok(mut transport) => {
                    failures = 0;
                    // A server that is merely bound is not connected yet;
                    // `run_connection` announces it as listening instead.
                    if transport.is_ready() {
                        task_connected.store(true, Ordering::Relaxed);
                        let _ = events
                            .send(Event::ConnectionStatus {
                                id: id.clone(),
                                status: ConnectionStatus::Connected,
                            })
                            .await;
                    }
                    let ended = run_connection(
                        id.clone(),
                        &mut transport,
                        &mut outgoing_rx,
                        &events,
                        &heard_tx,
                        &task_connected,
                    )
                    .await;
                    task_connected.store(false, Ordering::Relaxed);
                    ended
                }
                Err(source) => {
                    report_error(
                        &events,
                        Some(id.clone()),
                        EngineError::Transport {
                            id: id.clone(),
                            source,
                        },
                    )
                    .await;
                    Ended::Link
                }
            };

            let Some(policy) = retry else { break };
            if outcome == Ended::Locally || !policy.allows(failures) {
                break;
            }
            // Aborting the task cancels this sleep, so a manual disconnect is
            // never held up by a long backoff.
            tokio::time::sleep(policy.delay_after(failures)).await;
            failures = failures.saturating_add(1);
        }

        // The engine announces the disconnection when it picks this up, once the
        // name is free again. Announcing it from here would let a caller learn of
        // it while the slot is still held.
        let _ = finished_tx.send((id, generation)).await;
    });

    ConnectionHandle {
        task,
        outgoing_tx,
        connected,
        generation,
    }
}

async fn open_transport(config: TransportConfig) -> Result<TransportKind, TransportError> {
    match config {
        TransportConfig::Udp { bind, remote } => UdpTransport::bind(bind, remote)
            .await
            .map(TransportKind::Udp),
        TransportConfig::UdpMulticast { group, interface } => {
            UdpTransport::join_multicast(group, interface)
                .await
                .map(TransportKind::Udp)
        }
        TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        } => TcpTransport::connect(addr).await.map(TransportKind::Tcp),
        TransportConfig::Tcp {
            mode: TcpMode::Server { listen },
        } => TcpTransport::listen(listen).await.map(TransportKind::Tcp),
        TransportConfig::Serial {
            port_name,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
        } => SerialTransport::open(
            &port_name,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
        )
        .map(TransportKind::Serial),
    }
}

enum TransportKind {
    Udp(UdpTransport),
    Tcp(TcpTransport),
    Serial(SerialTransport),
}

impl Transport for TransportKind {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            TransportKind::Udp(t) => t.send(bytes).await,
            TransportKind::Tcp(t) => t.send(bytes).await,
            TransportKind::Serial(t) => t.send(bytes).await,
        }
    }

    async fn recv(&mut self) -> Result<Received, TransportError> {
        match self {
            TransportKind::Udp(t) => t.recv().await,
            TransportKind::Tcp(t) => t.recv().await,
            TransportKind::Serial(t) => t.recv().await,
        }
    }

    fn send_error_is_fatal(&self) -> bool {
        match self {
            TransportKind::Udp(t) => t.send_error_is_fatal(),
            TransportKind::Tcp(t) => t.send_error_is_fatal(),
            TransportKind::Serial(t) => t.send_error_is_fatal(),
        }
    }

    fn is_ready(&self) -> bool {
        match self {
            TransportKind::Udp(t) => t.is_ready(),
            TransportKind::Tcp(t) => t.is_ready(),
            TransportKind::Serial(t) => t.is_ready(),
        }
    }

    async fn relisten(&mut self) -> Result<bool, TransportError> {
        match self {
            TransportKind::Udp(t) => t.relisten().await,
            TransportKind::Tcp(t) => t.relisten().await,
            TransportKind::Serial(t) => t.relisten().await,
        }
    }
}

/// Pumps one open transport until it fails or the engine lets go of it.
///
/// The receiver is borrowed rather than owned because a retrying connection
/// reuses it across attempts.
async fn run_connection<T: Transport>(
    id: ConnectionId,
    transport: &mut T,
    outgoing_rx: &mut mpsc::Receiver<Vec<u8>>,
    events: &mpsc::Sender<Event>,
    heard: &broadcast::Sender<Heard>,
    connected: &AtomicBool,
) -> Ended {
    loop {
        // A server holds its port before anyone calls, and again after a peer
        // hangs up. Both are reported as `Listening` rather than left looking
        // half connected, and no send is accepted meanwhile.
        if !transport.is_ready() {
            connected.store(false, Ordering::Relaxed);
            let _ = events
                .send(Event::ConnectionStatus {
                    id: id.clone(),
                    status: ConnectionStatus::Listening,
                })
                .await;
            match transport.relisten().await {
                Ok(true) => {
                    connected.store(true, Ordering::Relaxed);
                    let _ = events
                        .send(Event::ConnectionStatus {
                            id: id.clone(),
                            status: ConnectionStatus::Connected,
                        })
                        .await;
                }
                // Nothing to accept on, so the link is done for good.
                Ok(false) => return Ended::Link,
                Err(source) => {
                    report_error(
                        events,
                        Some(id.clone()),
                        EngineError::Transport {
                            id: id.clone(),
                            source,
                        },
                    )
                    .await;
                    return Ended::Link;
                }
            }
        }

        tokio::select! {
            outgoing = outgoing_rx.recv() => {
                let Some(bytes) = outgoing else { return Ended::Locally };
                match transport.send(&bytes).await {
                    Ok(()) => {
                        let _ = events
                            .send(Event::FrameSent { id: id.clone(), bytes, timestamp: SystemTime::now() })
                            .await;
                    }
                    Err(source) => {
                        report_error(events, Some(id.clone()), EngineError::Transport { id: id.clone(), source }).await;
                        if transport.send_error_is_fatal() {
                            return Ended::Link;
                        }
                    }
                }
            }
            incoming = transport.recv() => {
                match incoming {
                    Ok(Received { bytes, source }) => {
                        // Republished for any scenario waiting on this link.
                        // An error only means nobody is listening.
                        let _ = heard.send((id.clone(), bytes.clone()));
                        let _ = events
                            .send(Event::FrameReceived { id: id.clone(), bytes, source, timestamp: SystemTime::now() })
                            .await;
                    }
                    Err(source) => {
                        report_error(events, Some(id.clone()), EngineError::Transport { id: id.clone(), source }).await;
                        // A transport that lost its peer reports itself as not
                        // ready, and the top of the loop waits for the next one.
                        // One that cannot recover stays ready and errors again,
                        // so end it here rather than spinning on a dead socket.
                        if transport.is_ready() {
                            return Ended::Link;
                        }
                    }
                }
            }
        }
    }
}
