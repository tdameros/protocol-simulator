use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::connection::{ConnectionId, ConnectionStatus, TcpMode, TransportConfig};
use crate::error::{EngineError, TransportError};
use crate::transport::serial::SerialTransport;
use crate::transport::tcp::TcpTransport;
use crate::transport::udp::UdpTransport;
use crate::transport::{Received, Transport};

const COMMAND_CHANNEL_CAPACITY: usize = 256;
const EVENT_CHANNEL_CAPACITY: usize = 1024;
const OUTGOING_CHANNEL_CAPACITY: usize = 256;

pub enum Command {
    Connect {
        id: ConnectionId,
        config: TransportConfig,
    },
    Disconnect {
        id: ConnectionId,
    },
    SendRaw {
        id: ConnectionId,
        bytes: Vec<u8>,
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

        std::thread::Builder::new()
            .name("sim-engine".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build engine tokio runtime");
                runtime.block_on(run(command_rx, event_tx));
            })
            .expect("failed to spawn engine thread");

        (command_tx, event_rx)
    }
}

struct ConnectionHandle {
    task: JoinHandle<()>,
    outgoing_tx: mpsc::Sender<Vec<u8>>,
    generation: u64,
}

async fn run(mut commands: mpsc::Receiver<Command>, events: mpsc::Sender<Event>) {
    let mut connections: HashMap<ConnectionId, ConnectionHandle> = HashMap::new();
    let (finished_tx, mut finished_rx) =
        mpsc::channel::<(ConnectionId, u64)>(EVENT_CHANNEL_CAPACITY);
    let mut next_generation: u64 = 0;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                handle_command(
                    command,
                    &mut connections,
                    &events,
                    &finished_tx,
                    &mut next_generation,
                )
                .await;
            }
            // A task that ended on its own frees its name straight away, so the
            // same connection can be recreated without a manual disconnect first.
            // The generation guards against a stale notice evicting the newer
            // connection that has since taken the name.
            Some((id, generation)) = finished_rx.recv() => {
                if connections.get(&id).is_some_and(|handle| handle.generation == generation) {
                    connections.remove(&id);
                }
            }
        }
    }
}

async fn handle_command(
    command: Command,
    connections: &mut HashMap<ConnectionId, ConnectionHandle>,
    events: &mpsc::Sender<Event>,
    finished_tx: &mpsc::Sender<(ConnectionId, u64)>,
    next_generation: &mut u64,
) {
    match command {
        Command::Connect { id, config } => {
            // An entry whose task already exited is just garbage awaiting its
            // notification on `finished_rx`. Checking the task directly keeps
            // reconnecting deterministic: a caller acting the instant it sees
            // `Disconnected` would otherwise race that notification and be told
            // the name is taken.
            let stale = connections
                .get(&id)
                .is_some_and(|handle| handle.task.is_finished());
            if stale {
                connections.remove(&id);
            }

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
                events.clone(),
                finished_tx.clone(),
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
            // A closed channel means the connection task already exited on its
            // own (transport error, peer hang-up). Drop the dead entry rather
            // than leaving it around to fail every later send.
            if handle.outgoing_tx.send(bytes).await.is_err() {
                connections.remove(&id);
                report_error(events, Some(id.clone()), EngineError::ConnectionDown(id)).await;
            }
        }
    }
}

async fn report_error(events: &mpsc::Sender<Event>, id: Option<ConnectionId>, error: EngineError) {
    let _ = events.send(Event::Error { id, error }).await;
}

fn start_connection(
    id: ConnectionId,
    config: TransportConfig,
    events: mpsc::Sender<Event>,
    finished_tx: mpsc::Sender<(ConnectionId, u64)>,
    generation: u64,
) -> ConnectionHandle {
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Vec<u8>>(OUTGOING_CHANNEL_CAPACITY);

    let task = tokio::spawn(async move {
        let _ = events
            .send(Event::ConnectionStatus {
                id: id.clone(),
                status: ConnectionStatus::Connecting,
            })
            .await;

        match open_transport(config).await {
            Ok(mut transport) => {
                let _ = events
                    .send(Event::ConnectionStatus {
                        id: id.clone(),
                        status: ConnectionStatus::Connected,
                    })
                    .await;
                run_connection(id.clone(), &mut transport, outgoing_rx, &events).await;
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
            }
        }

        let _ = events
            .send(Event::ConnectionStatus {
                id: id.clone(),
                status: ConnectionStatus::Disconnected,
            })
            .await;
        let _ = finished_tx.send((id, generation)).await;
    });

    ConnectionHandle {
        task,
        outgoing_tx,
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
}

async fn run_connection<T: Transport>(
    id: ConnectionId,
    transport: &mut T,
    mut outgoing_rx: mpsc::Receiver<Vec<u8>>,
    events: &mpsc::Sender<Event>,
) {
    loop {
        tokio::select! {
            outgoing = outgoing_rx.recv() => {
                let Some(bytes) = outgoing else { break };
                match transport.send(&bytes).await {
                    Ok(()) => {
                        let _ = events
                            .send(Event::FrameSent { id: id.clone(), bytes, timestamp: SystemTime::now() })
                            .await;
                    }
                    Err(source) => {
                        report_error(events, Some(id.clone()), EngineError::Transport { id: id.clone(), source }).await;
                        if transport.send_error_is_fatal() {
                            break;
                        }
                    }
                }
            }
            incoming = transport.recv() => {
                match incoming {
                    Ok(Received { bytes, source }) => {
                        let _ = events
                            .send(Event::FrameReceived { id: id.clone(), bytes, source, timestamp: SystemTime::now() })
                            .await;
                    }
                    Err(source) => {
                        report_error(events, Some(id.clone()), EngineError::Transport { id: id.clone(), source }).await;
                        break;
                    }
                }
            }
        }
    }
}
