//! Scenario definitions loaded from a directory, and the one being edited.
//!
//! The files on disk stay the source of truth. The library only reads them, so
//! Reload picks up whatever a real editor has changed, and the panel writes
//! back through [`sim_core::scenario::update_in`], which leaves the comments a
//! developer wrote where they were.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};

use sim_core::frame::value::seed_values;
use sim_core::frame::{schema, FrameDef};
use sim_core::scenario::{self, Action, Counter, Expect, Scenario, Step};
use sim_core::{ConnectionId, HexPattern};

/// A scenario and the file it came from.
///
/// The file is half of its identity: saving has to know what to overwrite, and
/// a folder holds as many files as someone cared to make.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub file: PathBuf,
    pub scenario: Scenario,
}

#[derive(Default)]
pub struct ScenarioLibrary {
    pub directory: Option<PathBuf>,
    pub entries: Vec<Entry>,
    /// Files that failed to load, as (file name, reason).
    pub failures: Vec<(String, String)>,
    pub selected: Option<usize>,
    /// The scenario being edited, if any.
    pub draft: Option<Draft>,
}

/// A scenario as the panel has it, which is not yet what the disk has.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub scenario: Scenario,
    /// Where it lives, and under which name it currently sits there.
    ///
    /// The name is kept apart from `scenario.name` because renaming is an edit
    /// like any other: the file still holds the old one until this is saved.
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub name: String,
}

