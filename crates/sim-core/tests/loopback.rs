use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use sim_core::{
    Command, ConnectionId, ConnectionStatus, Engine, Event, RetryPolicy, TcpMode, TransportConfig,
};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_serial::{DataBits, FlowControl, Parity, StopBits};

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

/// Collects events until the ones collected so far satisfy `enough`.
///
/// A scenario reports itself finished once it has issued its last send, which
/// is before that send has crossed the socket, so its own events and the
/// traffic it causes arrive in no fixed order. A test that cares about both has
/// to weigh the set rather than the sequence.
async fn gather_until<F>(rx: &mut mpsc::Receiver<Event>, mut enough: F) -> Vec<Event>
where
    F: FnMut(&[Event]) -> bool,
{
    let mut seen = Vec::new();
    timeout(TEST_TIMEOUT, async {
        loop {
            seen.push(
                rx.recv()
                    .await
                    .expect("engine event channel closed unexpectedly"),
            );
            if enough(&seen) {
                return;
            }
        }
    })
    .await
    .expect("timed out waiting for the expected events");
    seen
}

fn finished_with(events: &[Event], scenario: &str, expected: &sim_core::Outcome) -> bool {
    events.iter().any(|event| {
        matches!(event, Event::ScenarioFinished { name, outcome } if name == scenario && outcome == expected)
    })
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
    .unwrap_or_else(|_| panic!("timed out waiting for {pending:?} to report Connected"));
}

