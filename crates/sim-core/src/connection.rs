use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

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
    /// Bound and waiting for a peer, which only a server can be.
    ///
    /// Distinct from `Connected` because there is nowhere to send yet, and from
    /// `Connecting` because the port is already open: a listening server that
    /// reported `Connecting` would look broken when it is working perfectly.
    Listening,
    Connected,
    Disconnected,
}

/// How a connection reopens itself after the link goes away.
///
/// The delay doubles from `initial_delay` and is capped at `max_delay`, so a
/// peer that is merely restarting is caught within a second while one that is
/// gone for the afternoon is not hammered. A successful connection resets the
/// delay, so a flapping link does not inherit the long wait from last time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// `None` retries until the connection is disconnected by hand.
    pub max_attempts: Option<u32>,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Doubling from half a second to ten, for as long as it takes.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            max_attempts: None,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
        }
    }

    /// Whether a further attempt is allowed, `attempt` being how many have
    /// already failed.
    #[must_use]
    pub fn allows(&self, attempt: u32) -> bool {
        self.max_attempts.is_none_or(|max| attempt < max)
    }

    /// How long to wait before the attempt following `attempt` failures.
    #[must_use]
    pub fn delay_after(&self, attempt: u32) -> Duration {
        // Saturating throughout: a caller is free to ask for attempt 10_000
        // with a one hour initial delay, and the cap settles it either way.
        let factor = 2u32.saturating_pow(attempt.min(u32::BITS - 1));
        self.initial_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_doubles_up_to_the_cap() {
        let policy = RetryPolicy {
            max_attempts: None,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
        };
        let delays: Vec<u128> = (0..6)
            .map(|attempt| policy.delay_after(attempt).as_millis())
            .collect();
        assert_eq!(delays, [100, 200, 400, 800, 1000, 1000]);
    }

    #[test]
    fn an_absurd_attempt_count_still_lands_on_the_cap() {
        let policy = RetryPolicy::standard();
        assert_eq!(policy.delay_after(u32::MAX), policy.max_delay);
    }

    #[test]
    fn a_capped_policy_stops_allowing_attempts() {
        let policy = RetryPolicy {
            max_attempts: Some(3),
            ..RetryPolicy::standard()
        };
        assert!(policy.allows(0));
        assert!(policy.allows(2));
        assert!(!policy.allows(3));
        // The default policy never gives up.
        assert!(RetryPolicy::standard().allows(u32::MAX));
    }
}
