//! What the frame library does, checked without a window.
//!
//! Split out because the checks outgrew the code they cover: keeping both in
//! one file made the part that ships hard to find.

use super::*;
use crate::layout;
use sim_core::frame::schema::Subtype;
use sim_core::frame::value::Value;
use sim_core::frame::{Endianness, FieldDef, FieldKind, FieldSpan, ScalarType, Stated, ValueRange};

const GOOD: &str = r#"
name = "Telemetry"
endian = "big"

[[field]]
name = "sync"
type = "u16"
default = 0xAA55

[[field]]
name = "mode"
type = "enum"
repr = "u8"
variants = { IDLE = 0, RUN = 1 }

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "sync", to = "mode" }
"#;

/// A second frame, for the cases that need two files in a folder.
const OTHER: &str = r#"
name = "Beacon"

[[field]]
name = "tick"
type = "u8"
"#;

const BROKEN: &str = r#"
name = "Broken"
[[field]]
name = "flags"
type = "bits"
repr = "u8"
bits = [{ name = "only", width = 3 }]
"#;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sim-lib-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn loads_valid_frames_and_reports_broken_ones_separately() {
    let dir = scratch("mixed");
    std::fs::write(dir.join("telemetry.toml"), GOOD).unwrap();
    std::fs::write(dir.join("broken.toml"), BROKEN).unwrap();
    std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());

    // One bad file must not cost you the others.
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].frame.name, "Telemetry");
    assert_eq!(library.failures.len(), 1);
    assert_eq!(library.failures[0].0, "broken.toml");
    assert!(library.failures[0].1.contains("bit widths"));
    assert_eq!(library.selected, Some(0));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn values_start_from_defaults_and_survive_switching_frames() {
    let dir = scratch("values");
    std::fs::write(dir.join("telemetry.toml"), GOOD).unwrap();

    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());
    let frame = library.selected_frame().unwrap().clone();

    assert_eq!(
        library.values_mut(&frame).get("sync"),
        Some(&Value::Uint(0xAA55))
    );
    // A checksum is computed, never seeded.
    assert!(!library.values_mut(&frame).contains_key("crc"));

    library
        .values_mut(&frame)
        .insert("sync".to_owned(), Value::Uint(1));
    assert_eq!(
        library.values_mut(&frame).get("sync"),
        Some(&Value::Uint(1))
    );

    library.reset_values(&frame);
    assert_eq!(
        library.values_mut(&frame).get("sync"),
        Some(&Value::Uint(0xAA55))
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shared_types_are_read_from_the_types_subfolder() {
    let dir = scratch("types");
    std::fs::create_dir_all(dir.join(TYPES_DIR)).unwrap();
    std::fs::write(
        dir.join(TYPES_DIR).join("led.toml"),
        r#"
[[type]]
name = "LedConfig"
[[type.field]]
name = "mode"
type = "u8"
[[type.field]]
name = "period_ms"
type = "u16"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("bank.toml"),
        r#"
name = "Bank"
[[field]]
name = "led"
type = "LedConfig"
repeat = 3
"#,
    )
    .unwrap();

    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());

    assert!(library.failures.is_empty(), "{:?}", library.failures);
    assert_eq!(
        library
            .type_entries
            .iter()
            .map(|entry| entry.definition.name())
            .collect::<Vec<_>>(),
        ["LedConfig"]
    );
    // The subfolder itself must not be mistaken for a frame file.
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].frame.fields.len(), 6);
    assert_eq!(library.entries[0].frame.size(), 9);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_constrained_field_starts_inside_its_subtype() {
    let dir = scratch("subtype");
    std::fs::write(
        dir.join("clamped.toml"),
        r#"
name = "Clamped"
[[field]]
name = "duty"
type = "u8"
range = { min = 10, max = 20 }
[[field]]
name = "trim"
type = "i8"
range = { min = -50, max = -10 }
"#,
    )
    .unwrap();

    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());
    let frame = library.selected_frame().unwrap().clone();

    // Zero is outside both, so neither may start there.
    assert_eq!(
        library.values_mut(&frame).get("duty"),
        Some(&Value::Uint(10))
    );
    assert_eq!(
        library.values_mut(&frame).get("trim"),
        Some(&Value::Int(-10))
    );
    // Which is the whole point: an untouched frame has to encode.
    assert!(sim_core::frame::codec::encode(&frame, library.values_mut(&frame)).is_ok());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seeded_values_always_encode() {
    let dir = scratch("encode");
    std::fs::write(dir.join("telemetry.toml"), GOOD).unwrap();

    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());
    let frame = library.selected_frame().unwrap().clone();

    // The preview has to render the moment a frame is opened, with nothing typed.
    let encoded = sim_core::frame::codec::encode(&frame, library.values_mut(&frame));
    assert!(encoded.is_ok(), "{:?}", encoded.err());
    assert_eq!(encoded.unwrap().len(), frame.size());

    std::fs::remove_dir_all(&dir).ok();
}

/// A frame stating a field through a shared subtype, which the writer is
/// deliberately unable to unpick.
const SUBTYPED: &str = r#"
name = "Setpoints"

[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 100 }

# Kept in percent on purpose.
[[field]]
name = "target"
type = "Percent"
default = 50
"#;

fn library_of(tag: &str, files: &[(&str, &str)]) -> (PathBuf, FrameLibrary) {
    let dir = scratch(tag);
    // Counting the files in the folder is how several of these tests check
    // that nothing was written behind their back, so a leftover from the
    // last run would be read as this run's fault.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());
    (dir, library)
}

#[test]
fn a_frame_remembers_the_file_it_came_from() {
    let (dir, library) = library_of("origin", &[("telemetry.toml", GOOD)]);
    assert_eq!(library.entries[0].file, dir.join("telemetry.toml"));
}

