use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sim_core::frame::schema::{self, TypeDef, TypeLibrary};
use sim_core::frame::value::{seed_values, FieldValues};
use sim_core::frame::FrameDef;

use crate::layout;

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
    pub selected: Option<usize>,
    /// The frame being edited, if any.
    pub draft: Option<Draft>,
    /// The shared types, and where each of them lives.
    pub type_entries: Vec<TypeEntry>,
    pub type_selected: Option<usize>,
    /// The shared type being edited, if any.
    pub type_draft: Option<TypeDraft>,
    /// The last answer given about the type draft, and the draft it was given
    /// about.
    ///
    /// Both answers go to the disk, and the panel asks for them on every
    /// repaint. Kept so that looking at an editor nobody is typing into costs
    /// nothing.
    type_review: Option<(TypeDraft, Review)>,
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
    /// The declared field of the frame underneath that asked for this type.
    ///
    /// Set when the type was started from a field's kind picker, which is a
    /// request for that field to be of it: making the type and then having to
    /// go and pick it is a step nobody meant to take.
    pub for_field: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeOrigin {
    pub file: PathBuf,
    pub text: String,
    /// The name the file still knows it by, renaming being an edit like any
    /// other.
    pub name: String,
}

/// What is known about the type draft as it stands.
#[derive(Debug, Clone, Default, PartialEq)]
struct Review {
    problem: Option<String>,
    impact: Vec<(String, Effect)>,
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
    /// The declared fields the file states through a named type, worked out
    /// once when editing starts rather than parsed again on every repaint.
    pub stated: BTreeMap<String, String>,
}

