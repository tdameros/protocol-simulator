//! Scenario definitions loaded from a directory, and the one being edited.
//!
//! The files on disk stay the source of truth. The library only reads them, so
//! Reload picks up whatever a real editor has changed, and the panel writes
//! back through [`sim_core::scenario::update_in`], which leaves the comments a
//! developer wrote where they were.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};

use crate::engine_handle::EngineHandle;
use crate::state::Session;
use sim_core::frame::value::seed_values;
use sim_core::frame::{schema, FrameDef};
use sim_core::rule::{self as rules, Rule};
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

        // Never write something that cannot be read back: it would cost every
        // other scenario sharing the file, not just this one.
        if let Some(reason) = draft.problem() {
            bail!("{reason}");
        }

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
                from_capture: BTreeMap::new(),
                capture: BTreeMap::new(),
            },
            // One byte rather than none: an empty `raw` is not a step the
            // loader accepts, and a half-made step must never be unsavable.
            Self::Raw => Action::Raw { bytes: vec![0] },
            Self::Wait => Action::Wait {
                delay: Duration::from_millis(100),
            },
            Self::WaitFor => Action::WaitFor {
                expect: Expect::Frame {
                    frame: frame.unwrap_or_default().to_owned(),
                    values: BTreeMap::new(),
                    match_from_capture: BTreeMap::new(),
                    capture: BTreeMap::new(),
                },
                timeout: Some(Duration::from_millis(500)),
                on_timeout: sim_core::scenario::TimeoutPolicy::Fail,
            },
        }
    }
}

impl Draft {
    /// Why the draft could not be read back if it were written, if it could not.
    ///
    /// Checked by writing it out and reading it in again, rather than by
    /// listing the rules here: the loader is the authority on what a scenario
    /// may be, and a second copy of its rules would drift from it.
    ///
    /// This matters more than it looks. Scenarios load a file at a time, so one
    /// unreadable scenario takes every other scenario in that file down with it
    /// at the next Reload. Refusing to write is the only place that can be
    /// stopped.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        let scenario = std::slice::from_ref(&self.scenario);
        match scenario::to_toml(scenario) {
            Err(error) => Some(error.to_string()),
            Ok(text) => scenario::from_toml(&text)
                .err()
                .map(|error| error.to_string()),
        }
    }

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
            prune_dangling_captures(&mut self.scenario);
            // Rules name steps by number, so the ones after this one shift
            // down. A rule that read the step or wrote into it has nothing
            // left to refer to and goes with it, as a dangling capture does.
            let gone = index + 1;
            self.scenario.rules.retain_mut(|rule| {
                rule.remap_steps(|step| match step.cmp(&gone) {
                    std::cmp::Ordering::Less => Some(step),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(step - 1),
                })
            });
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
            prune_dangling_captures(&mut self.scenario);
            // A rule follows the steps it named, not their positions. One
            // that now reads a step after the one it fills is left for
            // `problem` to report rather than dropped: the move is undone by
            // moving back, a dropped rule is not.
            let (first, second) = (index + 1, other + 1);
            for rule in &mut self.scenario.rules {
                rule.remap_steps(|step| {
                    Some(if step == first {
                        second
                    } else if step == second {
                        first
                    } else {
                        step
                    })
                });
            }
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

/// Points a `send` step at another frame, dropping what belonged to the old one.
///
/// Overrides and counters name fields, and the fields of two frames have
/// nothing to do with each other. Kept, they would be invisible in the editor,
/// which only lists the new frame's fields, and would fail the step at run time
/// with a name nobody could see.
pub fn set_frame(step: &mut Step, name: &str) {
    let Action::Send {
        frame,
        with,
        counters,
        from_capture,
        ..
    } = &mut step.action
    else {
        return;
    };
    if frame == name {
        return;
    }
    name.clone_into(frame);
    with.clear();
    counters.clear();
    from_capture.clear();
    if let Action::Send { capture, .. } = &mut step.action {
        capture.clear();
    }
}

/// Starts or stops overriding one field of a `send` step.
///
/// Switching it on seeds the frame's own default, so the value shown is the one
/// that would go out anyway and the technician changes it from there rather
/// than from zero.
pub fn set_override(step: &mut Step, frame: &FrameDef, field: &str, on: bool) {
    let Action::Send {
        with, from_capture, ..
    } = &mut step.action
    else {
        return;
    };
    if on {
        let seeded = seed_values(frame);
        if let Some(value) = seeded.get(field) {
            with.insert(field.to_owned(), value.clone());
        }
        // A field cannot be held at a value and filled from a capture too.
        from_capture.remove(field);
    } else {
        with.remove(field);
    }
}