impl ScenarioLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.entries.clear();
        self.failures.clear();
        self.draft = None;

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
            // One file at a time, so a typo in one costs only that file.
            match scenario::load(&path) {
                Ok(loaded) => self
                    .entries
                    .extend(loaded.into_iter().map(|scenario| Entry {
                        file: path.clone(),
                        scenario,
                    })),
                Err(error) => self.failures.push((file_label(&path), error.to_string())),
            }
        }

        self.entries
            .sort_by(|a, b| a.scenario.name.cmp(&b.scenario.name));

        // The engine keys a running scenario by name, and so does the panel, so
        // two of them called the same thing cannot be told apart: starting one
        // lights up both rows, and the other can never be started at all.
        let mut seen: Vec<String> = Vec::new();
        self.entries.retain(|entry| {
            if seen.contains(&entry.scenario.name) {
                self.failures.push((
                    entry.scenario.name.clone(),
                    "a second scenario of this name was ignored, names have to be unique"
                        .to_owned(),
                ));
                return false;
            }
            seen.push(entry.scenario.name.clone());
            true
        });

        self.selected = (!self.entries.is_empty()).then_some(0);
        self.directory = Some(directory);
    }

    pub fn reload(&mut self) {
        if let Some(directory) = self.directory.clone() {
            self.load_from(directory);
        }
    }

    /// Drops everything, for a window about to be given a different project.
    pub fn forget(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    #[must_use]
    pub fn selected_scenario(&self) -> Option<&Scenario> {
        self.selected_entry().map(|entry| &entry.scenario)
    }

    /// Starts editing the selected scenario, as a copy.
    ///
    /// A copy so that cancelling is free: nothing on disk or in the list has
    /// moved until a save says so.
    pub fn begin_edit(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        self.draft = Some(Draft {
            scenario: entry.scenario.clone(),
            origin: Some(Origin {
                file: entry.file.clone(),
                name: entry.scenario.name.clone(),
            }),
        });
    }

    /// Starts a scenario that does not exist yet.
    pub fn begin_new(&mut self, scenario: Scenario) {
        self.draft = Some(Draft {
            scenario,
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
            .find(|entry| entry.file == origin.file && entry.scenario.name == origin.name)
            .is_none_or(|entry| entry.scenario != draft.scenario)
    }

    /// Writes the draft to disk and takes it into the list.
    ///
    /// `into` says which file a scenario that has never been saved belongs in.
    /// It is ignored for one that already has a home, which is written back
    /// where it came from.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is already taken by another scenario, or if
    /// the file cannot be read or written.
    pub fn save_draft(&mut self, into: &Path) -> Result<()> {
        let Some(draft) = self.draft.clone() else {
            return Ok(());
        };
        let name = draft.scenario.name.clone();

        // Names are the handle everything else uses, so a clash is refused
        // before it reaches the disk rather than dropped on the next reload.
        let taken = self.entries.iter().any(|entry| {
            entry.scenario.name == name
                && draft
                    .origin
                    .as_ref()
                    .is_none_or(|origin| origin.name != entry.scenario.name)
        });
        if taken {
            bail!("a scenario named \"{name}\" already exists");
        }

        let file = draft
            .origin
            .as_ref()
            .map_or_else(|| into.to_path_buf(), |origin| origin.file.clone());
        let existing = read_if_there(&file)?;

        let written = match (&draft.origin, existing) {
            (Some(origin), Some(text)) => scenario::update_in(&text, &origin.name, &draft.scenario),
            (None, Some(text)) => scenario::append_to(&text, &draft.scenario),
            // No file to preserve, so there is nothing to preserve it with.
            (_, None) => scenario::to_toml(std::slice::from_ref(&draft.scenario)),
        }
        .with_context(|| format!("cannot describe {name}"))?;

        std::fs::write(&file, written)
            .with_context(|| format!("cannot write {}", file.display()))?;

        self.take_in(&file, draft);
        Ok(())
    }

    /// Deletes the selected scenario from its file and from the list.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or written.
    pub fn delete_selected(&mut self) -> Result<()> {
        let Some(entry) = self.selected_entry().cloned() else {
            return Ok(());
        };
        let text = read_if_there(&entry.file)?.unwrap_or_default();
        let written = scenario::remove_from(&text, &entry.scenario.name)
            .with_context(|| format!("cannot remove {}", entry.scenario.name))?;
        std::fs::write(&entry.file, written)
            .with_context(|| format!("cannot write {}", entry.file.display()))?;

        self.entries
            .retain(|held| held.file != entry.file || held.scenario.name != entry.scenario.name);
        self.selected = (!self.entries.is_empty())
            .then(|| self.selected.unwrap_or(0).min(self.entries.len() - 1));
        self.draft = None;
        Ok(())
    }

    /// Folds a saved draft into the list, replacing what it came from.
    fn take_in(&mut self, file: &Path, draft: Draft) {
        if let Some(origin) = &draft.origin {
            self.entries
                .retain(|entry| entry.file != origin.file || entry.scenario.name != origin.name);
        }
        self.entries.push(Entry {
            file: file.to_path_buf(),
            scenario: draft.scenario.clone(),
        });
        self.entries
            .sort_by(|a, b| a.scenario.name.cmp(&b.scenario.name));

        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.scenario.name == draft.scenario.name);
        // The draft now answers to the name it was saved under, so saving twice
        // in a row edits the same entry instead of appending a second one.
        self.draft = Some(Draft {
            origin: Some(Origin {
                file: file.to_path_buf(),
                name: draft.scenario.name.clone(),
            }),
            ..draft
        });
    }
}

/// The four things a step can be, as a picker offers them.
///
/// Named apart from `Action` because a picker needs a choice with no contents:
/// you pick "send" before you have said what to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Send,
    Raw,
    Wait,
    WaitFor,
}

