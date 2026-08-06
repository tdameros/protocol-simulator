//! Connection settings in the form they are written down.
//!
//! Same split as `frame::schema`: plain structs mirror the file and the domain
//! model stays free of serde attributes. It earns more here than tidiness.
//! `TransportConfig` carries tokio-serial's own enums, which do not serialise at
//! all, and its shape is chosen for the engine rather than for something a
//! person opens in an editor and hands to a colleague.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

use crate::connection::{ConnectionId, RetryPolicy, TcpMode, TransportConfig};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("a connection needs a name")]
    UnnamedConnection,

    #[error("connection {name}: {source}")]
    Connection {
        name: String,
        #[source]
        source: Box<ConfigError>,
    },

    #[error("{value} is not a usable number of data bits, expected 5, 6, 7 or 8")]
    DataBits { value: u8 },

    #[error("{value} is not a usable number of stop bits, expected 1 or 2")]
    StopBits { value: u8 },
}

/// One connection as it appears in a file.
///
/// The transport fields sit directly alongside `name` rather than under a
/// nested table, so an entry reads as one block of settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSpec {
    pub name: String,

    #[serde(flatten)]
    pub transport: TransportSpec,

    /// Whether loading the file should open this connection.
    ///
    /// Off by default: a file arriving from elsewhere must not start grabbing
    /// ports on the strength of what it says.
    #[serde(default)]
    pub autoconnect: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySpec>,
}

impl ConnectionSpec {
    #[must_use]
    pub fn describe(
        id: &ConnectionId,
        config: &TransportConfig,
        retry: Option<RetryPolicy>,
        autoconnect: bool,
    ) -> Self {
        Self {
            name: id.0.clone(),
            transport: TransportSpec::from(config),
            autoconnect,
            retry: retry.map(RetrySpec::from),
        }
    }

    /// Turns the written form back into what the engine takes.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry has no name, or if a serial setting is one
    /// no port can have. Errors name the connection, since a file holds several
    /// and the reader has to know which entry to go and fix.
    pub fn resolve(
        &self,
    ) -> Result<(ConnectionId, TransportConfig, Option<RetryPolicy>), ConfigError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(ConfigError::UnnamedConnection);
        }
        let config = self
            .transport
            .resolve()
            .map_err(|source| ConfigError::Connection {
                name: name.to_owned(),
                source: Box::new(source),
            })?;
        Ok((
            ConnectionId(name.to_owned()),
            config,
            self.retry.map(RetryPolicy::from),
        ))
    }
}

/// The four transports, flattened so the kind and its settings read as one
/// block. `transport = "udp"` picks the variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum TransportSpec {
    Udp {
        bind: SocketAddr,
        remote: SocketAddr,
    },
    UdpMulticast {
        group: SocketAddrV4,
        #[serde(default = "any_interface")]
        interface: Ipv4Addr,
    },
    TcpClient {
        addr: SocketAddr,
    },
    TcpServer {
        listen: SocketAddr,
    },
    /// Everything past the port and speed has a default, so the common case is
    /// two lines.
    Serial {
        port: String,
        baud: u32,
        #[serde(default = "eight_data_bits")]
        data_bits: u8,
        #[serde(default)]
        parity: ParitySpec,
        #[serde(default = "one_stop_bit")]
        stop_bits: u8,
        #[serde(default)]
        flow_control: FlowControlSpec,
    },
}

impl TransportSpec {
    fn resolve(&self) -> Result<TransportConfig, ConfigError> {
        Ok(match self {
            Self::Udp { bind, remote } => TransportConfig::Udp {
                bind: *bind,
                remote: *remote,
            },
            Self::UdpMulticast { group, interface } => TransportConfig::UdpMulticast {
                group: *group,
                interface: *interface,
            },
            Self::TcpClient { addr } => TransportConfig::Tcp {
                mode: TcpMode::Client { addr: *addr },
            },
            Self::TcpServer { listen } => TransportConfig::Tcp {
                mode: TcpMode::Server { listen: *listen },
            },
            Self::Serial {
                port,
                baud,
                data_bits,
                parity,
                stop_bits,
                flow_control,
            } => TransportConfig::Serial {
                port_name: port.clone(),
                baud_rate: *baud,
                data_bits: data_bits_from(*data_bits)?,
                parity: (*parity).into(),
                stop_bits: stop_bits_from(*stop_bits)?,
                flow_control: (*flow_control).into(),
            },
        })
    }
}