/// Waits until a server holds its port.
///
/// A client that dials before the listener exists is refused, and a refusal
/// ends its task for good, so the two commands cannot simply be fired off back
/// to back.
async fn wait_until_listening(rx: &mut mpsc::Receiver<Event>, name: &str) {
    wait_for(rx, |event| {
        matches!(
            event,
            Event::ConnectionStatus {
                id,
                status: ConnectionStatus::Listening
            } if id.0 == name
        )
    })
    .await;
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
    wait_until_listening(&mut rx, "server").await;

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

    // A port that cannot be opened, so the task exits right away without
    // assuming anything about what this machine happens to be running.
    tx.send(Command::Connect {
        id: ConnectionId::from("probe"),
        config: TransportConfig::Serial {
            port_name: "sim-core-test-no-such-port".to_owned(),
            baud_rate: 115_200,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
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

/// A server holding its port is up, even with nobody on the other end.
///
/// It used to sit on `Connecting` until a client turned up, which made a
/// perfectly working server look stuck.
#[tokio::test]
async fn a_bound_server_reports_listening_before_any_peer() {
    let (tx, mut rx) = Engine::spawn();
    let addr: std::net::SocketAddr = "127.0.0.1:19941".parse().unwrap();

    tx.send(Command::Connect {
        id: ConnectionId::from("srv"),
        config: TransportConfig::Tcp {
            mode: TcpMode::Server { listen: addr },
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
                status: ConnectionStatus::Listening
            } if id.0 == "srv"
        )
    })
    .await;

    // The port really is open: connecting to it has to succeed.
    let peer = tokio::net::TcpStream::connect(addr)
        .await
        .expect("a listening server must accept a connection");
    wait_all_connected(&mut rx, &["srv"]).await;

    // And losing the peer sends it back to listening rather than leaving it
    // looking connected with nothing on the line.
    drop(peer);
    wait_for(&mut rx, |event| {
        matches!(
            event,
            Event::ConnectionStatus {
                id,
                status: ConnectionStatus::Listening
            } if id.0 == "srv"
        )
    })
    .await;
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
///
/// Opening a serial port that cannot exist, rather than dialling a TCP port
/// assumed to be closed: the failure is immediate and identical on every
/// platform, where a port nobody is supposed to be listening on is only an
/// assumption about the machine the tests happen to run on.
#[tokio::test]
async fn a_capped_retry_gives_up_after_its_budget() {
    let (tx, mut rx) = Engine::spawn();

    tx.send(Command::Connect {
        id: ConnectionId::from("probe"),
        config: TransportConfig::Serial {
            port_name: "sim-core-test-no-such-port".to_owned(),
            baud_rate: 115_200,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
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

/// A frame definition small enough to reason about byte by byte: a sync word, a
/// sequence byte the scenario counts, and a mode byte it overrides.
fn scenario_frame() -> sim_core::frame::FrameDef {
    sim_core::frame::schema::from_toml(
        r#"
name = "Beacon"
endian = "big"

[[field]]
name = "sync"
type = "u16"
default = 0xAA55

[[field]]
name = "seq"
type = "u8"

[[field]]
name = "mode"
type = "u8"
"#,
    )
    .expect("the test frame should parse")
}

fn beacons(events: &[Event], id: &str) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::FrameReceived { id: got, bytes, .. } if got.0 == id => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

/// A repeating scenario encodes its frame, advances its counter and lands on
/// the far side, pass after pass.
#[tokio::test]
async fn a_repeating_scenario_emits_encoded_frames_with_a_counter() {
    let (tx, mut rx) = Engine::spawn();

    let addr_a = "127.0.0.1:19831".parse().unwrap();
    let addr_b = "127.0.0.1:19832".parse().unwrap();
    for (name, bind, remote) in [("a", addr_a, addr_b), ("b", addr_b, addr_a)] {
        tx.send(Command::Connect {
            id: ConnectionId::from(name),
            config: TransportConfig::Udp { bind, remote },
            retry: None,
        })
        .await
        .unwrap();
    }
    wait_all_connected(&mut rx, &["a", "b"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Beacon burst"
on = "a"
repeat = { every_ms = 20, times = 3 }

[[scenario.step]]
send = "Beacon"
with = { mode = 7 }
counters = { seq = { from = 10, step = 5 } }
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: vec![scenario_frame()],
    })
    .await
    .unwrap();

    // Both conditions, in either order: the scenario announces itself done
    // before its last frame has finished crossing the loopback.
    let seen = gather_until(&mut rx, |events| {
        beacons(events, "b").len() >= 3
            && finished_with(events, "Beacon burst", &sim_core::Outcome::Completed)
    })
    .await;

    let frames = beacons(&seen, "b");
    assert_eq!(frames.len(), 3, "one frame per pass");
    // Sync from the frame's own default, mode from the override, seq counting
    // 10, 15, 20 as the counter declares.
    assert_eq!(frames[0], vec![0xAA, 0x55, 10, 7]);
    assert_eq!(frames[1], vec![0xAA, 0x55, 15, 7]);
    assert_eq!(frames[2], vec![0xAA, 0x55, 20, 7]);
}

/// `wait_for` holds the sequence until the far side answers, and the step after
/// it only runs then.
#[tokio::test]
async fn a_scenario_waits_for_the_frame_it_was_told_to_expect() {
    let (tx, mut rx) = Engine::spawn();

    let addr_a = "127.0.0.1:19833".parse().unwrap();
    let addr_b = "127.0.0.1:19834".parse().unwrap();
    for (name, bind, remote) in [("a", addr_a, addr_b), ("b", addr_b, addr_a)] {
        tx.send(Command::Connect {
            id: ConnectionId::from(name),
            config: TransportConfig::Udp { bind, remote },
            retry: None,
        })
        .await
        .unwrap();
    }
    wait_all_connected(&mut rx, &["a", "b"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Handshake"
on = "a"

[[scenario.step]]
wait_for = { hex = "C0 ?? FE", at = 0, timeout_ms = 4000 }

[[scenario.step]]
raw = "01 02"
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: Vec::new(),
    })
    .await
    .unwrap();

    // A frame that does not match must not release the wait.
    tx.send(Command::SendRaw {
        id: ConnectionId::from("b"),
        bytes: vec![0x11, 0x22, 0x33],
    })
    .await
    .unwrap();
    wait_for(&mut rx, |event| {
        matches!(event, Event::FrameReceived { id, bytes, .. } if id.0 == "a" && bytes == &[0x11, 0x22, 0x33])
    })
    .await;

    // Then the one it is waiting for, wildcard in the middle.
    tx.send(Command::SendRaw {
        id: ConnectionId::from("b"),
        bytes: vec![0xC0, 0x99, 0xFE],
    })
    .await
    .unwrap();

    // The step after the wait is what proves the wait was released at all.
    gather_until(&mut rx, |events| {
        let arrived = events.iter().any(|event| {
            matches!(event, Event::FrameReceived { id, bytes, .. } if id.0 == "b" && bytes == &[0x01, 0x02])
        });
        arrived && finished_with(events, "Handshake", &sim_core::Outcome::Completed)
    })
    .await;
}

/// A wait that is never answered fails the scenario, naming the step, rather
/// than hanging forever.
#[tokio::test]
async fn a_wait_that_times_out_fails_the_scenario() {
    let (tx, mut rx) = Engine::spawn();

    tx.send(Command::Connect {
        id: ConnectionId::from("lonely"),
        config: TransportConfig::Udp {
            bind: "127.0.0.1:19835".parse().unwrap(),
            remote: "127.0.0.1:19836".parse().unwrap(),
        },
        retry: None,
    })
    .await
    .unwrap();
    wait_all_connected(&mut rx, &["lonely"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Hopeful"
on = "lonely"

[[scenario.step]]
wait_for = { hex = "AA BB", timeout_ms = 60 }
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: Vec::new(),
    })
    .await
    .unwrap();

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::ScenarioFinished { name, .. } if name == "Hopeful"),
    )
    .await;

    let Event::ScenarioFinished { outcome, .. } = event else {
        panic!("expected the scenario to finish");
    };
    let sim_core::Outcome::Failed(reason) = outcome else {
        panic!("a timeout is a failure, got {outcome:?}");
    };
    assert!(reason.contains("step 1"), "{reason}");
    assert!(reason.contains("lonely"), "{reason}");
}

/// Stopping frees the name at once, so the same scenario can be started again
/// without waiting for anything to settle.
#[tokio::test]
async fn a_stopped_scenario_frees_its_name_immediately() {
    let (tx, mut rx) = Engine::spawn();

    tx.send(Command::Connect {
        id: ConnectionId::from("link"),
        config: TransportConfig::Udp {
            bind: "127.0.0.1:19837".parse().unwrap(),
            remote: "127.0.0.1:19838".parse().unwrap(),
        },
        retry: None,
    })
    .await
    .unwrap();
    wait_all_connected(&mut rx, &["link"]).await;

    let forever = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Forever"
on = "link"
repeat = { every_ms = 10 }

[[scenario.step]]
raw = "00"
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    for round in 0..2 {
        tx.send(Command::StartScenario {
            scenario: Box::new(forever.clone()),
            frames: Vec::new(),
        })
        .await
        .unwrap();

        wait_for(
            &mut rx,
            |event| matches!(event, Event::ScenarioStep { name, .. } if name == "Forever"),
        )
        .await;

        tx.send(Command::StopScenario {
            name: "Forever".to_owned(),
        })
        .await
        .unwrap();
        wait_for(&mut rx, |event| {
            matches!(
                event,
                Event::ScenarioFinished { name, outcome } if name == "Forever" && *outcome == sim_core::Outcome::Stopped
            )
        })
        .await;

        assert!(round < 2, "both rounds ran");
    }
}

/// One step, two links: the same bytes go out on both, and a `wait_for` aimed
/// at both is only satisfied once each has answered.
#[tokio::test]
async fn a_step_can_drive_two_links_at_once() {
    let (tx, mut rx) = Engine::spawn();

    // Two independent loopback pairs, so "uart" and "udp" are genuinely
    // separate links with their own far side.
    let pairs = [
        ("uart", 19841, "uart-peer", 19842),
        ("udp", 19843, "udp-peer", 19844),
    ];
    for (near, near_port, far, far_port) in pairs {
        for (name, bind, remote) in [(near, near_port, far_port), (far, far_port, near_port)] {
            tx.send(Command::Connect {
                id: ConnectionId::from(name),
                config: TransportConfig::Udp {
                    bind: format!("127.0.0.1:{bind}").parse().unwrap(),
                    remote: format!("127.0.0.1:{remote}").parse().unwrap(),
                },
                retry: None,
            })
            .await
            .unwrap();
        }
    }
    wait_all_connected(&mut rx, &["uart", "uart-peer", "udp", "udp-peer"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Both"
on = ["uart", "udp"]

[[scenario.step]]
raw = "AA 55"

[[scenario.step]]
wait_for = { hex = "C0 FE", timeout_ms = 4000 }

[[scenario.step]]
raw = "99"
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: Vec::new(),
    })
    .await
    .unwrap();

    // Step one reached both far sides with the same bytes.
    gather_until(&mut rx, |events| {
        ["uart-peer", "udp-peer"].iter().all(|peer| {
            events.iter().any(|event| {
                matches!(event, Event::FrameReceived { id, bytes, .. } if id.0 == *peer && bytes == &[0xAA, 0x55])
            })
        })
    })
    .await;

    // Only one side answers, so the wait must hold: the step after it has not
    // run, which is what "all of them" means.
    tx.send(Command::SendRaw {
        id: ConnectionId::from("uart-peer"),
        bytes: vec![0xC0, 0xFE],
    })
    .await
    .unwrap();
    wait_for(&mut rx, |event| {
        matches!(event, Event::FrameReceived { id, bytes, .. } if id.0 == "uart" && bytes == &[0xC0, 0xFE])
    })
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            wait_for(&mut rx, |event| matches!(
                event,
                Event::FrameReceived { bytes, .. } if bytes == &[0x99]
            ))
        )
        .await
        .is_err(),
        "one answer out of two must not release the wait"
    );

    // The second answer releases it, and the last step runs on both links.
    tx.send(Command::SendRaw {
        id: ConnectionId::from("udp-peer"),
        bytes: vec![0xC0, 0xFE],
    })
    .await
    .unwrap();
    gather_until(&mut rx, |events| {
        ["uart-peer", "udp-peer"].iter().all(|peer| {
            events.iter().any(|event| {
                matches!(event, Event::FrameReceived { id, bytes, .. } if id.0 == *peer && bytes == &[0x99])
            })
        }) && finished_with(events, "Both", &sim_core::Outcome::Completed)
    })
    .await;
}

/// A wait aimed at two links that only one answers fails, and says which one
/// stayed silent.
#[tokio::test]
async fn a_partial_answer_times_out_naming_who_is_missing() {
    let (tx, mut rx) = Engine::spawn();

    for (name, bind, remote) in [
        ("left", 19845, 19846),
        ("left-peer", 19846, 19845),
        ("right", 19847, 19848),
    ] {
        tx.send(Command::Connect {
            id: ConnectionId::from(name),
            config: TransportConfig::Udp {
                bind: format!("127.0.0.1:{bind}").parse().unwrap(),
                remote: format!("127.0.0.1:{remote}").parse().unwrap(),
            },
            retry: None,
        })
        .await
        .unwrap();
    }
    wait_all_connected(&mut rx, &["left", "left-peer", "right"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Both must answer"
on = ["left", "right"]

[[scenario.step]]
wait_for = { hex = "C0 FE", timeout_ms = 300 }
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: Vec::new(),
    })
    .await
    .unwrap();

    tx.send(Command::SendRaw {
        id: ConnectionId::from("left-peer"),
        bytes: vec![0xC0, 0xFE],
    })
    .await
    .unwrap();

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::ScenarioFinished { name, .. } if name == "Both must answer"),
    )
    .await;

    let Event::ScenarioFinished { outcome, .. } = event else {
        panic!("expected the scenario to finish");
    };
    let sim_core::Outcome::Failed(reason) = outcome else {
        panic!("a half answer is a failure, got {outcome:?}");
    };
    // Names what is still missing, not what was asked for.
    assert!(reason.contains("right"), "{reason}");
    assert!(!reason.contains("left"), "{reason}");
}

/// A wait must not be released by a frame that arrived before it was reached,
/// which on a repeating scenario means a pass answering with the previous
/// pass's reply and reporting success nobody earned.
#[tokio::test]
async fn a_wait_is_not_satisfied_by_an_earlier_passs_answer() {
    let (tx, mut rx) = Engine::spawn();

    let near = "127.0.0.1:19851".parse().unwrap();
    let far = "127.0.0.1:19852".parse().unwrap();
    for (name, bind, remote) in [("near", near, far), ("far", far, near)] {
        tx.send(Command::Connect {
            id: ConnectionId::from(name),
            config: TransportConfig::Udp { bind, remote },
            retry: None,
        })
        .await
        .unwrap();
    }
    wait_all_connected(&mut rx, &["near", "far"]).await;

    let scenario = sim_core::scenario::from_toml(
        r#"
[[scenario]]
name = "Twice"
on = "near"
repeat = { every_ms = 400, times = 2 }

[[scenario.step]]
wait_ms = 150

[[scenario.step]]
wait_for = { hex = "C0 FE", timeout_ms = 120 }
"#,
    )
    .expect("scenario should parse")
    .remove(0);

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: Vec::new(),
    })
    .await
    .unwrap();

    // Two answers during the first pass's delay, then silence. The first pass
    // is satisfied by one of them; the second must not be satisfied by the
    // leftover.
    for _ in 0..2 {
        tx.send(Command::SendRaw {
            id: ConnectionId::from("far"),
            bytes: vec![0xC0, 0xFE],
        })
        .await
        .unwrap();
    }

    let event = wait_for(
        &mut rx,
        |event| matches!(event, Event::ScenarioFinished { name, .. } if name == "Twice"),
    )
    .await;

    let Event::ScenarioFinished { outcome, .. } = event else {
        panic!("expected the scenario to finish");
    };
    let sim_core::Outcome::Failed(reason) = outcome else {
        panic!("the second pass was never answered, got {outcome:?}");
    };
    assert!(reason.contains("step 2"), "{reason}");
    assert!(reason.contains("near"), "{reason}");
}
