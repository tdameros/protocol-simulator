//! Scenarios: an ordered list of things to do to a link, and when.
//!
//! Same split as `frame::schema`, and for the same reason: plain `Raw*` structs
//! mirror the file, the model they build is what the engine runs, and a file
//! that does not make sense is refused with a message naming the step rather
//! than with a deserialiser error.
//!
//! A scenario is one sequence. Concurrency comes from running several at once,
//! not from tracks inside one file, which keeps periodic emission from being a
//! second concept: a sender at 10 Hz is a one-step scenario that repeats.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::connection::ConnectionId;
use crate::frame::value::Value;
use crate::pattern::PatternSpec;

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid toml: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("cannot serialise the scenario: {0}")]
    Serialise(#[from] toml::ser::Error),

    #[error("cannot lay out the scenario: {0}")]
    Reparse(toml_edit::TomlError),

    #[error("a scenario needs a name")]
    Unnamed,

    #[error("scenario {name} has no steps")]
    Empty { name: String },

    #[error("scenario {name} repeats every 0 ms, which is no period at all")]
    ZeroPeriod { name: String },

    #[error("scenario {name}, step {step}: {reason}")]
    Step {
        name: String,
        step: usize,
        reason: StepError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("says nothing to do, expected one of send, raw, wait_ms or wait_for")]
    Empty,

    #[error("says to do several things at once, a step does one")]
    Ambiguous,

    #[error("no connection to act on, and the scenario names no default")]
    NoConnection,

    #[error("a delay acts on no connection, so `on` means nothing here")]
    PointlessConnection,

    #[error("{hex} is not a usable byte pattern")]
    BadPattern { hex: String },

    #[error("{hex} is not an even run of hex digits")]
    BadBytes { hex: String },
}

/// A scenario as the engine runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<Step>,
    /// How the whole sequence repeats. `None` runs it once.
    pub repeat: Option<Repeat>,
}

