use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sim_core::frame::schema::{self, TypeDef, TypeLibrary};
use sim_core::frame::value::{seed_values, FieldValues};
use sim_core::frame::FrameDef;

/// Subdirectory holding the type definitions every frame in the folder can use.
const TYPES_DIR: &str = "types";

/// A frame and the file it came from.
///
/// One frame per file, so the path is its whole identity: unlike a scenario, it
/// never has to be told apart from a neighbour sharing the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub file: PathBuf,
    pub frame: FrameDef,
}

/// Frame definitions loaded from a directory, plus the values being edited.
///
/// The TOML files stay the source of truth. The editor writes back into them
/// rather than owning them, so a definition can still be edited in a real text
/// editor and picked up with Reload.
#[derive(Default)]
pub struct FrameLibrary {
    pub directory: Option<PathBuf>,
    pub entries: Vec<Entry>,
    /// Files that failed to load, as (file name, reason).
    pub failures: Vec<(String, String)>,
    /// Names of the shared types available to every frame in the folder.
    pub shared_types: Vec<String>,
    pub selected: Option<usize>,
    /// The frame being edited, if any.
    pub draft: Option<Draft>,
    /// The shared types, and where each of them lives.
    pub type_entries: Vec<TypeEntry>,
    pub type_selected: Option<usize>,
    /// The shared type being edited, if any.
    pub type_draft: Option<TypeDraft>,
    /// Kept so a draft can be read back against the same types it was written
    /// against, which is the only way the guard means anything.
    types: TypeLibrary,
    /// Edited values, keyed by frame name so switching frames keeps your input.
    values: BTreeMap<String, FieldValues>,
}

/// A shared type and the file it came from.
///
/// Unlike a frame, a types file holds as many as someone put in it, so the name
/// is half of the identity.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeEntry {
    pub file: PathBuf,
    pub definition: TypeDef,
}

/// A shared type as the panel has it, which is not yet what the disk has.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDraft {
    pub definition: TypeDef,
    pub origin: Option<TypeOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeOrigin {
    pub file: PathBuf,
    pub text: String,
    /// The name the file still knows it by, renaming being an edit like any
    /// other.
    pub name: String,
}

/// What editing a shared type would do to a frame that uses it.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// The frame would no longer load at all.
    Broken(String),
    /// The frame still loads, but not as the same bytes.
    Resized { was: usize, now: usize },
    /// Same size, different layout.
    Reshaped,
}

/// A frame as the panel has it, which is not yet what the disk has.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub frame: FrameDef,
    /// The file it came from, with the text it held when editing started.
    ///
    /// The text is carried rather than re-read so that the guard checks the
    /// exact bytes the save will produce, and so that asking whether a draft is
    /// savable does not touch the disk on every repaint.
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub text: String,
}

