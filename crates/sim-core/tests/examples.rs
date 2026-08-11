//! The shipped examples are held to the same standard as the code: they must
//! load, encode, decode, and match the byte count their description claims.
//!
//! The folder is walked rather than listed, so a new example is covered the
//! moment it is added.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sim_core::frame::codec;
use sim_core::frame::schema::{self, TypeLibrary};
use sim_core::frame::value::{FieldValues, Value};
use sim_core::frame::{Endianness, FieldDef, FieldKind, FrameDef, ScalarType, ValueRange};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/frames")
}

fn load_examples() -> Vec<(String, FrameDef)> {
    let dir = examples_dir();
    let types = TypeLibrary::load_dir(&dir.join("types")).expect("shared types should load");

    let paths = schema::toml_files(&dir).expect("examples folder should be readable");
    assert!(!paths.is_empty(), "no example found in {}", dir.display());

    paths
        .into_iter()
        .map(|path| {
            let label = path.file_name().unwrap().to_string_lossy().into_owned();
            let frame =
                schema::load_with(&path, &types).unwrap_or_else(|error| panic!("{label}: {error}"));
            (label, frame)
        })
        .collect()
}

/// Every field gets a value of the right shape, as the editor does when a frame
/// is opened, so encoding exercises the layout rather than the defaults.
fn seed(frame: &FrameDef) -> FieldValues {
    let mut values = FieldValues::new();
    for field in &frame.fields {
        // Clamped, so a field declared 40..60 is exercised at 40 rather than
        // failing the encode on a value its own subtype forbids.
        let value = match &field.kind {
            FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => {
                Value::Float(match field.range {
                    Some(ValueRange::Float { min, max }) => 1.5_f64.clamp(min, max),
                    _ => 1.5,
                })
            }
            FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
                Value::Uint(match field.range {
                    Some(ValueRange::Uint { min, max }) => 1u64.clamp(min, max),
                    _ => 1,
                })
            }
            FieldKind::Scalar(_) => Value::Int(match field.range {
                Some(ValueRange::Int { min, max }) => (-1i64).clamp(min, max),
                _ => -1,
            }),
            FieldKind::Bytes { len } => Value::Bytes(vec![0xA5; *len]),
            FieldKind::Text { len } => Value::Text("x".repeat(*len)),
            FieldKind::Enum { variants, .. } => Value::Uint(variants[0].value),
            FieldKind::Bits { bits, .. } => Value::Bits(
                bits.iter()
                    .map(|bit| (bit.name.clone(), 0))
                    .collect::<BTreeMap<_, _>>(),
            ),
            FieldKind::Checksum { .. } => continue,
        };
        values.insert(field.name.clone(), value);
    }
    values
}

#[test]
fn every_example_loads() {
    for (label, frame) in load_examples() {
        assert!(!frame.fields.is_empty(), "{label} declares no field");
        assert!(frame.size() > 0, "{label} encodes to nothing");
    }
}

#[test]
fn every_example_round_trips_through_the_codec() {
    for (label, frame) in load_examples() {
        let values = seed(&frame);
        let bytes =
            codec::encode(&frame, &values).unwrap_or_else(|error| panic!("{label}: {error}"));
        assert_eq!(bytes.len(), frame.size(), "{label}");

        let decoded =
            codec::decode(&frame, &bytes).unwrap_or_else(|error| panic!("{label}: {error}"));
        assert!(
            decoded.checksum_mismatches.is_empty(),
            "{label}: a freshly encoded frame failed its own checksum"
        );
    }
}

#[test]
fn the_documented_sizes_are_accurate() {
    for (label, frame) in load_examples() {
        let description = frame
            .description
            .as_deref()
            .unwrap_or_else(|| panic!("{label} has no description"));
        let claimed = description
            .rsplit_once(", ")
            .and_then(|(_, tail)| tail.strip_suffix(" bytes"))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| panic!("{label}: description should end with \"N bytes\""));

        assert_eq!(
            claimed,
            frame.size(),
            "{label} claims {claimed} bytes but encodes to {}",
            frame.size()
        );
    }
}

