//! What the panels actually put on screen, driven without a window.
//!
//! Everything else in this crate is tested below the drawing code, which left
//! a gap the size of every defect a user meets first: a button that is not
//! there, a box that will not take what is typed into it, a row that loses its
//! place. Three of those reached a user before this existed.
//!
//! `egui_kittest` runs a panel against a real egui context and an accessibility
//! tree, so a test can ask what a person would ask: is the button there, and
//! can I press it.

use egui_kittest::kittest::{By, NodeT, Queryable};
use egui_kittest::Harness;

use crate::engine_handle::EngineHandle;
use crate::state::AppState;

/// What a panel needs around it, held together so the harness can own it.
struct World {
    state: AppState,
    engine: EngineHandle,
}

impl World {
    fn new() -> Self {
        Self {
            state: AppState::default(),
            engine: EngineHandle::default(),
        }
    }
}

/// A frames folder holding exactly the files given, and a library pointed at it.
fn folder(tag: &str, files: &[(&str, &str)]) -> (std::path::PathBuf, World) {
    let dir = std::env::temp_dir().join(format!("sim-panel-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let mut world = World::new();
    world.state.frames.load_from(dir.clone());
    (dir, world)
}

/// A text box holding this, told apart from the run of text egui puts inside
/// it, which carries the same value.
fn name_box(value: &str) -> By<'_> {
    By::new()
        .value(value)
        .predicate(|node| format!("{:?}", node.role()) == "TextInput")
}

const PAIR: &str = r#"
name = "Pair"
description = "Two bytes, 2 bytes"

[[field]]
name = "first"
type = "u8"

[[field]]
name = "second"
type = "u8"
"#;

const FRAME: &str = r#"
name = "Telemetry"
description = "One byte, 1 bytes"

[[field]]
name = "mode"
type = "u8"
"#;

fn frames_panel(world: World) -> Harness<'static, World> {
    Harness::new_ui_state(
        |ui, world| {
            let engine = EngineHandle::default();
            let _ = &world.engine;
            super::frame_editor::show(ui, &mut world.state, &engine);
        },
        world,
    )
}

/// Deleting the last frame used to leave a folder with no way out: New lived on
/// a row that was only drawn when the list had something in it.
#[test]
fn an_emptied_folder_still_offers_a_way_to_make_a_frame() {
    let (_, world) = folder("emptied", &[("telemetry.toml", FRAME)]);
    let mut harness = frames_panel(world);
    harness.run();
    assert!(harness.query_by_label_contains("New").is_some());

    harness.state_mut().state.frames.delete_selected().unwrap();
    harness.run();

    assert!(harness.state().state.frames.is_empty());
    let new = harness
        .query_by_label_contains("New")
        .expect("New is still offered with nothing left in the folder");
    assert!(
        !new.accesskit_node().is_disabled(),
        "and it can still be pressed"
    );
}

/// A frame with no folder at all has nowhere to put one, so the panel says so
/// rather than offering a button that cannot work.
#[test]
fn a_panel_with_no_folder_asks_for_one() {
    let mut harness = frames_panel(World::new());
    harness.run();

    assert!(harness.query_by_label_contains("Pick the folder").is_some());
    assert!(harness.query_by_label_contains("New").is_none());
}

/// Typing reaches the model a letter at a time, and the box stays the one being
/// typed into.
///
/// It does not cover the row identity itself: the id used to carry the field
/// name, so every keystroke built a different row, and nothing here tells the
/// two apart. What that defect produced in the end was a duplicate name and the
/// clash below.
#[test]
fn a_field_name_can_be_typed_into() {
    let (_, world) = folder("typing", &[("telemetry.toml", FRAME)]);
    let mut harness = frames_panel(world);
    harness.state_mut().state.frames.begin_edit();
    harness.run();

    harness.get(name_box("mode")).focus();
    harness.run();
    harness.get(name_box("mode")).type_text("x");
    harness.run();

    assert_eq!(
        harness
            .state()
            .state
            .frames
            .draft
            .as_ref()
            .unwrap()
            .frame
            .declared,
        ["modex"],
        "what was typed reached the model"
    );
    assert!(
        harness.get(name_box("modex")).is_focused(),
        "and the box kept the focus, so the next letter lands in it too"
    );
}

/// Clearing the box is how a name gets replaced, so it has to be allowed, with
/// the save refused for as long as it stands empty.
#[test]
fn clearing_a_field_name_is_allowed_and_blocks_the_save() {
    let (_, world) = folder("clearing", &[("telemetry.toml", FRAME)]);
    let mut harness = frames_panel(world);
    harness.state_mut().state.frames.begin_edit();
    harness.run();

    harness.get(name_box("mode")).focus();
    for _ in 0.."mode".len() {
        harness.key_press(egui::Key::Backspace);
        harness.run();
    }

    assert_eq!(
        harness
            .state()
            .state
            .frames
            .draft
            .as_ref()
            .unwrap()
            .frame
            .declared,
        [""],
        "the box empties rather than keeping a last letter"
    );
    let problem = harness.state().state.frames.draft_problem();
    assert!(
        problem.is_some_and(|said| said.contains("needs a name")),
        "and the save says why it will not go"
    );
}

/// Two fields of one name gave two rows of one id, which egui paints over with
/// a red square, and a frame the loader refuses.
#[test]
fn a_field_cannot_be_typed_onto_the_name_of_another() {
    let (_, world) = folder("clash", &[("pair.toml", PAIR)]);
    let mut harness = frames_panel(world);
    harness.state_mut().state.frames.begin_edit();
    harness.run();

    harness.get(name_box("first")).focus();
    harness.run();
    for _ in 0.."first".len() {
        harness.key_press(egui::Key::Backspace);
        harness.run();
    }
    harness.get(name_box("")).type_text("second");
    harness.run();

    assert_eq!(
        harness
            .state()
            .state
            .frames
            .draft
            .as_ref()
            .unwrap()
            .frame
            .declared,
        ["", "second"],
        "the name another field answers to is refused"
    );
    assert_eq!(
        harness.query_all_by_value("second").count(),
        2,
        "one box and the text inside it, not two fields"
    );
}