impl FrameLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.entries.clear();
        self.failures.clear();
        self.shared_types.clear();
        self.type_entries.clear();
        self.draft = None;
        self.type_draft = None;

        self.types = self.load_shared_types(&directory);

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
            match schema::load_with(&path, &self.types) {
                Ok(frame) => self.entries.push(Entry { file: path, frame }),
                Err(error) => self.failures.push((file_label(&path), error.to_string())),
            }
        }

        self.entries.sort_by(|a, b| a.frame.name.cmp(&b.frame.name));
        self.selected = (!self.entries.is_empty()).then_some(0);
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
        let entries = &self.entries;
        let stored = &mut self.values;
        for Entry { frame, .. } in entries {
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
        self.attribute_types(&types_dir, &types);
        types
    }

    /// Records which file each shared type was written in.
    ///
    /// Read a second time, one file at a time, only to learn the names each one
    /// holds: a type may name a type from another file, so what they mean can
    /// only be worked out from the library as a whole.
    fn attribute_types(&mut self, directory: &Path, types: &TypeLibrary) {
        let Ok(paths) = schema::toml_files(directory) else {
            return;
        };
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut alone = TypeLibrary::default();
            if alone.merge_toml(&text).is_err() {
                continue;
            }
            for name in alone.names() {
                match types.definition(name) {
                    Ok(Some(definition)) => self.type_entries.push(TypeEntry {
                        file: path.clone(),
                        definition,
                    }),
                    Ok(None) => {}
                    Err(error) => self.failures.push((
                        format!("{TYPES_DIR}/{}", file_label(&path)),
                        error.to_string(),
                    )),
                }
            }
        }
        self.type_entries
            .sort_by(|a, b| a.definition.name().cmp(b.definition.name()));
        self.type_selected = (!self.type_entries.is_empty()).then_some(0);
    }

    pub fn reload(&mut self) {
        if let Some(directory) = self.directory.clone() {
            self.load_from(directory);
        }
    }

    #[must_use]
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    #[must_use]
    pub fn selected_frame(&self) -> Option<&FrameDef> {
        self.selected_entry().map(|entry| &entry.frame)
    }

    /// Every frame loaded, in the order the list shows them.
    pub fn frames(&self) -> impl Iterator<Item = &FrameDef> {
        self.entries.iter().map(|entry| &entry.frame)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

    #[must_use]
    pub fn selected_type(&self) -> Option<&TypeEntry> {
        self.type_selected.and_then(|at| self.type_entries.get(at))
    }

    /// Starts editing the selected shared type, as a copy.
    pub fn begin_type_edit(&mut self) {
        let Some(entry) = self.selected_type() else {
            return;
        };
        let origin = std::fs::read_to_string(&entry.file)
            .ok()
            .map(|text| TypeOrigin {
                file: entry.file.clone(),
                text,
                name: entry.definition.name().to_owned(),
            });
        self.type_draft = Some(TypeDraft {
            definition: entry.definition.clone(),
            origin,
        });
    }

    pub fn begin_new_type(&mut self, definition: TypeDef) {
        self.type_draft = Some(TypeDraft {
            definition,
            origin: None,
        });
    }

    pub fn cancel_type_edit(&mut self) {
        self.type_draft = None;
    }

    #[must_use]
    pub fn type_draft_is_dirty(&self) -> bool {
        let Some(draft) = &self.type_draft else {
            return false;
        };
        let Some(origin) = &draft.origin else {
            return true;
        };
        self.type_entries
            .iter()
            .find(|entry| entry.definition.name() == origin.name)
            .is_none_or(|entry| entry.definition != draft.definition)
    }

    /// What the type draft would do to the frames that use it.
    ///
    /// Nothing on this list stops a save. A type exists to be shared, so
    /// changing it is meant to reach the frames naming it, and the point is
    /// that it says so first rather than after the next Reload.
    #[must_use]
    pub fn type_draft_impact(&self) -> Vec<(String, Effect)> {
        let Some(candidate) = self.candidate_types() else {
            return Vec::new();
        };
        self.entries
            .iter()
            .filter_map(|entry| {
                let effect = match schema::load_with(&entry.file, &candidate) {
                    Err(error) => Effect::Broken(error.to_string()),
                    Ok(now) if now.size() != entry.frame.size() => Effect::Resized {
                        was: entry.frame.size(),
                        now: now.size(),
                    },
                    Ok(now) if now.fields != entry.frame.fields => Effect::Reshaped,
                    Ok(_) => return None,
                };
                Some((entry.frame.name.clone(), effect))
            })
            .collect()
    }

    /// Why the type draft cannot be saved, if it cannot.
    #[must_use]
    pub fn type_draft_problem(&self) -> Option<String> {
        let draft = self.type_draft.as_ref()?;
        let name = draft.definition.name();
        if name.trim().is_empty() {
            return Some("a type needs a name".to_owned());
        }
        let taken = self.type_entries.iter().any(|entry| {
            entry.definition.name() == name
                && draft
                    .origin
                    .as_ref()
                    .is_none_or(|origin| origin.name != entry.definition.name())
        });
        if taken {
            return Some(format!("a type named \"{name}\" already exists"));
        }

        let Some(candidate) = self.candidate_types() else {
            return Some("the types folder cannot be read as it stands".to_owned());
        };
        match candidate.definition(name) {
            Err(error) => Some(error.to_string()),
            Ok(None) => Some(format!("{name} would not be read back")),
            Ok(Some(reread)) if reread != draft.definition => Some(format!(
                "{name} cannot be written the way this file states it"
            )),
            Ok(Some(_)) => None,
        }
    }

    /// The library as it would be with the draft saved.
    fn candidate_types(&self) -> Option<TypeLibrary> {
        let draft = self.type_draft.as_ref()?;
        let written = self.type_text(draft).ok()?;
        let edited = draft
            .origin
            .as_ref()
            .map_or_else(|| self.types_file(), |origin| origin.file.clone());

        let mut candidate = TypeLibrary::default();
        let directory = self.directory.as_ref()?.join(TYPES_DIR);
        let others = schema::toml_files(&directory).unwrap_or_default();
        for path in others.iter().filter(|path| **path != edited) {
            // A broken file costs only itself here too, as it does at load
            // time. Giving up on the whole library would leave the guard with
            // nothing to check against, which reads as nothing to complain
            // about.
            let _ = candidate.merge_file(path);
        }
        candidate.merge_toml(&written).ok()?;
        Some(candidate)
    }

    /// The text saving the type draft would write.
    fn type_text(&self, draft: &TypeDraft) -> Result<String, schema::SchemaError> {
        match &draft.origin {
            Some(origin) => schema::update_type_in(&origin.text, &origin.name, &draft.definition),
            None => match std::fs::read_to_string(self.types_file()) {
                Ok(text) => {
                    schema::update_type_in(&text, draft.definition.name(), &draft.definition)
                }
                Err(_) => schema::type_to_toml(&draft.definition),
            },
        }
    }

    /// Where a type nobody has saved yet belongs.
    ///
    /// One file for everything shared, unless someone has made others: types
    /// name each other freely, so splitting them buys nothing and a technician
    /// should not have to choose.
    fn types_file(&self) -> PathBuf {
        let directory = self.directory.clone().unwrap_or_default().join(TYPES_DIR);
        self.type_entries
            .first()
            .map_or_else(|| directory.join("types.toml"), |entry| entry.file.clone())
    }

    /// Writes the type draft to disk and reloads, every frame naming it having
    /// possibly changed shape.
    ///
    /// # Errors
    ///
    /// Returns an error if the draft cannot be written back faithfully, or the
    /// file cannot be written.
    pub fn save_type_draft(&mut self) -> Result<()> {
        let Some(draft) = self.type_draft.clone() else {
            return Ok(());
        };
        if let Some(reason) = self.type_draft_problem() {
            bail!("{reason}");
        }
        let file = draft
            .origin
            .as_ref()
            .map_or_else(|| self.types_file(), |origin| origin.file.clone());
        let written = self
            .type_text(&draft)
            .with_context(|| format!("cannot describe {}", draft.definition.name()))?;

        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&file, written)
            .with_context(|| format!("cannot write {}", file.display()))?;

        self.reload();
        Ok(())
    }

    /// Deletes the selected shared type from its file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or written.
    pub fn delete_selected_type(&mut self) -> Result<()> {
        let Some(entry) = self.selected_type().cloned() else {
            return Ok(());
        };
        let text = std::fs::read_to_string(&entry.file)
            .with_context(|| format!("cannot read {}", entry.file.display()))?;
        let written = schema::remove_type_from(&text, entry.definition.name())
            .with_context(|| format!("cannot remove {}", entry.definition.name()))?;
        std::fs::write(&entry.file, written)
            .with_context(|| format!("cannot write {}", entry.file.display()))?;

        self.reload();
        Ok(())
    }

    /// Starts editing the selected frame, as a copy.
    ///
    /// A copy so that cancelling is free: nothing on disk or in the list has
    /// moved until a save says so.
    pub fn begin_edit(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let origin = std::fs::read_to_string(&entry.file)
            .ok()
            .map(|text| Origin {
                file: entry.file.clone(),
                text,
            });
        self.draft = Some(Draft {
            frame: entry.frame.clone(),
            origin,
        });
    }

    /// Starts a frame that does not exist yet.
    pub fn begin_new(&mut self, frame: FrameDef) {
        self.draft = Some(Draft {
            frame,
            origin: None,
        });
    }

    pub fn cancel_edit(&mut self) {
        self.draft = None;
    }

    /// Whether the draft says something the disk does not.
    ///
    /// One that has never been saved always does, having nothing to be compared
    /// against.
    #[must_use]
    pub fn draft_is_dirty(&self) -> bool {
        let Some(draft) = &self.draft else {
            return false;
        };
        let Some(origin) = &draft.origin else {
            return true;
        };
        self.entries
            .iter()
            .find(|entry| entry.file == origin.file)
            .is_none_or(|entry| entry.frame != draft.frame)
    }

    /// Why the draft cannot be saved, if it cannot.
    #[must_use]
    pub fn draft_problem(&self) -> Option<String> {
        let draft = self.draft.as_ref()?;
        draft
            .problem(&self.types)
            .or_else(|| {
                self.name_taken(draft).map(|file| {
                    format!(
                        "a frame named \"{}\" already lives in {}",
                        draft.frame.name,
                        file_label(&file)
                    )
                })
            })
            .or_else(|| self.file_taken(draft))
    }

    /// Whether saving would land on a file another frame already occupies.
    ///
    /// The name is not the file. `suggested_file` folds case and punctuation
    /// away, so "telemetry" and "Telemetry" both want `telemetry.toml`, and the
    /// name check above lets the pair through. Writing would take the other
    /// frame's definition with it, and nothing would say so.
    fn file_taken(&self, draft: &Draft) -> Option<String> {
        if draft.origin.is_some() {
            return None;
        }
        let directory = self.directory.as_ref()?;
        let wanted = suggested_file(directory, &draft.frame.name);
        let held = self.entries.iter().find(|entry| entry.file == wanted);
        match held {
            Some(entry) => Some(format!(
                "{} already holds {}",
                file_label(&wanted),
                entry.frame.name
            )),
            None if wanted.exists() => Some(format!("{} already exists", file_label(&wanted))),
            None => None,
        }
    }

    /// The file already holding a frame of that name, if it is not this one.
    ///
    /// Names have to be unique across the folder, not merely across a file:
    /// scenarios name the frame they send, and the values panel remembers what
    /// you typed by name. Two frames answering to one name would make both
    /// ambiguous.
    fn name_taken(&self, draft: &Draft) -> Option<PathBuf> {
        self.entries
            .iter()
            .find(|entry| {
                entry.frame.name == draft.frame.name
                    && draft
                        .origin
                        .as_ref()
                        .is_none_or(|origin| origin.file != entry.file)
            })
            .map(|entry| entry.file.clone())
    }

    /// Writes the draft to disk and takes it into the list.
    ///
    /// `into` says which file a frame that has never been saved belongs in. It
    /// is ignored for one that already has a home, which is written back where
    /// it came from.
    ///
    /// # Errors
    ///
    /// Returns an error if the draft cannot be written back faithfully, if the
    /// name is already taken, or if the file cannot be written.
    pub fn save_draft(&mut self, into: &Path) -> Result<()> {
        let Some(draft) = self.draft.clone() else {
            return Ok(());
        };
        if let Some(reason) = self.draft_problem() {
            bail!("{reason}");
        }

        let file = draft
            .origin
            .as_ref()
            .map_or_else(|| into.to_path_buf(), |origin| origin.file.clone());
        let written = draft
            .written()
            .with_context(|| format!("cannot describe {}", draft.frame.name))?;
        std::fs::write(&file, &written)
            .with_context(|| format!("cannot write {}", file.display()))?;

        self.take_in(&file, written, draft);
        Ok(())
    }

    /// Deletes the selected frame, file and all.
    ///
    /// One frame per file, so there is nothing left in it to keep.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be removed.
    pub fn delete_selected(&mut self) -> Result<()> {
        let Some(entry) = self.selected_entry().cloned() else {
            return Ok(());
        };
        match std::fs::remove_file(&entry.file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot remove {}", entry.file.display()))
            }
        }

        self.values.remove(&entry.frame.name);
        self.entries.retain(|held| held.file != entry.file);
        self.selected = (!self.entries.is_empty())
            .then(|| self.selected.unwrap_or(0).min(self.entries.len() - 1));
        self.draft = None;
        Ok(())
    }

    /// Folds a saved draft into the list, replacing what it came from.
    fn take_in(&mut self, file: &Path, written: String, draft: Draft) {
        self.entries.retain(|entry| entry.file != file);
        self.entries.push(Entry {
            file: file.to_path_buf(),
            frame: draft.frame.clone(),
        });
        self.entries.sort_by(|a, b| a.frame.name.cmp(&b.frame.name));
        self.selected = self.entries.iter().position(|entry| entry.file == file);
        // The values kept under the old shape may no longer fit the new one.
        self.conform_values();

        // The draft now stands on what was just written, so saving twice in a
        // row edits that rather than reverting to how the file used to read.
        self.draft = Some(Draft {
            origin: Some(Origin {
                file: file.to_path_buf(),
                text: written,
            }),
            ..draft
        });
    }
}