impl ActionKind {
    pub const ALL: [Self; 4] = [Self::Send, Self::Raw, Self::Wait, Self::WaitFor];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "send a frame",
            Self::Raw => "send raw bytes",
            Self::Wait => "wait",
            Self::WaitFor => "wait for a frame",
        }
    }

    #[must_use]
    pub fn of(action: &Action) -> Self {
        match action {
            Action::Send { .. } => Self::Send,
            Action::Raw { .. } => Self::Raw,
            Action::Wait { .. } => Self::Wait,
            Action::WaitFor { .. } => Self::WaitFor,
        }
    }

    /// Whether it acts on a link, and so needs one to exist.
    #[must_use]
    pub fn needs_a_connection(self) -> bool {
        self != Self::Wait
    }

    fn blank(self, frame: Option<&str>) -> Action {
        match self {
            Self::Send => Action::Send {
                frame: frame.unwrap_or_default().to_owned(),
                with: BTreeMap::new(),
                counters: BTreeMap::new(),
            },
            Self::Raw => Action::Raw { bytes: Vec::new() },
            Self::Wait => Action::Wait {
                delay: Duration::from_millis(100),
            },
            Self::WaitFor => Action::WaitFor {
                expect: Expect::Frame {
                    frame: frame.unwrap_or_default().to_owned(),
                    fields: Vec::new(),
                },
                timeout: Some(Duration::from_millis(500)),
            },
        }
    }
}

impl Draft {
    /// Adds a step at the end, of the simplest kind that can exist here.
    ///
    /// A delay when there is nowhere to send, since a step with no link would
    /// make the scenario unsavable.
    pub fn add_step(&mut self, links: &[ConnectionId], frame: Option<&str>) {
        let kind = if links.is_empty() {
            ActionKind::Wait
        } else {
            ActionKind::Raw
        };
        self.scenario.steps.push(Step {
            targets: targets_for(kind, links),
            action: kind.blank(frame),
        });
    }

    pub fn remove_step(&mut self, index: usize) {
        if index < self.scenario.steps.len() {
            self.scenario.steps.remove(index);
        }
    }

    /// Moves a step one place up or down, doing nothing at either end.
    pub fn move_step(&mut self, index: usize, down: bool) {
        let steps = &mut self.scenario.steps;
        let Some(other) = (if down {
            index.checked_add(1)
        } else {
            index.checked_sub(1)
        }) else {
            return;
        };
        if index < steps.len() && other < steps.len() {
            steps.swap(index, other);
        }
    }

    /// Turns a step into a different kind of step.
    ///
    /// Refused where it would produce something that cannot be saved: an action
    /// that acts on a link, in a project that has none.
    pub fn set_action(&mut self, index: usize, kind: ActionKind, links: &[ConnectionId]) {
        if kind.needs_a_connection() && links.is_empty() {
            return;
        }
        let Some(step) = self.scenario.steps.get_mut(index) else {
            return;
        };
        if ActionKind::of(&step.action) == kind {
            return;
        }

        // Whatever it was aimed at is worth keeping, a delay excepted, which
        // canonically aims at nothing.
        let kept: Vec<ConnectionId> = step.targets.clone();
        step.action = kind.blank(None);
        step.targets = if kind.needs_a_connection() {
            if kept.is_empty() {
                targets_for(kind, links)
            } else {
                kept
            }
        } else {
            Vec::new()
        };
    }
}

fn targets_for(kind: ActionKind, links: &[ConnectionId]) -> Vec<ConnectionId> {
    if kind.needs_a_connection() {
        links.first().cloned().into_iter().collect()
    } else {
        Vec::new()
    }
}

/// Starts or stops overriding one field of a `send` step.
///
/// Switching it on seeds the frame's own default, so the value shown is the one
/// that would go out anyway and the technician changes it from there rather
/// than from zero.
pub fn set_override(step: &mut Step, frame: &FrameDef, field: &str, on: bool) {
    let Action::Send { with, .. } = &mut step.action else {
        return;
    };
    if on {
        let seeded = seed_values(frame);
        if let Some(value) = seeded.get(field) {
            with.insert(field.to_owned(), value.clone());
        }
    } else {
        with.remove(field);
    }
}

/// Starts or stops counting one field of a `send` step up on every pass.
pub fn set_counter(step: &mut Step, field: &str, on: bool) {
    let Action::Send { counters, with, .. } = &mut step.action else {
        return;
    };
    if on {
        counters.insert(
            field.to_owned(),
            Counter {
                from: 0,
                step: 1,
                wrap: None,
            },
        );
        // A field cannot be both held at a value and counted up from it.
        with.remove(field);
    } else {
        counters.remove(field);
    }
}

