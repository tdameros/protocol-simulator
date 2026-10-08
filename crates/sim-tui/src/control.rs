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

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;
use tokio::sync::mpsc;

use sim_core::frame::codec;
use sim_core::frame::value::Value;
use sim_core::frame::{FieldKind, FrameDef};
use sim_core::scenario::Action;
use sim_core::{Command, ConnectionId, Scenario};

/// The most recent bytes received on each connection, for `last_received` to
/// read back. Written by the headless front end's own event loop on every
/// `Event::FrameReceived`, read here on request.
pub type LastReceived = Arc<Mutex<HashMap<ConnectionId, Vec<u8>>>>;

/// The most recent bytes sent on each connection, for `last_sent` to read
/// back. Written by the headless front end's event loop on every
/// `Event::FrameSent`, read here on request.
pub type LastSent = Arc<Mutex<HashMap<ConnectionId, Vec<u8>>>>;
pub type RunStatus = Arc<Mutex<serde_json::Value>>;

/// The editable fields and the number of steps in the running scenario.
///
/// Keeping the count lets the control socket distinguish an unknown step from
/// a real step that simply is not a `send` action.
#[derive(Default)]
pub struct EditableSteps {
    count: usize,
    fields: HashMap<usize, HashMap<String, FieldKind>>,
}

pub type EditableFields = Arc<EditableSteps>;

/// Collects every declared field of each `send` step's frame.
#[must_use]
pub fn editable_fields(scenario: &Scenario, frames: &[FrameDef]) -> EditableFields {
    Arc::new(EditableSteps {
        count: scenario.steps.len(),
        fields: scenario
            .steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                let Action::Send { frame, .. } = &step.action else {
                    return None;
                };
                let fields = frames
                    .iter()
                    .find(|definition| definition.name == *frame)
                    .map_or_else(HashMap::new, |definition| {
                        definition
                            .fields
                            .iter()
                            .map(|field| (field.name.clone(), field.kind.clone()))
                            .collect()
                    });
                Some((index + 1, fields))
            })
            .collect(),
    })
}

/// Explains that a requested live value is not a field declared by this step's
/// send frame. Listing the accepted names makes a typo or an unsupported
/// nested path immediately apparent to a control-socket client.
fn unknown_field_error(field: &str, step: usize, fields: &HashMap<String, FieldKind>) -> String {
    let mut available = fields.keys().map(String::as_str).collect::<Vec<_>>();
    available.sort_unstable();
    format!(
        "field {field} does not exist in the send frame for step {step} (available fields: {})",
        available.join(", ")
    )
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Status,
    Set {
        step: usize,
        field: String,
        value: Value,
    },
    SetMany {
        step: usize,
        fields: HashMap<String, Value>,
    },
    LastReceived {
        on: String,
        #[serde(rename = "as")]
        frame: String,
    },
    LastSent {
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
#[expect(
    clippy::too_many_arguments,
    reason = "the socket service's shared state is explicit at its construction boundary"
)]
pub fn spawn(
    port: u16,
    commands: mpsc::Sender<Command>,
    scenario: String,
    editable_fields: EditableFields,
    last_received: LastReceived,
    last_sent: LastSent,
    status: RunStatus,
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
                let editable_fields = Arc::clone(&editable_fields);
                let last_received = Arc::clone(&last_received);
                let last_sent = Arc::clone(&last_sent);
                let status = Arc::clone(&status);
                let frames = Arc::clone(&frames);
                thread::spawn(move || {
                    serve(
                        &stream,
                        &commands,
                        &scenario,
                        &editable_fields,
                        &last_received,
                        &last_sent,
                        &status,
                        &frames,
                    );
                });
            }
        })
        .expect("failed to spawn control socket thread");
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "a connection handler receives the socket service's explicit shared state"
)]
fn serve(
    stream: &TcpStream,
    commands: &mpsc::Sender<Command>,
    scenario: &str,
    editable_fields: &EditableFields,
    last_received: &LastReceived,
    last_sent: &LastSent,
    status: &RunStatus,
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
            Ok(request) => handle(
                request,
                commands,
                scenario,
                editable_fields,
                last_received,
                last_sent,
                status,
                frames,
            ),
            Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
        };
        if writeln!(writer, "{response}").is_err() {
            return;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "request handling needs each independently shared control resource"
)]
fn handle(
    request: Request,
    commands: &mpsc::Sender<Command>,
    scenario: &str,
    editable_fields: &EditableFields,
    last_received: &LastReceived,
    last_sent: &LastSent,
    status: &RunStatus,
    frames: &[FrameDef],
) -> serde_json::Value {
    match request {
        Request::Status => status.lock().expect("status mutex poisoned").clone(),
        Request::Set { step, field, value } => {
            if step == 0 || step > editable_fields.count {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("no step {step}; expected 1 through {}", editable_fields.count),
                });
            }
            let Some(fields) = editable_fields.fields.get(&step) else {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("step {step} is not a send step"),
                });
            };
            let Some(kind) = fields.get(&field) else {
                return serde_json::json!({
                    "ok": false,
                    "error": unknown_field_error(&field, step, fields),
                });
            };
            let value = match control_value(value, kind) {
                Ok(value) => value,
                Err(error) => return serde_json::json!({ "ok": false, "error": error }),
            };
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
        Request::SetMany { step, fields } => {
            if step == 0 || step > editable_fields.count {
                return serde_json::json!({ "ok": false, "error": format!("no step {step}; expected 1 through {}", editable_fields.count) });
            }
            let Some(known) = editable_fields.fields.get(&step) else {
                return serde_json::json!({ "ok": false, "error": format!("step {step} is not a send step") });
            };
            let mut values = Vec::new();
            for (field, value) in fields {
                let Some(kind) = known.get(&field) else {
                    return serde_json::json!({ "ok": false, "error": unknown_field_error(&field, step, known) });
                };
                let Ok(value) = control_value(value, kind) else {
                    return serde_json::json!({ "ok": false, "error": "invalid field value" });
                };
                values.push((field, value));
            }
            for (field, value) in values {
                if commands
                    .blocking_send(Command::SetStepValue {
                        scenario: scenario.to_owned(),
                        step,
                        field,
                        value,
                    })
                    .is_err()
                {
                    return serde_json::json!({ "ok": false, "error": "the engine is not running" });
                }
            }
            serde_json::json!({ "ok": true })
        }
        Request::LastReceived { on, frame: name } => {
            let id = ConnectionId::from(on.as_str());
            let bytes = last_received
                .lock()
                .expect("last-received mutex poisoned")
                .get(&id)
                .cloned();
            last_frame_response(bytes, &on, &name, "received", frames)
        }
        Request::LastSent { on, frame: name } => {
            let id = ConnectionId::from(on.as_str());
            let bytes = last_sent
                .lock()
                .expect("last-sent mutex poisoned")
                .get(&id)
                .cloned();
            last_frame_response(bytes, &on, &name, "sent", frames)
        }
    }
}

