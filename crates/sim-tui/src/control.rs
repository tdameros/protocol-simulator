//! The local control socket `sim-tui --run` opens under `--control-port`.
//!
//! One connection at a time or several, each on its own thread: a technician
//! script that reconnects between menu actions must never be locked out by
//! one still-open connection. One JSON request per line in, one JSON
//! response per line out. The scenario name never appears on the wire: the
//! caller already knows it, it is the argument `--run` was given, and
//! supplies it to every command this module issues.
//!
//! These threads outlive the scenario on purpose. A technician's script may
//! still want the last frame received for a moment after the run reports
//! done, and in the real binary the whole process exits with it anyway.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;
use tokio::sync::mpsc;

use sim_core::frame::codec;
use sim_core::frame::value::Value;
use sim_core::frame::FrameDef;
use sim_core::{Command, ConnectionId};

/// The most recent bytes received on each connection, for `last_received` to
/// read back. Written by the headless front end's own event loop on every
/// `Event::FrameReceived`, read here on request.
pub type LastReceived = Arc<Mutex<HashMap<ConnectionId, Vec<u8>>>>;

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Set {
        step: usize,
        field: String,
        value: Value,
    },
    LastReceived {
        on: String,
        #[serde(rename = "as")]
        frame: String,
    },
}

/// Opens the control socket and serves requests on their own threads until
/// the process exits.
///
/// # Errors
///
/// Returns an error if the port cannot be bound. A technician's script that
/// can never reach the socket is worse than a run that refuses to start over
/// a port already taken.
pub fn spawn(
    port: u16,
    commands: mpsc::Sender<Command>,
    scenario: String,
    last_received: LastReceived,
    frames: Vec<FrameDef>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let frames = Arc::new(frames);
    thread::Builder::new()
        .name("sim-control".to_owned())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let commands = commands.clone();
                let scenario = scenario.clone();
                let last_received = Arc::clone(&last_received);
                let frames = Arc::clone(&frames);
                thread::spawn(move || {
                    serve(&stream, &commands, &scenario, &last_received, &frames);
                });
            }
        })
        .expect("failed to spawn control socket thread");
    Ok(())
}

fn serve(
    stream: &TcpStream,
    commands: &mpsc::Sender<Command>,
    scenario: &str,
    last_received: &LastReceived,
    frames: &[FrameDef],
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(request, commands, scenario, last_received, frames),
            Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
        };
        if writeln!(writer, "{response}").is_err() {
            return;
        }
    }
}

