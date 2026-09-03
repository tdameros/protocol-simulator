//! Adding a connection, a field at a time.
//!
//! `sim_session::state::NewConnectionForm` already knows what a valid
//! connection is; this only decides which of its fields are worth asking for,
//! given the transport picked, and how a key moves between them.

use std::net::Ipv4Addr;

use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

use sim_session::links;
use sim_session::state::{ConnectionEntry, NewConnectionForm, TransportKindChoice};

/// One field of the form: what it is called, what it holds, and how a key
/// changes it.
pub enum Field {
    Kind,
    Name,
    UdpRemote,
    UdpBind,
    Interface,
    TcpAddr,
    SerialPort,
    SerialBaud,
    SerialDataBits,
    SerialParity,
    SerialStopBits,
    SerialFlowControl,
    AutoReconnect,
    Autoconnect,
}

impl Field {
    fn label(&self) -> &'static str {
        match self {
            Self::Kind => "Type",
            Self::Name => "Name",
            Self::UdpRemote => "Remote/Group",
            Self::UdpBind => "Bind (local)",
            Self::Interface => "Interface",
            Self::TcpAddr => "Address",
            Self::SerialPort => "Port",
            Self::SerialBaud => "Baud rate",
            Self::SerialDataBits => "Data bits",
            Self::SerialParity => "Parity",
            Self::SerialStopBits => "Stop bits",
            Self::SerialFlowControl => "Flow control",
            Self::AutoReconnect => "Reopens itself when the link drops",
            Self::Autoconnect => "Open when the project is loaded",
        }
    }
}

const DATA_BITS: [DataBits; 4] = [
    DataBits::Five,
    DataBits::Six,
    DataBits::Seven,
    DataBits::Eight,
];
const PARITY: [Parity; 3] = [Parity::None, Parity::Odd, Parity::Even];
const STOP_BITS: [StopBits; 2] = [StopBits::One, StopBits::Two];
const FLOW_CONTROL: [FlowControl; 3] = [
    FlowControl::None,
    FlowControl::Software,
    FlowControl::Hardware,
];

/// One step along a short, fixed list, wrapping at either end.
#[expect(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "every list this cycles is a handful of transport or serial settings"
)]
fn cycle<T: Copy + PartialEq>(options: &[T], current: T, delta: isize) -> T {
    let at = options
        .iter()
        .position(|option| *option == current)
        .unwrap_or(0);
    let next = (at as isize + delta).rem_euclid(options.len() as isize);
    options[next as usize]
}

#[derive(Default)]
pub struct ConnectionForm {
    form: NewConnectionForm,
    focus: usize,
}

