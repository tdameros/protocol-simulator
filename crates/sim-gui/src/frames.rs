use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sim_core::frame::schema;
use sim_core::frame::value::{seed_values, FieldValues};
use sim_core::frame::FrameDef;

/// Subdirectory holding the type definitions every frame in the folder can use.
const TYPES_DIR: &str = "types";

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
    /// Names of the shared types available to every frame in the folder.
    pub shared_types: Vec<String>,
    pub selected: Option<usize>,
    /// Edited values, keyed by frame name so switching frames keeps your input.
    values: BTreeMap<String, FieldValues>,
}

impl FrameLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.frames.clear();
        self.failures.clear();
        self.shared_types.clear();

        let types = self.load_shared_types(&directory);

        let paths = match schema::toml_files(&directory) {
            Ok(paths) => paths,
            Err(error) => {
                self.failures
                    .push((directory.display().to_string(), error.to_string()));
                self.directory = Some(directory);
                return;
            }
        };

        for path in paths {
            match schema::load_with(&path, &types) {
                Ok(frame) => self.frames.push(frame),
                Err(error) => self.failures.push((file_label(&path), error.to_string())),
            }
        }

        self.frames.sort_by(|a, b| a.name.cmp(&b.name));
        self.selected = (!self.frames.is_empty()).then_some(0);
        self.directory = Some(directory);
        // Also on a plain reload: a definition edited on disk can have changed
        // the shape of a field someone already typed a value into.
        self.conform_values();
    }

    /// Drops everything, for a window about to be given a different project.
    pub fn forget(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn saved_values(&self) -> &BTreeMap<String, FieldValues> {
        &self.values
    }

    /// Takes in values written down elsewhere, against the definitions loaded
    /// now.
    ///
    /// Values whose frame is not loaded are kept untouched rather than dropped:
    /// a frames folder that is missing today may well be back tomorrow, and
    /// saving in the meantime must not quietly empty the file.
    pub fn restore_values(&mut self, values: BTreeMap<String, FieldValues>) {
        self.values = values;
        self.conform_values();
    }

    /// Rebuilds each loaded frame's values from its defaults, overlaid with
    /// whatever was supplied that still fits.
    fn conform_values(&mut self) {
        let frames = &self.frames;
        let stored = &mut self.values;
        for frame in frames {
            let Some(supplied) = stored.remove(&frame.name) else {
                continue;
            };
            let mut values = seed_values(frame);
            for field in &frame.fields {
                let Some(value) = supplied.get(&field.name) else {
                    continue;
                };
                // A value that cannot mean anything for this field leaves the
                // default in place rather than an unencodable frame.
                if let Some(value) = value.clone().coerced_to(&field.kind) {
                    values.insert(field.name.clone(), value);
                }
            }
            stored.insert(frame.name.clone(), values);
        }
    }

    /// Reads `types/`, one file at a time so a broken one costs only itself.
    fn load_shared_types(&mut self, directory: &Path) -> schema::TypeLibrary {
        let mut types = schema::TypeLibrary::default();
        let types_dir = directory.join(TYPES_DIR);
        if !types_dir.is_dir() {
            return types;
        }

        match schema::toml_files(&types_dir) {
            Ok(paths) => {
                for path in paths {
                    if let Err(error) = types.merge_file(&path) {
                        self.failures.push((
                            format!("{TYPES_DIR}/{}", file_label(&path)),
                            error.to_string(),
                        ));
                    }
                }
            }
            Err(error) => self
                .failures
                .push((TYPES_DIR.to_owned(), error.to_string())),
        }

        self.shared_types = types.names().into_iter().map(ToOwned::to_owned).collect();
        types
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

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::frame::value::Value;

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
        assert_eq!(library.shared_types, ["LedConfig"]);
        // The subfolder itself must not be mistaken for a frame file.
        assert_eq!(library.frames.len(), 1);
        assert_eq!(library.frames[0].fields.len(), 6);
        assert_eq!(library.frames[0].size(), 9);

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
}
