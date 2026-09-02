# Architecture

Three crates. `sim-core` holds the engine, the frame model, the codec and the
file formats, with no GUI dependency. `sim-session` holds what a front end
needs that is not drawing. `sim-gui` draws the panels and holds no protocol
logic. The split is what lets a second front end exist, and it is worth
refusing a change over.

```
    sim-gui                sim-session                     sim-core
  panels  ->  Session  ->  EngineHandle  ==>  Engine thread  ->  transport tasks
     ^                          ^                   |                  |
     |                          +===== Event =======+                  |
     +------- drained once per frame ------------------------- sockets and ports
```

The two arrows crossing into `sim-core` are `mpsc` channels carrying `Command`
one way and `Event` the other. There is no other path.

`sim-session` and `sim-core` differ in error strategy as much as in subject.
`sim-core` is a library with typed errors a caller can branch on. `sim-session`
is application state, where a failure ends up in front of a person, so `anyhow`
carries the context instead.

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
implements `eframe::App`, drains the event channel once per frame into the
`Session`, and hands the dock to `egui_dock`.

`Session` in `sim-session` owns everything the panels draw. Panels take it by
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

Meaning lives beside the drawing, not inside it. `sim-session` holds what an
edit means, the panels hold what it looks like, which is what makes the meaning
testable without a window.

## Files

Every format is mirrored by plain `Raw*` structs and converted into the model.
Serde attributes stay out of the model, and a file that does not make sense is
refused with a message naming the field rather than with a deserialiser error.

| Format | Read and written by | Mirrors |
| --- | --- | --- |
| Frame definition | `frame/schema.rs` | `FrameDef` and `TypeLibrary` |
| Scenario | `scenario.rs` | `Scenario` |
| Connection settings | `config.rs` | `TransportConfig` |
| Project | `sim-gui/src/project.rs` | the parts of `Session` worth keeping |

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

### sim-session

| File | Holds |
| --- | --- |
| `engine_handle.rs` | the only way to the engine |
| `state.rs` | `Session`, `LogEntry`, `MonitorState`, `TrafficFilter` |
| `frames.rs` | the frame folder, the one being edited, what an edit means |
| `scenarios.rs` | the same for scenarios |
| `layout.rs` | field list operations on a `FrameDef`, spans kept correct |

### sim-gui

| File | Holds |
| --- | --- |
| `app.rs` | `SimApp`, the frame loop, the dock, the menu |
| `project.rs` | the project file |
| `prefs.rs` | what belongs to this machine |
| `theme.rs` | the theme, applied once to the context |
| `panels/mod.rs` | `Tab`, the dock viewer, the shared formatters |

## Seams

A second front end depends on `sim-session` and stops there. Nothing in it
knows a window exists, and the panels are the only thing that does.

One piece is still on the wrong side. `project.rs` lives in `sim-gui` because
its `[ui]` section holds the dock arrangement, which only `egui_dock` can read.
A front end that opens the same project needs the rest of the file, so that
section has to travel through as an opaque value rather than be understood,
or a project saved from a terminal would come back with its window layout
gone.
