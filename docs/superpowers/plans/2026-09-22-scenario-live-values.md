# Scenario Live Values Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a running scenario's `send` step field be changed while it runs, and let `sim-tui --run` expose that plus the last received frame over a local TCP control socket for an external Python script to drive.

**Architecture:** A `Command::SetStepValue` reaches the engine like any other command, but the override it carries lives in an `Arc<Mutex<_>>` shared directly with the running scenario's task rather than crossing a channel into it, following the same pattern `StopScenario` already uses (`ScenarioHandle` reached into directly). `sim-tui --run` opens a `std::net::TcpListener` on its own thread, one thread per connection, speaking one JSON request/response per line, translating each request into that same command plus a read against a shared "last bytes received per connection" map fed by the existing headless event loop.

**Tech Stack:** Rust, tokio (`sync` feature only, in `sim-tui`), `serde` + `serde_json` (new to `sim-tui`), `std::net::TcpListener` (no async networking needed for the control socket).

**Spec:** `docs/superpowers/specs/2026-09-22-scenario-live-values-design.md`

## Global Constraints

- Run `make ci` before every commit (fmt, clippy `-D warnings`, release type-check, tests). Fix, do not suppress, any clippy finding.
- `#![deny(clippy::all)]` and `#![warn(clippy::pedantic)]` apply to every file touched.
- Comments explain why, never what. No comment that paraphrases the next line.
- Commits: conventional prefixes (`feat:`, `fix:`, `test:`, `chore:`, `docs:`), subject line only, no body, ≤ 72 characters, in English, imperative mood.
- A "step" number is always counted from one, matching `Event::ScenarioStep` and every existing step-numbered error message. Never the zero-based `Vec<Step>` index.
- An override only ever replaces a field already present in that step's `with`. No validation rejects a `set` request naming a step or field the running scenario does not recognise: it behaves exactly as a bad `with` value in the file already does today (silently unread if the step index is never a `send`, or the pass fails if the field does not exist on the frame).
- No dependency choice happens silently: `serde_json` is the one new dependency this plan adds, already agreed. Anything else discovered mid-task gets flagged before it is added.

---

### Task 1: Live overrides reach a running scenario (sim-core)

**Files:**
- Modify: `crates/sim-core/src/engine.rs`
- Modify: `crates/sim-core/src/runner.rs`
- Test: `crates/sim-core/tests/loopback.rs`

**Interfaces:**
- Produces: `sim_core::Command::SetStepValue { scenario: String, step: usize, field: String, value: sim_core::frame::value::Value }`, re-exported the same way `Command::StopScenario` already is (via `pub use engine::{Command, Engine, Event};` in `lib.rs`, unchanged).
- Produces: `pub(crate) type runner::Overrides = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<(usize, String), sim_core::frame::value::Value>>>`, crate-internal, not used outside `sim-core`.

- [ ] **Step 1: Write the failing test**

Append to `crates/sim-core/tests/loopback.rs`, after `a_repeating_scenario_emits_encoded_frames_with_a_counter`:

```rust
/// A live override reaches a running scenario without restarting it: the
/// pass already under way when it lands still carries the file's own value,
/// and every pass after that carries the pushed one.
#[tokio::test]
async fn a_live_override_changes_what_a_running_scenario_sends() {
    let (tx, mut rx) = Engine::spawn();

    let addr_a = "127.0.0.1:19891".parse().unwrap();
    let addr_b = "127.0.0.1:19892".parse().unwrap();
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

    tx.send(Command::StartScenario {
        scenario: Box::new(scenario),
        frames: vec![scenario_frame()],
    })
    .await
    .unwrap();

    // The first pass has to have landed before the override is pushed, or it
    // would be ambiguous which pass it took effect from.
    wait_for(
        &mut rx,
        |event| matches!(event, Event::FrameReceived { id, .. } if id.0 == "b"),
    )
    .await;

    tx.send(Command::SetStepValue {
        scenario: "Steerable".to_owned(),
        step: 1,
        field: "mode".to_owned(),
        value: sim_core::frame::value::Value::Uint(9),
    })
    .await
    .unwrap();

    let seen = gather_until(&mut rx, |events| {
        finished_with(events, "Steerable", &sim_core::Outcome::Completed)
    })
    .await;

    let frames = beacons(&seen, "b");
    assert_eq!(frames.len(), 3, "one frame per pass");
    assert_eq!(frames[0][3], 1, "the file's own value, before the override");
    assert_eq!(frames[2][3], 9, "the pushed value, once it has landed");
}
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p sim-core --test loopback a_live_override_changes_what_a_running_scenario_sends`
Expected: FAIL to compile, `Command::SetStepValue` does not exist yet.