/// Starts or stops counting one field of a `send` step up on every pass.
pub fn set_counter(step: &mut Step, field: &str, on: bool) {
    let Action::Send {
        counters,
        with,
        from_capture,
        ..
    } = &mut step.action
    else {
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
        // A field cannot be both held at a value and counted up from it, or
        // filled from a capture and counted up from it.
        with.remove(field);
        from_capture.remove(field);
    } else {
        counters.remove(field);
    }
}

/// Starts or stops filling one field of a `send` step from a variable an
/// earlier step captured.
pub fn set_from_capture(step: &mut Step, field: &str, variable: Option<&str>) {
    let Action::Send {
        with,
        counters,
        from_capture,
        ..
    } = &mut step.action
    else {
        return;
    };
    match variable {
        Some(variable) => {
            from_capture.insert(field.to_owned(), variable.to_owned());
            // A field cannot be filled from a capture and also held at a
            // value or counted up from it.
            with.remove(field);
            counters.remove(field);
        }
        None => {
            from_capture.remove(field);
        }
    }
}

/// Starts or stops remembering one field from a sent frame. The saved value is
/// the final value on the wire, including counters and live overrides.
pub fn set_sent_capture(step: &mut Step, field: &str, on: bool) {
    let Action::Send { capture, .. } = &mut step.action else {
        return;
    };
    if on {
        capture.insert(field.to_owned(), field.to_owned());
    } else {
        capture.remove(field);
    }
}

/// Renames the variable a sent field is stored as.
pub fn rename_sent_capture(step: &mut Step, field: &str, name: &str) {
    let Action::Send { capture, .. } = &mut step.action else {
        return;
    };
    if let Some(held) = capture.get_mut(field) {
        name.clone_into(held);
    }
}

/// Starts or stops watching one field of a frame, seeded from the frame's own
/// default so that ticking a sync word is still a single click.
pub fn set_match(step: &mut Step, frame: &FrameDef, field: &str, on: bool) {
    let Action::WaitFor {
        expect:
            Expect::Frame {
                values,
                match_from_capture,
                ..
            },
        ..
    } = &mut step.action
    else {
        return;
    };
    if on {
        let seeded = seed_values(frame);
        if let Some(value) = seeded.get(field) {
            values.insert(field.to_owned(), value.clone());
        }
        match_from_capture.remove(field);
    } else {
        values.remove(field);
    }
}

/// Matches a reply field against a variable produced by an earlier step.
pub fn set_match_from_capture(step: &mut Step, field: &str, variable: Option<&str>) {
    let Action::WaitFor {
        expect:
            Expect::Frame {
                values,
                match_from_capture,
                ..
            },
        ..
    } = &mut step.action
    else {
        return;
    };
    match variable {
        Some(variable) => {
            match_from_capture.insert(field.to_owned(), variable.to_owned());
            values.remove(field);
        }
        None => {
            match_from_capture.remove(field);
        }
    }
}

/// Starts or stops remembering one field of a matched frame as a variable,
/// named after the field to start with and free to be renamed from there.
pub fn set_capture(step: &mut Step, field: &str, on: bool) {
    let Action::WaitFor {
        expect: Expect::Frame { capture, .. },
        ..
    } = &mut step.action
    else {
        return;
    };
    if on {
        capture.insert(field.to_owned(), field.to_owned());
    } else {
        capture.remove(field);
    }
}

/// Renames the variable a captured field is stored as.
pub fn rename_capture(step: &mut Step, field: &str, name: &str) {
    let Action::WaitFor {
        expect: Expect::Frame { capture, .. },
        ..
    } = &mut step.action
    else {
        return;
    };
    if let Some(held) = capture.get_mut(field) {
        name.clone_into(held);
    }
}