impl Scenario {
    /// Every frame name the scenario will need to encode.
    ///
    /// The engine is handed the definitions rather than sharing a library with
    /// the editor: a running scenario then keeps working on the frames it
    /// started with, whatever is edited on disk meanwhile.
    #[must_use]
    pub fn frames_used(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .steps
            .iter()
            .filter_map(|step| match &step.action {
                Action::Send { frame, .. } => Some(frame.as_str()),
                _ => None,
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repeat {
    /// Time between the start of one pass and the start of the next.
    ///
    /// Measured from start to start, not end to end, so a pass that takes
    /// longer than the period does not push the following ones later and later.
    pub every: Duration,
    /// `None` repeats until stopped.
    pub times: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The links this acts on, already resolved from the scenario's default.
    ///
    /// Empty only for a plain delay, which touches no link at all. Everything
    /// else has at least one, and never the same one twice: a step aimed at
    /// `["uart", "uart"]` would otherwise send everything in duplicate, and a
    /// wait would count one answer as two.
    pub targets: Vec<ConnectionId>,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Encode a frame from the library and send it.
    Send {
        frame: String,
        /// Field values overriding the frame's own defaults.
        with: BTreeMap<String, Value>,
        /// Fields that count up on every pass, by name.
        counters: BTreeMap<String, Counter>,
    },
    /// Send bytes as they are, for the malformed frame a definition cannot
    /// express.
    Raw {
        bytes: Vec<u8>,
    },
    Wait {
        delay: Duration,
    },
    /// Hold until a frame matching `pattern` has arrived on *every* target.
    ///
    /// All rather than the first: aimed at one link the two readings agree, and
    /// aimed at several the strict one is the one worth writing down, since it
    /// turns the timeout into a test that every side answered.
    WaitFor {
        pattern: crate::pattern::HexPattern,
        anchor: crate::pattern::Anchor,
        /// Giving up is a scenario failure, not a silent pass.
        timeout: Option<Duration>,
    },
}

/// A field that carries a different number on every pass.
///
/// The smallest thing that separates a simulation from a replay: a sequence
/// number appears in nearly every protocol, and a scenario that resends the
/// same bytes forever is not exercising the receiver's counter handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counter {
    pub from: u64,
    pub step: u64,
    /// Value after which it returns to `from`, inclusive.
    pub wrap: Option<u64>,
}

impl Counter {
    /// The value on pass `pass`, counting from zero.
    #[must_use]
    pub fn at(&self, pass: u64) -> u64 {
        let advanced = self.step.saturating_mul(pass);
        match self.wrap {
            // The span includes both ends, so `from = 0, wrap = 255` is a byte.
            Some(wrap) if wrap >= self.from => match (wrap - self.from).checked_add(1) {
                Some(span) => self.from + advanced % span,
                // The span is the whole of u64, so there is nothing to fold
                // back into: counting up is already staying inside it.
                None => self.from.wrapping_add(advanced),
            },
            _ => self.from.saturating_add(advanced),
        }
    }
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

// `deny_unknown_fields` throughout: a misspelt key used to be dropped in
// silence, so `repeatt = { every_ms = 100 }` gave a scenario that ran once and
// said nothing about why. Being refused by name is worth the strictness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default, rename = "scenario")]
    scenarios: Vec<RawScenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Connections every step acts on unless it says otherwise.
    #[serde(default, rename = "on", skip_serializing_if = "Option::is_none")]
    connection: Option<RawTargets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repeat: Option<RawRepeat>,
    #[serde(default, rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepeat {
    every_ms: u64,
    /// Absent repeats until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    times: Option<u32>,
}

/// One link or several, written whichever way reads better on the day.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum RawTargets {
    One(String),
    Many(Vec<String>),
}

impl RawTargets {
    /// The names it holds, trimmed, with the blank ones dropped.
    fn names(&self) -> Vec<&str> {
        match self {
            Self::One(name) => vec![name.trim()],
            Self::Many(names) => names.iter().map(|name| name.trim()).collect(),
        }
        .into_iter()
        .filter(|name| !name.is_empty())
        .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    #[serde(default, rename = "on", skip_serializing_if = "Option::is_none")]
    connection: Option<RawTargets>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    send: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    with: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    counters: BTreeMap<String, RawCounter>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait_ms: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait_for: Option<RawWaitFor>,
}

/// Spelt out rather than flattening a `PatternSpec`, because serde refuses to
/// police unknown keys on a struct that flattens another, and a typo here is
/// exactly what needs catching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWaitFor {
    hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCounter {
    #[serde(default, skip_serializing_if = "is_zero")]
    from: u64,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    step: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wrap: Option<u64>,
}

fn one() -> u64 {
    1
}

// Both take a reference because that is the signature serde's
// `skip_serializing_if` calls them with.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature is serde's, not ours"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature is serde's, not ours"
)]
fn is_one(value: &u64) -> bool {
    *value == 1
}

/// Parses every scenario in one file's text.
///
/// # Errors
///
/// Returns an error if the text is not valid TOML, or if a scenario is unnamed,
/// stepless, or holds a step that says nothing or several things at once.
pub fn from_toml(text: &str) -> Result<Vec<Scenario>, ScenarioError> {
    let raw: RawFile = toml::from_str(text)?;
    raw.scenarios.into_iter().map(build).collect()
}

/// # Errors
///
/// As [`from_toml`], plus an error if the file cannot be read.
pub fn load(path: &Path) -> Result<Vec<Scenario>, ScenarioError> {
    let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Read {
        path: path.display().to_string(),
        source,
    })?;
    from_toml(&text)
}

/// Renders scenarios back to TOML, as a whole file.
///
/// Used for a file the editor is creating. Changing one that already exists
/// goes through `writer`, which keeps the comments a hand-written file carries.
///
/// # Errors
///
/// Returns an error if the scenarios cannot be serialised.
pub fn to_toml(scenarios: &[Scenario]) -> Result<String, ScenarioError> {
    let file = RawFile {
        scenarios: scenarios.iter().map(lower).collect(),
    };
    // Two passes on purpose. `toml` decides where the sections go, which is
    // what puts each step under its own `[[scenario.step]]`; `toml_edit` then
    // does the cosmetics, which `toml` has no way of expressing.
    //
    // Serde gives every nested struct a section of its own, which turns a
    // three-word override into `[scenario.step.with]` three lines below the
    // step it belongs to, and leaves the step's own header standing empty. The
    // small ones read far better folded back onto one line, which is also how
    // a person writes them.
    let mut document: toml_edit::DocumentMut = toml::to_string_pretty(&file)?
        .parse()
        .map_err(ScenarioError::Reparse)?;
    if let Some(scenarios) = document["scenario"].as_array_of_tables_mut() {
        for scenario in scenarios.iter_mut() {
            fold(scenario, "repeat");
            let Some(steps) = scenario["step"].as_array_of_tables_mut() else {
                continue;
            };
            for step in steps.iter_mut() {
                for key in ["with", "counters", "wait_for"] {
                    fold(step, key);
                }
                compact(step, "on");
            }
            compact(scenario, "on");
        }
    }

    Ok(document.to_string())
}