#[test]
fn a_draft_nobody_touched_is_not_dirty() {
    let (_, mut library) = library_of("clean", &[("telemetry.toml", GOOD)]);
    library.begin_edit();

    assert!(!library.draft_is_dirty());
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn changing_a_default_is_dirty_and_saves_back_into_the_same_file() {
    let (dir, mut library) = library_of("edit", &[("telemetry.toml", GOOD)]);
    library.begin_edit();
    let draft = library.draft.as_mut().unwrap();
    let mode = draft.frame.field_index("mode").unwrap();
    draft.frame.fields[mode].default = Some(Value::Uint(1));

    assert!(library.draft_is_dirty());
    library.save_draft(&dir.join("unused.toml")).unwrap();

    assert!(!dir.join("unused.toml").exists());
    assert_eq!(library.entries.len(), 1);
    assert!(!library.draft_is_dirty());

    let mut reloaded = FrameLibrary::default();
    reloaded.load_from(dir);
    let mode = reloaded.entries[0].frame.field_index("mode").unwrap();
    assert_eq!(
        reloaded.entries[0].frame.fields[mode].default,
        Some(Value::Uint(1))
    );
}

#[test]
fn editing_twice_in_a_row_edits_the_same_file_rather_than_reverting() {
    let (dir, mut library) = library_of("twice", &[("telemetry.toml", GOOD)]);

    // Saving closes the editor, so a second edit reopens it, which is what
    // keeps the second save from writing over the first from a stale copy.
    for value in [1u64, 0] {
        library.begin_edit();
        let draft = library.draft.as_mut().unwrap();
        let mode = draft.frame.field_index("mode").unwrap();
        draft.frame.fields[mode].default = Some(Value::Uint(value));
        library.save_draft(&dir).unwrap();
        assert!(library.draft.is_none(), "saving leaves the editor");
    }

    let text = std::fs::read_to_string(dir.join("telemetry.toml")).unwrap();
    assert_eq!(text.matches("default = 0\n").count(), 1);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn an_edit_the_file_cannot_state_is_refused_rather_than_written_wrong() {
    let (dir, mut library) = library_of("draft-subtype", &[("setpoints.toml", SUBTYPED)]);
    library.begin_edit();
    let draft = library.draft.as_mut().unwrap();
    let target = draft.frame.field_index("target").unwrap();
    // Widening past what `Percent` allows. The file says `Percent`, and the
    // writer will not replace that with the bounds it stands for.
    draft.frame.fields[target].range = Some(ValueRange::Uint { min: 0, max: 200 });

    let problem = library.draft_problem().expect("refused");
    assert!(problem.contains("target"), "{problem}");
    assert!(library.save_draft(&dir).is_err());

    let text = std::fs::read_to_string(dir.join("setpoints.toml")).unwrap();
    assert!(text.contains(r#"type = "Percent""#));
    assert!(text.contains("# Kept in percent on purpose."));
}

#[test]
fn a_new_frame_lands_in_a_file_named_after_it() {
    let (dir, mut library) = library_of("new", &[("telemetry.toml", GOOD)]);
    library.begin_new(FrameDef::flat(
        "Heartbeat",
        vec![FieldDef {
            name: "tick".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: Endianness::default(),
            default: None,
            range: None,
        }],
    ));

    let into = suggested_file(&dir, "Heartbeat");
    library.save_draft(&into).unwrap();

    assert!(into.ends_with("heartbeat.toml"));
    assert_eq!(library.entries.len(), 2);
    let mut reloaded = FrameLibrary::default();
    reloaded.load_from(dir);
    assert_eq!(
        reloaded
            .frames()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["Heartbeat", "Telemetry"]
    );
}

#[test]
fn a_name_another_file_already_answers_to_is_refused() {
    let (dir, mut library) = library_of("clash", &[("telemetry.toml", GOOD)]);
    library.begin_new(FrameDef::flat(
        "Telemetry",
        vec![FieldDef {
            name: "tick".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: Endianness::default(),
            default: None,
            range: None,
        }],
    ));

    let problem = library.draft_problem().expect("refused");
    assert!(problem.contains("telemetry.toml"), "{problem}");
    assert!(library
        .save_draft(&suggested_file(&dir, "Telemetry"))
        .is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn renaming_a_frame_takes_its_file_with_it() {
    let (dir, mut library) = library_of("rename", &[("telemetry.toml", GOOD)]);
    library.begin_edit();
    library.draft.as_mut().unwrap().frame.name = "Beacon".to_owned();

    assert_eq!(library.draft_problem(), None);
    library.save_draft(&suggested_file(&dir, "Beacon")).unwrap();

    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].file, dir.join("beacon.toml"));
    assert_eq!(library.entries[0].frame.name, "Beacon");
    assert!(!dir.join("telemetry.toml").exists(), "the old one is gone");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn a_rename_onto_a_file_another_frame_holds_is_refused() {
    let (dir, mut library) = library_of(
        "rename-clash",
        &[("telemetry.toml", GOOD), ("beacon.toml", OTHER)],
    );
    library.selected = library
        .entries
        .iter()
        .position(|entry| entry.frame.name == "Telemetry");
    library.begin_edit();
    library.draft.as_mut().unwrap().frame.name = "beacon".to_owned();

    let problem = library.draft_problem().expect("refused");
    assert!(problem.contains("beacon.toml"), "{problem}");
    assert!(library.save_draft(&suggested_file(&dir, "beacon")).is_err());
    assert!(dir.join("telemetry.toml").exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn renaming_a_type_alone_in_its_file_takes_the_file_with_it() {
    let (dir, mut library) = shared("type-rename-file");
    std::fs::write(
        dir.join(TYPES_DIR).join("solo.toml"),
        "[[type]]\nname = \"Solo\"\nbase = \"u8\"\n",
    )
    .unwrap();
    library.reload();
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Solo");
    library.begin_type_edit();
    library.type_draft.as_mut().unwrap().definition.layout.name = "Duty".to_owned();

    library.save_type_draft().unwrap();

    assert!(dir.join(TYPES_DIR).join("duty.toml").exists());
    assert!(!dir.join(TYPES_DIR).join("solo.toml").exists());
}

#[test]
fn renaming_a_type_sharing_a_file_leaves_the_file_alone() {
    let (dir, mut library) = shared("type-rename-shared-file");
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");
    library.begin_type_edit();
    library.type_draft.as_mut().unwrap().definition.layout.name = "Colour".to_owned();

    library.save_type_draft().unwrap();

    // `shared.toml` is named after none of the three it holds.
    assert!(dir.join(TYPES_DIR).join("shared.toml").exists());
    assert!(!dir.join(TYPES_DIR).join("colour.toml").exists());
}

#[test]
fn deleting_a_frame_takes_its_file_with_it() {
    let (dir, mut library) = library_of("delete", &[("telemetry.toml", GOOD)]);
    library.delete_selected().unwrap();

    assert!(library.entries.is_empty());
    assert_eq!(library.selected, None);
    assert!(!dir.join("telemetry.toml").exists());
}

/// A frame with a checksum at the end and a type instance in the middle,
/// which is where every structural edit can go wrong at once.
const LAYERED: &str = r#"
name = "Layered"

[[type]]
name = "Point"
[[type.field]]
name = "x"
type = "u8"
[[type.field]]
name = "y"
type = "u8"

[[field]]
name = "header"
type = "u8"

[[field]]
name = "here"
type = "Point"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "header", to = "here" }
"#;

fn draft_of(text: &str) -> Draft {
    Draft {
        frame: schema::from_toml(text).expect("valid frame"),
        origin: None,
    }
}

fn plain(name: &str) -> FieldDef {
    FieldDef {
        name: name.to_owned(),
        description: None,
        kind: FieldKind::Scalar(ScalarType::U8),
        endian: Endianness::default(),
        default: None,
        range: None,
    }
}

fn covered(draft: &Draft) -> (String, String) {
    let field = draft.frame.field("crc").expect("checksum");
    let FieldKind::Checksum { covers, .. } = &field.kind else {
        panic!("not a checksum");
    };
    (
        draft.frame.fields[covers.from].name.clone(),
        draft.frame.fields[covers.to].name.clone(),
    )
}

#[test]
fn a_type_instance_stands_for_every_field_it_expanded_into() {
    let draft = draft_of(LAYERED);
    assert_eq!(draft.frame.declared, ["header", "here", "crc"]);
    assert_eq!(draft.frame.expansion_of("here"), 1..3);
    assert_eq!(draft.frame.expansion_of("header"), 0..1);
    assert!(layout::is_expanded(&draft.frame, "here"));
    assert!(!layout::is_expanded(&draft.frame, "header"));
}

#[test]
fn inserting_a_field_does_not_shift_what_a_checksum_covers() {
    let mut draft = draft_of(LAYERED);
    assert_eq!(covered(&draft), ("header".to_owned(), "here.y".to_owned()));

    layout::add_field(&mut draft.frame, Some(0), plain("inserted"));

    assert_eq!(draft.frame.declared, ["header", "inserted", "here", "crc"]);
    assert_eq!(covered(&draft), ("header".to_owned(), "here.y".to_owned()));
}

#[test]
fn moving_a_type_instance_moves_all_of_it_at_once() {
    let mut draft = draft_of(LAYERED);
    layout::move_field(&mut draft.frame, 1, false);

    assert_eq!(draft.frame.declared, ["here", "header", "crc"]);
    assert_eq!(
        draft
            .frame
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["here.x", "here.y", "header", "crc"]
    );
    // The same three fields as before, which are now in another order.
    assert_eq!(covered(&draft), ("here.x".to_owned(), "header".to_owned()));
}

#[test]
fn renaming_a_plain_field_leaves_what_a_checksum_covers_alone() {
    let mut draft = draft_of(LAYERED);
    layout::rename_field(&mut draft.frame, 0, "start");

    assert_eq!(draft.frame.declared, ["start", "here", "crc"]);
    assert_eq!(covered(&draft), ("start".to_owned(), "here.y".to_owned()));
}

#[test]
fn renaming_a_type_instance_carries_its_expansion_and_is_written_back() {
    let mut draft = Draft {
        origin: Some(Origin {
            file: PathBuf::from("layered.toml"),
            text: LAYERED.to_owned(),
        }),
        ..draft_of(LAYERED)
    };
    layout::rename_field(&mut draft.frame, 1, "corner");

    assert_eq!(draft.frame.declared, ["header", "corner", "crc"]);
    assert_eq!(
        draft
            .frame
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["header", "corner.x", "corner.y", "crc"]
    );
    // The checksum still covers the same three fields under their new names.
    assert_eq!(
        covered(&draft),
        ("header".to_owned(), "corner.y".to_owned())
    );

    assert_eq!(draft.problem(&TypeLibrary::default()), None);
    let written = draft.written().expect("written");
    assert!(written.contains(r#"name = "corner""#), "{written}");
    assert!(written.contains(r#"type = "Point""#), "{written}");
}

#[test]
fn removing_a_type_instance_removes_all_of_it() {
    let mut draft = draft_of(LAYERED);
    layout::remove_field(&mut draft.frame, 1);

    assert_eq!(draft.frame.declared, ["header", "crc"]);
    assert_eq!(
        draft
            .frame
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["header", "crc"]
    );
}

#[test]
fn a_checksum_losing_the_end_of_its_range_falls_back_rather_than_dangling() {
    let mut draft = draft_of(LAYERED);
    layout::remove_field(&mut draft.frame, 1);

    // Was covering up to `here.y`, which is gone. Anything is better than
    // an index past the end, which the encoder would follow.
    let (from, to) = covered(&draft);
    assert_eq!((from.as_str(), to.as_str()), ("header", "header"));
    assert!(draft.problem(&TypeLibrary::default()).is_none());
}

#[test]
fn a_new_field_does_not_take_a_name_already_in_use() {
    let mut draft = draft_of(LAYERED);
    layout::add_field(&mut draft.frame, None, plain("header"));
    layout::add_field(&mut draft.frame, None, plain("header"));

    assert_eq!(
        draft.frame.declared,
        ["header", "here", "crc", "header2", "header3"]
    );
}

#[test]
fn coverage_is_set_by_naming_both_ends_either_way_round() {
    let mut draft = draft_of(LAYERED);
    let crc = draft.frame.field_index("crc").expect("checksum");
    layout::set_coverage(&mut draft.frame, crc, "here.y", "header");

    assert_eq!(covered(&draft), ("header".to_owned(), "here.y".to_owned()));
}

#[test]
fn every_structural_edit_leaves_a_file_that_still_reads_back() {
    let mut draft = Draft {
        origin: Some(Origin {
            file: PathBuf::from("layered.toml"),
            text: LAYERED.to_owned(),
        }),
        ..draft_of(LAYERED)
    };
    layout::add_field(&mut draft.frame, Some(0), plain("inserted"));
    layout::move_field(&mut draft.frame, 1, true);
    layout::rename_field(&mut draft.frame, 0, "start");

    assert_eq!(draft.problem(&TypeLibrary::default()), None);
    let written = draft.written().expect("written");
    assert!(written.contains(r#"type = "Point""#), "{written}");
}

#[test]
fn coverage_is_set_against_the_declared_field_not_the_wire_position() {
    let mut draft = draft_of(LAYERED);
    // `crc` is declared third and sits fourth on the wire, the type in
    // front of it having expanded into two. Told the wrong one, this would
    // set the coverage of `here.y`, which is not a checksum at all.
    let declared = draft
        .frame
        .declared
        .iter()
        .position(|name| name == "crc")
        .expect("declared");
    assert_ne!(
        declared,
        draft.frame.field_index("crc").expect("on the wire")
    );

    layout::set_coverage(&mut draft.frame, declared, "header", "here.x");

    assert_eq!(covered(&draft), ("header".to_owned(), "here.x".to_owned()));
}

#[test]
fn a_frame_built_from_nothing_but_edits_still_encodes() {
    let mut draft = Draft {
        frame: FrameDef::flat("Built", vec![plain("id")]),
        origin: None,
    };
    layout::add_field(&mut draft.frame, Some(0), plain("count"));
    layout::add_field(&mut draft.frame, Some(1), plain("crc"));

    let crc = draft
        .frame
        .declared
        .iter()
        .position(|n| n == "crc")
        .unwrap();
    if let Some(field) = layout::plain_field_mut(&mut draft.frame, crc) {
        field.kind = FieldKind::Checksum {
            spec: sim_core::frame::checksum::ChecksumSpec::Xor8,
            covers: FieldSpan { from: 0, to: 1 },
        };
    }

    assert_eq!(draft.problem(&TypeLibrary::default()), None);
    let written = draft.written().expect("written");
    let reread = schema::from_toml(&written).expect("valid");
    assert_eq!(reread.declared, ["id", "count", "crc"]);
    assert_eq!(reread.size(), 3);
}

const SHARED: &str = r#"# Types everything here shares.

[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 100 }

# Three bytes of colour.
[[type]]
name = "Rgb"

[[type.field]]
name = "red"
type = "u8"

[[type.field]]
name = "green"
type = "u8"

[[type.field]]
name = "blue"
type = "u8"
"#;

const USES_RGB: &str = r#"
name = "Lamp"

[[field]]
name = "colour"
type = "Rgb"
"#;

fn shared(tag: &str) -> (PathBuf, FrameLibrary) {
    let dir = scratch(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(TYPES_DIR)).unwrap();
    std::fs::write(dir.join(TYPES_DIR).join("shared.toml"), SHARED).unwrap();
    std::fs::write(dir.join("lamp.toml"), USES_RGB).unwrap();
    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());
    (dir, library)
}

#[test]
fn a_shared_type_is_listed_with_the_file_holding_it() {
    let (dir, library) = shared("types-listed");
    assert_eq!(
        library
            .type_entries
            .iter()
            .map(|entry| entry.definition.name())
            .collect::<Vec<_>>(),
        ["Percent", "Rgb"]
    );
    assert_eq!(
        library.type_entries[0].file,
        dir.join(TYPES_DIR).join("shared.toml")
    );
}

#[test]
fn widening_a_subtype_touches_no_frame_and_keeps_the_comments() {
    let (dir, mut library) = shared("types-widen");
    library.type_selected = Some(0);
    library.begin_type_edit();
    let draft = library.type_draft.as_mut().unwrap();
    draft.definition.narrows.as_mut().unwrap().range = Some(ValueRange::Uint { min: 0, max: 200 });

    assert_eq!(library.type_draft_problem(), None);
    assert!(library.type_draft_impact().is_empty());
    library.save_type_draft().unwrap();

    let text = std::fs::read_to_string(dir.join(TYPES_DIR).join("shared.toml")).unwrap();
    assert!(text.contains("max = 200"));
    assert!(text.contains("# Types everything here shares."));
    assert!(text.contains("# Three bytes of colour."));
}

#[test]
fn adding_a_field_to_a_type_says_which_frames_it_resizes() {
    let (_, mut library) = shared("types-grow");
    library.type_selected = Some(1);
    library.begin_type_edit();
    let draft = library.type_draft.as_mut().unwrap();
    layout::add_field(
        &mut draft.definition.layout,
        None,
        FieldDef {
            name: "white".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: Endianness::default(),
            default: None,
            range: None,
        },
    );

    assert_eq!(library.type_draft_problem(), None);
    assert_eq!(
        library.type_draft_impact(),
        vec![("Lamp".to_owned(), Effect::Resized { was: 3, now: 4 })]
    );

    library.save_type_draft().unwrap();
    assert_eq!(library.entries[0].frame.size(), 4);
}

#[test]
fn deleting_a_type_writes_it_out_in_the_frames_that_used_it() {
    let (dir, mut library) = shared("types-delete");
    let was = library.entries[0].frame.fields.clone();
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");

    library.delete_selected_type().unwrap();

    // The name is gone; the bytes are not.
    assert!(library.failures.is_empty(), "{:?}", library.failures);
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].frame.fields, was);
    assert_eq!(
        library.entries[0].frame.declared,
        ["colour.red", "colour.green", "colour.blue"]
    );
    assert!(library
        .type_entries
        .iter()
        .all(|entry| entry.definition.name() != "Rgb"));

    let text = std::fs::read_to_string(dir.join("lamp.toml")).unwrap();
    assert!(!text.contains(r#"type = "Rgb""#), "{text}");
    assert!(text.contains(r#"name = "colour.green""#), "{text}");
}

#[test]
fn deleting_a_type_another_type_uses_writes_it_out_there_too() {
    let (dir, mut library) = shared("types-delete-nested");
    std::fs::write(
        dir.join(TYPES_DIR).join("lamp-type.toml"),
        "[[type]]\nname = \"Lamp\"\n\n[[type.field]]\nname = \"level\"\ntype = \"Percent\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("panel.toml"),
        "name = \"Panel\"\n\n[[field]]\nname = \"main\"\ntype = \"Lamp\"\n",
    )
    .unwrap();
    library.reload();
    let was = library
        .entries
        .iter()
        .find(|entry| entry.frame.name == "Panel")
        .expect("loaded")
        .frame
        .fields
        .clone();

    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Percent");
    library.delete_selected_type().unwrap();

    // `Panel` never mentioned `Percent`; `Lamp` did, and a frame using
    // `Lamp` must not notice.
    assert!(library.failures.is_empty(), "{:?}", library.failures);
    let panel = library
        .entries
        .iter()
        .find(|entry| entry.frame.name == "Panel")
        .expect("still loads");
    assert_eq!(panel.frame.fields, was);
    let held = std::fs::read_to_string(dir.join(TYPES_DIR).join("lamp-type.toml")).unwrap();
    assert!(!held.contains(r#"type = "Percent""#), "{held}");
    assert!(held.contains("max = 100"), "{held}");
}

#[test]
fn a_type_nobody_saved_yet_gets_a_file_of_its_own() {
    let (dir, mut library) = shared("types-new");
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat(
                "Pair",
                vec![
                    FieldDef {
                        name: "left".to_owned(),
                        description: None,
                        kind: FieldKind::Scalar(ScalarType::U8),
                        endian: Endianness::default(),
                        default: None,
                        range: None,
                    },
                    FieldDef {
                        name: "right".to_owned(),
                        description: None,
                        kind: FieldKind::Scalar(ScalarType::U8),
                        endian: Endianness::default(),
                        default: None,
                        range: None,
                    },
                ],
            ),
            narrows: None,
        },
        None,
    );

    assert_eq!(library.type_draft_problem(), None);
    library.save_type_draft().unwrap();

    assert_eq!(library.type_entries.len(), 3);
    // Its own file, named after it, as a frame gets one.
    let own = dir.join(TYPES_DIR).join("pair.toml");
    assert!(own.exists(), "{own:?}");
    assert!(std::fs::read_to_string(&own)
        .unwrap()
        .contains(r#"name = "Pair""#));
    // And the file it did not go in is untouched, comments and all.
    let shared = std::fs::read_to_string(dir.join(TYPES_DIR).join("shared.toml")).unwrap();
    assert_eq!(shared, SHARED);
}

#[test]
fn deleting_the_last_type_in_a_file_takes_the_file_with_it() {
    let (dir, mut library) = shared("types-lonely");
    std::fs::write(
        dir.join(TYPES_DIR).join("lonely.toml"),
        "# On its own.\n[[type]]\nname = \"Solo\"\nbase = \"u8\"\n",
    )
    .unwrap();
    library.reload();
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Solo");

    library.delete_selected_type().unwrap();

    assert!(!dir.join(TYPES_DIR).join("lonely.toml").exists());
    // The file holding three keeps its other two.
    assert_eq!(library.type_entries.len(), 2);
}

#[test]
fn a_type_whose_file_is_taken_is_refused_rather_than_writing_over_it() {
    let (dir, mut library) = shared("types-file-clash");
    std::fs::write(
        dir.join(TYPES_DIR).join("solo.toml"),
        "[[type]]\nname = \"Solo\"\nbase = \"u8\"\n",
    )
    .unwrap();
    library.reload();
    // A different name, which the name check would let through, wanting the
    // same file.
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat("solo", Vec::new()),
            narrows: Some(Subtype {
                base: "u8".to_owned(),
                range: None,
            }),
        },
        None,
    );

    let problem = library.type_draft_problem().expect("refused");
    assert!(problem.contains("solo.toml"), "{problem}");
    assert!(library.save_type_draft().is_err());
    assert!(
        std::fs::read_to_string(dir.join(TYPES_DIR).join("solo.toml"))
            .unwrap()
            .contains(r#"name = "Solo""#)
    );
}

#[test]
fn a_type_name_already_taken_is_refused() {
    let (_, mut library) = shared("types-clash");
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat("Rgb", vec![]),
            narrows: None,
        },
        None,
    );

    let problem = library.type_draft_problem().expect("refused");
    assert!(problem.contains("Rgb"), "{problem}");
    assert!(library.save_type_draft().is_err());
}

#[test]
fn a_new_frame_whose_file_is_taken_is_refused_rather_than_writing_over_it() {
    let (dir, mut library) = library_of("file-clash", &[("telemetry.toml", GOOD)]);
    // A different name, since the name check would catch the same one, but
    // one that wants the same file.
    library.begin_new(FrameDef::flat("telemetry", vec![plain("tick")]));

    let problem = library.draft_problem().expect("refused");
    assert!(problem.contains("telemetry.toml"), "{problem}");
    assert!(library
        .save_draft(&suggested_file(&dir, "telemetry"))
        .is_err());

    let mut reloaded = FrameLibrary::default();
    reloaded.load_from(dir);
    assert_eq!(reloaded.entries.len(), 1);
    assert_eq!(reloaded.entries[0].frame.name, "Telemetry");
}

#[test]
fn a_broken_types_file_does_not_turn_the_guard_off() {
    let (dir, mut library) = shared("types-broken");
    std::fs::write(dir.join(TYPES_DIR).join("bad.toml"), "[[type]]\nname =").unwrap();

    library.type_selected = Some(1);
    library.begin_type_edit();
    let draft = library.type_draft.as_mut().unwrap();
    layout::add_field(
        &mut draft.definition.layout,
        None,
        FieldDef {
            name: "white".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: Endianness::default(),
            default: None,
            range: None,
        },
    );

    // Giving up on the library because a neighbour is broken leaves the
    // guard with nothing to check against, which reads as nothing to say.
    assert_eq!(library.type_draft_problem(), None);
    assert_eq!(
        library.type_draft_impact(),
        vec![("Lamp".to_owned(), Effect::Resized { was: 3, now: 4 })]
    );
}

const LITTLE: &str = r#"
name = "Little"
endian = "little"

[[field]]
name = "a"
type = "u16"

[[field]]
name = "b"
type = "u16"
endian = "big"
"#;

#[test]
fn changing_the_frames_order_carries_the_fields_that_were_following_it() {
    let mut draft = draft_of(LITTLE);
    assert_eq!(draft.frame.fields[0].endian, Endianness::Little);
    assert_eq!(draft.frame.fields[1].endian, Endianness::Big);

    layout::set_endian(&mut draft.frame, Endianness::Big);

    // `a` was following the frame and follows it still. `b` had said its
    // own, and keeps saying it.
    assert_eq!(draft.frame.fields[0].endian, Endianness::Big);
    assert_eq!(draft.frame.fields[1].endian, Endianness::Big);
}

#[test]
fn changing_the_frames_order_is_written_and_reads_back() {
    let mut draft = Draft {
        origin: Some(Origin {
            file: PathBuf::from("little.toml"),
            text: LITTLE.to_owned(),
        }),
        ..draft_of(LITTLE)
    };
    layout::set_endian(&mut draft.frame, Endianness::Big);

    assert_eq!(draft.problem(&TypeLibrary::default()), None);
    let written = draft.written().expect("written");
    assert!(written.contains(r#"endian = "big""#));
    assert!(!written.contains(r#"endian = "little""#));
    assert_eq!(schema::from_toml(&written).expect("valid"), draft.frame);
}

#[test]
fn giving_one_field_its_own_order_says_so_and_leaves_the_rest_alone() {
    let mut draft = Draft {
        origin: Some(Origin {
            file: PathBuf::from("little.toml"),
            text: LITTLE.to_owned(),
        }),
        ..draft_of(LITTLE)
    };
    let a = draft.frame.field_index("a").expect("there");
    draft.frame.fields[a].endian = Endianness::Big;

    assert_eq!(draft.problem(&TypeLibrary::default()), None);
    let written = draft.written().expect("written");
    let reread = schema::from_toml(&written).expect("valid");
    assert_eq!(reread.endian, Endianness::Little);
    assert_eq!(reread.fields[a].endian, Endianness::Big);
}

#[test]
fn a_group_type_with_no_fields_is_refused() {
    let (_, mut library) = shared("types-empty");
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat("NewType", Vec::new()),
            narrows: None,
        },
        None,
    );

    // It reads back exactly as written, so only asking what it means
    // catches it: a frame naming it would fail with "has no fields".
    let problem = library.type_draft_problem().expect("refused");
    assert!(problem.contains("NewType"), "{problem}");
    assert!(library.save_type_draft().is_err());
}

#[test]
fn a_subtype_named_field_is_shown_as_what_the_file_states() {
    let (dir, mut library) = library_of("stated", &[("setpoints.toml", SUBTYPED)]);
    let _ = dir;
    library.begin_edit();
    let draft = library.draft.as_ref().expect("editing");

    // What the panel shows in place of a kind picker, and what the writer
    // refuses to reword. Read from the model now that a frame remembers how
    // the file states each of its fields.
    assert_eq!(
        draft
            .frame
            .stated
            .get("target")
            .map(|held| held.kind.as_str()),
        Some("Percent")
    );
}

#[test]
fn a_removal_that_would_leave_a_checksum_first_is_refused() {
    let text = r#"
name = "Guarded"

[[field]]
name = "data"
type = "u8"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "data", to = "data" }
"#;
    let mut draft = draft_of(text);
    assert!(!layout::may_remove(&draft.frame, 0));
    assert!(!layout::remove_field(&mut draft.frame, 0));
    assert_eq!(draft.frame.declared, ["data", "crc"]);

    // Moving it in front is another matter: a checksum before what it
    // covers still loads and still encodes, so nothing stands in the way.
    layout::move_field(&mut draft.frame, 1, false);
    assert_eq!(draft.frame.declared, ["crc", "data"]);
    assert_eq!(draft.problem(&TypeLibrary::default()), None);
}

#[test]
fn the_type_review_is_worked_out_once_per_change() {
    let (dir, mut library) = shared("types-cached");
    library.type_selected = Some(1);
    library.begin_type_edit();
    let first = library.type_draft_problem();

    // Taking the folder away must not change the answer: asking twice about
    // the same draft must not go back to the disk.
    std::fs::remove_dir_all(dir.join(TYPES_DIR)).unwrap();
    assert_eq!(library.type_draft_problem(), first);
    assert!(library.type_draft_impact().is_empty());
}

/// The frame a technician would build: one byte, then a shared type.
#[test]
fn a_field_can_be_stated_as_a_shared_type_and_written_back_factorised() {
    let (dir, mut library) = shared("state-as");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("id"), plain("colour")]));
    let draft = library.draft.as_mut().unwrap();

    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    let expansion =
        schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
    layout::state_as(&mut draft.frame, 1, Some(&stated), expansion);

    // One declaration, three fields on the wire.
    assert_eq!(draft.frame.declared, ["id", "colour"]);
    assert_eq!(
        draft
            .frame
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "colour.red", "colour.green", "colour.blue"]
    );
    assert_eq!(draft.frame.size(), 4);

    assert_eq!(library.draft_problem(), None);
    library.save_draft(&suggested_file(&dir, "Built")).unwrap();

    let written = std::fs::read_to_string(suggested_file(&dir, "Built")).unwrap();
    assert!(written.contains(r#"type = "Rgb""#), "{written}");
    assert!(!written.contains("colour.red"), "{written}");
}

#[test]
fn a_repeated_instance_expands_under_the_names_it_is_given() {
    let (_, mut library) = shared("state-instances");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Zones", vec![plain("zone")]));
    let draft = library.draft.as_mut().unwrap();

    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: Some(vec!["left".to_owned(), "right".to_owned()]),
    };
    let expansion =
        schema::instantiate(&types, "zone", "Rgb", &stated, draft.frame.endian).unwrap();
    layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);

    assert_eq!(draft.frame.declared, ["zone"]);
    assert_eq!(draft.frame.size(), 6);
    assert!(draft.frame.field_index("zone.right.blue").is_some());
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn dropping_the_type_leaves_one_plain_field_again() {
    let (_, mut library) = shared("state-drop");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("colour")]));
    let draft = library.draft.as_mut().unwrap();

    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: Some(2),
        instances: None,
    };
    let expansion =
        schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
    layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);
    assert_eq!(draft.frame.fields.len(), 6);

    layout::state_as(&mut draft.frame, 0, None, vec![plain("colour")]);

    assert_eq!(draft.frame.declared, ["colour"]);
    assert_eq!(draft.frame.fields.len(), 1);
    assert!(draft.frame.stated.is_empty());
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn a_field_set_to_a_type_by_mistake_can_be_set_back() {
    let (_, mut library) = shared("state-undo");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("colour")]));
    let draft = library.draft.as_mut().unwrap();

    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    let expansion =
        schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
    layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);
    assert_eq!(draft.frame.fields.len(), 3);

    // What the kind picker does when a builtin is chosen on it: back to one
    // plain field, under the name the frame still declares.
    layout::state_as(
        &mut draft.frame,
        0,
        None,
        vec![FieldDef {
            name: "colour".to_owned(),
            ..plain("colour")
        }],
    );

    assert_eq!(draft.frame.declared, ["colour"]);
    assert_eq!(draft.frame.fields.len(), 1);
    assert_eq!(draft.frame.fields[0].name, "colour");
    assert!(draft.frame.stated.is_empty());
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn a_field_written_as_a_type_can_still_be_renamed() {
    let (_, mut library) = shared("state-rename");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("field")]));
    let draft = library.draft.as_mut().unwrap();

    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    let expansion =
        schema::instantiate(&types, "field", "Rgb", &stated, draft.frame.endian).unwrap();
    layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);

    // Add field names it "field"; being stuck with `field.red` afterwards
    // is not a frame anybody meant to build.
    layout::rename_field(&mut draft.frame, 0, "tint");

    assert_eq!(draft.frame.declared, ["tint"]);
    assert!(draft.frame.field_index("tint.green").is_some());
    assert_eq!(draft.frame.stated.keys().collect::<Vec<_>>(), ["tint"]);
    assert_eq!(library.draft_problem(), None);
}

