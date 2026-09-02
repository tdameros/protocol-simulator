# Testing

`make ci` is the gate. It runs formatting, clippy with `-D warnings`, a release
type-check, and every test, which is word for word what
`.github/workflows/ci.yml` applies.

## Layers

| Suite | Where | Covers |
| --- | --- | --- |
| Engine units | `sim-core/src/**` in `mod tests` | the codec, the schema, patterns, scenario loading |
| Examples | `sim-core/tests/examples.rs` | every file under `examples/frames` loads, encodes, decodes, and is the length it claims |
| Loopback | `sim-core/tests/loopback.rs` | the engine against real UDP and TCP sockets, retry and shutdown included |
| Front end units | `sim-gui/src/**` in `mod tests` | what an edit means, the project file, the traffic buffer |
| Panels | `sim-gui/src/panels/panels_tests.rs` | what a person sees and clicks |
| Screenshots | `sim-gui/src/panels/shots.rs` | the documentation images, redrawn from the real panels |

The examples folder is walked rather than listed, so a new example is covered
the moment it is added.

```sh
make test                                     # everything
make test-core                                # the engine only
cargo test -p sim-gui                          # the front end only
cargo test -p sim-gui --features shots shots   # redraw docs/images
```

Screenshots are rendered, not captured, so a picture in the documentation is a
picture of the code as it stands. The feature is off by default because it
pulls in a GPU backend the ordinary run has no use for. Clippy does not see it
in CI, so lint that crate with `--features shots` after touching `shots.rs`.

## Panel tests

`egui_kittest` runs a panel against a real egui context and an accessibility
tree, so a test asks what a person would ask. Is the button there, and does
pressing it do the thing.

```rust
let mut harness = Harness::new_ui_state(|ui, world| super::panel::show(ui, world), world);
harness.run();

assert!(harness.query_by_label_contains("New").is_some());
harness.get_by_label_contains("New").click();
harness.run();
```

`query_` answers with an option, `get_` panics when the node is not there.
`Node::click` presses and releases a real pointer at the centre of the node's
rectangle, so it exercises hit testing rather than calling the handler, and it
needs a `run` after it before the result can be read.

Three defects reached a user before this suite existed. Every one of them was
invisible below the drawing code.

## Proving a test works

A test that has never failed has not been shown to test anything. Plant the
fault it is meant to catch, watch it fail, remove the fault, watch it pass.
Every panel test here was written that way. A planted fault that goes through
untouched means the test is the thing to fix, not the code.

## The release profile compiles different code

egui puts its debug painting behind `debug_assertions`. `egui::Style::debug`
does not exist in a release build, so code reading it compiles in `cargo test`
and fails in `cargo build --release`. `make check-release` exists because the
release workflow was the only thing that used to find out, one tag too late.
Gate anything touching that field with `#[cfg(debug_assertions)]`, tests
included.

## egui behaviour worth knowing

Each of these cost a debugging session.

| Behaviour | What it does | What to do |
| --- | --- | --- |
| Selectable labels sense `click_and_drag` | every text cell wins the click meant for the row behind it | `ui.style_mut().interaction.selectable_labels = false` inside the list |
| `Sense::click()` is `CLICK` plus `FOCUSABLE` | a clickable row takes the keyboard focus | `Sense::CLICK` when only clicks are wanted |
| `warn_if_rect_changes_id` defaults on in debug | a red rectangle on every widget of a virtualised list whose viewport resizes | turned off in `theme::apply`. `warn_on_id_clash` stays on, it catches real duplicates |
| `ScrollArea::auto_shrink` defaults to true | the scrollbar is pulled in to the content width | `.auto_shrink([false; 2])` |
| A scroll area with no `id_salt` | loses its position when a sibling appears and shifts the generated id | name it, as `traffic_rows` is named |
| `Panel::default_size` | is honoured on the first pass only | in the shot harness that pass runs before fonts load, so the stored `PanelState` is dropped each pass |
| `ScrollArea::show_rows` | needs a uniform row height and draws the visible slice only | a test must scroll to a row before querying it |

The red rectangles are the one to recognise on sight. They carry no text, which
rules out every other painter egui has, and they appear the moment a list
resizes rather than on any particular click.

Names that moved in egui 0.35, since the older ones are what search engines
still return.

| Was | Is |
| --- | --- |
| `TopBottomPanel::bottom(id)` | `egui::Panel::bottom(egui::Id)` |
| `show_inside(ui, ...)` | `show(ui, ...)` |
| `default_height(h)` | `default_size(h)` |
| `Rounding::same(f32)` | `CornerRadius::same(u8)` |