/// Puts an array back on one line. Two connection names do not need four.
fn compact(table: &mut toml_edit::Table, key: &str) {
    if let Some(array) = table.get_mut(key).and_then(toml_edit::Item::as_array_mut) {
        array.fmt();
    }
}

/// Turns `table[key]`, if it is a section, into a value on one line.
fn fold(table: &mut toml_edit::Table, key: &str) {
    let Some(section) = table.remove(key) else {
        return;
    };
    let folded = match section {
        toml_edit::Item::Table(inner) => {
            toml_edit::Item::Value(toml_edit::Value::InlineTable(inner.into_inline_table()))
        }
        other => other,
    };
    table.insert(key, folded);
}

fn lower(scenario: &Scenario) -> RawScenario {
    // Steps carry their links resolved, so writing each one out would repeat
    // the same name down the whole file. Hoisting the commonest set into the
    // scenario's own `on` gives back a file shaped like one a person would
    // write, and says the same thing.
    let default = commonest_targets(scenario);

    RawScenario {
        name: scenario.name.clone(),
        description: scenario.description.clone(),
        connection: default.as_ref().map(|targets| lower_targets(targets)),
        repeat: scenario.repeat.map(|repeat| RawRepeat {
            every_ms: as_millis(repeat.every),
            times: repeat.times,
        }),
        steps: scenario
            .steps
            .iter()
            .map(|step| lower_step(step, default.as_deref()))
            .collect(),
    }
}

/// The target list most steps share, or `None` when no step has one.
fn commonest_targets(scenario: &Scenario) -> Option<Vec<ConnectionId>> {
    let mut tally: Vec<(&[ConnectionId], usize)> = Vec::new();
    for step in &scenario.steps {
        if step.targets.is_empty() {
            continue;
        }
        match tally.iter_mut().find(|(seen, _)| *seen == step.targets) {
            Some((_, count)) => *count += 1,
            None => tally.push((&step.targets, 1)),
        }
    }
    // First past the post on a tie, so the earliest in the file wins and the
    // output does not shuffle between runs.
    tally
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(targets, _)| targets.to_vec())
}

fn lower_targets(targets: &[ConnectionId]) -> RawTargets {
    match targets {
        [only] => RawTargets::One(only.0.clone()),
        many => RawTargets::Many(many.iter().map(|id| id.0.clone()).collect()),
    }
}