/// The one thing a field of a narrowed scalar may still say for itself, and
/// the reason a subtype is not a group of one.
#[test]
fn a_field_of_a_narrowed_scalar_keeps_its_own_default() {
    let dir = scratch("subtype-default");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(TYPES_DIR)).unwrap();
    std::fs::write(
        dir.join(TYPES_DIR).join("percent.toml"),
        "[[type]]\nname = \"Percent\"\nbase = \"u8\"\nrange = { min = 0, max = 100 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lamp.toml"),
        "name = \"Lamp\"\n\n[[field]]\nname = \"brightness\"\ntype = \"Percent\"\ndefault = 80\n",
    )
    .unwrap();
    let mut library = FrameLibrary::default();
    library.load_from(dir.clone());

    // One field under its own name, not `brightness.value`, carrying a
    // default a group could not have given it.
    let frame = &library.entries[0].frame;
    assert_eq!(frame.declared, ["brightness"]);
    assert_eq!(frame.fields[0].default, Some(Value::Uint(80)));
    assert_eq!(
        frame.fields[0].range,
        Some(ValueRange::Uint { min: 0, max: 100 })
    );

    library.begin_edit();
    let draft = library.draft.as_mut().unwrap();
    draft.frame.fields[0].default = Some(Value::Uint(40));

    assert_eq!(library.draft_problem(), None);
    library.save_draft(&dir).unwrap();
    let text = std::fs::read_to_string(dir.join("lamp.toml")).unwrap();
    assert!(text.contains("default = 40"), "{text}");
    assert!(text.contains(r#"type = "Percent""#), "{text}");
}

#[test]
fn a_second_new_frame_does_not_offer_a_name_already_taken() {
    let (dir, mut library) = library_of("new-names", &[]);
    assert_eq!(library.unused_frame_name("New frame"), "New frame");

    // What New does, twice over, with a save in between.
    for expected in ["New frame", "New frame 2", "New frame 3"] {
        let name = library.unused_frame_name("New frame");
        assert_eq!(name, expected);
        library.begin_new(FrameDef::flat(name.clone(), vec![plain("id")]));
        assert_eq!(library.draft_problem(), None, "{name} should be savable");
        library
            .save_draft(&suggested_file(&dir, &name))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }

    assert_eq!(library.entries.len(), 3);
}

#[test]
fn a_name_is_skipped_when_only_its_file_is_taken() {
    let (dir, library) = library_of("new-file-taken", &[("telemetry.toml", GOOD)]);
    let _ = dir;
    // `Telemetry` is the frame's name, so the name check alone would let
    // `telemetry` through, and the file check catches it.
    assert_eq!(library.unused_frame_name("telemetry"), "telemetry 2");
}

#[test]
fn a_second_new_type_does_not_offer_a_name_already_taken() {
    let (_, mut library) = shared("new-type-names");
    assert_eq!(library.unused_type_name("NewType"), "NewType");

    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat(library.unused_type_name("NewType"), vec![plain("id")]),
            narrows: None,
        },
        None,
    );
    library.save_type_draft().unwrap();

    assert_eq!(library.unused_type_name("NewType"), "NewType 2");
}