fn handle(
    request: Request,
    commands: &mpsc::Sender<Command>,
    scenario: &str,
    last_received: &LastReceived,
    frames: &[FrameDef],
) -> serde_json::Value {
    match request {
        Request::Set { step, field, value } => {
            match commands.blocking_send(Command::SetStepValue {
                scenario: scenario.to_owned(),
                step,
                field,
                value,
            }) {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(_) => serde_json::json!({ "ok": false, "error": "the engine is not running" }),
            }
        }
        Request::LastReceived { on, frame: name } => {
            let id = ConnectionId::from(on.as_str());
            let bytes = last_received
                .lock()
                .expect("last-received mutex poisoned")
                .get(&id)
                .cloned();
            let Some(bytes) = bytes else {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("nothing received yet on {on}"),
                });
            };
            let Some(definition) = frames.iter().find(|frame| frame.name == name) else {
                return serde_json::json!({ "ok": false, "error": format!("no frame named {name}") });
            };
            match codec::decode(definition, &bytes) {
                Ok(decoded) => {
                    let fields: serde_json::Map<String, serde_json::Value> = decoded
                        .values
                        .iter()
                        .map(|(field, value)| {
                            (
                                field.clone(),
                                serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
                            )
                        })
                        .collect();
                    serde_json::json!({
                        "ok": true,
                        "bytes": sim_session::hex::spaced(&bytes),
                        "fields": fields,
                    })
                }
                Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    use sim_core::{Engine, Event, TransportConfig};

    fn free_tcp_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("a bound address")
            .port()
    }

    fn beacon_frame() -> FrameDef {
        sim_core::frame::schema::from_toml(
            r#"
name = "Beacon"
endian = "big"
[[field]]
name = "mode"
type = "u8"
default = 1
"#,
        )
        .expect("the test frame should parse")
    }

    /// Sends one JSON request and reads one JSON response line, keeping the
    /// connection open for the caller to reuse.
    fn round_trip(client: &mut TcpStream, request: &str) -> serde_json::Value {
        writeln!(client, "{request}").expect("write should succeed");
        let mut reader = BufReader::new(client.try_clone().expect("clone should succeed"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read should succeed");
        serde_json::from_str(&line).expect("response should be valid JSON")
    }

    #[test]
    fn set_reaches_a_running_scenario_over_the_socket() {
        let (tx, mut rx) = Engine::spawn();

        let addr_a = "127.0.0.1:19991".parse().unwrap();
        let addr_b = "127.0.0.1:19992".parse().unwrap();
        for (name, bind, remote) in [("a", addr_a, addr_b), ("b", addr_b, addr_a)] {
            tx.blocking_send(Command::Connect {
                id: ConnectionId::from(name),
                config: TransportConfig::Udp { bind, remote },
                retry: None,
            })
            .unwrap();
        }
        let mut connected = 0;
        while connected < 2 {
            if let Some(Event::ConnectionStatus { .. }) = rx.blocking_recv() {
                connected += 1;
            }
        }

        let scenario = sim_core::scenario::from_toml(
            r#"
[[scenario]]
name = "Steerable"
on = "a"
repeat = { every_ms = 30, times = 3 }

[[scenario.step]]
send = "Beacon"
with = { mode = 1 }
"#,
        )
        .expect("scenario should parse")
        .remove(0);

        tx.blocking_send(Command::StartScenario {
            scenario: Box::new(scenario),
            frames: vec![beacon_frame()],
        })
        .unwrap();

        // The first pass landed before the override is pushed.
        loop {
            if let Some(Event::FrameReceived { id, .. }) = rx.blocking_recv() {
                if id.0 == "b" {
                    break;
                }
            }
        }

        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
        spawn(
            port,
            tx,
            "Steerable".to_owned(),
            Arc::clone(&last_received),
            vec![beacon_frame()],
        )
        .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(
            &mut client,
            r#"{"cmd":"set","step":1,"field":"mode","value":9}"#,
        );
        assert_eq!(response["ok"], true, "{response}");

        // Drain the rest of the run and remember every "b" received.
        let mut last_mode = None;
        loop {
            match rx.blocking_recv() {
                Some(Event::FrameReceived { id, bytes, .. }) if id.0 == "b" => {
                    // The test frame declares only `mode`, so it is the whole
                    // frame: one byte, at offset zero.
                    last_mode = Some(bytes[0]);
                    last_received
                        .lock()
                        .unwrap()
                        .insert(ConnectionId::from("b"), bytes);
                }
                Some(Event::ScenarioFinished { name, .. }) if name == "Steerable" => break,
                Some(_) => {}
                None => panic!("engine event channel closed unexpectedly"),
            }
        }
        assert_eq!(last_mode, Some(9), "the pushed value reached the last pass");

        let response = round_trip(
            &mut client,
            r#"{"cmd":"last_received","on":"b","as":"Beacon"}"#,
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["fields"]["mode"], 9, "{response}");
    }

    #[test]
    fn last_received_reports_nothing_yet_before_any_frame_arrives() {
        let (tx, _rx) = Engine::spawn();
        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
        spawn(port, tx, "Unused".to_owned(), last_received, Vec::new())
            .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(
            &mut client,
            r#"{"cmd":"last_received","on":"drive","as":"Beacon"}"#,
        );
        assert_eq!(response["ok"], false, "{response}");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("nothing received yet"));
    }

    #[test]
    fn a_bad_request_gets_a_json_error_without_closing_the_connection() {
        let (tx, _rx) = Engine::spawn();
        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
        spawn(port, tx, "Unused".to_owned(), last_received, Vec::new())
            .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(&mut client, "not json at all");
        assert_eq!(response["ok"], false, "{response}");

        // The same connection still answers the next, well-formed request.
        let response = round_trip(
            &mut client,
            r#"{"cmd":"last_received","on":"drive","as":"Beacon"}"#,
        );
        assert_eq!(response["ok"], false, "{response}");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("nothing received yet"));
    }
}
