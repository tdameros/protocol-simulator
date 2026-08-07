use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::SystemTime;

pub use sim_core::pattern::{Anchor as HexAnchor, HexPattern};
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
    /// Traffic tabs, each a different view of the one shared buffer.
    pub monitors: BTreeMap<MonitorId, MonitorState>,
    next_monitor: usize,
    next_seq: u64,
    /// A tab the panel asked for. The dock cannot be touched while it draws, so
    /// the request waits for the frame to end.
    pub monitor_requested: bool,
    /// Bytes a traffic row sent to the frame editor, waiting to be decoded.
    pub pending_frame_hex: Option<Vec<u8>>,
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

    /// Appends a frame to the shared buffer, stamping it with its position.
    pub fn push_log(&mut self, mut entry: LogEntry) {
        entry.seq = self.next_seq;
        self.next_seq += 1;
        if self.log.len() == MAX_LOG_ENTRIES {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }

    /// Sequence number the next frame will carry.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn open_monitor(&mut self) -> MonitorId {
        self.next_monitor += 1;
        let id = MonitorId(self.next_monitor);
        let title = if self.next_monitor == 1 {
            "Traffic".to_owned()
        } else {
            format!("Traffic {}", self.next_monitor)
        };
        self.monitors
            .insert(id, MonitorState::new(title, self.next_seq));
        id
    }

    pub fn close_monitor(&mut self, id: MonitorId) {
        self.monitors.remove(&id);
    }

    /// Installs the tabs a project came with, and resumes numbering after them
    /// so a tab opened later cannot collide with one that was restored.
    pub fn restore_monitors(&mut self, monitors: BTreeMap<MonitorId, MonitorState>) {
        self.next_monitor = monitors.keys().map(|id| id.0).max().unwrap_or(0);
        self.monitors = monitors;
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
    /// Whether opening the project this belongs to should open the connection.
    pub autoconnect: bool,
}

pub struct LogEntry {
    /// Position in the stream, never reused.
    ///
    /// The buffer drops its oldest entries, so an index into it means something
    /// different a minute later; a monitor remembering where it paused needs a
    /// number that stays put.
    pub seq: u64,
    pub id: ConnectionId,
    pub direction: Direction,
    pub bytes: Vec<u8>,
    pub source: Option<SocketAddr>,
    pub timestamp: SystemTime,
}

/// One Traffic tab. Several may watch the same buffer through different filters.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct MonitorId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectionFilter {
    #[default]
    Both,
    Sent,
    Received,
}

impl DirectionFilter {
    pub const ALL: [Self; 3] = [Self::Both, Self::Sent, Self::Received];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Sent => "sent",
            Self::Received => "received",
        }
    }

    fn wanted(self) -> Option<Direction> {
        match self {
            Self::Both => None,
            Self::Sent => Some(Direction::Sent),
            Self::Received => Some(Direction::Received),
        }
    }
}

/// What a Traffic tab shows, as the operator set it up.
#[derive(Debug, Default, Clone)]
pub struct TrafficFilter {
    /// Empty means every connection, which is also the state of a fresh tab.
    pub connections: BTreeSet<String>,
    pub direction: DirectionFilter,
    /// Substring of the peer address. An entry with no source never matches a
    /// non-empty one, which is what makes it useful on a multicast group.
    pub source: String,
    pub min_len: Option<usize>,
    pub max_len: Option<usize>,
    pub hex: String,
    pub anchor: HexAnchor,
    /// Looked for in the frame bytes, not in the dotted rendering, so a match
    /// means the characters really are there.
    pub text: String,
    /// Keep what matches, or drop it. Inverting is how you hide heartbeats.
    pub invert: bool,
}

impl TrafficFilter {
    /// Resolves everything that costs more than a comparison, once per repaint
    /// rather than once per entry.
    #[must_use]
    pub fn compile(&self) -> CompiledFilter<'_> {
        CompiledFilter {
            filter: self,
            pattern: HexPattern::parse(&self.hex),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.connections.is_empty()
            || self.direction != DirectionFilter::Both
            || !self.source.trim().is_empty()
            || self.min_len.is_some()
            || self.max_len.is_some()
            || !self.hex.trim().is_empty()
            || !self.text.is_empty()
    }
}

