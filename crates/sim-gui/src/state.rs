use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::SystemTime;

use sim_core::{
    ConnectionId, ConnectionStatus, EngineError, RetryPolicy, TcpMode, TransportConfig,
};
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

/// Cap on retained traffic entries.
///
/// A periodic frame at 100 Hz produces 360k entries an hour, so the log has to
/// forget or it grows without bound for as long as the app is left running.
pub const MAX_LOG_ENTRIES: usize = 10_000;

#[derive(Default)]
pub struct AppState {
    pub connections: Vec<(ConnectionId, ConnectionEntry)>,
    pub log: VecDeque<LogEntry>,
    pub new_connection: NewConnectionForm,
    pub hex_input: String,
    pub hex_target: Option<ConnectionId>,
    pub frames: crate::frames::FrameLibrary,
    /// Text of the frame editor's hex preview while it is being typed into.
    ///
    /// Held apart from the fields because the two disagree mid-edit: half a
    /// byte typed is not a frame yet, and overwriting the box with the
    /// re-encoded bytes on every repaint would fight whoever is typing.
    pub frame_hex: String,
    /// Why the typed hex was not applied, or what it changed on the way in.
    pub frame_hex_note: Option<String>,
    pub frame_target: Option<ConnectionId>,
    pub last_error: Option<String>,
}

impl AppState {
    pub fn connection_mut(&mut self, id: &ConnectionId) -> Option<&mut ConnectionEntry> {
        self.connections
            .iter_mut()
            .find(|(cid, _)| cid == id)
            .map(|(_, entry)| entry)
    }

    /// Puts a dropped connection back into `Connecting` and hands back the
    /// config it was created with, so reopening it costs one click instead of
    /// deleting the entry and typing everything again.
    ///
    /// Returns `None` for a connection that is not down, which is what makes
    /// the button safe to press twice.
    pub fn begin_reconnect(
        &mut self,
        id: &ConnectionId,
    ) -> Option<(TransportConfig, Option<RetryPolicy>)> {
        let entry = self.connection_mut(id)?;
        if entry.status != ConnectionStatus::Disconnected {
            return None;
        }
        entry.status = ConnectionStatus::Connecting;
        Some((entry.config.clone(), entry.retry))
    }

    pub fn remove_connection(&mut self, id: &ConnectionId) {
        self.connections.retain(|(cid, _)| cid != id);
        if self.hex_target.as_ref() == Some(id) {
            self.hex_target = None;
        }
    }

    pub fn record_error(&mut self, id: Option<ConnectionId>, error: &EngineError) {
        self.last_error = Some(match id {
            Some(id) => format!("[{id}] {error}"),
            None => error.to_string(),
        });
    }

    pub fn push_log(&mut self, entry: LogEntry) {
        if self.log.len() == MAX_LOG_ENTRIES {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }

    pub fn status_of(&self, id: &ConnectionId) -> Option<ConnectionStatus> {
        self.connections
            .iter()
            .find(|(cid, _)| cid == id)
            .map(|(_, entry)| entry.status)
    }
}

pub struct ConnectionEntry {
    pub config: TransportConfig,
    pub status: ConnectionStatus,
    /// Kept so a manual reconnect reuses the policy the connection was made
    /// with, rather than silently dropping it.
    pub retry: Option<RetryPolicy>,
}

pub struct LogEntry {
    pub id: ConnectionId,
    pub direction: Direction,
    pub bytes: Vec<u8>,
    pub source: Option<SocketAddr>,
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKindChoice {
    Udp,
    TcpClient,
    TcpServer,
    Serial,
}

impl TransportKindChoice {
    pub const ALL: [Self; 4] = [Self::Udp, Self::TcpClient, Self::TcpServer, Self::Serial];