#[test]
fn a_frame_waiting_under_a_type_follows_the_type_when_it_changes() {
    let (_, mut library) = shared("draft-under-type");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("colour")]));
    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    {
        let draft = library.draft.as_mut().unwrap();
        let expansion =
            schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
        layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);
    }
    assert_eq!(library.draft.as_ref().unwrap().frame.size(), 3);

    // Off to the type, as the Edit button beside the field does.
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");
    library.begin_type_edit();
    let type_draft = library.type_draft.as_mut().unwrap();
    layout::add_field(
        &mut type_draft.definition.layout,
        None,
        FieldDef {
            name: "white".to_owned(),
            ..plain("white")
        },
    );
    library.save_type_draft().unwrap();

    // Back to the frame, which is still there and now four bytes wide.
    let draft = library.draft.as_ref().expect("the frame is still waiting");
    assert_eq!(draft.frame.declared, ["colour"]);
    assert_eq!(draft.frame.size(), 4);
    assert!(draft.frame.field_index("colour.white").is_some());
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn a_type_made_from_a_field_is_given_to_that_field_once_saved() {
    let (_, mut library) = shared("type-for-field");
    library.begin_new(FrameDef::flat(
        "Built",
        vec![plain("header"), plain("payload")],
    ));

    // What "New type..." in the second field's kind picker does.
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat("Stamp", Vec::new()),
            narrows: None,
        },
        Some(1),
    );
    let type_draft = library.type_draft.as_mut().unwrap();
    // Renamed on the way, as anyone would: the field must follow the name
    // it was saved under, not the one it was offered.
    type_draft.definition.layout.name = "Timestamp".to_owned();
    layout::add_field(
        &mut type_draft.definition.layout,
        None,
        FieldDef {
            name: "seconds".to_owned(),
            kind: FieldKind::Scalar(ScalarType::U32),
            ..plain("seconds")
        },
    );
    library.save_type_draft().unwrap();

    let draft = library.draft.as_ref().expect("the frame is still waiting");
    assert_eq!(draft.frame.declared, ["header", "payload"]);
    assert_eq!(
        draft
            .frame
            .stated
            .get("payload")
            .map(|held| held.kind.as_str()),
        Some("Timestamp")
    );
    assert!(draft.frame.field_index("payload.seconds").is_some());
    assert_eq!(draft.frame.size(), 5);
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn a_type_made_on_its_own_is_given_to_nothing() {
    let (_, mut library) = shared("type-for-nobody");
    library.begin_new(FrameDef::flat("Built", vec![plain("header")]));
    library.begin_new_type(
        TypeDef {
            layout: FrameDef::flat("Spare", vec![plain("pad")]),
            narrows: None,
        },
        None,
    );
    library.save_type_draft().unwrap();

    let draft = library.draft.as_ref().expect("still waiting");
    assert!(draft.frame.stated.is_empty());
    assert_eq!(draft.frame.size(), 1);
}