impl ConnectionForm {
    /// What went wrong the last time this was submitted, if anything still
    /// stands.
    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.form.error.as_deref()
    }

    /// The fields worth asking for, given the transport currently picked.
    ///
    /// Recomputed rather than stored: typing a multicast address into `Remote`
    /// changes which fields make sense without a separate step to notice it.
    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name, Field::Kind];
        match self.form.kind {
            TransportKindChoice::Udp => {
                fields.push(Field::UdpRemote);
                if self.form.udp_multicast_group().is_some() {
                    fields.push(Field::Interface);
                } else {
                    fields.push(Field::UdpBind);
                }
            }
            TransportKindChoice::TcpClient | TransportKindChoice::TcpServer => {
                fields.push(Field::TcpAddr);
            }
            TransportKindChoice::Serial => {
                fields.extend([
                    Field::SerialPort,
                    Field::SerialBaud,
                    Field::SerialDataBits,
                    Field::SerialParity,
                    Field::SerialStopBits,
                    Field::SerialFlowControl,
                ]);
            }
        }
        fields.extend([Field::AutoReconnect, Field::Autoconnect]);
        fields
    }

    /// Each field as a line: its label, what it holds, and whether it is the
    /// one a key would change.
    #[must_use]
    pub fn lines(&self) -> Vec<(String, String, bool)> {
        let fields = self.fields();
        let focus = self.focus.min(fields.len().saturating_sub(1));
        fields
            .iter()
            .enumerate()
            .map(|(at, field)| (field.label().to_owned(), self.shown(field), at == focus))
            .collect()
    }

    fn shown(&self, field: &Field) -> String {
        match field {
            Field::Kind => self.form.kind.label().to_owned(),
            Field::Name => self.form.name.clone(),
            Field::UdpRemote => self.form.udp_remote.clone(),
            Field::UdpBind => self.form.udp_bind.clone(),
            Field::Interface => {
                if self.form.multicast_interface.is_unspecified() {
                    "auto (0.0.0.0)".to_owned()
                } else {
                    self.form.multicast_interface.to_string()
                }
            }
            Field::TcpAddr => self.form.tcp_addr.clone(),
            Field::SerialPort => self.form.serial_port.clone(),
            Field::SerialBaud => self.form.serial_baud.clone(),
            Field::SerialDataBits => self.form.serial_data_bits.to_string(),
            Field::SerialParity => self.form.serial_parity.to_string(),
            Field::SerialStopBits => self.form.serial_stop_bits.to_string(),
            Field::SerialFlowControl => self.form.serial_flow_control.to_string(),
            Field::AutoReconnect => yes_no(self.form.auto_reconnect).to_owned(),
            Field::Autoconnect => yes_no(self.form.autoconnect).to_owned(),
        }
    }

    pub fn next(&mut self) {
        self.focus = (self.focus + 1) % self.fields().len().max(1);
    }

    pub fn previous(&mut self) {
        let held = self.fields().len().max(1);
        self.focus = (self.focus + held - 1) % held;
    }

    /// Cycles the field under focus, for a field with more than two shapes to
    /// be. Does nothing on a field typed or toggled instead.
    pub fn cycle(&mut self, delta: isize) {
        let fields = self.fields();
        match fields.get(self.focus.min(fields.len().saturating_sub(1))) {
            Some(Field::Kind) => {
                self.form.kind = cycle(&TransportKindChoice::ALL, self.form.kind, delta);
            }
            Some(Field::Interface) => {
                let known: Vec<Ipv4Addr> = std::iter::once(Ipv4Addr::UNSPECIFIED)
                    .chain(links::interfaces().into_iter().map(|(_, addr)| addr))
                    .collect();
                self.form.multicast_interface = cycle(&known, self.form.multicast_interface, delta);
            }
            Some(Field::SerialDataBits) => {
                self.form.serial_data_bits = cycle(&DATA_BITS, self.form.serial_data_bits, delta);
            }
            Some(Field::SerialParity) => {
                self.form.serial_parity = cycle(&PARITY, self.form.serial_parity, delta);
            }
            Some(Field::SerialStopBits) => {
                self.form.serial_stop_bits = cycle(&STOP_BITS, self.form.serial_stop_bits, delta);
            }
            Some(Field::SerialFlowControl) => {
                self.form.serial_flow_control =
                    cycle(&FLOW_CONTROL, self.form.serial_flow_control, delta);
            }
            _ => {}
        }
    }

    /// Flips the field under focus, for one that is only ever yes or no.
    pub fn toggle(&mut self) {
        let fields = self.fields();
        match fields.get(self.focus.min(fields.len().saturating_sub(1))) {
            Some(Field::AutoReconnect) => self.form.auto_reconnect = !self.form.auto_reconnect,
            Some(Field::Autoconnect) => self.form.autoconnect = !self.form.autoconnect,
            _ => {}
        }
    }

    pub fn type_char(&mut self, letter: char) {
        if let Some(text) = self.text_mut() {
            text.push(letter);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(text) = self.text_mut() {
            text.pop();
        }
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        let fields = self.fields();
        match fields.get(self.focus.min(fields.len().saturating_sub(1)))? {
            Field::Name => Some(&mut self.form.name),
            Field::UdpRemote => Some(&mut self.form.udp_remote),
            Field::UdpBind => Some(&mut self.form.udp_bind),
            Field::TcpAddr => Some(&mut self.form.tcp_addr),
            Field::SerialPort => Some(&mut self.form.serial_port),
            Field::SerialBaud => Some(&mut self.form.serial_baud),
            _ => None,
        }
    }

    /// Validates the form against the connections already held, and builds
    /// the one it describes.
    ///
    /// # Errors
    ///
    /// Returns nothing further: the reason is left in [`Self::trouble`],
    /// which is what a person reads.
    pub fn submit(
        &mut self,
        existing: &[(sim_core::ConnectionId, ConnectionEntry)],
    ) -> Option<(sim_core::ConnectionId, sim_core::TransportConfig)> {
        self.form.build(existing)
    }

    #[must_use]
    pub fn autoconnect(&self) -> bool {
        self.form.autoconnect
    }

    #[must_use]
    pub fn auto_reconnect(&self) -> bool {
        self.form.auto_reconnect
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