impl FrameLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.entries.clear();
        self.failures.clear();
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
    pub fn types(&self) -> &TypeLibrary {
        &self.types
    }

    /// The shared types a field may be declared as.
    ///
    /// `without` leaves one out, for the type editor: a type naming itself is
    /// the one thing the loader refuses outright.
    #[must_use]
    pub fn shared_choices(&self, without: Option<&str>) -> Vec<crate::panels::frame_edit::Shared> {
        self.type_entries
            .iter()
            .filter(|entry| Some(entry.definition.name()) != without)
            .map(|entry| crate::panels::frame_edit::Shared {
                name: entry.definition.name().to_owned(),
                group: entry.definition.narrows.is_none(),
                size: entry.definition.layout.size(),
            })
            .collect()
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
            for_field: None,
        });
    }

    pub fn begin_new_type(&mut self, definition: TypeDef, for_field: Option<usize>) {
        self.type_draft = Some(TypeDraft {
            definition,
            origin: None,
            for_field,
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

    /// Whether the type draft can be saved, and what it would do to the frames
    /// that use it.
    ///
    /// Worked out once per change rather than once per repaint: both answers
    /// read every file in `types/`, and the impact reloads every frame besides.
    fn type_review(&mut self) -> Review {
        let Some(draft) = self.type_draft.clone() else {
            return Review::default();
        };
        if let Some((asked, review)) = &self.type_review {
            if *asked == draft {
                return review.clone();
            }
        }
        let review = Review {
            problem: self.compute_type_problem(),
            impact: self.compute_type_impact(),
        };
        self.type_review = Some((draft, review.clone()));
        review
    }

    /// What the type draft would do to the frames that use it.
    ///
    /// Nothing on this list stops a save. A type exists to be shared, so
    /// changing it is meant to reach the frames naming it, and the point is
    /// that it says so first rather than after the next Reload.
    pub fn type_draft_impact(&mut self) -> Vec<(String, Effect)> {
        self.type_review().impact
    }

    pub fn type_draft_problem(&mut self) -> Option<String> {
        self.type_review().problem
    }

    fn compute_type_impact(&self) -> Vec<(String, Effect)> {
        let Some(candidate) = self.candidate_types() else {
            return Vec::new();
        };
        let Some(draft) = self.type_draft.as_ref() else {
            return Vec::new();
        };
        let pending = self.pending_for_type_draft(draft);
        self.entries
            .iter()
            .filter_map(|entry| {
                // Read as the save would leave it, not as it stands: a frame
                // brought along by a rename has nothing to report.
                let read = match pending.get(&entry.file) {
                    Some(text) => schema::from_toml_with(text, &candidate),
                    None => schema::load_with(&entry.file, &candidate),
                };
                let effect = match read {
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
    fn compute_type_problem(&self) -> Option<String> {
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
        // A type with neither fields nor a base is refused where it is named,
        // not where it is written, so the round-trip guard below reads it back
        // happily and only a frame using it would ever complain.
        if draft.definition.narrows.is_none() && draft.definition.layout.fields.is_empty() {
            return Some(format!("{name} needs at least one field"));
        }
        // The name is not the file. Two names folding to one path would see the
        // save take the other type's definition with it, saying nothing.
        if draft.origin.is_none() {
            let wanted = self.types_file(draft);
            if let Some(held) = self.type_entries.iter().find(|entry| entry.file == wanted) {
                return Some(format!(
                    "{} already holds {}",
                    file_label(&wanted),
                    held.definition.name()
                ));
            }
            if wanted.exists() {
                return Some(format!("{} already exists", file_label(&wanted)));
            }
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
    /// Every file saving the type draft would rewrite, and what it would put in
    /// them.
    ///
    /// Worked out in one place so that the warning shown beside Save and the
    /// save itself cannot say different things. Renaming a type is the case
    /// that makes this necessary: the frames naming it have to be brought
    /// along, or they stop loading the moment it is saved.
    fn pending_for_type_draft(&self, draft: &TypeDraft) -> BTreeMap<PathBuf, String> {
        let mut pending: BTreeMap<PathBuf, String> = BTreeMap::new();
        let now = draft.definition.name();
        let renamed = draft
            .origin
            .as_ref()
            .map(|origin| origin.name.clone())
            .filter(|was| was != now);

        if let Some(was) = &renamed {
            for held in &self.type_entries {
                if held.definition.name() == was || !layout::uses_type(&held.definition.layout, was)
                {
                    continue;
                }
                let mut moved = held.definition.clone();
                layout::retype(&mut moved.layout, was, now);
                let Ok(text) = Self::text_of(&pending, &held.file) else {
                    continue;
                };
                if let Ok(written) = schema::update_type_in(&text, held.definition.name(), &moved) {
                    pending.insert(held.file.clone(), written);
                }
            }
            for held in &self.entries {
                if !layout::uses_type(&held.frame, was) {
                    continue;
                }
                let mut moved = held.frame.clone();
                layout::retype(&mut moved, was, now);
                let Ok(text) = Self::text_of(&pending, &held.file) else {
                    continue;
                };
                if let Ok(written) = schema::update_in(&text, &moved) {
                    pending.insert(held.file.clone(), written);
                }
            }
        }

        // The type itself last, over whatever the cascade has already put in
        // its file: another type in there may name it too.
        let file = draft
            .origin
            .as_ref()
            .map_or_else(|| self.types_file(draft), |origin| origin.file.clone());
        let written = match (&draft.origin, Self::text_of(&pending, &file)) {
            (Some(origin), Ok(text)) => {
                schema::update_type_in(&text, &origin.name, &draft.definition)
            }
            _ => schema::type_to_toml(&draft.definition),
        };
        if let Ok(written) = written {
            pending.insert(file, written);
        }
        pending
    }

    /// A file as the pending rewrites have it, or as the disk does.
    fn text_of(pending: &BTreeMap<PathBuf, String>, path: &Path) -> Result<String> {
        match pending.get(path) {
            Some(text) => Ok(text.clone()),
            None => std::fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn candidate_types(&self) -> Option<TypeLibrary> {
        let draft = self.type_draft.as_ref()?;
        let pending = self.pending_for_type_draft(draft);

        let mut candidate = TypeLibrary::default();
        let directory = self.directory.as_ref()?.join(TYPES_DIR);
        for path in schema::toml_files(&directory).unwrap_or_default() {
            // A broken file costs only itself here too, as it does at load
            // time. Giving up on the whole library would leave the guard with
            // nothing to check against, which reads as nothing to complain
            // about.
            let _ = match pending.get(&path) {
                Some(text) => candidate.merge_toml(text),
                None => candidate.merge_file(&path),
            };
        }
        // A type in a file of its own that nothing has written yet.
        for (path, text) in &pending {
            if !path.starts_with(&directory) || path.exists() {
                continue;
            }
            let _ = candidate.merge_toml(text);
        }
        Some(candidate)
    }

    /// Where a type nobody has saved yet belongs.
    fn types_file(&self, draft: &TypeDraft) -> PathBuf {
        let directory = self.directory.clone().unwrap_or_default();
        suggested_type_file(&directory, draft.definition.name())
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
        // The frame waiting underneath still calls the type by the name it had
        // when it was opened. Renaming the type on disk without telling it
        // would leave it naming something that no longer exists.
        let renamed = draft
            .origin
            .as_ref()
            .map(|origin| origin.name.clone())
            .filter(|was| was != draft.definition.name());
        if let (Some(was), Some(held)) = (&renamed, self.draft.as_mut()) {
            layout::retype(&mut held.frame, was, draft.definition.name());
        }

        for (path, text) in &self.pending_for_type_draft(&draft) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(path, text)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }

        self.reload_keeping_the_frame_draft();
        if let Some(index) = draft.for_field {
            self.give_field_the_type(index, draft.definition.name());
        }
        Ok(())
    }

    /// Declares the field that asked for this type as being of it.
    fn give_field_the_type(&mut self, index: usize, kind: &str) {
        let types = self.types.clone();
        let Some(draft) = &mut self.draft else {
            return;
        };
        let Some(name) = draft.frame.declared.get(index).cloned() else {
            return;
        };
        let stated = sim_core::frame::Stated {
            kind: kind.to_owned(),
            repeat: None,
            instances: None,
        };
        let Ok(expansion) = schema::instantiate(&types, &name, kind, &stated, draft.frame.endian)
        else {
            return;
        };
        layout::state_as(&mut draft.frame, index, Some(&stated), expansion);
    }

    /// Reloads without throwing away a frame someone is in the middle of.
    ///
    /// A type can be opened from a field of a frame being edited, and that
    /// frame is waiting underneath. Its expanded fields came from the type as
    /// it was, so they are asked for again from the type as it now is.
    fn reload_keeping_the_frame_draft(&mut self) {
        let held = self.draft.take();
        self.reload();
        self.draft = held;

        let types = self.types.clone();
        let Some(draft) = &mut self.draft else {
            return;
        };
        let endian = draft.frame.endian;
        let stated: Vec<(String, sim_core::frame::Stated)> = draft
            .frame
            .stated
            .iter()
            .map(|(name, held)| (name.clone(), held.clone()))
            .collect();
        for (name, held) in stated {
            let Ok(expansion) = schema::instantiate(&types, &name, &held.kind, &held, endian)
            else {
                continue;
            };
            let Some(at) = draft.frame.declared.iter().position(|held| *held == name) else {
                continue;
            };
            layout::state_as(&mut draft.frame, at, Some(&held), expansion);
        }
    }

    /// Deletes the selected shared type, writing out in full everything that
    /// named it.
    ///
    /// The name disappears; the bytes do not. Every frame and every other type
    /// that used it keeps the fields it produced, written in place of the
    /// declaration that produced them. Refused as a whole if any of those
    /// rewrites would not read back as the same wire layout, rather than
    /// leaving half a folder rewritten.
    ///
    /// # Errors
    ///
    /// Returns an error if a file cannot be read or written, or if the wire
    /// would change.
    pub fn delete_selected_type(&mut self) -> Result<()> {
        let Some(entry) = self.selected_type().cloned() else {
            return Ok(());
        };
        let name = entry.definition.name().to_owned();
        let mut pending: BTreeMap<PathBuf, String> = BTreeMap::new();

        // Types first: one may name another, and a frame using the middle one
        // must not notice the bottom one going away.
        for held in &self.type_entries {
            if held.definition.name() == name || !layout::uses_type(&held.definition.layout, &name)
            {
                continue;
            }
            let mut spent = held.definition.clone();
            layout::inline_type(&mut spent.layout, &name);
            let text = Self::pending_text(&mut pending, &held.file)?;
            let written = schema::update_type_in(&text, held.definition.name(), &spent)
                .with_context(|| format!("cannot rewrite {}", held.definition.name()))?;
            pending.insert(held.file.clone(), written);
        }

        let text = Self::pending_text(&mut pending, &entry.file)?;
        let stripped = schema::remove_type_from(&text, &name)
            .with_context(|| format!("cannot remove {name}"))?;
        pending.insert(entry.file.clone(), stripped);

        let mut touched: Vec<(PathBuf, FrameDef)> = Vec::new();
        for held in &self.entries {
            if !layout::uses_type(&held.frame, &name) {
                continue;
            }
            let mut spent = held.frame.clone();
            layout::inline_type(&mut spent, &name);
            let text = Self::pending_text(&mut pending, &held.file)?;
            let written = schema::update_in(&text, &spent)
                .with_context(|| format!("cannot rewrite {}", held.frame.name))?;
            pending.insert(held.file.clone(), written);
            touched.push((held.file.clone(), held.frame.clone()));
        }

        self.check_wire_survives(&pending, &touched)?;

        for (path, text) in &pending {
            std::fs::write(path, text)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        // A types file with nothing left in it is not worth keeping.
        let mut left = TypeLibrary::default();
        if pending
            .get(&entry.file)
            .is_some_and(|text| left.merge_toml(text).is_ok() && left.is_empty())
        {
            std::fs::remove_file(&entry.file)
                .with_context(|| format!("cannot remove {}", entry.file.display()))?;
        }

        self.reload();
        Ok(())
    }

    /// The text of a file as the pending rewrites have it, read from disk the
    /// first time it is asked for.
    fn pending_text(pending: &mut BTreeMap<PathBuf, String>, path: &Path) -> Result<String> {
        if let Some(text) = pending.get(path) {
            return Ok(text.clone());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        pending.insert(path.to_path_buf(), text.clone());
        Ok(text)
    }

    /// Checks that every frame about to be rewritten still encodes the same
    /// bytes, which is the whole promise of writing a type out rather than
    /// deleting what it produced.
    fn check_wire_survives(
        &self,
        pending: &BTreeMap<PathBuf, String>,
        touched: &[(PathBuf, FrameDef)],
    ) -> Result<()> {
        let mut candidate = TypeLibrary::default();
        let directory = self
            .directory
            .as_ref()
            .map(|held| held.join(TYPES_DIR))
            .unwrap_or_default();
        for path in schema::toml_files(&directory).unwrap_or_default() {
            let text = match pending.get(&path) {
                Some(text) => text.clone(),
                None => std::fs::read_to_string(&path).unwrap_or_default(),
            };
            let _ = candidate.merge_toml(&text);
        }

        for (path, was) in touched {
            let Some(text) = pending.get(path) else {
                continue;
            };
            let now = schema::from_toml_with(text, &candidate)
                .with_context(|| format!("{} would not load", was.name))?;
            if now.fields != was.fields {
                bail!("{} would not send the same bytes", was.name);
            }
        }
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
                stated: schema::stated_as_types(&text),
                file: entry.file.clone(),
                text,
            });
        self.draft = Some(Draft {
            frame: entry.frame.clone(),
            origin,
        });
    }

    /// A frame name nothing answers to yet, and whose file is free as well.
    ///
    /// Both, because a name that is free while its file is not would open an
    /// editor that Save refuses on the first click.
    #[must_use]
    pub fn unused_frame_name(&self, wanted: &str) -> String {
        let directory = self.directory.clone().unwrap_or_default();
        unused(wanted, self.entries.len(), |name| {
            let file = suggested_file(&directory, name);
            self.entries
                .iter()
                .all(|entry| entry.frame.name != *name && entry.file != file)
                && !file.exists()
        })
    }

    /// The same for a shared type.
    #[must_use]
    pub fn unused_type_name(&self, wanted: &str) -> String {
        let directory = self.directory.clone().unwrap_or_default();
        unused(wanted, self.type_entries.len(), |name| {
            let file = suggested_type_file(&directory, name);
            self.type_entries
                .iter()
                .all(|entry| entry.definition.name() != name && entry.file != file)
                && !file.exists()
        })
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

        self.take_in(&file, draft);
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
    fn take_in(&mut self, file: &Path, draft: Draft) {
        self.entries.retain(|entry| entry.file != file);
        self.entries.push(Entry {
            file: file.to_path_buf(),
            frame: draft.frame,
        });
        self.entries.sort_by(|a, b| a.frame.name.cmp(&b.frame.name));
        self.selected = self.entries.iter().position(|entry| entry.file == file);
        // The values kept under the old shape may no longer fit the new one.
        self.conform_values();
        // Saving is finishing. Leaving the editor open on a draft that now says
        // exactly what the disk says only invites wondering whether it saved.
        self.draft = None;
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

/// `wanted`, or `wanted 2`, `wanted 3` and so on until something is free.
///
/// Bounded by how many are already there: one of that many suffixes has to be
/// free, since that is how many names there are to collide with.
fn unused(wanted: &str, held: usize, free: impl Fn(&str) -> bool) -> String {
    if free(wanted) {
        return wanted.to_owned();
    }
    (2..=held + 2)
        .map(|suffix| format!("{wanted} {suffix}"))
        .find(|name| free(name))
        .unwrap_or_else(|| wanted.to_owned())
}

/// The file a frame of this name would go in, had it none yet.
#[must_use]
pub fn suggested_file(directory: &Path, name: &str) -> PathBuf {
    file_named(directory, name, "frame")
}

/// The file a shared type of this name would go in, had it none yet.
///
/// One type per file, as one frame per file: the path is then the whole of its
/// identity, deleting it is deleting the file, and nobody has to be asked which
/// of several files a new type belongs in. Types name each other across files
/// freely, the library being merged before anything is resolved, so keeping
/// them together buys nothing.
#[must_use]
pub fn suggested_type_file(directory: &Path, name: &str) -> PathBuf {
    file_named(&directory.join(TYPES_DIR), name, "type")
}

fn file_named(directory: &Path, name: &str, fallback: &str) -> PathBuf {
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
        if stem.is_empty() { fallback } else { &stem }
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
    use sim_core::frame::schema::Subtype;
    use sim_core::frame::value::Value;
    use sim_core::frame::{
        Endianness, FieldDef, FieldKind, FieldSpan, ScalarType, Stated, ValueRange,
    };

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
                stated: schema::stated_as_types(LAYERED),
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
                stated: schema::stated_as_types(LAYERED),
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
                stated: schema::stated_as_types(LITTLE),
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
                stated: schema::stated_as_types(LITTLE),
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

        // What the panel greys out, and what the writer refuses to reword.
        assert_eq!(
            draft.origin.as_ref().map(|origin| origin.stated.clone()),
            Some([("target".to_owned(), "Percent".to_owned())].into())
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

        // Nor by moving the checksum in front of it, which lands in the same
        // place: a checksum with nothing to cover.
        layout::move_field(&mut draft.frame, 1, false);
        assert_eq!(draft.frame.declared, ["data", "crc"]);
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
                stated: schema::stated_as_types(LAYERED),
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
}