pub struct CompiledFilter<'a> {
    filter: &'a TrafficFilter,
    pattern: Option<HexPattern>,
}

impl CompiledFilter<'_> {
    #[must_use]
    pub fn keeps(&self, entry: &LogEntry) -> bool {
        self.matches(entry) != self.filter.invert
    }

    /// Whether the typed pattern is usable. A broken one is ignored rather than
    /// left to silently hide the whole stream.
    #[must_use]
    pub fn hex_is_valid(&self) -> bool {
        self.filter.hex.trim().is_empty() || self.pattern.is_some()
    }

    fn matches(&self, entry: &LogEntry) -> bool {
        let filter = self.filter;

        if !filter.connections.is_empty() && !filter.connections.contains(&entry.id.0) {
            return false;
        }
        if filter
            .direction
            .wanted()
            .is_some_and(|wanted| wanted != entry.direction)
        {
            return false;
        }

        let source = filter.source.trim();
        if !source.is_empty()
            && !entry
                .source
                .is_some_and(|addr| addr.to_string().contains(source))
        {
            return false;
        }

        if filter.min_len.is_some_and(|min| entry.bytes.len() < min)
            || filter.max_len.is_some_and(|max| entry.bytes.len() > max)
        {
            return false;
        }

        if let Some(pattern) = &self.pattern {
            if !pattern.found_in(&entry.bytes, filter.anchor) {
                return false;
            }
        }

        if !filter.text.is_empty() {
            let needle = filter.text.as_bytes();
            if !entry
                .bytes
                .windows(needle.len())
                .any(|window| window == needle)
            {
                return false;
            }
        }

        true
    }
}

pub struct MonitorState {
    pub title: String,
    pub filter: TrafficFilter,
    pub show_filter: bool,
    pub follow: bool,
    /// The last sequence number admitted, frozen while paused.
    pub paused_at: Option<u64>,
    /// Hides everything logged before this view was last cleared.
    pub since: u64,
}

impl MonitorState {
    /// A view of everything the buffer holds, for a tab that is not being
    /// opened in the middle of a running session.
    #[must_use]
    pub fn named(title: String) -> Self {
        Self::new(title, 0)
    }

    fn new(title: String, since: u64) -> Self {
        Self {
            title,
            filter: TrafficFilter::default(),
            show_filter: false,
            follow: true,
            paused_at: None,
            since,
        }
    }

    /// Whether an entry is inside this view's window, filters aside.
    #[must_use]
    pub fn in_window(&self, entry: &LogEntry) -> bool {
        entry.seq >= self.since && self.paused_at.is_none_or(|last| entry.seq <= last)
    }
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
    pub autoconnect: bool,
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
            // On by default: a connection you just set up is one you want back
            // the next time you open the project.
            autoconnect: true,
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

    fn logged(id: &str, direction: Direction, bytes: &[u8], source: Option<&str>) -> LogEntry {
        LogEntry {
            seq: 0,
            id: ConnectionId(id.to_owned()),
            direction,
            bytes: bytes.to_vec(),
            source: source.map(|addr| addr.parse().unwrap()),
            timestamp: SystemTime::now(),
        }
    }

    fn sample() -> Vec<LogEntry> {
        vec![
            logged("bus", Direction::Sent, &[0xAA, 0x55, 0x01, 0x02], None),
            logged(
                "bus",
                Direction::Received,
                &[0xAA, 0x55, 0x02, 0x02],
                Some("192.168.1.10:9000"),
            ),
            logged(
                "serial",
                Direction::Received,
                b"AT+OK\r\n",
                Some("10.0.0.4:9000"),
            ),
        ]
    }