- [ ] **Step 3: Add the type overrides live in, and read it when encoding a send**

In `crates/sim-core/src/runner.rs`, add near the top, after the existing `use` block (after the `use crate::scenario::{...}` line):

```rust
use std::sync::{Arc, Mutex};
```

Add just above `pub(crate) struct Context`:

```rust
/// Live overrides for a running scenario's `send` steps, keyed by the step's
/// one-based number and field name, matching what `Event::ScenarioStep` and
/// every step-numbered error already mean by "step 2".
///
/// Shared directly with the engine's command loop rather than reached through
/// a channel into this task, the same way `StopScenario` already reaches
/// `ScenarioHandle` directly instead of sending the task a message.
pub(crate) type Overrides = Arc<Mutex<std::collections::BTreeMap<(usize, String), Value>>>;
```

Add a field to `Context`:

```rust
pub(crate) struct Context {
    pub scenario: Scenario,
    pub frames: Vec<FrameDef>,
    pub commands: mpsc::WeakSender<Command>,
    pub events: mpsc::Sender<Event>,
    pub received: broadcast::Receiver<Heard>,
    pub overrides: Overrides,
}
```

In `run()`, the call to `execute` currently reads:

```rust
            match execute(
                step,
                pass,
                &context.frames,
                &context.commands,
                &mut received,
                &mut variables,
            )
            .await
```

Change it to pass the step's one-based number and the overrides:

```rust
            match execute(
                step,
                index + 1,
                pass,
                &context.frames,
                &context.commands,
                &mut received,
                &mut variables,
                &context.overrides,
            )
            .await
```

Change `execute`'s signature and its `Action::Send` arm:

```rust
async fn execute(
    step: &Step,
    number: usize,
    pass: u32,
    frames: &[FrameDef],
    commands: &mpsc::WeakSender<Command>,
    received: &mut broadcast::Receiver<Heard>,
    variables: &mut BTreeMap<String, Value>,
    overrides: &Overrides,
) -> StepResult {
    match &step.action {
        Action::Wait { delay } => {
            tokio::time::sleep(*delay).await;
            StepResult::Done
        }
        Action::Raw { bytes } => send_to_all(commands, &step.targets, bytes).await,
        Action::Send {
            frame,
            with,
            counters,
            from_capture,
        } => {
            let with = merged(with, number, overrides);
            match encode(frame, &with, counters, from_capture, variables, pass, frames) {
                Ok(bytes) => send_to_all(commands, &step.targets, &bytes).await,
                Err(reason) => StepResult::Failed(reason),
            }
        }
```

(The `Action::WaitFor { .. }` arm below is unchanged.)

Add the merge helper near `encode`:

```rust
/// The step's own `with`, replaced field by field with whatever a live
/// override currently holds for this step's number.
///
/// Only ever touches a field already present in `with`: a scenario declares,
/// by putting a field there at all, which of its fields can be steered live.
/// An override naming anything else is written down but never read here,
/// since no field of `with` ever matches it.
fn merged(
    with: &BTreeMap<String, Value>,
    number: usize,
    overrides: &Overrides,
) -> BTreeMap<String, Value> {
    let mut merged = with.clone();
    let held = overrides.lock().expect("overrides mutex poisoned");
    for field in with.keys() {
        if let Some(value) = held.get(&(number, field.clone())) {
            merged.insert(field.clone(), value.clone());
        }
    }
    merged
}
```