impl From<&TransportConfig> for TransportSpec {
    fn from(config: &TransportConfig) -> Self {
        match config {
            TransportConfig::Udp { bind, remote } => Self::Udp {
                bind: *bind,
                remote: *remote,
            },
            TransportConfig::UdpMulticast { group, interface } => Self::UdpMulticast {
                group: *group,
                interface: *interface,
            },
            TransportConfig::Tcp {
                mode: TcpMode::Client { addr },
            } => Self::TcpClient { addr: *addr },
            TransportConfig::Tcp {
                mode: TcpMode::Server { listen },
            } => Self::TcpServer { listen: *listen },
            TransportConfig::Serial {
                port_name,
                baud_rate,
                data_bits,
                parity,
                stop_bits,
                flow_control,
            } => Self::Serial {
                port: port_name.clone(),
                baud: *baud_rate,
                data_bits: data_bits_of(*data_bits),
                parity: ParitySpec::from(*parity),
                stop_bits: stop_bits_of(*stop_bits),
                flow_control: FlowControlSpec::from(*flow_control),
            },
        }
    }
}

/// Delays are written in milliseconds: a file is read by people, and
/// `initial_delay_ms = 500` needs no explaining.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl From<RetryPolicy> for RetrySpec {
    fn from(policy: RetryPolicy) -> Self {
        Self {
            max_attempts: policy.max_attempts,
            initial_delay_ms: as_millis(policy.initial_delay),
            max_delay_ms: as_millis(policy.max_delay),
        }
    }
}