/// A frame says which of its fields the file actually writes, and which it
/// produced by expanding a type or a repeat.
///
/// The editor leans on this: an expanded field cannot be changed where it sits,
/// only where it is declared, and writing one back as a plain field would
/// flatten what someone took the trouble to factorise.
#[test]
fn a_frame_knows_which_of_its_fields_the_file_wrote() {
    let dir = examples_dir();
    let types = TypeLibrary::load_dir(&dir.join("types")).expect("shared types should load");

    // Four entries in the file, twenty-one on the wire.
    let templates = schema::load_with(&dir.join("06-templates.toml"), &types).expect("should load");
    assert_eq!(
        templates.declared,
        ["header", "zone_count", "zone", "crc"],
        "the names the file writes, in the order it writes them"
    );
    assert_eq!(templates.fields.len(), 21);
    assert!(templates.fields.len() > templates.declared.len());

    // Everything on the wire is either written down or attributable to
    // something that is. Nothing may be orphaned, or the editor would not know
    // where to send someone who wants to change it.
    for field in &templates.fields {
        let declared = templates.declared.contains(&field.name);
        let generated = templates.generated_by(&field.name);
        assert!(
            declared || generated.is_some(),
            "{} belongs to nothing",
            field.name
        );
    }
    // Two levels of type and a repeat, all attributed to the one field the file
    // writes.
    assert_eq!(
        templates.generated_by("zone.left.led[0].mode"),
        Some("zone")
    );
    assert_eq!(
        templates.generated_by("zone.right.accent.blue"),
        Some("zone")
    );
    // Fields the file writes are nobody's expansion, and `zone_count` in
    // particular must not be mistaken for something `zone` produced.
    assert_eq!(templates.generated_by("zone_count"), None);
    assert_eq!(templates.generated_by("crc"), None);

    // A frame with no types and no repeats declares exactly what it carries.
    let flat = schema::load_with(&dir.join("02-scalars.toml"), &types).expect("should load");
    let names: Vec<&str> = flat
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(flat.declared, names);
    assert!(flat
        .fields
        .iter()
        .all(|field| flat.generated_by(&field.name).is_none()));
}

/// The shipped scenarios are held to the same standard: they parse, and every
/// frame they name is one the example frames actually define.
#[test]
fn every_example_scenario_loads_and_names_frames_that_exist() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/scenarios");
    let paths = schema::toml_files(&dir).expect("scenarios folder should be readable");
    assert!(!paths.is_empty(), "no scenario found in {}", dir.display());

    let frames: Vec<FrameDef> = load_examples()
        .into_iter()
        .map(|(_, frame)| frame)
        .collect();

    for path in paths {
        let label = path.file_name().unwrap().to_string_lossy().into_owned();
        let scenarios =
            sim_core::scenario::load(&path).unwrap_or_else(|error| panic!("{label}: {error}"));

        for scenario in scenarios {
            for step in &scenario.steps {
                let sim_core::scenario::Action::Send { frame, with, .. } = &step.action else {
                    continue;
                };
                let Some(definition) = frames.iter().find(|known| &known.name == frame) else {
                    panic!(
                        "{label}: scenario {} sends {frame}, which no example frame defines",
                        scenario.name
                    );
                };

                // Names resolving is not enough: an override has to fit the
                // field it names, or the step fails on its first pass. A float
                // written for an integer field is the way that happens.
                let mut values = sim_core::frame::value::seed_values(definition);
                for (field, value) in with {
                    let declared = definition
                        .fields
                        .iter()
                        .find(|declared| &declared.name == field)
                        .unwrap_or_else(|| panic!("{label}: {frame} has no field named {field}"));
                    let coerced = value.clone().coerced_to(&declared.kind).unwrap_or_else(|| {
                        panic!(
                            "{label}: {frame}.{field} is {}, which cannot hold {value:?}",
                            declared.kind.type_name()
                        )
                    });
                    values.insert(field.clone(), coerced);
                }
                codec::encode(definition, &values).unwrap_or_else(|error| {
                    panic!(
                        "{label}: scenario {} cannot encode {frame}: {error}",
                        scenario.name
                    )
                });
            }
        }
    }
}

