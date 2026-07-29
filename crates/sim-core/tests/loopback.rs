use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use sim_core::{
    Command, ConnectionId, ConnectionStatus, Engine, Event, RetryPolicy, TcpMode, TransportConfig,
};
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
        retry: None,
    })
    .await
    .unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("b"),
        config: TransportConfig::Udp {
            bind: addr_b,
            remote: addr_a,
        },
        retry: None,
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
        retry: None,
    })
    .await
    .unwrap();
    tx.send(Command::Connect {
        id: ConnectionId::from("client"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        },
        retry: None,
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
            retry: None,
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
        retry: None,
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
        retry: None,
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

/// A retrying connection keeps knocking and comes up on its own once the peer
/// finally shows up, with no command from the caller in between.
#[tokio::test]
async fn a_retrying_connection_comes_up_when_the_peer_appears() {
    let (tx, mut rx) = Engine::spawn();
    let addr: std::net::SocketAddr = "127.0.0.1:19961".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("client"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        },
        retry: Some(RetryPolicy {
            max_attempts: None,
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(50),
        }),
    })
    .await
    .unwrap();

    // Nothing is listening yet, so the first attempt has to fail.
    wait_for(
        &mut rx,
        |event| matches!(event, Event::Error { id: Some(id), .. } if id.0 == "client"),
    )
    .await;

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        // Held rather than dropped, so the link stays up once accepted.
        let _accepted = listener.accept().await;
        tokio::time::sleep(TEST_TIMEOUT).await;
    });

    wait_all_connected(&mut rx, &["client"]).await;
}

/// A capped policy stops on its own rather than retrying forever.
#[tokio::test]
async fn a_capped_retry_gives_up_after_its_budget() {
    let (tx, mut rx) = Engine::spawn();
    let dead: std::net::SocketAddr = "127.0.0.1:19962".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("probe"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Client { addr: dead },
        },
        retry: Some(RetryPolicy {
            max_attempts: Some(2),
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(10),
        }),
    })
    .await
    .unwrap();

    let mut attempts = 0;
    timeout(TEST_TIMEOUT, async {
        loop {
            match rx.recv().await.expect("channel closed") {
                Event::ConnectionStatus {
                    id,
                    status: ConnectionStatus::Connecting,
                } if id.0 == "probe" => attempts += 1,
                Event::ConnectionStatus {
                    id,
                    status: ConnectionStatus::Disconnected,
                } if id.0 == "probe" => break,
                _ => {}
            }
        }
    })
    .await
    .expect("a capped retry should stop by itself");

    assert_eq!(attempts, 3, "the first attempt plus two retries");
}

/// A TCP server outlives the peer that hangs up and serves the next one.
#[tokio::test]
async fn tcp_server_accepts_a_second_peer() {
    use tokio::io::AsyncWriteExt;

    let (tx, mut rx) = Engine::spawn();
    let addr: std::net::SocketAddr = "127.0.0.1:19971".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("srv"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Server { listen: addr },
        },
        retry: None,
    })
    .await
    .unwrap();

    for expected in [vec![0xAAu8], vec![0xBBu8]] {
        let mut client = timeout(TEST_TIMEOUT, async {
            loop {
                if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
                    return stream;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("server never accepted a connection");

        wait_all_connected(&mut rx, &["srv"]).await;

        client.write_all(&expected).await.unwrap();
        let event = wait_for(
            &mut rx,
            |event| matches!(event, Event::FrameReceived { id, .. } if id.0 == "srv"),
        )
        .await;
        let Event::FrameReceived { bytes, .. } = event else {
            unreachable!()
        };
        assert_eq!(bytes, expected);

        // Hang up so the next iteration exercises the re-accept path.
        drop(client);
    }
}