fn lower_step(step: &Step, default: Option<&[ConnectionId]>) -> RawStep {
    let mut raw = RawStep {
        // Written only where it differs from what the scenario already says.
        connection: (!step.targets.is_empty() && default != Some(step.targets.as_slice()))
            .then(|| lower_targets(&step.targets)),
        ..RawStep::default()
    };

    match &step.action {
        Action::Send {
            frame,
            with,
            counters,
        } => {
            raw.send = Some(frame.clone());
            raw.with = with.clone();
            raw.counters = counters
                .iter()
                .map(|(field, counter)| {
                    (
                        field.clone(),
                        RawCounter {
                            from: counter.from,
                            step: counter.step,
                            wrap: counter.wrap,
                        },
                    )
                })
                .collect();
        }
        Action::Raw { bytes } => {
            raw.raw = Some(
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        Action::Wait { delay } => raw.wait_ms = Some(as_millis(*delay)),
        Action::WaitFor {
            pattern,
            anchor,
            timeout,
        } => {
            raw.wait_for = Some(RawWaitFor {
                hex: pattern.to_hex(),
                at: anchor.offset(),
                timeout_ms: timeout.map(as_millis),
            });
        }
    }

    raw
}

fn as_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn build(raw: RawScenario) -> Result<Scenario, ScenarioError> {
    let name = raw.name.trim().to_owned();
    if name.is_empty() {
        return Err(ScenarioError::Unnamed);
    }
    if raw.steps.is_empty() {
        return Err(ScenarioError::Empty { name });
    }

    let default = raw
        .connection
        .as_ref()
        .map(RawTargets::names)
        .unwrap_or_default();
    let steps = raw
        .steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            build_step(step, &default).map_err(|reason| ScenarioError::Step {
                name: name.clone(),
                // Counted from one: the file is read by people, and the first
                // step is the first one.
                step: index + 1,
                reason,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let repeat = match raw.repeat {
        // A zero period is not a fast scenario, it is a scenario with no clock:
        // the timer it would build refuses to exist, and the task driving it
        // would die on the spot, taking its name with it.
        Some(repeat) if repeat.every_ms == 0 => return Err(ScenarioError::ZeroPeriod { name }),
        Some(repeat) => Some(Repeat {
            every: Duration::from_millis(repeat.every_ms),
            times: repeat.times,
        }),
        None => None,
    };

    Ok(Scenario {
        name,
        description: raw.description,
        steps,
        repeat,
    })
}

fn build_step(raw: RawStep, default: &[&str]) -> Result<Step, StepError> {
    let chosen = usize::from(raw.send.is_some())
        + usize::from(raw.raw.is_some())
        + usize::from(raw.wait_ms.is_some())
        + usize::from(raw.wait_for.is_some());
    match chosen {
        0 => return Err(StepError::Empty),
        1 => {}
        _ => return Err(StepError::Ambiguous),
    }

    let action = if let Some(frame) = raw.send {
        Action::Send {
            frame,
            with: raw.with,
            counters: raw
                .counters
                .into_iter()
                .map(|(field, counter)| {
                    (
                        field,
                        Counter {
                            from: counter.from,
                            step: counter.step,
                            wrap: counter.wrap,
                        },
                    )
                })
                .collect(),
        }
    } else if let Some(hex) = raw.raw {
        Action::Raw {
            bytes: parse_bytes(&hex).ok_or(StepError::BadBytes { hex })?,
        }
    } else if let Some(delay) = raw.wait_ms {
        Action::Wait {
            delay: Duration::from_millis(delay),
        }
    } else if let Some(wait) = raw.wait_for {
        let spec = PatternSpec {
            hex: wait.hex,
            at: wait.at,
        };
        let (pattern, anchor) = spec.compile().ok_or_else(|| StepError::BadPattern {
            hex: spec.hex.clone(),
        })?;
        Action::WaitFor {
            pattern,
            anchor,
            timeout: wait.timeout_ms.map(Duration::from_millis),
        }
    } else {
        return Err(StepError::Empty);
    };

    // A delay touches no link, so it neither needs one nor inherits the
    // scenario's. Saying so canonically is what lets a scenario be written back
    // out and read in again unchanged.
    if matches!(action, Action::Wait { .. }) {
        if raw.connection.is_some() {
            return Err(StepError::PointlessConnection);
        }
        return Ok(Step {
            targets: Vec::new(),
            action,
        });
    }

    let named = raw.connection.as_ref().map(RawTargets::names);
    let chosen = named.as_deref().unwrap_or(default);

    let mut targets: Vec<ConnectionId> = Vec::new();
    for name in chosen {
        let id = ConnectionId((*name).to_owned());
        if !targets.contains(&id) {
            targets.push(id);
        }
    }
    if targets.is_empty() {
        return Err(StepError::NoConnection);
    }

    Ok(Step { targets, action })
}

fn parse_bytes(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&cleaned[index..index + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: &str = r#"
[[scenario]]
name = "Boot then telemetry"
description = "Wakes the board up and checks it answers"
on = "bus"

[[scenario.step]]
send = "BootRequest"
with = { session = 1 }

[[scenario.step]]
wait_for = { hex = "AA 55 ?? 01", at = 0, timeout_ms = 500 }

[[scenario.step]]
raw = "DE AD BE EF"
on = "uart"

[[scenario.step]]
wait_ms = 100
"#;

    const TELEMETRY: &str = r#"
[[scenario]]
name = "Telemetry 10 Hz"
on = "bus"
repeat = { every_ms = 100 }

[[scenario.step]]
send = "Telemetry"
with = { speed = 120 }
counters = { seq = { wrap = 255 } }
"#;

    fn one(text: &str) -> Scenario {
        let mut scenarios = from_toml(text).expect("should parse");
        assert_eq!(scenarios.len(), 1);
        scenarios.remove(0)
    }

    #[test]
    fn a_sequence_reads_step_by_step() {
        let scenario = one(BOOT);
        assert_eq!(scenario.name, "Boot then telemetry");
        assert!(scenario.repeat.is_none(), "one pass unless asked");
        assert_eq!(scenario.steps.len(), 4);

        assert_eq!(scenario.steps[0].targets, [ConnectionId::from("bus")]);
        assert!(matches!(
            &scenario.steps[0].action,
            Action::Send { frame, with, .. }
                if frame == "BootRequest" && with["session"] == Value::Uint(1)
        ));

        assert!(matches!(
            &scenario.steps[1].action,
            Action::WaitFor { timeout: Some(t), .. } if *t == Duration::from_millis(500)
        ));

        // A step may act on another link than the scenario's default.
        assert_eq!(scenario.steps[2].targets, [ConnectionId::from("uart")]);
        assert!(matches!(
            &scenario.steps[2].action,
            Action::Raw { bytes } if bytes == &[0xDE, 0xAD, 0xBE, 0xEF]
        ));

        assert!(matches!(
            scenario.steps[3].action,
            Action::Wait { delay } if delay == Duration::from_millis(100)
        ));
        // A delay is the one step the default does not reach, having no link
        // to act on in the first place.
        assert!(scenario.steps[3].targets.is_empty());
    }

    #[test]
    fn a_periodic_sender_is_a_scenario_that_repeats() {
        let scenario = one(TELEMETRY);
        assert_eq!(
            scenario.repeat,
            Some(Repeat {
                every: Duration::from_millis(100),
                times: None,
            })
        );
        assert_eq!(scenario.steps.len(), 1);
        assert_eq!(scenario.frames_used(), ["Telemetry"]);
    }

    #[test]
    fn a_counter_advances_and_wraps() {
        let counter = Counter {
            from: 0,
            step: 1,
            wrap: Some(255),
        };
        assert_eq!(counter.at(0), 0);
        assert_eq!(counter.at(255), 255);
        // Inclusive on both ends, so a byte counter comes back to zero at 256.
        assert_eq!(counter.at(256), 0);

        let stepped = Counter {
            from: 10,
            step: 5,
            wrap: Some(24),
        };
        assert_eq!(
            (0..4).map(|pass| stepped.at(pass)).collect::<Vec<_>>(),
            [10, 15, 20, 10]
        );

        // Without a wrap it just climbs, and cannot overflow into a panic.
        let free = Counter {
            from: 0,
            step: u64::MAX,
            wrap: None,
        };
        assert_eq!(free.at(u64::MAX), u64::MAX);
    }

    #[test]
    fn a_counter_defaults_to_counting_by_one_from_zero() {
        let scenario = one(TELEMETRY);
        let Action::Send { counters, .. } = &scenario.steps[0].action else {
            panic!("expected a send");
        };
        assert_eq!(
            counters["seq"],
            Counter {
                from: 0,
                step: 1,
                wrap: Some(255)
            }
        );
    }

    #[test]
    fn a_step_can_aim_at_several_links_at_once() {
        let scenario = one(r#"
[[scenario]]
name = "Both buses"
on = ["uart", "udp"]

[[scenario.step]]
send = "Telemetry"

[[scenario.step]]
raw = "AA55"
on = "uart"

[[scenario.step]]
wait_for = { hex = "C0FE" }
on = ["udp", "uart", "udp"]
"#);

        // The scenario default reaches every step that says nothing.
        assert_eq!(
            scenario.steps[0].targets,
            [ConnectionId::from("uart"), ConnectionId::from("udp")]
        );
        // A single name still works, and still overrides the default.
        assert_eq!(scenario.steps[1].targets, [ConnectionId::from("uart")]);
        // Order is kept, and a name repeated by accident counted once: sending
        // twice down one link, or waiting for two answers from it, is never
        // what the list meant.
        assert_eq!(
            scenario.steps[2].targets,
            [ConnectionId::from("udp"), ConnectionId::from("uart")]
        );
    }

    #[test]
    fn an_empty_list_of_links_is_no_list_at_all() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Nowhere"
on = ["  "]
[[scenario.step]]
raw = "00"
"#,
        )
        .expect_err("a blank name is not a connection");
        assert!(error.to_string().contains("no connection"), "{error}");
    }

    #[test]
    fn a_step_that_says_nothing_names_itself() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Broken"
on = "bus"
[[scenario.step]]
send = "Telemetry"
[[scenario.step]]
"#,
        )
        .expect_err("an empty step is not a step");

        let message = error.to_string();
        assert!(message.contains("Broken"), "{message}");
        assert!(message.contains("step 2"), "{message}");
    }

    #[test]
    fn a_step_that_says_two_things_is_refused() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Greedy"
on = "bus"
[[scenario.step]]
send = "Telemetry"
wait_ms = 10
"#,
        )
        .expect_err("one step does one thing");
        assert!(error.to_string().contains("several things"), "{error}");
    }

    #[test]
    fn a_scenario_that_only_paces_itself_needs_no_link() {
        let scenario = one(r#"
[[scenario]]
name = "Just waiting"
[[scenario.step]]
wait_ms = 100
"#);
        assert!(
            scenario.steps[0].targets.is_empty(),
            "a delay touches nothing"
        );
    }

    /// What is written has to read back as the very same thing, or the editor
    /// would quietly reshape a scenario every time it saved one.
    fn round_trips(text: &str) -> Vec<Scenario> {
        let first = from_toml(text).expect("should parse");
        let written = to_toml(&first).expect("should serialise");
        let second = from_toml(&written)
            .unwrap_or_else(|error| panic!("what it wrote, it cannot read: {error}\n{written}"));
        assert_eq!(first, second, "through:\n{written}");
        second
    }

    #[test]
    fn everything_a_scenario_can_hold_survives_being_written_out() {
        round_trips(BOOT);
        round_trips(TELEMETRY);
        round_trips(&format!("{BOOT}\n{TELEMETRY}"));

        round_trips(
            r#"
[[scenario]]
name = "The lot"
description = "Every shape of step there is"
on = ["uart", "udp"]
repeat = { every_ms = 250, times = 7 }

[[scenario.step]]
send = "Telemetry"
with = { mode = 1, label = "hello", payload = "DEADBEEF", trim = -8, ratio = 1.5 }
counters = { seq = { from = 3, step = 5, wrap = 255 }, plain = {} }

[[scenario.step]]
raw = "AA 55 00 FF"
on = "uart"

[[scenario.step]]
wait_ms = 40

[[scenario.step]]
wait_for = { hex = "C0 ?? FE", at = 2, timeout_ms = 500 }

[[scenario.step]]
wait_for = { hex = "0102" }
on = ["udp"]
"#,
        );
    }

    #[test]
    fn what_it_writes_reads_like_something_a_person_wrote() {
        let written = to_toml(&from_toml(BOOT).expect("should parse")).expect("should serialise");

        // Each step under its own header, and the small tables on one line
        // rather than exiled into sections of their own below the step.
        assert!(written.contains("[[scenario.step]]"), "{written}");
        assert!(written.contains("with = { session = 1 }"), "{written}");
        assert!(
            written.contains(r#"wait_for = { hex = "AA 55 ?? 01", at = 0, timeout_ms = 500 }"#),
            "{written}"
        );
        assert!(
            !written.contains("[scenario.step.with]"),
            "no section for three words:\n{written}"
        );

        // And a couple of link names stay on one line too.
        let many = to_toml(
            &from_toml(
                r#"
[[scenario]]
name = "Both"
on = ["bus", "uart"]
[[scenario.step]]
raw = "00"
"#,
            )
            .expect("should parse"),
        )
        .expect("should serialise");
        assert!(many.contains(r#"on = ["bus", "uart"]"#), "{many}");
    }

    #[test]
    fn writing_hoists_the_link_most_steps_share() {
        let scenarios = from_toml(
            r#"
[[scenario]]
name = "Mostly one link"
on = "bus"
[[scenario.step]]
raw = "00"
[[scenario.step]]
raw = "01"
[[scenario.step]]
raw = "02"
on = "uart"
"#,
        )
        .expect("should parse");
        let written = to_toml(&scenarios).expect("should serialise");

        // Said once at the top, and only the odd one out repeats it.
        assert_eq!(written.matches(r#"on = "bus""#).count(), 1, "{written}");
        assert!(written.contains("uart"), "{written}");
    }

    #[test]
    fn a_delay_neither_takes_a_link_nor_is_given_one() {
        let scenario = one(r#"
[[scenario]]
name = "Paced"
on = "bus"
[[scenario.step]]
wait_ms = 10
[[scenario.step]]
raw = "00"
"#);
        // The scenario default reaches the send and stops at the delay, so
        // writing it back cannot invent a link the delay never had.
        assert!(scenario.steps[0].targets.is_empty());
        assert_eq!(scenario.steps[1].targets, [ConnectionId::from("bus")]);

        // And saying it outright is refused rather than quietly ignored.
        let error = from_toml(
            r#"
[[scenario]]
name = "Confused"
[[scenario.step]]
wait_ms = 10
on = "bus"
"#,
        )
        .expect_err("a delay acts on nothing");
        assert!(error.to_string().contains("means nothing"), "{error}");
    }

    #[test]
    fn a_period_of_zero_is_refused_rather_than_built() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Impossible"
on = "bus"
repeat = { every_ms = 0 }
[[scenario.step]]
raw = "00"
"#,
        )
        .expect_err("a zero period is no period");
        assert!(error.to_string().contains("Impossible"), "{error}");
    }

    #[test]
    fn a_counter_spanning_the_whole_width_just_counts() {
        // `wrap = u64::MAX` is how a full-width sequence number is written, and
        // its span is one larger than a u64 can hold.
        let full = Counter {
            from: 0,
            step: 1,
            wrap: Some(u64::MAX),
        };
        assert_eq!(full.at(0), 0);
        assert_eq!(full.at(3), 3);
        assert_eq!(full.at(u64::MAX), u64::MAX);

        // One short of it still folds, at the pass after its last value.
        let almost = Counter {
            from: 0,
            step: 1,
            wrap: Some(u64::MAX - 1),
        };
        assert_eq!(almost.at(u64::MAX - 1), u64::MAX - 1);
        assert_eq!(almost.at(u64::MAX), 0);
    }

    #[test]
    fn a_misspelt_setting_is_refused_by_name() {
        // The trap this exists for: silently ignored, this ran once instead of
        // repeating, and said nothing about why.
        let error = from_toml(
            r#"
[[scenario]]
name = "Typo"
on = "bus"
repeatt = { every_ms = 100 }
[[scenario.step]]
raw = "00"
"#,
        )
        .expect_err("repeatt is not repeat");
        assert!(error.to_string().contains("repeatt"), "{error}");

        for (label, text) in [
            (
                "step",
                r#"
[[scenario]]
name = "Typo"
on = "bus"
[[scenario.step]]
raw = "00"
delay_ms = 5
"#,
            ),
            (
                "wait_for",
                r#"
[[scenario]]
name = "Typo"
on = "bus"
[[scenario.step]]
wait_for = { hex = "AA55", timeout = 500 }
"#,
            ),
            (
                "counter",
                r#"
[[scenario]]
name = "Typo"
on = "bus"
[[scenario.step]]
send = "Thing"
counters = { seq = { wraps = 255 } }
"#,
            ),
        ] {
            assert!(
                from_toml(text).is_err(),
                "a stray key in a {label} should be refused"
            );
        }
    }

    #[test]
    fn a_step_with_nowhere_to_send_is_refused() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Homeless"
[[scenario.step]]
send = "Telemetry"
"#,
        )
        .expect_err("no default and no override");
        assert!(error.to_string().contains("no connection"), "{error}");
    }

    #[test]
    fn a_broken_pattern_is_caught_at_load_rather_than_at_run() {
        let error = from_toml(
            r#"
[[scenario]]
name = "Typo"
on = "bus"
[[scenario.step]]
wait_for = { hex = "AA 5" }
"#,
        )
        .expect_err("half a byte is not a pattern");
        assert!(error.to_string().contains("AA 5"), "{error}");
    }

    #[test]
    fn an_unnamed_or_stepless_scenario_is_refused() {
        assert!(matches!(
            from_toml("[[scenario]]\nname = \"  \"\n"),
            Err(ScenarioError::Unnamed)
        ));
        assert!(matches!(
            from_toml("[[scenario]]\nname = \"Nothing\"\n"),
            Err(ScenarioError::Empty { .. })
        ));
    }

    #[test]
    fn a_file_may_hold_several_scenarios() {
        let text = format!("{BOOT}\n{TELEMETRY}");
        let scenarios = from_toml(&text).expect("should parse");
        assert_eq!(scenarios.len(), 2);
        assert_eq!(scenarios[1].name, "Telemetry 10 Hz");
    }
}