- [ ] **Step 4: Wire the command and the handle that carries the overrides**

In `crates/sim-core/src/engine.rs`, add to the imports:

```rust
use std::collections::BTreeMap;
use std::sync::Mutex;
```

(`HashMap` and `Arc` are already imported; keep them.)

Add to the `use crate::...` block:

```rust
use crate::frame::value::Value;
```

Add a variant to `Command`, after `StopScenario`:

```rust
    /// Overrides one field of a running scenario's `send` step, effective
    /// from the next time that step runs. `step` counts from one, as the
    /// file and `Event::ScenarioStep` already do. Naming a step or field the
    /// scenario does not recognise is not reported: it behaves exactly as a
    /// bad `with` value in the file already does.
    SetStepValue {
        scenario: String,
        step: usize,
        field: String,
        value: Value,
    },
```

Add a field to `ScenarioHandle`:

```rust
struct ScenarioHandle {
    task: JoinHandle<()>,
    generation: u64,
    overrides: runner::Overrides,
}
```

Add a match arm to `handle_command`, after the `Command::StopScenario` arm:

```rust
        Command::SetStepValue {
            scenario,
            step,
            field,
            value,
        } => match scenarios.get(&scenario) {
            Some(handle) => {
                handle
                    .overrides
                    .lock()
                    .expect("overrides mutex poisoned")
                    .insert((step, field), value);
            }
            None => report_error(events, None, EngineError::UnknownScenario(scenario)).await,
        },
```

In `start_scenario`, build the shared overrides, clone it into the `Context`, and keep the other clone in the handle:

```rust
fn start_scenario(
    scenario: Scenario,
    frames: Vec<FrameDef>,
    wiring: &Wiring,
    generation: u64,
) -> ScenarioHandle {
    let name = scenario.name.clone();
    let overrides: runner::Overrides = Arc::new(Mutex::new(BTreeMap::new()));
    let context = runner::Context {
        scenario,
        frames,
        commands: wiring.issue.clone(),
        events: wiring.events.clone(),
        received: wiring.heard.subscribe(),
        overrides: Arc::clone(&overrides),
    };
    let ended = wiring.ended.clone();

    let task = tokio::spawn(async move {
        let outcome = runner::run(context).await;
        let _ = ended.send((name, generation, outcome)).await;
    });

    ScenarioHandle {
        task,
        generation,
        overrides,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p sim-core --test loopback a_live_override_changes_what_a_running_scenario_sends`
Expected: PASS

- [ ] **Step 6: Format, run the full sim-core suite, and lint**

Run: `cargo fmt --all`
Run: `make test-core`
Run: `cargo clippy -p sim-core --all-targets -- -D warnings`
Expected: all three clean.

- [ ] **Step 7: Commit**

```bash
git checkout -b feat/scenario-live-values
git add crates/sim-core/src/engine.rs crates/sim-core/src/runner.rs crates/sim-core/tests/loopback.rs
git commit -m "feat(core): let a live command override a running send step"
```

---

### Task 2: A cloneable command sender for a thread outside EngineHandle (sim-session)

**Files:**
- Modify: `crates/sim-session/src/engine_handle.rs`

**Interfaces:**
- Consumes: `sim_core::Command` (from Task 1, unchanged shape otherwise).
- Produces: `EngineHandle::command_sender(&self) -> tokio::sync::mpsc::Sender<sim_core::Command>`.

- [ ] **Step 1: Write the failing test**