impl Draft {
    /// The exact text a save would write.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be serialised.
    pub fn written(&self) -> Result<String, schema::SchemaError> {
        match &self.origin {
            Some(origin) => schema::update_in(&origin.text, &self.frame),
            // No file to preserve, so there is nothing to preserve it with.
            None => schema::to_toml(&self.frame),
        }
    }

    /// Why the draft could not be saved as it stands, if it could not.
    ///
    /// Checked by writing it out and reading it back, rather than by listing
    /// the rules here: the loader is the authority on what a frame may be, and
    /// a second copy of its rules would drift from it.
    ///
    /// The read-back is held to equality, not merely to loading. A frame that
    /// comes back different is worse than one that does not come back at all,
    /// because it would go on the wire as bytes nobody asked for. This is what
    /// catches an edit the file's own wording cannot express, such as widening
    /// a field the file states as a named subtype.
    #[must_use]
    pub fn problem(&self, types: &TypeLibrary) -> Option<String> {
        let text = match self.written() {
            Ok(text) => text,
            Err(error) => return Some(error.to_string()),
        };
        match schema::from_toml_with(&text, types) {
            Err(error) => Some(error.to_string()),
            Ok(reread) if reread != self.frame => Some(disagreement(&self.frame, &reread)),
            Ok(_) => None,
        }
    }
}

