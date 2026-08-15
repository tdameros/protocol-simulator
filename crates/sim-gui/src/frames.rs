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

/// The files a change to a shared type would rewrite, gathered before any one
/// of them is touched.
///
/// Nothing reaches the disk until every frame among them is known to come back
/// with the same bytes, so a change that would break the folder breaks nothing
/// instead of half of it.
#[derive(Default)]
struct Rewrites {
    /// Path to the text that would replace it, each built on the last so that
    /// two changes to one file do not undo each other.
    pending: BTreeMap<PathBuf, String>,
    /// The frames among them, as they stand, to be held to what they encode.
    touched: Vec<(PathBuf, FrameDef)>,
}

impl Rewrites {
    /// A file as the gathered rewrites have it, or as the disk does.
    fn text_of(&self, path: &Path) -> Result<String> {
        match self.pending.get(path) {
            Some(text) => Ok(text.clone()),
            None => std::fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn insert(&mut self, path: &Path, text: String) {
        self.pending.insert(path.to_path_buf(), text);
    }

    fn write(&self) -> Result<()> {
        for (path, text) in &self.pending {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(path, text)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        Ok(())
    }
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
        let Ok(rewrites) = self.pending_for_type_draft(draft) else {
            return Vec::new();
        };
        let pending = rewrites.pending;
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
    fn pending_for_type_draft(&self, draft: &TypeDraft) -> Result<Rewrites> {
        let now = draft.definition.name();
        let renamed = draft
            .origin
            .as_ref()
            .map(|origin| origin.name.clone())
            .filter(|was| was != now);

        let mut rewrites = match &renamed {
            Some(was) => self.cascade(was, |layout| layout::retype(layout, was, now))?,
            None => Rewrites::default(),
        };

        // The type itself last, over whatever the cascade has already put in
        // its file: another type in there may name it too.
        let file = draft
            .origin
            .as_ref()
            .map_or_else(|| self.types_file(draft), |origin| origin.file.clone());
        let written = match &draft.origin {
            // Falling back on the copy taken when editing started, never on a
            // file holding this type alone: the one on disk may hold others,
            // and writing them away because it briefly could not be read would
            // be the worst possible answer.
            Some(origin) => {
                let text = rewrites
                    .text_of(&file)
                    .unwrap_or_else(|_| origin.text.clone());
                schema::update_type_in(&text, &origin.name, &draft.definition)
            }
            None => schema::type_to_toml(&draft.definition),
        }
        .with_context(|| format!("cannot describe {now}"))?;
        rewrites.insert(&file, written);
        Ok(rewrites)
    }

    /// Rewrites every type and every frame that names `kind`, putting `change`
    /// through each field list that does.
    ///
    /// Types first: one may name another, and a frame using the middle one must
    /// not notice what happened at the bottom. Renaming and writing a type out
    /// in full are the same walk with a different edit.
    fn cascade(&self, kind: &str, change: impl Fn(&mut FrameDef)) -> Result<Rewrites> {
        let mut rewrites = Rewrites::default();
        for held in &self.type_entries {
            if held.definition.name() == kind || !layout::uses_type(&held.definition.layout, kind) {
                continue;
            }
            let mut moved = held.definition.clone();
            change(&mut moved.layout);
            let text = rewrites.text_of(&held.file)?;
            let written = schema::update_type_in(&text, held.definition.name(), &moved)
                .with_context(|| format!("cannot rewrite {}", held.definition.name()))?;
            rewrites.insert(&held.file, written);
        }
        for held in &self.entries {
            if !layout::uses_type(&held.frame, kind) {
                continue;
            }
            let mut moved = held.frame.clone();
            change(&mut moved);
            let text = rewrites.text_of(&held.file)?;
            let written = schema::update_in(&text, &moved)
                .with_context(|| format!("cannot rewrite {}", held.frame.name))?;
            rewrites.insert(&held.file, written);
            rewrites
                .touched
                .push((held.file.clone(), held.frame.clone()));
        }
        Ok(rewrites)
    }

    fn candidate_types(&self) -> Option<TypeLibrary> {
        let draft = self.type_draft.as_ref()?;
        let pending = self.pending_for_type_draft(draft).ok()?.pending;

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

        // As for a deletion: nothing is written until every frame the rewrite
        // reaches is known to come back with the same bytes. A rename touches
        // files the guard on the draft itself never looks at.
        let rewrites = self.pending_for_type_draft(&draft)?;
        self.check_wire_survives(&rewrites)?;
        rewrites.write()?;

        self.move_type_file(&draft)?;

        self.reload_keeping_the_frame_draft();
        if let Some(index) = draft.for_field {
            self.give_field_the_type(index, draft.definition.name());
        }
        Ok(())
    }

    /// Moves a renamed type's file to match, when the type is alone in it.
    ///
    /// A file holding several types is named after none of them in particular,
    /// so it stays as it was. One holding a single type is named after it, and
    /// a name that no longer matches is only confusing.
    fn move_type_file(&mut self, draft: &TypeDraft) -> Result<()> {
        let Some(origin) = &draft.origin else {
            return Ok(());
        };
        if origin.name == draft.definition.name() {
            return Ok(());
        }
        let alone = self
            .type_entries
            .iter()
            .filter(|entry| entry.file == origin.file)
            .count()
            == 1;
        if !alone {
            return Ok(());
        }
        let Some(directory) = self.directory.as_ref() else {
            return Ok(());
        };
        let wanted = suggested_type_file(directory, draft.definition.name());
        if wanted == origin.file || wanted.exists() {
            return Ok(());
        }
        std::fs::rename(&origin.file, &wanted).with_context(|| {
            format!(
                "cannot move {} to {}",
                origin.file.display(),
                wanted.display()
            )
        })
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
        let mut rewrites = self.cascade(&name, |layout| layout::inline_type(layout, &name))?;

        let text = rewrites.text_of(&entry.file)?;
        let stripped = schema::remove_type_from(&text, &name)
            .with_context(|| format!("cannot remove {name}"))?;
        rewrites.insert(&entry.file, stripped);

        self.check_wire_survives(&rewrites)?;
        rewrites.write()?;

        // A types file with nothing left in it is not worth keeping.
        let mut left = TypeLibrary::default();
        if rewrites
            .pending
            .get(&entry.file)
            .is_some_and(|text| left.merge_toml(text).is_ok() && left.is_empty())
        {
            std::fs::remove_file(&entry.file)
                .with_context(|| format!("cannot remove {}", entry.file.display()))?;
        }

        // A frame may be open underneath, having reached the type from one of
        // its own fields. It still names what has just gone, so it is written
        // out there too rather than left pointing at nothing.
        if let Some(draft) = &mut self.draft {
            layout::inline_type(&mut draft.frame, &name);
        }
        self.reload_keeping_the_frame_draft();
        Ok(())
    }

    /// Checks that every frame about to be rewritten still encodes the same
    /// bytes, which is the whole promise of writing a type out rather than
    /// deleting what it produced.
    fn check_wire_survives(&self, rewrites: &Rewrites) -> Result<()> {
        let pending = &rewrites.pending;
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

        for (path, was) in &rewrites.touched {
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
        let wanted = self.target_file(draft)?;
        if draft.origin.as_ref().map(|origin| &origin.file) == Some(&wanted) {
            return None;
        }
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

    /// The file the draft belongs in under the name it now carries.
    ///
    /// Renaming a frame moves its file, so that what the folder shows on disk
    /// and what the frames are called cannot drift apart.
    fn target_file(&self, draft: &Draft) -> Option<PathBuf> {
        let directory = self.directory.as_ref()?;
        Some(suggested_file(directory, &draft.frame.name))
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

        // Renamed, so the file follows. Written first and moved after, which
        // leaves the content somewhere at every moment.
        let file = match self.target_file(&draft) {
            Some(wanted) if wanted != file => {
                std::fs::rename(&file, &wanted).with_context(|| {
                    format!("cannot move {} to {}", file.display(), wanted.display())
                })?;
                wanted
            }
            _ => file,
        };

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
        let was = draft.origin.as_ref().map(|origin| origin.file.clone());
        self.entries
            .retain(|entry| entry.file != file && Some(&entry.file) != was.as_ref());
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
#[path = "frames_tests.rs"]
mod tests;