    fn kept(filter: &TrafficFilter) -> Vec<usize> {
        let compiled = filter.compile();
        sample()
            .iter()
            .enumerate()
            .filter(|(_, entry)| compiled.keeps(entry))
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn an_untouched_filter_keeps_everything() {
        let filter = TrafficFilter::default();
        assert!(!filter.is_active());
        assert_eq!(kept(&filter), [0, 1, 2]);
    }

    #[test]
    fn connection_and_direction_narrow_independently() {
        let mut filter = TrafficFilter {
            connections: BTreeSet::from(["bus".to_owned()]),
            ..TrafficFilter::default()
        };
        assert_eq!(kept(&filter), [0, 1]);

        filter.direction = DirectionFilter::Received;
        assert_eq!(kept(&filter), [1]);
    }

    #[test]
    fn inverting_hides_what_it_would_have_shown() {
        let filter = TrafficFilter {
            connections: BTreeSet::from(["bus".to_owned()]),
            invert: true,
            ..TrafficFilter::default()
        };
        // Which is how a heartbeat gets out of the way.
        assert_eq!(kept(&filter), [2]);
    }

    #[test]
    fn a_hex_pattern_matches_with_wildcards_and_can_be_anchored() {
        let mut filter = TrafficFilter {
            hex: "AA 55 ?? 02".to_owned(),
            ..TrafficFilter::default()
        };
        assert!(filter.compile().hex_is_valid());
        assert_eq!(kept(&filter), [0, 1]);

        // The same pattern one byte along matches nothing.
        filter.anchor = HexAnchor::At(1);
        assert_eq!(kept(&filter), Vec::<usize>::new());

        filter.anchor = HexAnchor::At(0);
        assert_eq!(kept(&filter), [0, 1]);
    }

    #[test]
    fn a_pattern_can_sit_anywhere_in_the_frame() {
        let filter = TrafficFilter {
            hex: "0102".to_owned(),
            ..TrafficFilter::default()
        };
        assert_eq!(kept(&filter), [0]);
    }

    #[test]
    fn a_broken_pattern_is_ignored_rather_than_hiding_the_stream() {
        let filter = TrafficFilter {
            // Half a byte typed, or a nibble wildcard, which is refused.
            hex: "AA 5".to_owned(),
            ..TrafficFilter::default()
        };
        assert!(!filter.compile().hex_is_valid());
        assert_eq!(
            kept(&filter),
            [0, 1, 2],
            "an unusable pattern filters nothing"
        );
    }

    #[test]
    fn source_and_length_narrow_the_view() {
        let by_source = TrafficFilter {
            source: "192.168".to_owned(),
            ..TrafficFilter::default()
        };
        // The sent frame has no peer, so it cannot match a source filter.
        assert_eq!(kept(&by_source), [1]);

        let by_length = TrafficFilter {
            min_len: Some(5),
            ..TrafficFilter::default()
        };
        assert_eq!(kept(&by_length), [2]);
    }

    #[test]
    fn text_is_looked_for_in_the_bytes_themselves() {
        let filter = TrafficFilter {
            text: "AT+".to_owned(),
            ..TrafficFilter::default()
        };
        assert_eq!(kept(&filter), [2]);
    }

    #[test]
    fn a_paused_monitor_stops_at_the_frame_it_froze_on() {
        let mut state = AppState::default();
        let id = state.open_monitor();
        for _ in 0..3 {
            state.push_log(logged("bus", Direction::Sent, &[1], None));
        }

        let monitor = state.monitors.get_mut(&id).unwrap();
        monitor.paused_at = Some(1);
        let monitor = &state.monitors[&id];
        let visible = state.log.iter().filter(|e| monitor.in_window(e)).count();
        assert_eq!(visible, 2, "frames 0 and 1, not the one that came after");

        // Clearing the view hides what came before without touching the buffer.
        let monitor = state.monitors.get_mut(&id).unwrap();
        monitor.paused_at = None;
        monitor.since = 2;
        let monitor = &state.monitors[&id];
        assert_eq!(state.log.len(), 3);
        assert_eq!(state.log.iter().filter(|e| monitor.in_window(e)).count(), 1);
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
                    autoconnect: true,
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
                seq: 0,
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
        // And the stamp survives the drop, which is why a monitor can rely on it.
        assert_eq!(state.log.front().unwrap().seq, 50);
    }
}
