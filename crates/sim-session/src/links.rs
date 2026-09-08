//! A link as a person sees it: what it is, where it goes, and how it is doing.
//!
//! The words are here rather than beside a widget, so that a window and a
//! terminal cannot end up calling the same state two different things.

use std::net::{IpAddr, Ipv4Addr};

use sim_core::ConnectionStatus;

use crate::state::ConnectionEntry;

#[must_use]
pub fn interfaces() -> Vec<(String, Ipv4Addr)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut found: Vec<_> = interfaces
        .into_iter()
        .filter_map(|iface| match iface.addr.ip() {
            IpAddr::V4(addr) => Some((iface.name, addr)),
            IpAddr::V6(_) => None,
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

#[must_use]
pub fn status(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Listening => "Port open, waiting for a peer",
        ConnectionStatus::Connected => "Connected",
        ConnectionStatus::Disconnected => "Disconnected",
    }
}

#[must_use]
pub fn summary(entry: &ConnectionEntry) -> String {
    use sim_core::{TcpMode, TransportConfig};

    match &entry.config {
        TransportConfig::Udp { bind, remote } => format!("UDP {bind} -> {remote}"),
        TransportConfig::UdpMulticast { group, interface } => {
            let via = if interface.is_unspecified() {
                "auto".to_owned()
            } else {
                interface.to_string()
            };
            format!("UDP multicast {group} via {via}")
        }
        TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        } => format!("TCP client -> {addr}"),
        TransportConfig::Tcp {
            mode: TcpMode::Server { listen },
        } => format!("TCP server on {listen}"),
        TransportConfig::Serial {
            port_name,
            baud_rate,
            ..
        } => format!("Serial {port_name} @ {baud_rate}"),
    }
}