Add at the end of `crates/sim-session/src/engine_handle.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::EngineHandle;
    use sim_core::{Command, ConnectionId};

    /// The sender handed out reaches the same engine `drain_events` does,
    /// proven by a command that produces an event `drain_events` can see.
    #[test]
    fn command_sender_reaches_the_same_engine() {
        let mut handle = EngineHandle::new();
        let sender = handle.command_sender();

        sender
            .blocking_send(Command::Disconnect {
                id: ConnectionId::from("nothing"),
            })
            .expect("the engine's command channel should still be open");

        let events = loop {
            let events = handle.drain_events();
            if !events.is_empty() {
                break events;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            events
                .iter()
                .any(|event| matches!(event, sim_core::Event::Error { .. })),
            "disconnecting a connection that does not exist should be reported: {events:?}"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p sim-session --lib engine_handle::tests::command_sender_reaches_the_same_engine`
Expected: FAIL to compile, `command_sender` does not exist yet.

- [ ] **Step 3: Add the accessor**

In `crates/sim-session/src/engine_handle.rs`, add a method next to `stop_scenario`:

```rust
    /// A cloned handle to the command channel alone, for a caller that only
    /// ever issues commands and has no use for the event side.
    ///
    /// `EngineHandle` itself cannot be shared across threads: its drop
    /// counter is a `Cell`, so the type is deliberately not `Sync`. A caller
    /// on another thread, such as a control socket's own connection
    /// handler, takes this instead.
    #[must_use]
    pub fn command_sender(&self) -> mpsc::Sender<Command> {
        self.command_tx.clone()
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p sim-session --lib engine_handle::tests::command_sender_reaches_the_same_engine`
Expected: PASS

- [ ] **Step 5: Format, run the full sim-session suite, and lint**

Run: `cargo fmt --all`
Run: `cargo test -p sim-session`
Run: `cargo clippy -p sim-session --all-targets -- -D warnings`
Expected: all three clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sim-session/src/engine_handle.rs
git commit -m "feat(session): expose a cloneable command sender"
```

---

### Task 3: The control socket protocol (sim-tui)

**Files:**
- Modify: `crates/sim-tui/Cargo.toml`
- Create: `crates/sim-tui/src/control.rs`
- Modify: `crates/sim-tui/src/main.rs` (add `mod control;`)

**Interfaces:**
- Consumes: `sim_core::Command::SetStepValue` (Task 1), `EngineHandle::command_sender` (Task 2, used only in Task 4's wiring, not here).
- Produces:
  - `pub type control::LastReceived = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<sim_core::ConnectionId, Vec<u8>>>>`
  - `pub fn control::spawn(port: u16, commands: tokio::sync::mpsc::Sender<sim_core::Command>, scenario: String, last_received: LastReceived, frames: Vec<sim_core::frame::FrameDef>) -> std::io::Result<()>`

- [ ] **Step 1: Add the dependencies**

Run:

```bash
cargo add serde --package sim-tui --features derive
cargo add serde_json --package sim-tui
cargo add tokio --package sim-tui --features sync --no-default-features
```

Check `crates/sim-tui/Cargo.toml` afterwards: `tokio` must end up with only the `sync` feature (matching `sim-session`'s own `tokio = { version = "...", features = ["sync"] }`), not pulling in `rt`, `net`, or anything else sim-tui does not need. Trim the line by hand if `cargo add` pulled in more than `sync`.

- [ ] **Step 2: Regenerate THIRD-PARTY.md**

Run: `make third-party`
Expected: `THIRD-PARTY.md` gains entries for `serde_json` and whatever it pulls in that was not already there.

- [ ] **Step 3: Write the failing test**

Create `crates/sim-tui/src/control.rs` with just enough to compile the test against, and the test itself:

```rust
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