    pub fn label(self) -> &'static str {
        match self {
            Self::Udp => "UDP",
            Self::TcpClient => "TCP (client)",
            Self::TcpServer => "TCP (server)",
            Self::Serial => "RS232 / Serial",
        }
    }
}

pub struct NewConnectionForm {
    pub name: String,
    pub kind: TransportKindChoice,
    pub udp_bind: String,
    pub udp_remote: String,
    pub multicast_interface: Ipv4Addr,
    pub tcp_addr: String,
    pub serial_port: String,
    pub serial_baud: String,
    pub serial_data_bits: DataBits,
    pub serial_parity: Parity,
    pub serial_stop_bits: StopBits,
    pub serial_flow_control: FlowControl,
    pub auto_reconnect: bool,
    pub error: Option<String>,
}

impl Default for NewConnectionForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: TransportKindChoice::Udp,
            udp_bind: "127.0.0.1:9000".to_owned(),
            udp_remote: "127.0.0.1:9001".to_owned(),
            multicast_interface: Ipv4Addr::UNSPECIFIED,
            tcp_addr: "127.0.0.1:9000".to_owned(),
            serial_port: String::new(),
            serial_baud: "115200".to_owned(),
            serial_data_bits: DataBits::Eight,
            serial_parity: Parity::None,
            serial_stop_bits: StopBits::One,
            serial_flow_control: FlowControl::None,
            auto_reconnect: false,
            error: None,
        }
    }
}

impl NewConnectionForm {
    /// The multicast group currently typed in the UDP destination field, if any.
    ///
    /// Drives the form layout: a multicast destination hides the bind field (the
    /// port is dictated by the group) and reveals the interface picker.
    pub fn udp_multicast_group(&self) -> Option<SocketAddrV4> {
        match self.udp_remote.trim().parse::<SocketAddr>() {
            Ok(SocketAddr::V4(addr)) if addr.ip().is_multicast() => Some(addr),
            _ => None,
        }
    }

    /// Validates the current form and, if valid, returns the connection to create.
    ///
    /// On failure, `self.error` is set to a message describing the problem.
    pub fn build(
        &mut self,
        existing: &[(ConnectionId, ConnectionEntry)],
    ) -> Option<(ConnectionId, TransportConfig)> {
        self.error = None;

        let name = self.name.trim();
        if name.is_empty() {
            self.error = Some("Connection name is required.".to_owned());
            return None;
        }
        let id = ConnectionId(name.to_owned());
        if existing.iter().any(|(cid, _)| cid == &id) {
            self.error = Some(format!("A connection named \"{name}\" already exists."));
            return None;
        }

        let config = match self.kind {
            TransportKindChoice::Udp => {
                if let Some(group) = self.udp_multicast_group() {
                    TransportConfig::UdpMulticast {
                        group,
                        interface: self.multicast_interface,
                    }
                } else {
                    let remote = self.parse_addr(&self.udp_remote.clone(), "remote")?;
                    if remote.ip().is_multicast() {
                        self.error = Some(
                            "IPv6 multicast is not supported yet; use an IPv4 group.".to_owned(),
                        );
                        return None;
                    }
                    let bind = self.parse_addr(&self.udp_bind.clone(), "bind")?;
                    TransportConfig::Udp { bind, remote }
                }
            }
            TransportKindChoice::TcpClient => {
                let addr = self.parse_addr(&self.tcp_addr.clone(), "remote")?;
                TransportConfig::Tcp {
                    mode: TcpMode::Client { addr },
                }
            }
            TransportKindChoice::TcpServer => {
                let listen = self.parse_addr(&self.tcp_addr.clone(), "listen")?;
                TransportConfig::Tcp {
                    mode: TcpMode::Server { listen },
                }
            }
            TransportKindChoice::Serial => {
                if self.serial_port.trim().is_empty() {
                    self.error = Some("Serial port is required.".to_owned());
                    return None;
                }
                let Ok(baud_rate) = self.serial_baud.trim().parse::<u32>() else {
                    self.error = Some(format!("Invalid baud rate: \"{}\".", self.serial_baud));
                    return None;
                };
                TransportConfig::Serial {
                    port_name: self.serial_port.trim().to_owned(),
                    baud_rate,
                    data_bits: self.serial_data_bits,
                    parity: self.serial_parity,
                    stop_bits: self.serial_stop_bits,
                    flow_control: self.serial_flow_control,
                }
            }
        };

        Some((id, config))
    }

