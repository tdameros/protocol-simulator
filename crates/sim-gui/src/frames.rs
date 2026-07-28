use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sim_core::frame::schema;
use sim_core::frame::value::{FieldValues, Value};
use sim_core::frame::{FieldKind, FrameDef, ScalarType};

/// Frame definitions loaded from a directory, plus the values being edited.
///
/// The TOML files stay the source of truth: the library only reads them, so the
/// definitions can be edited in a real text editor and picked up with Reload.
#[derive(Default)]
pub struct FrameLibrary {
    pub directory: Option<PathBuf>,
    pub frames: Vec<FrameDef>,
    /// Files that failed to load, as (file name, reason).
    pub failures: Vec<(String, String)>,
    pub selected: Option<usize>,
    /// Edited values, keyed by frame name so switching frames keeps your input.
    values: BTreeMap<String, FieldValues>,
}

impl FrameLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.frames.clear();
        self.failures.clear();

        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                self.failures
                    .push((directory.display().to_string(), error.to_string()));
                self.directory = Some(directory);
                return;
            }
        };

        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();

        for path in paths {
            match schema::load(&path) {
                Ok(frame) => self.frames.push(frame),
                Err(error) => self.failures.push((file_label(&path), error.to_string())),
            }
        }

        self.frames.sort_by(|a, b| a.name.cmp(&b.name));
        self.selected = (!self.frames.is_empty()).then_some(0);
        self.directory = Some(directory);
    }

    pub fn reload(&mut self) {
        if let Some(directory) = self.directory.clone() {
            self.load_from(directory);
        }
    }

    #[must_use]
    pub fn selected_frame(&self) -> Option<&FrameDef> {
        self.selected.and_then(|index| self.frames.get(index))
    }

    /// Values for `frame`, seeded from its defaults the first time it is opened.
    pub fn values_mut(&mut self, frame: &FrameDef) -> &mut FieldValues {
        self.values
            .entry(frame.name.clone())
            .or_insert_with(|| seed_values(frame))
    }

    pub fn reset_values(&mut self, frame: &FrameDef) {
        self.values.insert(frame.name.clone(), seed_values(frame));
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Every field starts at its declared default, or at a neutral value of the
/// right shape, so the hex preview renders from the moment a frame is opened.
fn seed_values(frame: &FrameDef) -> FieldValues {
    let mut values = FieldValues::new();
    for field in &frame.fields {
        if let Some(default) = &field.default {
            values.insert(field.name.clone(), default.clone());
            continue;
        }
        let Some(value) = neutral_value(&field.kind) else {
            continue;
        };
        values.insert(field.name.clone(), value);
    }
    values
}

fn neutral_value(kind: &FieldKind) -> Option<Value> {
    Some(match kind {
        FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => Value::Float(0.0),
        FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => Value::Uint(0),
        FieldKind::Scalar(_) => Value::Int(0),
        FieldKind::Bytes { len } => Value::Bytes(vec![0; *len]),
        FieldKind::Text { .. } => Value::Text(String::new()),
        FieldKind::Enum { variants, .. } => {
            Value::Uint(variants.first().map_or(0, |variant| variant.value))
        }
        FieldKind::Bits { bits, .. } => {
            Value::Bits(bits.iter().map(|bit| (bit.name.clone(), 0u64)).collect())
        }
        // Computed at encode time; nothing for the operator to supply.
        FieldKind::Checksum { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(library.frames.len(), 1);
        assert_eq!(library.frames[0].name, "Telemetry");
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
}