fn last_frame_response(
    bytes: Option<Vec<u8>>,
    on: &str,
    name: &str,
    direction: &str,
    frames: &[FrameDef],
) -> serde_json::Value {
    let Some(bytes) = bytes else {
        return serde_json::json!({
            "ok": false,
            "error": format!("nothing {direction} yet on {on}"),
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
            let kinds: serde_json::Map<String, serde_json::Value> = definition
                .fields
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        serde_json::Value::String(field.kind.type_name().to_owned()),
                    )
                })
                .collect();
            let field_order: Vec<String> = definition
                .fields
                .iter()
                .map(|field| field.name.clone())
                .collect();
            let bit_layouts: serde_json::Map<String, serde_json::Value> = definition
                .fields
                .iter()
                .filter_map(|field| {
                    let FieldKind::Bits { bits, .. } = &field.kind else {
                        return None;
                    };
                    Some((
                        field.name.clone(),
                        serde_json::json!(bits
                            .iter()
                            .map(|bit| serde_json::json!({ "name": bit.name, "width": bit.width }))
                            .collect::<Vec<_>>()),
                    ))
                })
                .collect();
            serde_json::json!({
                "ok": true,
                "bytes": sim_session::hex::spaced(&bytes),
                "fields": fields,
                "kinds": kinds,
                "field_order": field_order,
                "bit_layouts": bit_layouts,
            })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    }
}

fn control_value(value: Value, kind: &FieldKind) -> Result<Value, String> {
    let FieldKind::Bits { repr, bits } = kind else {
        return Ok(value);
    };
    let Some(raw) = value.as_uint() else {
        return Err("a bitfield needs an unsigned decimal integer".to_owned());
    };
    let width = repr.size() * 8;
    if width < 64 && raw >= (1u64 << width) {
        return Err(format!(
            "bitfield value {raw} does not fit in {}",
            repr.name()
        ));
    }
    let mut remaining = u32::try_from(width).unwrap_or(u32::MAX);
    let mut values = BTreeMap::new();
    for bit in bits {
        remaining -= bit.width;
        let mask = if bit.width >= 64 {
            u64::MAX
        } else {
            (1u64 << bit.width) - 1
        };
        values.insert(bit.name.clone(), (raw >> remaining) & mask);
    }
    Ok(Value::Bits(values))
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
[[field]]
name = "spare"
type = "u8"
default = 0
"#,
        )
        .expect("the test frame should parse")
    }

    #[test]
    fn decimal_bitfield_values_are_accepted() {
        let frame = sim_core::frame::schema::from_toml(
            "name = \"Flags\"\n[[field]]\nname = \"flags\"\ntype = \"bits\"\nrepr = \"u8\"\nbits = [{ name = \"alarm\", width = 1 }, { name = \"level\", width = 2 }, { name = \"spare\", width = 5 }]\n",
        )
        .expect("the test frame should parse");
        let value = control_value(Value::Uint(196), &frame.fields[0].kind)
            .expect("the raw value should fit");
        let Value::Bits(bits) = value else {
            panic!("a raw bitfield value should become sub-fields");
        };
        assert_eq!(bits["alarm"], 1);
        assert_eq!(bits["level"], 2);
        assert_eq!(bits["spare"], 4);
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
    #[expect(
        clippy::too_many_lines,
        reason = "the integration test makes a complete socket-to-engine interaction explicit"
    )]
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
repeat = { every_ms = 30, times = 10 }