use std::collections::BTreeMap;
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
pub type LastReceived = Arc<Mutex<BTreeMap<ConnectionId, Vec<u8>>>>;

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
                thread::spawn(move || serve(&stream, &commands, &scenario, &last_received, &frames));
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
    use std::time::Duration;

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
        let last_received: LastReceived = Arc::new(Mutex::new(BTreeMap::new()));
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

        let response = round_trip(&mut client, r#"{"cmd":"last_received","on":"b","as":"Beacon"}"#);
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["fields"]["mode"], 9, "{response}");
    }

    #[test]
    fn last_received_reports_nothing_yet_before_any_frame_arrives() {
        let (tx, _rx) = Engine::spawn();
        let port = free_tcp_port();
        let last_received: LastReceived = Arc::new(Mutex::new(BTreeMap::new()));
        spawn(port, tx, "Unused".to_owned(), last_received, Vec::new())
            .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(&mut client, r#"{"cmd":"last_received","on":"drive","as":"Beacon"}"#);
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
        let last_received: LastReceived = Arc::new(Mutex::new(BTreeMap::new()));
        spawn(port, tx, "Unused".to_owned(), last_received, Vec::new())
            .expect("the control socket should bind");

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("should connect");
        let response = round_trip(&mut client, "not json at all");
        assert_eq!(response["ok"], false, "{response}");

        // The same connection still answers the next, well-formed request.
        let response = round_trip(&mut client, r#"{"cmd":"last_received","on":"drive","as":"Beacon"}"#);
        assert_eq!(response["ok"], false, "{response}");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("nothing received yet"));
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail to compile**

Run: `cargo test -p sim-tui --lib control::`
Expected: FAIL to compile to start with (module not wired into `main.rs` yet), then once wired (next step), the three tests should compile and pass immediately since the implementation above is not test-then-code split further — this module is small enough, and its three tests are the acceptance test for the whole file, that writing it complete and then running it is the practical TDD unit here rather than red-green-red-green across a dozen micro-diffs of one file.

- [ ] **Step 5: Wire the module in**

In `crates/sim-tui/src/main.rs`, add near the other `mod` declarations:

```rust
mod control;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p sim-tui --lib control::`
Expected: PASS, all three tests.

- [ ] **Step 7: Format, run the full sim-tui suite, and lint**

Run: `cargo fmt --all`
Run: `cargo test -p sim-tui`
Run: `cargo clippy -p sim-tui --all-targets -- -D warnings`
Expected: all three clean. Pay attention to `clippy::pedantic` findings on `control.rs` specifically (e.g. `must_use`, `missing_errors_doc`) and fix them rather than allow them, matching every other file in this crate.

- [ ] **Step 8: Commit**

```bash
git add crates/sim-tui/Cargo.toml crates/sim-tui/src/control.rs crates/sim-tui/src/main.rs THIRD-PARTY.md
git commit -m "feat(tui): add a local control socket protocol"
```

---

### Task 4: `--control-port` wires the socket into `--run` (sim-tui)

**Files:**
- Modify: `crates/sim-tui/src/headless.rs`
- Modify: `crates/sim-tui/src/main.rs`
- Modify: `README.md`
- Modify: `docs/scenarios.md`

**Interfaces:**
- Consumes: `control::spawn` and `control::LastReceived` (Task 3), `EngineHandle::command_sender` (Task 2).
- Produces: `headless::run(opened_with: Option<PathBuf>, scenario_name: &str, control_port: Option<u16>) -> i32` (signature change from the current two-argument form; every existing caller and test updates in this task).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `crates/sim-tui/src/headless.rs`, after the existing fixture helpers. It reuses `a_project_on_disk`, which needs one change: the `Ping` frame it writes gains a `mode` field, so a `with = { mode = ... }` step has something to override. Update the frame file written there:

```rust
        std::fs::write(
            root.join("frames").join("ping.toml"),
            "name = \"Ping\"\n[[field]]\nname = \"seq\"\ntype = \"u8\"\n\
             [[field]]\nname = \"mode\"\ntype = \"u8\"\ndefault = 1\n",
        )
        .expect("a frame file");
```

(This only adds a field with a default; the four existing tests using this fixture send `Ping` with no `with` at all, so nothing about what they assert changes.)

Add:

```rust
    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("a bound address")
            .port()
    }

    #[test]
    fn a_control_port_lets_an_external_client_steer_the_run() {
        let path = a_project_on_disk(
            "control",
            "[[scenario]]\nname = \"Steerable\"\non = \"loop\"\nrepeat = { every_ms = 30, times = 3 }\n\n\
             [[scenario.step]]\nsend = \"Ping\"\nwith = { mode = 1 }\n",
        );
        let control_port = free_tcp_port();

        let path_for_thread = path.clone();
        let runner = std::thread::spawn(move || {
            run(Some(path_for_thread), "Steerable", Some(control_port))
        });

        // Give the run a moment to open its connection and the control
        // socket before dialling in.
        let mut client = loop {
            match std::net::TcpStream::connect(("127.0.0.1", control_port)) {
                Ok(stream) => break stream,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };

        use std::io::{BufRead, BufReader, Write};
        writeln!(client, r#"{{"cmd":"set","step":1,"field":"mode","value":9}}"#)
            .expect("write should succeed");
        let mut reader = BufReader::new(client.try_clone().expect("clone should succeed"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read should succeed");
        assert!(line.contains("\"ok\":true"), "{line}");

        assert_eq!(runner.join().expect("the run thread should not panic"), 0);

        writeln!(
            client,
            r#"{{"cmd":"last_received","on":"loop","as":"Ping"}}"#
        )
        .expect("write should succeed");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read should succeed");
        let response: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(response["ok"], true, "{line}");
        assert_eq!(response["fields"]["mode"], 9, "{line}");
    }
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p sim-tui --lib headless::tests::a_control_port_lets_an_external_client_steer_the_run`
Expected: FAIL to compile, `run` still takes two arguments.

- [ ] **Step 3: Change `run`'s signature and wire the control socket in**

In `crates/sim-tui/src/headless.rs`, add to the imports:

```rust
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::control::{self, LastReceived};
```

Change the signature and body of `run`:

```rust
/// Loads `opened_with`, starts the scenario named `scenario_name`, and prints
/// what happens until it ends. The exit code a shell or a service manager
/// reads: 0 once the scenario completed, 1 for anything else.
///
/// `control_port`, when given, opens a local TCP socket an external script
/// can use to override a `send` step's field live and read back the last
/// frame received on a connection, without restarting the scenario.
pub fn run(opened_with: Option<PathBuf>, scenario_name: &str, control_port: Option<u16>) -> i32 {
    let Some(path) = opened_with else {
        eprintln!("--run needs a project file: protocol-simulator-tui project.toml --run \"Name\"");
        return 1;
    };

    let mut session = Session::default();
    let restored = Project::read(&path).and_then(|read| read.apply(&mut session, Some(&path)));
    let restored = match restored {
        Ok(restored) => restored,
        Err(error) => {
            eprintln!("{}: {error:#}", path.display());
            return 1;
        }
    };

    let Some(entry) = session
        .scenarios
        .entries
        .iter()
        .find(|entry| entry.scenario.name == scenario_name)
    else {
        eprintln!(
            "no scenario named \"{scenario_name}\" in {}",
            path.display()
        );
        return 1;
    };
    let scenario = entry.scenario.clone();

    let mut engine = EngineHandle::new();
    for (id, config, retry) in restored.connect {
        println!("connecting {}...", id.0);
        engine.connect(id, config, retry);
    }

    let last_received: LastReceived = Arc::new(Mutex::new(BTreeMap::new()));
    if let Some(port) = control_port {
        let frames = session.frames.frames().cloned().collect();
        if let Err(error) = control::spawn(
            port,
            engine.command_sender(),
            scenario_name.to_owned(),
            Arc::clone(&last_received),
            frames,
        ) {
            eprintln!("cannot open the control socket on port {port}: {error}");
            return 1;
        }
    }

    scenarios::start(&mut session, &engine, &scenario);
    if let Some(error) = session.last_error.take() {
        eprintln!("{error}");
        return 1;
    }

    watch(engine, scenario_name, &last_received)
}
```

Change `watch`'s signature to record every received frame:

```rust
fn watch(mut engine: EngineHandle, scenario_name: &str, last_received: &LastReceived) -> i32 {
    loop {
        for event in engine.drain_events() {
            match event {
                Event::ConnectionStatus { id, status } => {
                    println!("{}: {}", id.0, links::status(status));
                }
                Event::FrameSent { id, bytes, .. } => {
                    println!("TX {} {}", id.0, hex::spaced(&bytes));
                }
                Event::FrameReceived { id, bytes, .. } => {
                    println!("RX {} {}", id.0, hex::spaced(&bytes));
                    last_received.lock().unwrap().insert(id, bytes);
                }
                Event::Error { id, error } => {
                    let who = id.map(|id| id.0).unwrap_or_default();
                    eprintln!("{who}: {error}");
                }
                Event::ScenarioStep { name, step, pass } if name == scenario_name => {
                    println!("[{name}] pass {pass}, step {step}");
                }
                Event::ScenarioFinished { name, outcome } if name == scenario_name => {
                    println!("[{name}] {outcome:?}");
                    return i32::from(outcome != Outcome::Completed);
                }
                Event::ScenarioStep { .. } | Event::ScenarioFinished { .. } => {}
            }
        }
        std::thread::sleep(POLL);
    }
}
```

Note `last_received.lock().unwrap().insert(id, bytes)` moves `bytes` after it was already used by the `println!` on the line above: reorder so the print happens after the move, or clone. Write it as:

```rust
                Event::FrameReceived { id, bytes, .. } => {
                    println!("RX {} {}", id.0, hex::spaced(&bytes));
                    last_received.lock().unwrap().insert(id, bytes);
                }
```

(`hex::spaced(&bytes)` borrows, it does not move, so this order is already fine as written; `insert(id, bytes)` is the only place `bytes` moves, and it happens last.)

- [ ] **Step 4: Update the four existing call sites in the same file's tests**

In the `tests` module of `crates/sim-tui/src/headless.rs`, every existing `run(Some(path), "...")` becomes `run(Some(path), "...", None)`:

```rust
    #[test]
    fn a_scenario_run_from_the_command_line_exits_zero_once_it_completes() {
        let path = a_project_on_disk(
            "completes",
            "[[scenario]]\nname = \"Once\"\non = \"loop\"\n\n[[scenario.step]]\nsend = \"Ping\"\n",
        );
        assert_eq!(run(Some(path), "Once", None), 0);
    }

    #[test]
    fn an_unknown_scenario_name_exits_nonzero() {
        let path = a_project_on_disk(
            "unknown",
            "[[scenario]]\nname = \"Once\"\non = \"loop\"\n\n[[scenario.step]]\nsend = \"Ping\"\n",
        );
        assert_eq!(run(Some(path), "Never heard of it", None), 1);
    }

    #[test]
    fn a_missing_project_path_exits_nonzero() {
        assert_eq!(run(None, "Once", None), 1);
    }

    #[test]
    fn a_project_file_that_does_not_exist_exits_nonzero() {
        assert_eq!(run(Some(PathBuf::from("/does/not/exist.toml")), "Once", None), 1);
    }
```

- [ ] **Step 5: Update `main.rs`'s CLI parsing and the one call to `run`**

In `crates/sim-tui/src/main.rs`, change the argument parsing:

```rust
    let mut opened_with = None;
    let mut run_scenario = None;
    let mut control_port = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--run" {
            run_scenario = args.next();
        } else if arg == "--control-port" {
            control_port = args.next().and_then(|value| value.parse().ok());
        } else {
            opened_with = Some(PathBuf::from(arg));
        }
    }

    if let Some(name) = run_scenario {
        std::process::exit(headless::run(opened_with, &name, control_port));
    }
```

Update the doc comment just above this block, which currently only describes `--run`, to mention `--control-port`:

```rust
    // One positional argument, as the window takes: a project file, or a folder
    // of frame definitions. No file picker, since the machine this runs on is
    // usually reached over ssh and has no desktop to put one on.
    //
    // `--run NAME` skips the screen entirely and runs one scenario from the
    // project, for a service started at boot rather than a person at a
    // keyboard. `--control-port PORT`, only meaningful alongside `--run`,
    // opens a local TCP socket an external script can use to override a
    // `send` step's field live and read back the last frame received on a
    // connection.
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p sim-tui --lib headless::tests`
Expected: PASS, all of them, including the new one.

- [ ] **Step 7: Document `--control-port`**

In `README.md`, in the `--run` code block under "Install", add a line showing the flag:

```
`--run` skips the screen and runs one scenario from a project, exiting once it
completes, for a service started at boot rather than a person at a keyboard:

```sh
protocol-simulator-tui project.toml --run "Heartbeat 10 Hz"
```

`--control-port PORT`, alongside `--run`, opens a local TCP socket an external
script can use to push a new field value into the running scenario and read
back the last frame received on a connection, without restarting it:

```sh
protocol-simulator-tui project.toml --run "Heartbeat 10 Hz" --control-port 7878
```
```

In `docs/scenarios.md`, under the existing "From the command line" section, add after the paragraph about the exit code:

```markdown
`--control-port PORT`, given alongside `--run`, opens a local TCP socket for
an external script: one JSON request per line in, one JSON response per line
out.

```
{"cmd":"set","step":2,"field":"speed","value":42}
-> {"ok":true}  |  {"ok":false,"error":"..."}

{"cmd":"last_received","on":"drive","as":"Telemetry"}
-> {"ok":true,"bytes":"AA 55 ...","fields":{"speed":42,"mode":1}}
-> {"ok":false,"error":"nothing received yet on drive"}
```

`step` counts from one, as the row above it in this page already does. `set`
only ever takes effect on a field already present in that step's `with`: the
scenario file declares up front which fields are live-editable by putting
them there. `set` reports no error for a step or field the running scenario
does not recognise; it behaves exactly as a mistyped `with` value in the file
already does.
```

- [ ] **Step 8: Run the full sim-tui suite, lints, and the whole gate**

Run: `make ci`
Expected: clean.

- [ ] **Step 9: Commit**

Documentation and code landed together here because the flag is not usable, or reviewable, without both; split further would leave a commit that adds a flag nothing explains, or docs for a flag that does not exist yet.

```bash
git add crates/sim-tui/src/headless.rs crates/sim-tui/src/main.rs README.md docs/scenarios.md
git commit -m "feat(tui): open a control socket for --run under --control-port"
```

---

## Self-review notes

- Spec coverage: `Command::SetStepValue` and the override merge (Task 1), `EngineHandle` cross-thread sender (Task 2), the two-command JSON protocol including "no discovery" and "no engine-side validation" (Task 3), `--control-port` wiring plus docs (Task 4). The spec's "Open questions" (default vs. required port, several concurrent clients) are both resolved by what got built: the flag is required for the socket to exist at all (no implicit default), and `control::spawn` already accepts more than one connection.
- Placeholder scan: none found on the final pass; an earlier draft of Task 3 Step 3 left a stray unused-import aside in the test module, corrected before this version.
- Type consistency checked: `Overrides` (Task 1) is `sim_core`-crate-internal and never named outside it. `command_sender` (Task 2) returns `tokio::sync::mpsc::Sender<Command>`, exactly the type `control::spawn`'s `commands` parameter expects (Task 3), exactly what `engine.command_sender()` produces at the one call site that matters (Task 4). `LastReceived` is defined once, in `control.rs` (Task 3), and only ever consumed by name from `headless.rs` (Task 4). `run`'s new signature is introduced and used consistently across Tasks 4's test, its four updated existing tests, and `main.rs`.