/// Every variable a step at `index` could read from, which is every one an
/// earlier step's `send` or `wait_for` captures.
///
/// Forward only, in file order: a run reaches step `index` having executed
/// every step before it and none after, so a name introduced later is not
/// there yet, whatever the file's own order of `[[scenario.step]]` sections
/// might suggest otherwise.
#[must_use]
pub fn captured_before(scenario: &Scenario, index: usize) -> Vec<String> {
    let mut names: Vec<String> = scenario
        .steps
        .iter()
        .take(index)
        .filter_map(|step| match &step.action {
            Action::Send { capture, .. }
            | Action::WaitFor {
                expect: Expect::Frame { capture, .. },
                ..
            } => Some(capture.values().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Drops a `from_capture` entry left pointing at a variable no step before it
/// captures any more, a step having been moved or removed out from under it.
///
/// Silent rather than reported: the field falls back to the frame's own
/// default, exactly as unticking its override by hand would leave it, so
/// there is nothing here for a save to refuse.
fn prune_dangling_captures(scenario: &mut Scenario) {
    let mut known: HashSet<String> = HashSet::new();
    for step in &mut scenario.steps {
        if let Action::Send { from_capture, .. } = &mut step.action {
            from_capture.retain(|_, variable| known.contains(variable));
        }
        if let Action::WaitFor {
            expect: Expect::Frame {
                match_from_capture, ..
            },
            ..
        } = &mut step.action
        {
            match_from_capture.retain(|_, variable| known.contains(variable));
        }
        if let Action::Send { capture, .. } = &step.action {
            known.extend(capture.values().cloned());
        }
        if let Action::WaitFor {
            expect: Expect::Frame { capture, .. },
            ..
        } = &step.action
        {
            known.extend(capture.values().cloned());
        }
    }
}

/// The steps before `target` a rule could read from, numbered from one: the
/// ones that send or wait for a frame, and wait on a single link.
#[must_use]
pub fn readable_before(scenario: &Scenario, target: usize) -> Vec<usize> {
    scenario
        .steps
        .iter()
        .enumerate()
        .take(target.saturating_sub(1))
        .filter(|(_, step)| match &step.action {
            Action::Send { .. } => true,
            Action::WaitFor {
                expect: Expect::Frame { .. },
                ..
            } => step.targets.len() == 1,
            _ => false,
        })
        .map(|(index, _)| index + 1)
        .collect()
}

/// A rule in one line, so a list of them reads without opening any.
#[must_use]
pub fn describe_rule(rule: &Rule, number: usize) -> String {
    let mut text = rule
        .name
        .clone()
        .unwrap_or_else(|| format!("rule {number}"));
    text.push(':');
    if let Some(condition) = &rule.when {
        let joiner = match condition.combine {
            rules::Combine::All => " and ",
            rules::Combine::Any => " or ",
        };
        let tests: Vec<String> = condition
            .tests
            .iter()
            .map(|test| format!("{} = {}", place(&test.source), value_text(&test.equals)))
            .collect();
        let _ = write!(text, " when {},", tests.join(joiner));
    }
    let target = place(&rules::Source {
        step: rule.set.step,
        field: rule.set.field.clone(),
        bit: rule.set.bit.clone(),
    });
    let _ = write!(text, " set {target}");
    match &rule.set.operand {
        rules::Operand::Constant(value) => {
            let _ = write!(text, " = {}", value_text(value));
        }
        rules::Operand::Copy { from, and, or } => {
            let _ = write!(text, " from {}", place(from));
            if let Some(mask) = and {
                let _ = write!(text, ", and {mask:#04X}");
            }
            if let Some(mask) = or {
                let _ = write!(text, ", or {mask:#04X}");
            }
        }
    }
    text
}

fn place(source: &rules::Source) -> String {
    match &source.bit {
        Some(bit) => format!("step {} {}.{bit}", source.step, source.field),
        None => format!("step {} {}", source.step, source.field),
    }
}

fn value_text(value: &sim_core::frame::value::Value) -> String {
    use sim_core::frame::value::Value;
    match value {
        Value::Uint(number) => number.to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Text(text) => format!("{text:?}"),
        Value::Bytes(bytes) => crate::hex::spaced(bytes),
        Value::Bits(_) => "bits".to_owned(),
    }
}

/// Appends a rule that is valid as it stands, for the editor to refine.
///
/// Returns whether there was somewhere to put one: it needs a `send` step with
/// a readable step before it, and a field of that send that is not counted or
/// filled from a capture.
pub fn add_rule(scenario: &mut Scenario, frames: &[FrameDef]) -> bool {
    let fields_of = |step: &Step| -> Vec<String> {
        rules::frame_of(step)
            .and_then(|name| frames.iter().find(|frame| frame.name == name))
            .map(|frame| {
                frame
                    .fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    };

    for target in (1..=scenario.steps.len()).rev() {
        let step = &scenario.steps[target - 1];
        let Action::Send {
            with,
            counters,
            from_capture,
            ..
        } = &step.action
        else {
            continue;
        };
        // A field the step sets from nowhere comes first, then one it only
        // fixes: a rule may replace a fixed value, but starting on a free
        // field leaves the step's own settings alone.
        let fields = fields_of(step);
        let Some(field) = fields
            .iter()
            .find(|field| {
                !with.contains_key(*field)
                    && !counters.contains_key(*field)
                    && !from_capture.contains_key(*field)
            })
            .or_else(|| {
                fields.iter().find(|field| {
                    !counters.contains_key(*field) && !from_capture.contains_key(*field)
                })
            })
            .cloned()
        else {
            continue;
        };
        let Some((from, read)) = readable_before(scenario, target)
            .into_iter()
            .rev()
            .find_map(|number| {
                let first = fields_of(&scenario.steps[number - 1]).into_iter().next()?;
                Some((number, first))
            })
        else {
            continue;
        };

        scenario.rules.push(Rule {
            name: None,
            when: None,
            set: rules::Assignment {
                step: target,
                field,
                bit: None,
                operand: rules::Operand::Copy {
                    from: rules::Source {
                        step: from,
                        field: read,
                        bit: None,
                    },
                    and: None,
                    or: None,
                },
            },
        });
        return true;
    }
    false
}

/// Why a rule cannot run against these frames, if it cannot.
#[must_use]
pub fn rule_problem(scenario: &Scenario, rule: &Rule, frames: &[FrameDef]) -> Option<String> {
    rules::check_against(rule, &scenario.steps, frames).err()
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
            values: BTreeMap::new(),
            match_from_capture: BTreeMap::new(),
            capture: BTreeMap::new(),
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

/// Writes the draft out, choosing a file for one that has never had a home.
pub fn save(state: &mut Session) {
    let Some(directory) = state.scenarios.directory.clone() else {
        state.last_error = Some("No scenarios folder to save into.".to_owned());
        return;
    };
    let name = state
        .scenarios
        .draft
        .as_ref()
        .map(|draft| draft.scenario.name.clone())
        .unwrap_or_default();

    let into = suggested_file(&directory, &name);
    if let Err(error) = state.scenarios.save_draft(&into) {
        state.last_error = Some(format!("{error:#}"));
    }
}

/// What New starts from: one delay, which is the only step that needs nothing
/// else to exist yet, since there may be no connection configured at all.
#[must_use]
pub fn blank() -> Scenario {
    Scenario {
        name: "New scenario".to_owned(),
        description: None,
        steps: vec![Step {
            targets: Vec::new(),
            action: Action::Wait {
                delay: Duration::from_millis(100),
            },
        }],
        rules: Vec::new(),
        repeat: None,
    }
}

/// Hands the scenario to the engine along with the definitions it will encode
/// against, so the run is unaffected by anything edited afterwards.
pub fn start(state: &mut Session, engine: &EngineHandle, scenario: &Scenario) {
    let wanted = scenario.frames_used();
    let frames: Vec<_> = state
        .frames
        .frames()
        .filter(|frame| wanted.contains(&frame.name.as_str()))
        .cloned()
        .collect();

    // Said here rather than left to fail on the step that needs it, since the
    // cause is a frames folder, not the scenario.
    let missing: Vec<&str> = wanted
        .into_iter()
        .filter(|name| !frames.iter().any(|frame| frame.name == *name))
        .collect();
    if !missing.is_empty() {
        state.last_error = Some(format!(
            "[{}] no frame loaded named {}",
            scenario.name,
            missing.join(", ")
        ));
        return;
    }

    // Connections get the same treatment. A scenario file is loaded on its own,
    // knowing nothing of the project's links, so a misspelt `on` used to sail
    // through and only show up as an error per send, once a second or once
    // every 10 ms, without stopping anything.
    let unknown = unknown_connections(state, scenario);
    if !unknown.is_empty() {
        state.last_error = Some(format!(
            "[{}] no connection named {}",
            scenario.name,
            unknown.join(", ")
        ));
        return;
    }

    engine.start_scenario(scenario.clone(), frames);
}

/// Names the scenario aims at that the project does not define, in the order
/// they first appear so the message points at the first line to fix.
#[must_use]
pub fn unknown_connections(state: &Session, scenario: &Scenario) -> Vec<String> {
    let mut unknown: Vec<String> = Vec::new();
    for target in scenario.steps.iter().flat_map(|step| &step.targets) {
        let known = state.connections.iter().any(|(id, _)| id == target);
        if !known && !unknown.contains(&target.0) {
            unknown.push(target.0.clone());
        }
    }
    unknown
}

/// What the scenario does, in one line, so the list is readable without opening
/// anything.
#[must_use]
pub fn shape(scenario: &Scenario) -> String {
    let steps = scenario.steps.len();
    let plural = if steps == 1 { "step" } else { "steps" };
    match scenario.repeat {
        None => format!("{steps} {plural}, once"),
        Some(repeat) => {
            let period = repeat.every.as_millis();
            match repeat.times {
                Some(times) => format!("{steps} {plural}, {times} times every {period} ms"),
                None => format!("{steps} {plural}, every {period} ms"),
            }
        }
    }
}

#[must_use]
pub fn describe(step: &Step) -> String {
    match &step.action {
        Action::Send {
            frame,
            with,
            counters,
            from_capture,
            capture,
        } => {
            let mut text = format!("send {frame}");
            if !with.is_empty() {
                let fields: Vec<&str> = with.keys().map(String::as_str).collect();
                let _ = write!(text, " with {}", fields.join(", "));
            }
            if !from_capture.is_empty() {
                let fields: Vec<&str> = from_capture.keys().map(String::as_str).collect();
                let _ = write!(text, " filling {} from capture", fields.join(", "));
            }
            if !capture.is_empty() {
                let fields: Vec<&str> = capture.keys().map(String::as_str).collect();
                let _ = write!(text, " saving {}", fields.join(", "));
            }
            if !counters.is_empty() {
                let fields: Vec<&str> = counters.keys().map(String::as_str).collect();
                let _ = write!(text, " counting {}", fields.join(", "));
            }
            text
        }
        Action::Raw { bytes } => {
            let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("send raw {}", hex.join(" "))
        }
        Action::Wait { delay } => format!("wait {} ms", delay.as_millis()),
        Action::WaitFor {
            expect, timeout, ..
        } => {
            let mut text = match expect {
                Expect::Frame {
                    frame,
                    values,
                    match_from_capture,
                    capture,
                } => {
                    let mut text = format!("wait for {frame}");
                    if !values.is_empty() {
                        let named: Vec<&str> = values.keys().map(String::as_str).collect();
                        let _ = write!(text, " matching {}", named.join(", "));
                    }
                    if !capture.is_empty() {
                        let named: Vec<&str> = capture.keys().map(String::as_str).collect();
                        let _ = write!(text, " capturing {}", named.join(", "));
                    }
                    if !match_from_capture.is_empty() {
                        let fields: Vec<&str> =
                            match_from_capture.keys().map(String::as_str).collect();
                        let _ = write!(text, " matching {} from capture", fields.join(", "));
                    }
                    text
                }
                Expect::Pattern { pattern, anchor } => {
                    let mut text = format!("wait for {}", pattern.to_hex());
                    if let Some(offset) = anchor.offset() {
                        let _ = write!(text, " at offset {offset}");
                    }
                    text
                }
            };
            if let Some(limit) = timeout {
                let _ = write!(text, ", giving up after {} ms", limit.as_millis());
            }
            text
        }
    }
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

    /// One way of breaking a draft, and what to call it.
    type Break = (&'static str, Box<dyn Fn(&mut Draft)>);

    /// Every way the editor can reach a scenario that would not load, all of
    /// which used to be written out and take the whole file down at the next
    /// Reload.
    #[test]
    fn a_draft_that_could_not_be_read_back_is_never_written() {
        let dir = scratch("unreadable");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).expect("should write");

        let cases: Vec<Break> = vec![
            (
                "a step with no bytes",
                Box::new(|draft: &mut Draft| {
                    draft.scenario.steps[0].action = Action::Raw { bytes: Vec::new() };
                }),
            ),
            (
                "a wait matching on nothing",
                Box::new(|draft: &mut Draft| {
                    draft.scenario.steps[0].action = Action::WaitFor {
                        expect: Expect::Frame {
                            frame: "Telemetry".to_owned(),
                            values: BTreeMap::new(),
                            match_from_capture: BTreeMap::new(),
                            capture: BTreeMap::new(),
                        },
                        timeout: None,
                        on_timeout: sim_core::scenario::TimeoutPolicy::Fail,
                    };
                }),
            ),
            (
                "no name at all",
                Box::new(|draft: &mut Draft| draft.scenario.name = String::new()),
            ),
            (
                "no steps at all",
                Box::new(|draft: &mut Draft| draft.scenario.steps.clear()),
            ),
        ];

        for (label, break_it) in cases {
            let mut library = loaded(&dir);
            library.begin_edit();
            break_it(library.draft.as_mut().expect("editing"));

            assert!(
                library.draft.as_ref().expect("editing").problem().is_some(),
                "{label} should be reported"
            );
            assert!(
                library.save_draft(&file).is_err(),
                "{label} should not reach the disk"
            );
            // And the file still holds what it held.
            assert_eq!(
                loaded(&dir).entries.len(),
                1,
                "{label} cost the file its contents"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_added_from_the_panel_can_always_be_saved() {
        let dir = scratch("addable");
        let file = dir.join("good.toml");
        std::fs::write(&file, GOOD).expect("should write");
        let mut library = loaded(&dir);

        library.begin_edit();
        library
            .draft
            .as_mut()
            .expect("editing")
            .add_step(&links(&["bus"]), None);

        assert_eq!(
            library.draft.as_ref().expect("editing").problem(),
            None,
            "a freshly added step must not be a scenario that cannot be saved"
        );
        library.save_draft(&file).expect("should save");
        assert_eq!(loaded(&dir).entries[0].scenario.steps.len(), 2);

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

    const RELAY: &str = r#"
[[scenario]]
name = "Relay"
[[scenario.step]]
wait_for = { frame = "Response", capture = { code = "server1_code" } }
on = "server1"
[[scenario.step]]
send = "Forward"
on = "server2"
from_capture = { payload = "server1_code" }
"#;

    fn from_capture_of(draft: &Draft, step: usize) -> BTreeMap<String, String> {
        let Action::Send { from_capture, .. } = &draft.scenario.steps[step].action else {
            panic!("expected a send");
        };
        from_capture.clone()
    }

    #[test]
    fn removing_a_capturing_step_drops_what_read_from_it() {
        let mut draft = draft_of(RELAY);
        assert_eq!(
            from_capture_of(&draft, 1).get("payload"),
            Some(&"server1_code".to_owned())
        );

        draft.remove_step(0);

        assert!(
            from_capture_of(&draft, 0).is_empty(),
            "the variable it named is gone with the step that captured it"
        );
    }

    #[test]
    fn moving_a_capturing_step_past_what_reads_it_drops_the_reference_too() {
        let mut draft = draft_of(RELAY);

        draft.move_step(0, true);

        assert!(
            from_capture_of(&draft, 0).is_empty(),
            "the capture now comes after the step that wanted to read it"
        );
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
        assert_eq!(last.targets, Vec::new());
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
        assert_eq!(draft.scenario.steps[0].targets, Vec::new());

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
    fn pointing_a_send_at_another_frame_drops_the_old_frames_fields() {
        let mut draft = draft_of(
            r#"
[[scenario]]
name = "S"
on = "bus"
[[scenario.step]]
send = "Telemetry"
with = { sync = 1 }
counters = { seq = { wrap = 255 } }
"#,
        );
        let step = &mut draft.scenario.steps[0];

        set_frame(step, "Status");
        let Action::Send {
            frame,
            with,
            counters,
            ..
        } = &step.action
        else {
            panic!("expected a send");
        };
        assert_eq!(frame, "Status");
        // Invisible in the editor if kept, and fatal at run time.
        assert_eq!(with.len(), 0);
        assert_eq!(counters.len(), 0);
    }

    #[test]
    fn a_wait_swaps_between_naming_a_frame_and_spelling_out_bytes() {
        let mut draft = draft_of(
            r#"
[[scenario]]
name = "S"
on = "bus"
[[scenario.step]]
wait_for = { frame = "Telemetry", match = { sync = 1 } }
"#,
        );
        let frame = sim_core::frame::schema::from_toml(
            r#"
name = "Telemetry"
[[field]]
name = "sync"
type = "u16"
default = 0xAA55
[[field]]
name = "mode"
type = "u8"
"#,
        )
        .expect("should parse");
        let step = &mut draft.scenario.steps[0];

        set_match(step, &frame, "mode", true);
        set_match(step, &frame, "sync", false);
        set_match(step, &frame, "mode", true);
        let Action::WaitFor {
            expect: Expect::Frame { values, .. },
            ..
        } = &step.action
        else {
            panic!("expected a frame wait");
        };
        assert_eq!(values.len(), 1, "ticked once, listed once");

        // Ticking seeds the frame's own value, so a sync word is right without
        // anyone typing it, and anything else can be changed from there.
        set_match(step, &frame, "sync", true);
        let Action::WaitFor {
            expect: Expect::Frame { values, .. },
            ..
        } = &step.action
        else {
            panic!("expected a frame wait");
        };
        assert_eq!(values["sync"], Value::Uint(0xAA55));

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
            expect: Expect::Frame { frame, values, .. },
            ..
        } = &step.action
        else {
            panic!("expected a frame wait");
        };
        assert_eq!(frame, "Status");
        assert!(
            values.is_empty(),
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

    use super::{describe, shape, unknown_connections};
    use crate::state::{ConnectionEntry, Session};
    use sim_core::scenario::Scenario;

    fn parse(text: &str) -> Scenario {
        sim_core::scenario::from_toml(text)
            .expect("should parse")
            .remove(0)
    }

    #[test]
    fn a_misspelt_connection_is_caught_before_anything_is_sent() {
        let mut state = Session::default();
        state.connections = vec![(
            sim_core::ConnectionId::from("bus"),
            ConnectionEntry {
                config: sim_core::TransportConfig::Udp {
                    bind: "127.0.0.1:9000".parse().expect("address"),
                    remote: "127.0.0.1:9001".parse().expect("address"),
                },
                status: sim_core::ConnectionStatus::Connected,
                retry: None,
                autoconnect: false,
            },
        )];

        let good = parse(
            r#"
[[scenario]]
name = "Fine"
on = "bus"
[[scenario.step]]
raw = "00"
"#,
        );
        assert_eq!(unknown_connections(&state, &good), Vec::<String>::new());

        let typo = parse(
            r#"
[[scenario]]
name = "Typo"
on = ["bus", "buss"]
[[scenario.step]]
raw = "00"
[[scenario.step]]
raw = "01"
on = "uart"
"#,
        );
        // Reported once each, in the order they appear, so the message points
        // at the first line to go and fix.
        assert_eq!(unknown_connections(&state, &typo), ["buss", "uart"]);

        // A delay names no link, so it can never be the reason for a refusal.
        let waiting = parse(
            r#"
[[scenario]]
name = "Waiting"
[[scenario.step]]
wait_ms = 10
"#,
        );
        assert_eq!(unknown_connections(&state, &waiting), Vec::<String>::new());
    }

    #[test]
    fn the_one_line_shape_says_how_often_it_runs() {
        let once = parse(
            r#"
[[scenario]]
name = "Boot"
on = "bus"
[[scenario.step]]
wait_ms = 5
"#,
        );
        assert_eq!(shape(&once), "1 step, once");

        let forever = parse(
            r#"
[[scenario]]
name = "Beat"
on = "bus"
repeat = { every_ms = 100 }
[[scenario.step]]
raw = "00"
[[scenario.step]]
wait_ms = 5
"#,
        );
        assert_eq!(shape(&forever), "2 steps, every 100 ms");

        let counted = parse(
            r#"
[[scenario]]
name = "Burst"
on = "bus"
repeat = { every_ms = 250, times = 10 }
[[scenario.step]]
raw = "00"
"#,
        );
        assert_eq!(shape(&counted), "1 step, 10 times every 250 ms");
    }

    #[test]
    fn every_kind_of_step_says_what_it_does() {
        let scenario = parse(
            r#"
[[scenario]]
name = "All of them"
on = "bus"
[[scenario.step]]
send = "Telemetry"
with = { mode = 1 }
counters = { seq = { wrap = 255 } }
[[scenario.step]]
raw = "AA 55"
[[scenario.step]]
wait_ms = 40
[[scenario.step]]
wait_for = { hex = "C0 FE", at = 2, timeout_ms = 500 }
[[scenario.step]]
wait_for = { frame = "Telemetry", match = { sync = 1, mode = 2 } }
"#,
        );

        let lines: Vec<String> = scenario.steps.iter().map(describe).collect();
        assert_eq!(lines[0], "send Telemetry with mode counting seq");
        assert_eq!(lines[1], "send raw AA 55");
        assert_eq!(lines[2], "wait 40 ms");
        assert_eq!(
            lines[3],
            "wait for C0 FE at offset 2, giving up after 500 ms"
        );
        assert_eq!(lines[4], "wait for Telemetry matching mode, sync");
    }

    const RULED: &str = r#"
[[scenario]]
name = "R"
on = "bus"

[[scenario.step]]
send = "Request"

[[scenario.step]]
wait_for = { frame = "Status", match = { sync = 1 }, timeout_ms = 500 }

[[scenario.step]]
wait_ms = 10

[[scenario.step]]
send = "Out"

[[scenario.step]]
send = "Out"

[[scenario.rule]]
name = "mirror"
when = { all = [{ step = 2, field = "state", bit = "fault", equals = 1 }] }
set = { step = 4, field = "flags", bit = "alarm", from = { step = 2, field = "state", bit = "fault" } }

[[scenario.rule]]
set = { step = 5, field = "speed", value = 3 }

[[scenario.rule]]
set = { step = 5, field = "mode", from = { step = 1, field = "id" }, and = 15, or = 128 }
"#;

    fn draft(text: &str) -> Draft {
        Draft {
            scenario: parse(text),
            origin: None,
        }
    }

    fn targets(draft: &Draft) -> Vec<usize> {
        draft
            .scenario
            .rules
            .iter()
            .map(|rule| rule.set.step)
            .collect()
    }

    #[test]
    fn removing_a_step_renumbers_the_rules_after_it() {
        let mut draft = draft(RULED);
        draft.remove_step(2); // the delay, step 3

        assert_eq!(targets(&draft), [3, 4, 4]);
        assert_eq!(draft.problem(), None, "still a scenario that loads");
    }

    #[test]
    fn removing_a_step_drops_the_rules_that_read_it() {
        let mut draft = draft(RULED);
        draft.remove_step(1); // the wait, step 2, which the first rule reads

        assert_eq!(draft.scenario.rules.len(), 2);
        assert_eq!(targets(&draft), [4, 4]);
        assert_eq!(draft.problem(), None);
    }

    #[test]
    fn removing_a_step_drops_the_rules_that_write_into_it() {
        let mut draft = draft(RULED);
        draft.remove_step(4); // the last send, step 5

        assert_eq!(draft.scenario.rules.len(), 1);
        assert_eq!(draft.scenario.rules[0].name.as_deref(), Some("mirror"));
    }

    #[test]
    fn moving_a_step_keeps_each_rule_on_the_step_it_named() {
        let mut draft = draft(RULED);
        draft.move_step(2, true); // the delay swaps with the send after it

        // The send that was step 4 is step 3 now, and the delay is step 4.
        assert_eq!(targets(&draft), [3, 5, 5]);
    }

    #[test]
    fn a_move_that_puts_a_source_after_its_target_is_reported_not_hidden() {
        let mut draft = draft(RULED);
        draft.move_step(1, true); // the wait is step 3 now, still before step 4
        assert_eq!(draft.problem(), None);
        draft.move_step(2, true); // and step 4 now, after the send it feeds

        let problem = draft.problem().expect("the rule now reads a later step");
        assert!(
            problem.contains("rule 1") && problem.contains("step 4"),
            "{problem}"
        );
    }

    #[test]
    fn only_steps_that_name_a_frame_before_the_target_can_be_read() {
        let scenario = parse(RULED);
        assert_eq!(readable_before(&scenario, 4), [1, 2]);
        assert_eq!(readable_before(&scenario, 5), [1, 2, 4]);
        assert_eq!(readable_before(&scenario, 1), Vec::<usize>::new());
    }

    #[test]
    fn a_rule_reads_in_one_line() {
        let scenario = parse(RULED);
        assert_eq!(
            describe_rule(&scenario.rules[0], 1),
            "mirror: when step 2 state.fault = 1, set step 4 flags.alarm from step 2 state.fault"
        );
        assert_eq!(
            describe_rule(&scenario.rules[1], 2),
            "rule 2: set step 5 speed = 3"
        );
        assert_eq!(
            describe_rule(&scenario.rules[2], 3),
            "rule 3: set step 5 mode from step 1 id, and 0x0F, or 0x80"
        );
    }

    fn frames_for_rules() -> Vec<FrameDef> {
        let parse = |text: &str| schema::from_toml(text).expect("a frame");
        vec![
            parse("name = \"Request\"\n[[field]]\nname = \"id\"\ntype = \"u8\"\n"),
            parse(
                "name = \"Status\"\n[[field]]\nname = \"sync\"\ntype = \"u8\"\n\
                 [[field]]\nname = \"state\"\ntype = \"u8\"\n",
            ),
            parse("name = \"Out\"\n[[field]]\nname = \"speed\"\ntype = \"u8\"\n"),
        ]
    }

    #[test]
    fn a_new_rule_starts_valid_and_on_a_free_field() {
        let mut scenario = parse(
            r#"
[[scenario]]
name = "R"
on = "bus"

[[scenario.step]]
wait_for = { frame = "Status", match = { sync = 1 }, timeout_ms = 500 }

[[scenario.step]]
send = "Out"
with = { speed = 1 }
"#,
        );
        let mut frames = frames_for_rules();
        frames[2] = schema::from_toml(
            "name = \"Out\"\n[[field]]\nname = \"speed\"\ntype = \"u8\"\n\
             [[field]]\nname = \"limit\"\ntype = \"u8\"\n",
        )
        .expect("a frame");

        assert!(add_rule(&mut scenario, &frames));

        let rule = &scenario.rules[0];
        assert_eq!(rule.set.step, 2);
        assert_eq!(rule.set.field, "limit", "speed is the step's own");
        let draft = Draft {
            scenario: scenario.clone(),
            origin: None,
        };
        assert_eq!(draft.problem(), None);
        assert_eq!(rule_problem(&scenario, rule, &frames), None);
    }

    #[test]
    fn a_new_rule_may_land_on_a_field_the_step_fixes_when_none_is_free() {
        let mut scenario = parse(
            r#"
[[scenario]]
name = "R"
on = "bus"

[[scenario.step]]
wait_for = { frame = "Status", match = { sync = 1 }, timeout_ms = 500 }

[[scenario.step]]
send = "Out"
with = { speed = 1 }
"#,
        );

        assert!(add_rule(&mut scenario, &frames_for_rules()));
        assert_eq!(scenario.rules[0].set.field, "speed");
    }

    #[test]
    fn there_is_no_rule_to_add_without_something_to_read_and_something_to_fill() {
        let mut only_sends = parse(
            "[[scenario]]\nname = \"R\"\non = \"bus\"\n\n[[scenario.step]]\nsend = \"Out\"\n",
        );
        assert!(!add_rule(&mut only_sends, &frames_for_rules()));
        assert!(only_sends.rules.is_empty());
    }

    #[test]
    fn a_rule_left_pointing_at_a_missing_field_is_reported() {
        let scenario = parse(RULED);
        let problem = rule_problem(&scenario, &scenario.rules[1], &frames_for_rules());
        assert_eq!(problem, None, "speed exists in Out");

        let problem = rule_problem(&scenario, &scenario.rules[0], &frames_for_rules())
            .expect("Out has no flags");
        assert!(problem.contains("flags"), "{problem}");
    }
}