[[scenario.step]]
send = "Beacon"
with = { mode = 1 }
"#,
        )
        .expect("scenario should parse")
        .remove(0);
        let frame = beacon_frame();
        let fields = editable_fields(&scenario, std::slice::from_ref(&frame));

        tx.blocking_send(Command::StartScenario {
            scenario: Box::new(scenario),
            frames: vec![frame.clone()],
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
        let last_sent: LastSent = Arc::new(Mutex::new(HashMap::new()));
        spawn(
            port,
            tx,
            "Steerable".to_owned(),
            fields,
            Arc::clone(&last_received),
            last_sent,
            Arc::new(Mutex::new(
                serde_json::json!({"ok": true, "state": "running"}),
            )),
            vec![frame],
        )
        .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(
            &mut client,
            r#"{"cmd":"set","step":2,"field":"mode","value":9}"#,
        );
        assert_eq!(response["ok"], false, "{response}");
        assert_eq!(response["error"], "no step 2; expected 1 through 1");

        let response = round_trip(
            &mut client,
            r#"{"cmd":"set","step":1,"field":"unknown","value":9}"#,
        );
        assert_eq!(response["ok"], false, "{response}");
        assert_eq!(
            response["error"],
            "field unknown does not exist in the send frame for step 1 (available fields: mode, spare)"
        );

        let response = round_trip(
            &mut client,
            r#"{"cmd":"set","step":1,"field":"spare","value":8}"#,
        );
        assert_eq!(response["ok"], true, "{response}");

        let response = round_trip(
            &mut client,
            r#"{"cmd":"set","step":1,"field":"mode","value":9}"#,
        );
        assert_eq!(response["ok"], true, "{response}");

        // Drain the rest of the run and remember every "b" received.
        let mut last_values = None;
        loop {
            match rx.blocking_recv() {
                Some(Event::FrameReceived { id, bytes, .. }) if id.0 == "b" => {
                    last_values = Some((bytes[0], bytes[1]));
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
        assert_eq!(
            last_values,
            Some((9, 8)),
            "the pushed values reached the last pass"
        );

        let response = round_trip(
            &mut client,
            r#"{"cmd":"last_received","on":"b","as":"Beacon"}"#,
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["fields"]["mode"], 9, "{response}");
        assert_eq!(response["fields"]["spare"], 8, "{response}");
        assert_eq!(response["kinds"]["mode"], "u8", "{response}");
        assert_eq!(
            response["field_order"],
            serde_json::json!(["mode", "spare"]),
            "{response}"
        );
    }

    #[test]
    fn last_received_reports_nothing_yet_before_any_frame_arrives() {
        let (tx, _rx) = Engine::spawn();
        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
        let last_sent: LastSent = Arc::new(Mutex::new(HashMap::new()));
        let fields: EditableFields = Arc::new(EditableSteps::default());
        spawn(
            port,
            tx,
            "Unused".to_owned(),
            fields,
            last_received,
            last_sent,
            Arc::new(Mutex::new(
                serde_json::json!({"ok": true, "state": "running"}),
            )),
            Vec::new(),
        )
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
        let last_sent: LastSent = Arc::new(Mutex::new(HashMap::new()));
        let fields: EditableFields = Arc::new(EditableSteps::default());
        spawn(
            port,
            tx,
            "Unused".to_owned(),
            fields,
            last_received,
            last_sent,
            Arc::new(Mutex::new(
                serde_json::json!({"ok": true, "state": "running"}),
            )),
            Vec::new(),
        )
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

    #[test]
    fn last_sent_decodes_the_most_recent_transmission() {
        let (tx, _rx) = Engine::spawn();
        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
        let last_sent: LastSent = Arc::new(Mutex::new(HashMap::from([(
            ConnectionId::from("drive"),
            vec![7, 0],
        )])));
        let fields: EditableFields = Arc::new(EditableSteps::default());
        spawn(
            port,
            tx,
            "Unused".to_owned(),
            fields,
            last_received,
            last_sent,
            Arc::new(Mutex::new(
                serde_json::json!({"ok": true, "state": "running"}),
            )),
            vec![beacon_frame()],
        )
        .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(
            &mut client,
            r#"{"cmd":"last_sent","on":"drive","as":"Beacon"}"#,
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["fields"]["mode"], 7, "{response}");
        assert_eq!(response["kinds"]["mode"], "u8", "{response}");
        assert_eq!(
            response["field_order"],
            serde_json::json!(["mode", "spare"]),
            "{response}"
        );
    }
}
