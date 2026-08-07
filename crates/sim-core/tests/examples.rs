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
use sim_core::frame::{FieldKind, FrameDef, ScalarType, ValueRange};

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

/// The shipped scenarios are held to the same standard: they parse, and every
/// frame they name is one the example frames actually define.
#[test]
fn every_example_scenario_loads_and_names_frames_that_exist() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/scenarios");
    let paths = schema::toml_files(&dir).expect("scenarios folder should be readable");
    assert!(!paths.is_empty(), "no scenario found in {}", dir.display());

    let known: Vec<String> = load_examples()
        .into_iter()
        .map(|(_, frame)| frame.name)
        .collect();

    for path in paths {
        let label = path.file_name().unwrap().to_string_lossy().into_owned();
        let scenarios =
            sim_core::scenario::load(&path).unwrap_or_else(|error| panic!("{label}: {error}"));

        for scenario in scenarios {
            for frame in scenario.frames_used() {
                assert!(
                    known.iter().any(|name| name == frame),
                    "{label}: scenario {} sends {frame}, which no example frame defines",
                    scenario.name
                );
            }
        }
    }
}