/// Turns the ticked fields of a frame into what a wait watches for.
pub fn set_match(step: &mut Step, field: &str, on: bool) {
    let Action::WaitFor {
        expect: Expect::Frame { fields, .. },
        ..
    } = &mut step.action
    else {
        return;
    };
    if on {
        if !fields.iter().any(|held| held == field) {
            fields.push(field.to_owned());
        }
    } else {
        fields.retain(|held| held != field);
    }
}

/// Swaps a wait between naming a frame and spelling out bytes, keeping nothing
/// from the other form since the two have nothing in common.
pub fn set_wait_by_frame(step: &mut Step, by_frame: bool, frame: Option<&str>) {
    let Action::WaitFor { expect, .. } = &mut step.action else {
        return;
    };
    *expect = if by_frame {
        Expect::Frame {
            frame: frame.unwrap_or_default().to_owned(),
            fields: Vec::new(),
        }
    } else {
        Expect::Pattern {
            pattern: HexPattern::parse("00").unwrap_or_else(|| HexPattern::masked(&[0], &[true])),
            anchor: sim_core::Anchor::Anywhere,
        }
    };
}

/// Where a scenario that has never been saved goes by default: a file of its
/// own, named after it, in the folder the library was loaded from.
///
/// One file per scenario rather than one big one, so two technicians working on
/// different scenarios do not collide in the same file.
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
        if stem.is_empty() { "scenario" } else { &stem }
    ))
}

