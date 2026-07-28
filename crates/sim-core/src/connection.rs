use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ConnectionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone)]
pub enum TransportConfig {
    Udp {
        bind: SocketAddr,
        remote: SocketAddr,
    },
    /// Joins `group` on `interface` and both sends to and receives from it.
    ///
    /// The socket always binds to `0.0.0.0:<group port>`: binding directly to a
    /// multicast address is rejected on Windows, and the port is dictated by the
    /// group anyway.
    UdpMulticast {
        group: SocketAddrV4,
        interface: Ipv4Addr,
    },
    Tcp {
        mode: TcpMode,
    },
    Serial {
        port_name: String,
        baud_rate: u32,
        data_bits: DataBits,
        parity: Parity,
        stop_bits: StopBits,
        flow_control: FlowControl,
    },
}

#[derive(Debug, Clone)]
pub enum TcpMode {
    Client { addr: SocketAddr },
    Server { listen: SocketAddr },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
}