#[test]
fn rewriting_an_example_frame_unchanged_leaves_the_file_alone() {
    let dir = examples_dir();
    let types = TypeLibrary::load_dir(&dir.join("types")).expect("shared types should load");
    for path in schema::toml_files(&dir).expect("examples folder should be readable") {
        let text = std::fs::read_to_string(&path).expect("readable");
        let frame = schema::from_toml_with(&text, &types).expect("valid");
        let written = schema::update_in(&text, &frame).expect("rewritten");
        assert_eq!(written, text, "{} was disturbed", path.display());
    }
}

#[test]
fn editing_a_subtyped_field_changes_the_value_and_not_the_type() {
    let dir = examples_dir();
    let types = TypeLibrary::load_dir(&dir.join("types")).expect("shared types should load");
    let path = dir.join("07-subtypes.toml");
    let text = std::fs::read_to_string(&path).expect("readable");

    let mut frame = schema::from_toml_with(&text, &types).expect("valid");
    let target = frame.field_index("target").expect("target field");
    frame.fields[target].default = Some(Value::Uint(75));

    let written = schema::update_in(&text, &frame).expect("rewritten");

    assert!(written.contains(r#"type = "Percent""#));
    assert!(written.contains("default = 75"));
    let reread = schema::from_toml_with(&written, &types).expect("still valid");
    assert_eq!(reread.fields[target].default, Some(Value::Uint(75)));
    assert_eq!(reread.fields[target].range, frame.fields[target].range);
}

#[test]
fn rewriting_a_shared_type_unchanged_leaves_the_file_alone() {
    let dir = examples_dir().join("types");
    let types = TypeLibrary::load_dir(&dir).expect("shared types should load");
    for path in schema::toml_files(&dir).expect("types folder should be readable") {
        let text = std::fs::read_to_string(&path).expect("readable");
        for definition in types.definitions().expect("every type is valid") {
            let written =
                schema::update_type_in(&text, definition.name(), &definition).expect("rewritten");
            assert_eq!(
                written,
                text,
                "{} disturbed by {}",
                path.display(),
                definition.name()
            );
        }
    }
}

#[test]
fn a_shared_type_is_read_as_the_two_things_a_type_can_be() {
    let types = TypeLibrary::load_dir(&examples_dir().join("types")).expect("types");

    let percent = types.definition("Percent").expect("valid").expect("there");
    assert_eq!(
        percent
            .narrows
            .as_ref()
            .map(|narrows| narrows.base.as_str()),
        Some("u8")
    );
    assert_eq!(
        percent.narrows.and_then(|narrows| narrows.range),
        Some(ValueRange::Uint { min: 0, max: 100 })
    );
    assert!(percent.layout.fields.is_empty());

    let led = types
        .definition("LedConfig")
        .expect("valid")
        .expect("there");
    assert!(led.narrows.is_none());
    assert_eq!(led.layout.declared, ["mode", "brightness", "period_ms"]);
    assert_eq!(led.layout.size(), 4);
}

/// A subtype narrowing another one, which the shipped examples do not have and
/// which is where an inherited range gets mistaken for a declared one.
const CHAINED: &str = r#"
[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 100 }

[[type]]
name = "Duty"
base = "Percent"
"#;

#[test]
fn a_subtype_that_declares_no_range_of_its_own_is_not_given_one() {
    let mut types = TypeLibrary::default();
    types.merge_toml(CHAINED).expect("valid");

    let duty = types.definition("Duty").expect("valid").expect("there");
    assert_eq!(
        duty.narrows.as_ref().and_then(|narrows| narrows.range),
        None
    );

    // An unchanged save must leave it following `Percent`, not pinned to what
    // `Percent` happens to say today.
    let written = schema::update_type_in(CHAINED, "Duty", &duty).expect("rewritten");
    assert_eq!(written, CHAINED);
}

#[test]
fn a_field_added_to_a_little_endian_frame_says_its_own_order() {
    let text =
        "name = \"Little\"\nendian = \"little\"\n\n[[field]]\nname = \"a\"\ntype = \"u16\"\n";
    let frame = schema::from_toml(text).expect("valid");
    assert_eq!(frame.endian, Endianness::Little);

    let mut grown = frame.clone();
    grown.fields.push(FieldDef {
        name: "b".to_owned(),
        description: None,
        kind: FieldKind::Scalar(ScalarType::U16),
        endian: Endianness::Big,
        default: None,
        range: None,
    });
    grown.declared.push("b".to_owned());

    let written = schema::update_in(text, &grown).expect("rewritten");

    // Without this the field reads back little-endian, and the editor refuses
    // to save a frame it cannot write faithfully.
    assert_eq!(schema::from_toml(&written).expect("valid"), grown);
    assert!(written.contains("endian = \"big\""));
}

const TWO_GROUPS: &str = r#"
[[type]]
name = "Rgb"

[[type.field]]
name = "red"
type = "u8"

[[type.field]]
name = "green"
type = "u8"

[[type]]
name = "Pair"

[[type.field]]
name = "left"
type = "u8"

[[type.field]]
name = "right"
type = "u8"
"#;

#[test]
fn reordering_one_types_fields_leaves_them_under_that_type() {
    let mut types = TypeLibrary::default();
    types.merge_toml(TWO_GROUPS).expect("valid");
    let mut rgb = types.definition("Rgb").expect("valid").expect("there");
    rgb.layout.fields.swap(0, 1);
    rgb.layout.declared.swap(0, 1);

    let written = schema::update_type_in(TWO_GROUPS, "Rgb", &rgb).expect("rewritten");

    // A `[[type.field]]` attaches to the last `[[type]]` written, so a run of
    // them sent past the end of the file is handed to somebody else's type.
    let mut after = TypeLibrary::default();
    after.merge_toml(&written).expect("still valid");
    assert_eq!(
        after
            .definition("Rgb")
            .expect("valid")
            .expect("there")
            .layout
            .declared,
        ["green", "red"]
    );
    assert_eq!(
        after
            .definition("Pair")
            .expect("valid")
            .expect("there")
            .layout
            .declared,
        ["left", "right"]
    );
}

#[test]
fn adding_a_field_to_the_first_of_two_types_does_not_give_it_to_the_second() {
    let mut types = TypeLibrary::default();
    types.merge_toml(TWO_GROUPS).expect("valid");
    let mut rgb = types.definition("Rgb").expect("valid").expect("there");
    rgb.layout.fields.push(FieldDef {
        name: "blue".to_owned(),
        description: None,
        kind: FieldKind::Scalar(ScalarType::U8),
        endian: Endianness::Big,
        default: None,
        range: None,
    });
    rgb.layout.declared.push("blue".to_owned());

    let written = schema::update_type_in(TWO_GROUPS, "Rgb", &rgb).expect("rewritten");

    let mut after = TypeLibrary::default();
    after.merge_toml(&written).expect("still valid");
    assert_eq!(
        after
            .definition("Rgb")
            .expect("valid")
            .expect("there")
            .layout
            .declared,
        ["red", "green", "blue"]
    );
    assert_eq!(
        after
            .definition("Pair")
            .expect("valid")
            .expect("there")
            .layout
            .declared,
        ["left", "right"]
    );
}
