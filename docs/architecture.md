# Architecture

Two crates. `sim-core` holds the engine, the frame model, the codec and the
file formats, with no GUI dependency. `sim-gui` draws the panels and holds no
protocol logic. The split is what makes a headless front end possible later,
and it is worth refusing a change over.

```
                sim-gui                          sim-core
  panels  ->  AppState  ->  EngineHandle  ==>  Engine thread  ->  transport tasks
     ^                            ^                  |                  |
     |                            +===== Event ======+                  |
     +--------- drained once per frame ------------------------ sockets and ports
```

The two arrows crossing the crate boundary are `mpsc` channels carrying
`Command` one way and `Event` the other. There is no other path.

## The engine

`Engine::spawn` starts a thread named `sim-engine` holding its own multi-thread
tokio runtime and returns the two channel ends. One task owns each connection.
Nothing outside that task touches a socket or a port.

| Command | Means |
| --- | --- |
| `Connect` | open a transport under an id, with an optional retry policy |
| `Disconnect` | close it |
| `SendRaw` | write these bytes to that connection |
| `StartScenario` | run this scenario against the frame definitions it carries |
| `StopScenario` | stop the one running under this name |

| Event | Reports |
| --- | --- |
| `ConnectionStatus` | a link opened, closed, or is retrying |
| `FrameSent` | bytes left, with the moment they did |
| `FrameReceived` | bytes arrived, with their source for a datagram |
| `Error` | a failure, tied to a connection when there is one |
| `ScenarioStep` | a scenario is about to run step `n` on pass `p` |
| `ScenarioFinished` | it ended, with its outcome |

A scenario is a client of the engine like any other. `runner.rs` issues
`SendRaw` back through the command channel rather than reaching into the
connection map, which gives it the same errors a person clicking Send would
get. It holds a `WeakSender`, since a strong one would keep the engine loop
alive after every real caller had gone.

`StartScenario` carries the definitions it encodes against. A scenario keeps
running against the frames it started with however they are edited meanwhile.

## The front end

`main.rs` builds the window and hands `SimApp` an optional path. `app.rs`
implements `eframe::App`, drains the event channel once per frame into
`AppState`, and hands the dock to `egui_dock`.

`AppState` in `state.rs` owns everything the panels draw. Panels take it by
reference and mutate it. They reach the engine only through `EngineHandle`,
which wraps the channel pair and counts what it had to drop.

Traffic is one shared buffer of `MAX_LOG_ENTRIES` rows, currently 10 000, with
the oldest dropped. Each `Tab::LiveMonitor(MonitorId)` is a view over it with
its own filter, its own paused state, and its own selected row.

| Tab | Panel |
| --- | --- |
| `Connections` | `panels/connections.rs` |
| `LiveMonitor(id)` | `panels/live_monitor.rs` and `panels/frame_detail.rs` |
| `HexInject` | `panels/hex_inject.rs` |
| `FrameEditor` | `panels/frame_editor.rs`, `frame_edit.rs`, `type_edit.rs` |
| `Scenarios` | `panels/scenario_list.rs` and `scenario_edit.rs` |

Meaning lives beside the drawing, not inside it. `frames.rs` and `scenarios.rs`
hold what an edit means, the panels hold what it looks like, which is what
makes the meaning testable without a window.

## Files

Every format is mirrored by plain `Raw*` structs and converted into the model.
Serde attributes stay out of the model, and a file that does not make sense is
refused with a message naming the field rather than with a deserialiser error.

| Format | Read and written by | Mirrors |
| --- | --- | --- |
| Frame definition | `frame/schema.rs` | `FrameDef` and `TypeLibrary` |
| Scenario | `scenario.rs` | `Scenario` |
| Connection settings | `config.rs` | `TransportConfig` |
| Project | `sim-gui/src/project.rs` | the parts of `AppState` worth keeping |

Writing back goes through `document.rs`, which copies changes into the existing
TOML key by key. Comments, blank lines and key order survive an edit made in
the GUI.

The project file stores paths relative to itself with forward slashes, so it
survives the trip between machines and between operating systems. What belongs
to one machine, meaning the list of projects opened on it, lives in `prefs.rs`
instead.

## Module map

### sim-core

| File | Holds |
| --- | --- |
| `engine.rs` | the thread, the runtime, the command loop, one task per connection |
| `connection.rs` | `ConnectionId`, `ConnectionStatus`, `RetryPolicy`, `TransportConfig` |
| `transport/` | `udp.rs`, `tcp.rs`, `serial.rs` behind one trait in `mod.rs` |
| `frame/mod.rs` | `FrameDef`, `FieldDef`, `FieldKind`, `ScalarType`, `FieldSpan` |
| `frame/codec.rs` | `encode` and `decode`, `Decoded`, `ChecksumMismatch`, `RangeViolation` |
| `frame/checksum.rs` | `ChecksumSpec` and the CRC parameters |
| `frame/schema.rs` | the frame file format and the shared type library |
| `frame/value.rs` | `Value`, `FieldValues`, `seed_values` |
| `scenario.rs` | the scenario model and its file format |
| `runner.rs` | running one scenario |
| `pattern.rs` | `AA 55 ?? 01`, shared by the traffic filter and scenario matching |
| `document.rs` | editing TOML in place |
| `config.rs` | connection settings as they are written down |
| `error.rs` | `EngineError` and `TransportError` |

### sim-gui

| File | Holds |
| --- | --- |
| `app.rs` | `SimApp`, the frame loop, the dock, the menu |
| `engine_handle.rs` | the only way to the engine |
| `state.rs` | `AppState`, `LogEntry`, `MonitorState`, `TrafficFilter` |
| `frames.rs` | the frame folder, the one being edited, what an edit means |
| `scenarios.rs` | the same for scenarios |
| `layout.rs` | field list operations on a `FrameDef`, spans kept correct |
| `project.rs` | the project file |
| `prefs.rs` | what belongs to this machine |
| `theme.rs` | the theme, applied once to the context |
| `panels/mod.rs` | `Tab`, the dock viewer, the shared formatters |

## Seams

A headless front end attaches at `EngineHandle`. It is the whole of the
dependency the panels have on the engine, and nothing below it knows a window
exists. What it would need beside the channels is the loading `frames.rs` and
`scenarios.rs` do, which is why the loading sits apart from the drawing.