/// The file's text, or `None` where there is no file yet.
fn read_if_there(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
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
[[scenario]]
name = "Telemetry"
on = "bus"
repeat = { every_ms = 100 }
[[scenario.step]]
raw = "AA55"
"#;

    const BROKEN: &str = r#"
[[scenario]]
name = "Broken"
on = "bus"
[[scenario.step]]
send = "Thing"
wait_ms = 10
"#;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sim-scen-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn loaded(dir: &Path) -> ScenarioLibrary {
        let mut library = ScenarioLibrary::default();
        library.load_from(dir.to_path_buf());
        library
    }

    fn first_step_hex(scenario: &Scenario) -> String {
        match &scenario.steps[0].action {
            sim_core::scenario::Action::Raw { bytes } => {
                use std::fmt::Write as _;
                bytes.iter().fold(String::new(), |mut text, byte| {
                    let _ = write!(text, "{byte:02X}");
                    text
                })
            }
            other => panic!("expected a raw step, got {other:?}"),
        }
    }

    fn set_first_step(scenario: &mut Scenario, byte: u8) {
        scenario.steps[0].action = sim_core::scenario::Action::Raw { bytes: vec![byte] };
    }

    #[test]
    fn loads_what_it_can_and_reports_the_rest() {
        let dir = scratch("mixed");
        std::fs::write(dir.join("good.toml"), GOOD).unwrap();
        std::fs::write(dir.join("broken.toml"), BROKEN).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let library = loaded(&dir);

        // One bad file must not cost you the others.
        assert_eq!(library.entries.len(), 1);
        assert_eq!(library.entries[0].scenario.name, "Telemetry");
        // And each one remembers where it came from.
        assert_eq!(library.entries[0].file, dir.join("good.toml"));
        assert_eq!(library.failures.len(), 1);
        assert_eq!(library.failures[0].0, "broken.toml");
        assert!(library.failures[0].1.contains("several things"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_used_twice_keeps_the_first_and_says_so() {
        let dir = scratch("dupes");
        std::fs::write(dir.join("a.toml"), GOOD).unwrap();
        std::fs::write(dir.join("b.toml"), GOOD).unwrap();

        let library = loaded(&dir);
        assert_eq!(library.entries.len(), 1, "one row, not two identical ones");
        assert!(library.failures[0].1.contains("unique"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_draft_is_clean_until_it_is_touched_and_clean_again_once_saved() {
        let dir = scratch("dirty");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).unwrap();
        let mut library = loaded(&dir);

        library.begin_edit();
        assert!(!library.draft_is_dirty(), "a fresh copy matches the disk");

        set_first_step(&mut library.draft.as_mut().expect("editing").scenario, 0x01);
        assert!(library.draft_is_dirty());

        library.save_draft(&file).expect("should save");
        assert!(!library.draft_is_dirty(), "saved is clean again");

        // On disk, and in the list, without a reload.
        assert_eq!(first_step_hex(&library.entries[0].scenario), "01");
        assert_eq!(first_step_hex(&loaded(&dir).entries[0].scenario), "01");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_leaves_the_disk_and_the_list_alone() {
        let dir = scratch("cancel");
        std::fs::write(dir.join("good.toml"), GOOD).unwrap();
        let mut library = loaded(&dir);

        library.begin_edit();
        set_first_step(&mut library.draft.as_mut().expect("editing").scenario, 0xEE);
        library.cancel_edit();

        assert!(!library.draft_is_dirty());
        assert_eq!(first_step_hex(&library.entries[0].scenario), "AA55");
        assert_eq!(first_step_hex(&loaded(&dir).entries[0].scenario), "AA55");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn renaming_replaces_the_entry_rather_than_leaving_two() {
        let dir = scratch("rename");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).unwrap();
        let mut library = loaded(&dir);

        library.begin_edit();
        library.draft.as_mut().expect("editing").scenario.name = "Renamed".to_owned();
        library.save_draft(&file).expect("should save");

        assert_eq!(
            loaded(&dir)
                .entries
                .iter()
                .map(|entry| entry.scenario.name.as_str())
                .collect::<Vec<_>>(),
            ["Renamed"],
            "the old name is gone from the file, not left beside the new one"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_twice_edits_the_same_entry() {
        let dir = scratch("twice");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).unwrap();
        let mut library = loaded(&dir);

        library.begin_edit();
        for byte in [0x01, 0x02] {
            set_first_step(&mut library.draft.as_mut().expect("editing").scenario, byte);
            library.save_draft(&file).expect("should save");
        }

        let back = loaded(&dir);
        assert_eq!(back.entries.len(), 1, "one entry, not one per save");
        assert_eq!(first_step_hex(&back.entries[0].scenario), "02");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_scenario_lands_in_the_file_it_was_given() {
        let dir = scratch("new");
        let existing = dir.join("good.toml");
        std::fs::write(&existing, GOOD).unwrap();
        let mut library = loaded(&dir);

        let fresh = scenario::from_toml(
            r#"
[[scenario]]
name = "Fresh"
on = "bus"
[[scenario.step]]
raw = "01"
"#,
        )
        .expect("should parse")
        .remove(0);

        // Into a file that already holds one: appended, not overwritten.
        library.begin_new(fresh.clone());
        assert!(library.draft_is_dirty(), "never saved, so always dirty");
        library.save_draft(&existing).expect("should save");
        assert_eq!(loaded(&dir).entries.len(), 2);

        // And into a file that does not exist yet.
        let mut library = loaded(&dir);
        let mut other = fresh;
        other.name = "Elsewhere".to_owned();
        library.begin_new(other);
        library
            .save_draft(&dir.join("more.toml"))
            .expect("should save");

        let back = loaded(&dir);
        assert_eq!(back.entries.len(), 3);
        assert_eq!(
            back.entries
                .iter()
                .find(|entry| entry.scenario.name == "Elsewhere")
                .map(|entry| entry.file.clone()),
            Some(dir.join("more.toml"))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_already_taken_is_refused_before_the_disk_is_touched() {
        let dir = scratch("clash");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).unwrap();
        let mut library = loaded(&dir);

        let clashing = library.entries[0].scenario.clone();
        library.begin_new(clashing);

        let error = library.save_draft(&file).expect_err("should refuse");
        assert!(error.to_string().contains("already exists"), "{error}");
        // Nothing was written, so the file still holds exactly one.
        assert_eq!(loaded(&dir).entries.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    fn draft_of(text: &str) -> Draft {
        Draft {
            scenario: scenario::from_toml(text).expect("should parse").remove(0),
            origin: None,
        }
    }

    fn links(names: &[&str]) -> Vec<ConnectionId> {
        names.iter().map(|name| ConnectionId::from(*name)).collect()
    }

    const THREE: &str = r#"
[[scenario]]
name = "Three"
on = "bus"
[[scenario.step]]
raw = "01"
[[scenario.step]]
raw = "02"
[[scenario.step]]
raw = "03"
"#;

    fn order(draft: &Draft) -> Vec<String> {
        draft
            .scenario
            .steps
            .iter()
            .map(|step| match &step.action {
                Action::Raw { bytes } => format!("{:02X}", bytes[0]),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn steps_move_up_and_down_and_stop_at_the_ends() {
        let mut draft = draft_of(THREE);

        draft.move_step(0, true);
        assert_eq!(order(&draft), ["02", "01", "03"]);
        draft.move_step(2, false);
        assert_eq!(order(&draft), ["02", "03", "01"]);

        // Neither end wraps around, and neither panics.
        draft.move_step(0, false);
        draft.move_step(2, true);
        assert_eq!(order(&draft), ["02", "03", "01"]);
        draft.move_step(99, true);
        assert_eq!(order(&draft), ["02", "03", "01"]);
    }

    #[test]
    fn steps_are_added_and_removed() {
        let mut draft = draft_of(THREE);

        draft.remove_step(1);
        assert_eq!(order(&draft), ["01", "03"]);
        draft.remove_step(99);
        assert_eq!(order(&draft), ["01", "03"]);

        draft.add_step(&links(&["bus"]), None);
        assert_eq!(draft.scenario.steps.len(), 3);
        assert_eq!(
            draft.scenario.steps[2].targets,
            [ConnectionId::from("bus")],
            "a new step aims somewhere, or the scenario could not be saved"
        );

        // With nowhere to send, the only step that can be made is a delay.
        draft.add_step(&[], None);
        let last = draft.scenario.steps.last().expect("just added");
        assert_eq!(ActionKind::of(&last.action), ActionKind::Wait);
        assert!(last.targets.is_empty());
    }

    #[test]
    fn changing_what_a_step_does_keeps_where_it_points() {
        let mut draft = draft_of(THREE);
        draft.scenario.steps[0].targets = links(&["bus", "uart"]);

        draft.set_action(0, ActionKind::Send, &links(&["bus"]));
        assert_eq!(
            ActionKind::of(&draft.scenario.steps[0].action),
            ActionKind::Send
        );
        assert_eq!(draft.scenario.steps[0].targets, links(&["bus", "uart"]));

        // A delay aims at nothing, canonically, so it drops what it had.
        draft.set_action(0, ActionKind::Wait, &links(&["bus"]));
        assert!(draft.scenario.steps[0].targets.is_empty());

        // And coming back out of a delay it is given somewhere to go.
        draft.set_action(0, ActionKind::Raw, &links(&["bus"]));
        assert_eq!(draft.scenario.steps[0].targets, links(&["bus"]));
    }

    #[test]
    fn a_step_that_would_need_a_link_is_refused_when_there_are_none() {
        let mut draft = draft_of(THREE);
        draft.set_action(0, ActionKind::Wait, &[]);

        for kind in [ActionKind::Send, ActionKind::Raw, ActionKind::WaitFor] {
            draft.set_action(0, kind, &[]);
            assert_eq!(
                ActionKind::of(&draft.scenario.steps[0].action),
                ActionKind::Wait,
                "{kind:?} cannot be made without a connection to aim it at"
            );
        }
    }

    #[test]
    fn ticking_a_field_seeds_it_from_the_frame_rather_than_from_zero() {
        let frame = sim_core::frame::schema::from_toml(
            r#"
name = "Telemetry"
[[field]]
name = "sync"
type = "u16"
default = 0xAA55
[[field]]
name = "seq"
type = "u8"
"#,
        )
        .expect("should parse");

        let mut draft = draft_of(
            r#"
[[scenario]]
name = "S"
on = "bus"
[[scenario.step]]
send = "Telemetry"
"#,
        );
        let step = &mut draft.scenario.steps[0];

        set_override(step, &frame, "sync", true);
        let Action::Send { with, .. } = &step.action else {
            panic!("expected a send");
        };
        assert_eq!(with["sync"], Value::Uint(0xAA55), "the frame's own default");

        set_override(step, &frame, "sync", false);
        let Action::Send { with, .. } = &step.action else {
            panic!("expected a send");
        };
        assert!(with.is_empty(), "unticked means the frame decides again");

        // Counting a field and holding it at a value are exclusive.
        set_override(step, &frame, "seq", true);
        set_counter(step, "seq", true);
        let Action::Send { with, counters, .. } = &step.action else {
            panic!("expected a send");
        };
        assert!(!with.contains_key("seq"));
        assert!(counters.contains_key("seq"));
    }

    #[test]
    fn a_wait_swaps_between_naming_a_frame_and_spelling_out_bytes() {
        let mut draft = draft_of(
            r#"
[[scenario]]
name = "S"
on = "bus"
[[scenario.step]]
wait_for = { frame = "Telemetry", match = ["sync"] }
"#,
        );
        let step = &mut draft.scenario.steps[0];

        set_match(step, "mode", true);
        set_match(step, "sync", false);
        set_match(step, "mode", true);
        let Action::WaitFor {
            expect: Expect::Frame { fields, .. },
            ..
        } = &step.action
        else {
            panic!("expected a frame wait");
        };
        assert_eq!(fields, &["mode".to_owned()], "ticked once, listed once");

        set_wait_by_frame(step, false, None);
        assert!(matches!(
            &step.action,
            Action::WaitFor {
                expect: Expect::Pattern { .. },
                ..
            }
        ));
        set_wait_by_frame(step, true, Some("Status"));
        let Action::WaitFor {
            expect: Expect::Frame { frame, fields },
            ..
        } = &step.action
        else {
            panic!("expected a frame wait");
        };
        assert_eq!(frame, "Status");
        assert!(
            fields.is_empty(),
            "a different frame keeps none of the old fields"
        );
    }

    #[test]
    fn a_new_scenario_is_offered_a_file_named_after_it() {
        let dir = Path::new("/tmp/scenarios");
        assert_eq!(
            suggested_file(dir, "Telemetry 10 Hz"),
            dir.join("telemetry-10-hz.toml")
        );
        // Something with nothing usable in it still gets a name.
        assert_eq!(suggested_file(dir, "  "), dir.join("scenario.toml"));
    }

    #[test]
    fn deleting_takes_it_out_of_the_file_and_leaves_its_neighbour() {
        let dir = scratch("delete");
        let file = dir.join("both.toml");
        std::fs::write(
            &file,
            r#"
[[scenario]]
name = "Keep"
on = "bus"
[[scenario.step]]
raw = "AA"

[[scenario]]
name = "Drop"
on = "bus"
[[scenario.step]]
raw = "BB"
"#,
        )
        .expect("should write");
        let mut library = loaded(&dir);

        library.selected = library
            .entries
            .iter()
            .position(|entry| entry.scenario.name == "Drop");
        library.delete_selected().expect("should delete");

        assert_eq!(
            loaded(&dir)
                .entries
                .iter()
                .map(|entry| entry.scenario.name.as_str())
                .collect::<Vec<_>>(),
            ["Keep"]
        );
        // The selection lands somewhere valid rather than off the end.
        assert_eq!(library.selected, Some(0));

        std::fs::remove_dir_all(&dir).ok();
    }
}