#[test]
fn renaming_a_type_brings_along_the_frames_that_name_it() {
    let (dir, mut library) = shared("type-rename");
    let was = library.entries[0].frame.fields.clone();
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");
    library.begin_type_edit();
    library.type_draft.as_mut().unwrap().definition.layout.name = "Colour".to_owned();

    // Nothing to report: the frames come along, so none of them changes.
    assert_eq!(library.type_draft_problem(), None);
    assert_eq!(library.type_draft_impact(), vec![]);

    library.save_type_draft().unwrap();

    assert!(library.failures.is_empty(), "{:?}", library.failures);
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].frame.fields, was);
    assert_eq!(
        library.entries[0]
            .frame
            .stated
            .get("colour")
            .map(|held| held.kind.as_str()),
        Some("Colour")
    );
    let text = std::fs::read_to_string(dir.join("lamp.toml")).unwrap();
    assert!(text.contains(r#"type = "Colour""#), "{text}");
}

#[test]
fn renaming_a_type_another_type_names_brings_that_one_along_too() {
    let (dir, mut library) = shared("type-rename-nested");
    std::fs::write(
        dir.join(TYPES_DIR).join("lamp-type.toml"),
        "[[type]]\nname = \"Lamp\"\n\n[[type.field]]\nname = \"level\"\ntype = \"Percent\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("panel.toml"),
        "name = \"Panel\"\n\n[[field]]\nname = \"main\"\ntype = \"Lamp\"\n",
    )
    .unwrap();
    library.reload();

    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Percent");
    library.begin_type_edit();
    library.type_draft.as_mut().unwrap().definition.layout.name = "Ratio".to_owned();
    library.save_type_draft().unwrap();

    assert!(library.failures.is_empty(), "{:?}", library.failures);
    let held = std::fs::read_to_string(dir.join(TYPES_DIR).join("lamp-type.toml")).unwrap();
    assert!(held.contains(r#"type = "Ratio""#), "{held}");
    assert!(library
        .entries
        .iter()
        .any(|entry| entry.frame.name == "Panel"));
}

#[test]
fn a_frame_waiting_under_a_type_follows_it_being_renamed() {
    let (_, mut library) = shared("draft-under-rename");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("colour")]));
    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    {
        let draft = library.draft.as_mut().unwrap();
        let expansion =
            schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
        layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);
    }

    // Off to the type, rename it, come back.
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");
    library.begin_type_edit();
    library.type_draft.as_mut().unwrap().definition.layout.name = "Colour".to_owned();
    library.save_type_draft().unwrap();

    let draft = library.draft.as_ref().expect("the frame is still waiting");
    assert_eq!(
        draft
            .frame
            .stated
            .get("colour")
            .map(|held| held.kind.as_str()),
        Some("Colour")
    );
    assert_eq!(draft.frame.size(), 3);
    // Without this the draft names a type nothing defines any more, and
    // Save stays refused until the editor is closed and reopened.
    assert_eq!(library.draft_problem(), None);
}