    fn parse_addr(&mut self, raw: &str, label: &str) -> Option<SocketAddr> {
        if let Ok(addr) = raw.trim().parse::<SocketAddr>() {
            return Some(addr);
        }
        self.error = Some(format!(
            "Invalid {label} address: \"{raw}\". Expected format ip:port."
        ));
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_with_remote(remote: &str) -> NewConnectionForm {
        NewConnectionForm {
            udp_remote: remote.to_owned(),
            ..NewConnectionForm::default()
        }
    }

    #[test]
    fn a_dropped_connection_reconnects_with_the_settings_it_had() {
        let id = ConnectionId("link".to_owned());
        let mut state = AppState {
            connections: vec![(
                id.clone(),
                ConnectionEntry {
                    config: TransportConfig::Udp {
                        bind: "127.0.0.1:9000".parse().unwrap(),
                        remote: "127.0.0.1:9001".parse().unwrap(),
                    },
                    status: ConnectionStatus::Connected,
                    retry: Some(RetryPolicy::standard()),
                },
            )],
            ..AppState::default()
        };

        // Nothing to reconnect while it is up, so the button cannot restart a
        // live connection behind your back.
        assert!(state.begin_reconnect(&id).is_none());

        state.connection_mut(&id).unwrap().status = ConnectionStatus::Disconnected;
        // The policy it was created with comes back with the config.
        assert!(matches!(
            state.begin_reconnect(&id),
            Some((TransportConfig::Udp { .. }, Some(policy))) if policy == RetryPolicy::standard()
        ));
        assert_eq!(state.status_of(&id), Some(ConnectionStatus::Connecting));

        // And now that it is connecting again, pressing twice is a no-op.
        assert!(state.begin_reconnect(&id).is_none());
        assert!(state
            .begin_reconnect(&ConnectionId("gone".to_owned()))
            .is_none());
    }

    #[test]
    fn detects_ipv4_multicast_range() {
        for addr in ["224.0.0.1:9000", "239.255.42.99:1234", "232.1.2.3:5"] {
            assert!(
                form_with_remote(addr).udp_multicast_group().is_some(),
                "{addr} should be detected as multicast"
            );
        }
    }

    #[test]
    fn leaves_unicast_and_boundaries_alone() {
        // 223.x and 240.x sit just outside 224.0.0.0/4.
        for addr in [
            "127.0.0.1:9000",
            "192.168.1.50:9000",
            "223.255.255.255:9000",
            "240.0.0.1:9000",
        ] {
            assert!(
                form_with_remote(addr).udp_multicast_group().is_none(),
                "{addr} should not be detected as multicast"
            );
        }
    }

    #[test]
    fn ignores_incomplete_input() {
        for addr in ["", "239.1.1.1", "not an address", "239.1.1.1:"] {
            assert!(form_with_remote(addr).udp_multicast_group().is_none());
        }
    }

    #[test]
    fn multicast_derives_bind_port_from_group() {
        let mut form = form_with_remote("239.1.1.1:5000");
        form.name = "mc".to_owned();
        let (_, config) = form.build(&[]).expect("form should validate");
        match config {
            TransportConfig::UdpMulticast { group, .. } => assert_eq!(group.port(), 5000),
            other => panic!("expected multicast config, got {other:?}"),
        }
    }

    #[test]
    fn ipv6_multicast_is_rejected_with_a_clear_message() {
        let mut form = form_with_remote("[ff02::1]:9000");
        form.name = "v6".to_owned();
        assert!(form.build(&[]).is_none());
        assert!(form.error.is_some_and(|e| e.contains("IPv6")));
    }

    #[test]
    fn log_forgets_oldest_entries_past_the_cap() {
        let mut state = AppState::default();
        for n in 0..MAX_LOG_ENTRIES + 50 {
            state.push_log(LogEntry {
                id: ConnectionId::from("c"),
                direction: Direction::Sent,
                bytes: vec![u8::try_from(n % 256).unwrap()],
                source: None,
                timestamp: SystemTime::now(),
            });
        }
        assert_eq!(state.log.len(), MAX_LOG_ENTRIES);
        // The 50 oldest were dropped, so the buffer now starts at entry 50.
        assert_eq!(state.log.front().unwrap().bytes, vec![50]);
    }
}
