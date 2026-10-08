# Live values for a running scenario, and a control socket for --run

## Goal

Turn the simulator from a developer tool into something a production test
station can drive unattended. A scenario with no `times` runs forever against
a device under test. Today the only way to change what it sends is to stop
it, edit the file, and start it again, which breaks the continuous stream a
device under test may depend on and resets pass counters and repeat cadence.

The concrete need: a Python script with a simple technician menu, running
alongside `sim-tui --run` on the same test bench, that can push a new field
value into the running scenario and read back the last frame received on a
connection, without ever stopping the scenario.

## Scope

In:

- One new engine command to override a `send` step's field value while a
  scenario runs, for fields already present in that step's `with`
- A TCP control socket, local only, opened by `sim-tui --run` and nothing
  else
- Reading the last frame received on a connection, decoded against a named
  frame definition

Out, deliberately:

- Interactive live-edit panels in the GUI or the interactive TUI. Same
  underlying mechanism, different consumer, separate brainstorm later.
- Writing a live-edited value back to the scenario's `.toml` file. An
  override is transient: it changes what the engine sends right away, the
  file and the next run keep the original value.
- Overriding anything other than a `send` step's `with` fields: no live
  counters, no live repeat period, no live wait timeouts.
- Overriding a field that was not already in the step's `with` at scenario
  start. The scenario file declares up front which fields are live-editable.
- Validating a `set` request against the scenario before acting on it. A
  step index or field name the running scenario does not recognise behaves
  exactly as a typo in the `.toml` file does today: the field is silently
  unreadable if the step index is never reached by a `send`, or the pass
  fails and the scenario stops if the field does not exist on the frame.
  Decided explicitly: no extra guard rail here, no new failure mode to
  design around.

## sim-core

| Change | Where |
| --- | --- |
| `Command::SetStepValue { scenario: String, step: usize, field: String, value: Value }` | `engine.rs` |
| `ScenarioHandle.overrides: Arc<Mutex<BTreeMap<(usize, String), Value>>>` | `engine.rs` |
| `Context.overrides: Arc<Mutex<BTreeMap<(usize, String), Value>>>`, cloned from the handle at `start_scenario` | `runner.rs` |

`step` is counted from one, the same convention `Event::ScenarioStep` and
every step-numbered error message already use. `execute()`'s own loop index
is zero-based (`enumerate()`), so it is `index + 1` that gets looked up in
`overrides`, and the map itself is keyed by the one-based number throughout,
matching what a person reading the file, an error message, or the wire
protocol below all mean by "step 2".

`handle_command` looks the scenario up in `scenarios` by name and locks its
`overrides` to insert `(step, field) -> value`. Unknown name reports
`EngineError::UnknownScenario`, the same error `StopScenario` already uses.

In `execute()`, for `Action::Send`, the step's static `with` is layered with
whatever the current `overrides` holds for that step's one-based number
before calling `encode()`. Precedence among `with`, `from_capture` and `counters` is
unchanged: an override only ever replaces what a plain `with` entry would
have supplied, so it cannot fight a counter or a capture, which the editor
already keeps mutually exclusive with `with` per field.

No new `Event`. `SetStepValue` is fire-and-forget, symmetric with `SendRaw`.
A value that fails to coerce to the field's kind fails that pass exactly as
a malformed `with` value in the file does today.

Shared state, not a channel, carries the override into the running task.
`StopScenario` already reaches into `ScenarioHandle` directly
(`handle.task.abort()`) rather than sending a message into the task, and an
`Arc<Mutex<_>>` follows that same precedent: one writer (the command loop),
one reader (the scenario task), same process, no round trip needed.

## sim-session

`EngineHandle::set_step_value(&self, scenario: String, step: usize, field: String, value: Value)`
sends `Command::SetStepValue`.

`EngineHandle` itself cannot be shared across threads as it stands: it holds
`event_rx: mpsc::Receiver<Event>` and a `Cell<usize>` drop counter, and
`Cell` is not `Sync`. The control socket's threads need to issue commands
without owning the event side, so `EngineHandle` exposes a way to get a
cloned `mpsc::Sender<Command>` on its own, which is `Send + Sync` and needs
no wrapper.

## sim-tui: the control socket

New module `control.rs`, wired in only when `sim-tui` is started with
`--run`.

CLI: `--control-port <PORT>`. Assumption taken: no implicit default, the
flag is required for the socket to open at all. `--run NAME` with no
`--control-port` behaves exactly as it does today, headless with no socket.
Flag me if a documented default port is preferred instead.

`TcpListener::bind(("127.0.0.1", port))`. One thread per accepted
connection, a connection stays open across several requests. Wire format:
one JSON request per line in, one JSON response per line out.

### Commands

```
{"cmd":"set","step":2,"field":"speed","value":42}
-> {"ok":true}
-> {"ok":false,"error":"<message from the engine, if any>"}

{"cmd":"last_received","on":"drive","as":"Telemetry"}
-> {"ok":true,"bytes":"AA55...","fields":{"speed":42,"mode":1}}
-> {"ok":false,"error":"nothing received yet on drive"}
```

`value` deserialises straight into `sim_core::frame::value::Value`, which is
already `#[serde(untagged)]`: a JSON number or string reads the same way a
TOML one already does when the scenario file is loaded. No new conversion
code.

`last_received` always names the frame to decode as (`as`), never guesses
from byte length: `reading.rs` already refuses to guess this in the GUI,
matched here. Errors: unknown connection, nothing received yet, or the
stored bytes are not `as`'s length.

The scenario name never appears in the protocol. `headless.rs` already
knows it, it is the argument to `--run`, and supplies it to
`Command::SetStepValue` itself. No discovery command either: the Python
script is expected to already know its field and connection names, from
the same `scenario.toml` a person maintains.

### Shared state

`watch()` in `headless.rs` gains a side effect: on every
`Event::FrameReceived`, it writes `bytes` into
`Arc<Mutex<BTreeMap<ConnectionId, Vec<u8>>>>`, keyed by connection, replacing
whatever was there. `last_received` reads from the same map, decoding with
`sim_core::frame::codec::decode` against the frame the request named, looked
up in the already-loaded `Session::frames`.

## Testing

- `sim-core`: a `runner.rs` test that starts a repeating scenario, injects an
  override mid-run through the same path `handle_command` would use, and
  asserts a later pass encodes the new value while an earlier pass already
  captured still encoded the old one.
- `sim-core`: a value that fails to coerce still fails the pass and stops
  the scenario, matching the existing `with` behaviour.
- `sim-tui`: an integration test under `control.rs` or `headless.rs`,
  reusing the existing loopback project fixture in `headless.rs`'s tests,
  that opens a real TCP connection to the control port, sends `set`,
  observes the next `FrameSent` carries the new value, sends
  `last_received` and gets back decoded fields for a frame the loopback
  sent to itself.
- `sim-tui`: `last_received` error paths, unknown connection and nothing
  received yet.

All of it runs under `make test-core` and `make ci` alongside everything
else; no new test tooling.

## Open questions

- Default vs. required `--control-port`, flagged above.
- Whether one Python client at a time is enough in practice, or several
  processes might dial in concurrently. The design as written already
  allows several connections, so this needs no decision now, only
  confirmation it is not a problem to leave open.

## Follow-up, not in this spec

Interactive live-edit panels for the GUI's Scenarios tab and the TUI's own
interactive screen, built on the same `Command::SetStepValue`. Separate
brainstorm once this lands, since the UI shape depends on what using the
socket in practice turns up.