#[test]
fn a_field_cannot_be_renamed_onto_another_one() {
    let mut draft = draft_of(LAYERED);
    assert_eq!(draft.frame.declared, ["header", "here", "crc"]);

    // Typed through on the way to something else, or meant: either way the
    // loader refuses two fields of one name, and the values a technician
    // types are held against it.
    layout::rename_field(&mut draft.frame, 0, "crc");
    assert_eq!(draft.frame.declared, ["header", "here", "crc"]);

    // Nor onto a field that only exists because a type expanded.
    layout::rename_field(&mut draft.frame, 0, "here.x");
    assert_eq!(draft.frame.declared, ["header", "here", "crc"]);

    // A name nothing else answers to goes through, expansion and all.
    layout::rename_field(&mut draft.frame, 0, "start");
    assert_eq!(draft.frame.declared, ["start", "here", "crc"]);
    assert_eq!(covered(&draft), ("start".to_owned(), "here.y".to_owned()));
}

#[test]
fn a_field_name_can_be_cleared_on_the_way_to_another_one() {
    let mut draft = Draft {
        origin: Some(Origin {
            file: PathBuf::from("layered.toml"),
            text: LAYERED.to_owned(),
        }),
        ..draft_of(LAYERED)
    };

    // Emptying the box is how a name gets replaced, so it goes through.
    layout::rename_field(&mut draft.frame, 0, "");
    assert_eq!(draft.frame.declared, ["", "here", "crc"]);

    // And the save is refused while it stands, rather than writing a field
    // that answers to nothing.
    let problem = draft.problem(&TypeLibrary::default()).expect("refused");
    assert!(problem.contains("needs a name"), "{problem}");

    layout::rename_field(&mut draft.frame, 0, "start");
    assert_eq!(draft.frame.declared, ["start", "here", "crc"]);
    assert_eq!(draft.problem(&TypeLibrary::default()), None);
}