impl From<RetrySpec> for RetryPolicy {
    fn from(spec: RetrySpec) -> Self {
        Self {
            max_attempts: spec.max_attempts,
            initial_delay: Duration::from_millis(spec.initial_delay_ms),
            max_delay: Duration::from_millis(spec.max_delay_ms),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParitySpec {
    #[default]
    None,
    Odd,
    Even,
}

impl From<Parity> for ParitySpec {
    fn from(parity: Parity) -> Self {
        match parity {
            Parity::None => Self::None,
            Parity::Odd => Self::Odd,
            Parity::Even => Self::Even,
        }
    }
}

impl From<ParitySpec> for Parity {
    fn from(spec: ParitySpec) -> Self {
        match spec {
            ParitySpec::None => Self::None,
            ParitySpec::Odd => Self::Odd,
            ParitySpec::Even => Self::Even,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowControlSpec {
    #[default]
    None,
    Software,
    Hardware,
}

impl From<FlowControl> for FlowControlSpec {
    fn from(flow: FlowControl) -> Self {
        match flow {
            FlowControl::None => Self::None,
            FlowControl::Software => Self::Software,
            FlowControl::Hardware => Self::Hardware,
        }
    }
}

impl From<FlowControlSpec> for FlowControl {
    fn from(spec: FlowControlSpec) -> Self {
        match spec {
            FlowControlSpec::None => Self::None,
            FlowControlSpec::Software => Self::Software,
            FlowControlSpec::Hardware => Self::Hardware,
        }
    }
}

fn any_interface() -> Ipv4Addr {
    Ipv4Addr::UNSPECIFIED
}

fn eight_data_bits() -> u8 {
    8
}

fn one_stop_bit() -> u8 {
    1
}

fn as_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn data_bits_of(bits: DataBits) -> u8 {
    match bits {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    }
}

fn data_bits_from(value: u8) -> Result<DataBits, ConfigError> {
    match value {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(ConfigError::DataBits { value }),
    }
}

fn stop_bits_of(bits: StopBits) -> u8 {
    match bits {
        StopBits::One => 1,
        StopBits::Two => 2,
    }
}

fn stop_bits_from(value: u8) -> Result<StopBits, ConfigError> {
    match value {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(ConfigError::StopBits { value }),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    struct File {
        connection: Vec<ConnectionSpec>,
    }

    fn round_trip(config: &TransportConfig) -> TransportConfig {
        let spec = TransportSpec::from(config);
        let text = toml::to_string(&spec).expect("spec should serialise");
        let back: TransportSpec = toml::from_str(&text).expect("spec should parse back");
        assert_eq!(spec, back, "through:\n{text}");
        back.resolve().expect("spec should resolve")
    }

    #[test]
    fn every_transport_survives_the_trip_to_a_file_and_back() {
        let configs = [
            TransportConfig::Udp {
                bind: "127.0.0.1:9000".parse().unwrap(),
                remote: "127.0.0.1:9001".parse().unwrap(),
            },
            TransportConfig::UdpMulticast {
                group: "239.1.1.1:5000".parse().unwrap(),
                interface: "192.168.1.20".parse().unwrap(),
            },
            TransportConfig::Tcp {
                mode: TcpMode::Client {
                    addr: "10.0.0.4:502".parse().unwrap(),
                },
            },
            TransportConfig::Tcp {
                mode: TcpMode::Server {
                    listen: "0.0.0.0:502".parse().unwrap(),
                },
            },
            TransportConfig::Serial {
                port_name: "/dev/ttyUSB0".to_owned(),
                baud_rate: 115_200,
                data_bits: DataBits::Seven,
                parity: Parity::Even,
                stop_bits: StopBits::Two,
                flow_control: FlowControl::Hardware,
            },
        ];

        for config in &configs {
            // Compared through the spec, TransportConfig having no PartialEq.
            let back = round_trip(config);
            assert_eq!(TransportSpec::from(config), TransportSpec::from(&back));
        }
    }

    #[test]
    fn a_connection_reads_as_one_flat_block() {
        let spec = ConnectionSpec::describe(
            &ConnectionId::from("bus"),
            &TransportConfig::Udp {
                bind: "127.0.0.1:9000".parse().unwrap(),
                remote: "127.0.0.1:9001".parse().unwrap(),
            },
            Some(RetryPolicy::standard()),
            true,
        );
        let text = toml::to_string(&File {
            connection: vec![spec.clone()],
        })
        .expect("file should serialise");

        for expected in [
            "[[connection]]",
            "name = \"bus\"",
            "transport = \"udp\"",
            "bind = \"127.0.0.1:9000\"",
            "autoconnect = true",
            "initial_delay_ms = 500",
        ] {
            assert!(text.contains(expected), "missing {expected} in:\n{text}");
        }

        let back: File = toml::from_str(&text).expect("file should parse back");
        assert_eq!(back.connection, vec![spec]);
    }

    #[test]
    fn a_serial_port_needs_only_its_port_and_speed() {
        let file: File = toml::from_str(
            r#"
[[connection]]
name = "uart"
transport = "serial"
port = "COM3"
baud = 9600
"#,
        )
        .expect("minimal serial entry should parse");

        let (id, config, retry) = file.connection[0].resolve().expect("should resolve");
        assert_eq!(id, ConnectionId::from("uart"));
        assert!(retry.is_none());
        assert!(!file.connection[0].autoconnect);
        assert!(matches!(
            config,
            TransportConfig::Serial {
                data_bits: DataBits::Eight,
                parity: Parity::None,
                stop_bits: StopBits::One,
                flow_control: FlowControl::None,
                ..
            }
        ));
    }

    #[test]
    fn an_impossible_serial_setting_names_the_connection_it_came_from() {
        let file: File = toml::from_str(
            r#"
[[connection]]
name = "uart"
transport = "serial"
port = "COM3"
baud = 9600
data_bits = 9
"#,
        )
        .expect("the file itself is valid toml");

        let error = file.connection[0].resolve().expect_err("9 is not a width");
        let message = error.to_string();
        assert!(message.contains("uart"), "{message}");
        assert!(
            error.source().is_some_and(|s| s.to_string().contains('9')),
            "the cause should say what was wrong"
        );
    }

    #[test]
    fn an_unnamed_connection_is_refused() {
        let spec = ConnectionSpec {
            name: "   ".to_owned(),
            transport: TransportSpec::TcpClient {
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            autoconnect: false,
            retry: None,
        };
        assert!(matches!(
            spec.resolve(),
            Err(ConfigError::UnnamedConnection)
        ));
    }

    #[test]
    fn a_retry_policy_keeps_its_delays() {
        let policy = RetryPolicy {
            max_attempts: Some(3),
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
        };
        assert_eq!(RetryPolicy::from(RetrySpec::from(policy)), policy);
    }
}