/// What the file would say back, put in terms of the field it concerns.
fn disagreement(wanted: &FrameDef, got: &FrameDef) -> String {
    if wanted.name != got.name {
        return format!("the file would read back as \"{}\"", got.name);
    }
    let differing = wanted
        .fields
        .iter()
        .zip(&got.fields)
        .find(|(wanted, got)| wanted != got)
        .map(|(wanted, _)| wanted.name.clone());
    match differing {
        Some(field) => format!("{field} cannot be written the way this file states it"),
        None => "this frame cannot be written back the way the file states it".to_owned(),
    }
}

/// The file a frame of this name would go in, had it none yet.
#[must_use]
pub fn suggested_file(directory: &Path, name: &str) -> PathBuf {
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches('-').to_owned();
    directory.join(format!(
        "{}.toml",
        if stem.is_empty() { "frame" } else { &stem }
    ))
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
    use crate::layout;
    use sim_core::frame::value::Value;
    use sim_core::frame::{Endianness, FieldDef, FieldKind, FieldSpan, ScalarType, ValueRange};

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
        assert_eq!(library.shared_types, ["LedConfig"]);
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
    fn saving_twice_in_a_row_edits_the_same_file_rather_than_reverting() {
        let (dir, mut library) = library_of("twice", &[("telemetry.toml", GOOD)]);
        library.begin_edit();

        for value in [1u64, 0] {
            let draft = library.draft.as_mut().unwrap();
            let mode = draft.frame.field_index("mode").unwrap();
            draft.frame.fields[mode].default = Some(Value::Uint(value));
            library.save_draft(&dir).unwrap();
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
                endian: Endianness::Big,
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
                endian: Endianness::Big,
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
    fn renaming_a_frame_keeps_it_in_its_own_file() {
        let (dir, mut library) = library_of("rename", &[("telemetry.toml", GOOD)]);
        library.begin_edit();
        library.draft.as_mut().unwrap().frame.name = "Beacon".to_owned();

        assert_eq!(library.draft_problem(), None);
        library.save_draft(&dir).unwrap();

        assert_eq!(library.entries.len(), 1);
        assert_eq!(library.entries[0].file, dir.join("telemetry.toml"));
        assert_eq!(library.entries[0].frame.name, "Beacon");
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
            endian: Endianness::Big,
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
    fn renaming_a_type_instance_is_refused_rather_than_shown_and_lost() {
        let mut draft = draft_of(LAYERED);
        layout::rename_field(&mut draft.frame, 1, "corner");

        assert_eq!(draft.frame.declared, ["header", "here", "crc"]);
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
        draft.definition.narrows.as_mut().unwrap().range =
            Some(ValueRange::Uint { min: 0, max: 200 });

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
                endian: Endianness::Big,
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
    fn a_type_a_frame_still_needs_cannot_be_deleted_without_the_frame_saying_so() {
        let (_, mut library) = shared("types-delete");
        library.type_selected = Some(1);
        library.delete_selected_type().unwrap();

        // The type is gone, and the frame that named it now says why it will
        // not load rather than loading as something else.
        assert!(library.entries.is_empty());
        assert_eq!(library.failures.len(), 1);
        assert!(
            library.failures[0].1.contains("Rgb"),
            "{:?}",
            library.failures
        );
    }

    #[test]
    fn a_type_nobody_saved_yet_lands_beside_the_ones_already_there() {
        let (dir, mut library) = shared("types-new");
        library.begin_new_type(TypeDef {
            layout: FrameDef::flat(
                "Pair",
                vec![
                    FieldDef {
                        name: "left".to_owned(),
                        description: None,
                        kind: FieldKind::Scalar(ScalarType::U8),
                        endian: Endianness::Big,
                        default: None,
                        range: None,
                    },
                    FieldDef {
                        name: "right".to_owned(),
                        description: None,
                        kind: FieldKind::Scalar(ScalarType::U8),
                        endian: Endianness::Big,
                        default: None,
                        range: None,
                    },
                ],
            ),
            narrows: None,
        });

        assert_eq!(library.type_draft_problem(), None);
        library.save_type_draft().unwrap();

        assert_eq!(library.type_entries.len(), 3);
        let text = std::fs::read_to_string(dir.join(TYPES_DIR).join("shared.toml")).unwrap();
        assert!(text.contains("# Three bytes of colour."));
        assert!(text.contains(r#"name = "Pair""#));
    }

    #[test]
    fn a_type_name_already_taken_is_refused() {
        let (_, mut library) = shared("types-clash");
        library.begin_new_type(TypeDef {
            layout: FrameDef::flat("Rgb", vec![]),
            narrows: None,
        });

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
                endian: Endianness::Big,
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
}