#[test]
fn a_checksum_cannot_be_moved_above_what_it_covers() {
    let text = r#"
name = "Guarded"

[[field]]
name = "a"
type = "u8"

[[field]]
name = "b"
type = "u8"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "a", to = "b" }
"#;
    let mut draft = draft_of(text);

    // Moving it up puts it between the two bytes it protects, which leaves
    // it inside its own range and unsavable.
    layout::move_field(&mut draft.frame, 2, false);

    assert_eq!(draft.frame.declared, ["a", "b", "crc"]);
    assert_eq!(draft.problem(&TypeLibrary::default()), None);
}

#[test]
fn a_frame_waiting_under_a_type_survives_it_being_deleted() {
    let (_, mut library) = shared("draft-under-delete");
    let types = library.types().clone();
    library.begin_new(FrameDef::flat("Built", vec![plain("colour")]));
    let stated = Stated {
        kind: "Rgb".to_owned(),
        repeat: None,
        instances: None,
    };
    {
        let draft = library.draft.as_mut().unwrap();
        let expansion =
            schema::instantiate(&types, "colour", "Rgb", &stated, draft.frame.endian).unwrap();
        layout::state_as(&mut draft.frame, 0, Some(&stated), expansion);
    }
    let was = library.draft.as_ref().unwrap().frame.fields.clone();

    // Off to the type from one of the frame's own fields, and delete it there.
    library.type_selected = library
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Rgb");
    library.delete_selected_type().unwrap();

    // The frame is still open, still three bytes, and no longer naming a type
    // nothing defines.
    let draft = library.draft.as_ref().expect("the frame is still waiting");
    assert_eq!(draft.frame.fields, was);
    assert!(draft.frame.stated.is_empty());
    assert_eq!(
        draft.frame.declared,
        ["colour.red", "colour.green", "colour.blue"]
    );
    assert_eq!(library.draft_problem(), None);
}
