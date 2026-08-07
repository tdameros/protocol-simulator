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

    #[error("a scenario needs a name")]
    Unnamed,

    #[error("scenario {name} has no steps")]
    Empty { name: String },

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
    /// The link this acts on, already resolved from the scenario's default.
    pub connection: ConnectionId,
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
    /// Hold until a frame matching `pattern` arrives on the step's connection.
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
            Some(wrap) if wrap >= self.from => {
                let span = wrap - self.from + 1;
                self.from + advanced % span
            }
            _ => self.from.saturating_add(advanced),
        }
    }
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawFile {
    #[serde(default, rename = "scenario")]
    scenarios: Vec<RawScenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawScenario {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Connection every step acts on unless it says otherwise.
    #[serde(default, rename = "on", skip_serializing_if = "Option::is_none")]
    connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repeat: Option<RawRepeat>,
    #[serde(default, rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RawRepeat {
    every_ms: u64,
    /// Absent repeats until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    times: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RawStep {
    #[serde(default, rename = "on", skip_serializing_if = "Option::is_none")]
    connection: Option<String>,

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawWaitFor {
    #[serde(flatten)]
    pattern: PatternSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RawCounter {
    #[serde(default)]
    from: u64,
    #[serde(default = "one")]
    step: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wrap: Option<u64>,
}

fn one() -> u64 {
    1
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
        .as_deref()
        .map(str::trim)
        .filter(|connection| !connection.is_empty());
    let steps = raw
        .steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            build_step(step, default).map_err(|reason| ScenarioError::Step {
                name: name.clone(),
                // Counted from one: the file is read by people, and the first
                // step is the first one.
                step: index + 1,
                reason,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Scenario {
        name,
        description: raw.description,
        steps,
        repeat: raw.repeat.map(|repeat| Repeat {
            every: Duration::from_millis(repeat.every_ms),
            times: repeat.times,
        }),
    })
}

fn build_step(raw: RawStep, default: Option<&str>) -> Result<Step, StepError> {
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
        let (pattern, anchor) = wait
            .pattern
            .compile()
            .ok_or_else(|| StepError::BadPattern {
                hex: wait.pattern.hex.clone(),
            })?;
        Action::WaitFor {
            pattern,
            anchor,
            timeout: wait.timeout_ms.map(Duration::from_millis),
        }
    } else {
        return Err(StepError::Empty);
    };

    // A wait needs no link, but giving it the default costs nothing and keeps
    // every step answering the same question.
    let connection = raw
        .connection
        .as_deref()
        .map(str::trim)
        .filter(|connection| !connection.is_empty())
        .or(default)
        .ok_or(StepError::NoConnection)?;

    Ok(Step {
        connection: ConnectionId(connection.to_owned()),
        action,
    })
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

        assert_eq!(scenario.steps[0].connection, ConnectionId::from("bus"));
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
        assert_eq!(scenario.steps[2].connection, ConnectionId::from("uart"));
        assert!(matches!(
            &scenario.steps[2].action,
            Action::Raw { bytes } if bytes == &[0xDE, 0xAD, 0xBE, 0xEF]
        ));

        assert!(matches!(
            scenario.steps[3].action,
            Action::Wait { delay } if delay == Duration::from_millis(100)
        ));
        // And falls back to the default when it says nothing.
        assert_eq!(scenario.steps[3].connection, ConnectionId::from("bus"));
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
