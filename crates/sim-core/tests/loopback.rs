use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use sim_core::{Command, ConnectionId, ConnectionStatus, Engine, Event, TcpMode, TransportConfig};
use tokio::sync::mpsc;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

async fn wait_for<F>(rx: &mut mpsc::Receiver<Event>, mut predicate: F) -> Event
where
    F: FnMut(&Event) -> bool,
{
    timeout(TEST_TIMEOUT, async {
        loop {
            let event = rx
                .recv()
                .await
                .expect("engine event channel closed unexpectedly");
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for expected event")
}

/// Waits until every id in `expected` has reported `Connected`, regardless of arrival order.
async fn wait_all_connected(rx: &mut mpsc::Receiver<Event>, expected: &[&str]) {
    let mut pending: std::collections::HashSet<&str> = expected.iter().copied().collect();
    timeout(TEST_TIMEOUT, async {
        while !pending.is_empty() {
            let event = rx
                .recv()
                .await
                .expect("engine event channel closed unexpectedly");
            if let Event::ConnectionStatus {
                id,
                status: ConnectionStatus::Connected,
            } = &event
            {
                pending.remove(id.0.as_str());
            }
        }
    })
    .await
    .expect("timed out waiting for connections to report Connected");
}

#[tokio::test]
async fn udp_round_trip() {
    let (tx, mut rx) = Engine::spawn();

    let addr_a = "127.0.0.1:19801".parse().unwrap();
    let addr_b = "127.0.0.1:19802".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("a"),
        config: TransportConfig::Udp {
            bind: addr_a,
            remote: addr_b,
        },
    })
    .await
    .unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("b"),
        config: TransportConfig::Udp {
            bind: addr_b,
            remote: addr_a,
        },
    })
    .await
    .unwrap();

    wait_all_connected(&mut rx, &["a", "b"]).await;

    tx.send(Command::SendRaw {
        id: ConnectionId::from("a"),
        bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
    })
    .await
    .unwrap();

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::FrameReceived { id, .. } if id.0 == "b"),
    )
    .await;
    let Event::FrameReceived { bytes, .. } = event else {
        unreachable!()
    };
    assert_eq!(bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[tokio::test]
async fn tcp_round_trip() {
    let (tx, mut rx) = Engine::spawn();

    let addr = "127.0.0.1:19901".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("server"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Server { listen: addr },
        },
    })
    .await
    .unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("client"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        },
    })
    .await
    .unwrap();

    wait_all_connected(&mut rx, &["server", "client"]).await;

    tx.send(Command::SendRaw {
        id: ConnectionId::from("client"),
        bytes: vec![1, 2, 3, 4, 5],
    })
    .await
    .unwrap();

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::FrameReceived { id, .. } if id.0 == "server"),
    )
    .await;
    let Event::FrameReceived { bytes, .. } = event else {
        unreachable!()
    };
    assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
}

/// Two members of the same group on this host: one sends, the other must receive.
///
/// Relies on the loopback interface being multicast-capable, which is why the
/// interface is pinned to 127.0.0.1 rather than left to the OS.
#[tokio::test]
async fn udp_multicast_round_trip() {
    let (tx, mut rx) = Engine::spawn();

    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 42, 99), 19951);
    let interface = Ipv4Addr::LOCALHOST;

    for name in ["sender", "listener"] {
        tx.send(Command::Connect {
            id: ConnectionId::from(name),
            config: TransportConfig::UdpMulticast { group, interface },
        })
        .await
        .unwrap();
    }

    wait_all_connected(&mut rx, &["sender", "listener"]).await;

    tx.send(Command::SendRaw {
        id: ConnectionId::from("sender"),
        bytes: vec![0xCA, 0xFE],
    })
    .await
    .unwrap();

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::FrameReceived { id, .. } if id.0 == "listener"),
    )
    .await;
    let Event::FrameReceived { bytes, source, .. } = event else {
        unreachable!()
    };
    assert_eq!(bytes, vec![0xCA, 0xFE]);
    assert!(source.is_some(), "multicast receive must report a source");
}

/// A connection whose task died must not keep its slot: reusing the name has to
/// work without first disconnecting a connection that is already gone.
#[tokio::test]
async fn dead_connection_frees_its_name() {
    let (tx, mut rx) = Engine::spawn();

    // Connecting to a closed port fails, so the task exits right away.
    let dead = "127.0.0.1:19987".parse().unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("probe"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Client { addr: dead },
        },
    })
    .await
    .unwrap();

    wait_for(&mut rx, |event| {
        matches!(
            event,
            Event::ConnectionStatus {
                id,
                status: ConnectionStatus::Disconnected
            } if id.0 == "probe"
        )
    })
    .await;

    // Reusing the name must succeed rather than report DuplicateConnection.
    let addr = "127.0.0.1:19988".parse().unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("probe"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Server { listen: addr },
        },
    })
    .await
    .unwrap();

    let event = timeout(TEST_TIMEOUT, async {
        loop {
            let event = rx.recv().await.expect("channel closed");
            match &event {
                Event::Error { .. } => return event,
                Event::ConnectionStatus {
                    id,
                    status: ConnectionStatus::Connecting,
                } if id.0 == "probe" => return event,
                _ => {}
            }
        }
    })
    .await
    .expect("timed out");

    assert!(
        matches!(event, Event::ConnectionStatus { .. }),
        "reusing a dead connection's name should be accepted, got an error instead"
    );
}
