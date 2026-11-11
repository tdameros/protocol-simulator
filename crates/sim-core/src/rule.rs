//! Rules: a value seen at one step decides what another step sends.
//!
//! A capture moves a whole field from a reply into a later frame. A rule is
//! what a capture cannot say: send this field only if that bit came back set,
//! fill that bit from this one, keep the low nibble of what was received and
//! raise the top bit.
//!
//! Rules belong to a scenario and name steps by number, counted from one as
//! the file numbers them. A rule reads what an earlier step sent or received
//! in the current pass and writes into a later `send`, so nothing here needs
//! the clock: the runner calls [`apply`] while it encodes that later step.
//!
//! Decided against: a rule that rewrites bytes after encoding. It would have
//! to know every field's width and endianness, which is the encoder's job.
//! The rule edits the field values the encoder is about to be given instead.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::frame::value::{seed_values, FieldValues, Value};
use crate::frame::{FieldKind, FrameDef};
use crate::scenario::{Action, Expect, Step};

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("step {step} does not exist")]
    NoSuchStep { step: usize },

    #[error("writes into step {step}, which does not send a frame from the library")]
    TargetNotASend { step: usize },

    #[error("reads step {source_step}, which does not come before step {target}")]
    SourceNotEarlier { source_step: usize, target: usize },

    #[error("reads step {step}, which neither sends nor waits for a frame from the library")]
    SourceNotReadable { step: usize },

    #[error("reads step {step}, which waits on {targets} links; only one answer would be kept")]
    SourceNeedsOneTarget { step: usize, targets: usize },

    #[error("sets nothing, expected `from` or `value`")]
    NoValue,

    #[error("sets from a step and to a fixed value, a rule does one")]
    AmbiguousValue,

    #[error("`and` and `or` change what `from` reads, and mean nothing next to `value`")]
    MaskNeedsASource,

    #[error("`when` needs exactly one of `all` or `any`, with at least one test")]
    BadCondition,

    #[error(
        "writes {field} of step {step}, which that step counts or fills from a capture itself"
    )]
    TargetAlreadySet { step: usize, field: String },
}

/// One rule: when its condition holds, one field of a later `send` takes a
/// value read from an earlier step.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: Option<String>,
    /// Absent means the rule always applies.
    pub when: Option<Condition>,
    pub set: Assignment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub combine: Combine,
    pub tests: Vec<Test>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine {
    All,
    Any,
}

/// One comparison against something an earlier step sent or received.
#[derive(Debug, Clone, PartialEq)]
pub struct Test {
    pub source: Source,
    pub equals: Value,
}

/// A field of the frame an earlier step sent or received, or one bit of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub step: usize,
    pub field: String,
    pub bit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub step: usize,
    pub field: String,
    /// Writes one bit of a bitfield and leaves the others as they were.
    pub bit: Option<String>,
    pub operand: Operand,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// `(read AND and) OR or`, each mask optional.
    Copy {
        from: Source,
        and: Option<u64>,
        or: Option<u64>,
    },
    Constant(Value),
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<RawWhen>,
    set: RawSet,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhen {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    all: Vec<RawTest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    any: Vec<RawTest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTest {
    step: usize,
    field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bit: Option<String>,
    equals: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    step: usize,
    field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSet {
    step: usize,
    field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<RawSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    and: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    or: Option<u64>,
}

impl Rule {
    /// Every place this rule reads from, conditions first.
    fn sources(&self) -> impl Iterator<Item = &Source> {
        let tests = self
            .when
            .iter()
            .flat_map(|condition| condition.tests.iter().map(|test| &test.source));
        let copied = match &self.set.operand {
            Operand::Copy { from, .. } => Some(from),
            Operand::Constant(_) => None,
        };
        tests.chain(copied)
    }

    /// Renumbers every step this rule names, for a step that was moved or
    /// removed. Returns `false`, leaving the rule as it was, when `map` has no
    /// number for one of them: the rule then names a step that is gone.
    pub fn remap_steps(&mut self, map: impl Fn(usize) -> Option<usize>) -> bool {
        let Some(target) = map(self.set.step) else {
            return false;
        };
        let mut places: Vec<&mut usize> = Vec::new();
        if let Some(condition) = &mut self.when {
            places.extend(condition.tests.iter_mut().map(|test| &mut test.source.step));
        }
        if let Operand::Copy { from, .. } = &mut self.set.operand {
            places.push(&mut from.step);
        }
        let Some(mapped) = places
            .iter()
            .map(|place| map(**place))
            .collect::<Option<Vec<usize>>>()
        else {
            return false;
        };
        for (place, number) in places.into_iter().zip(mapped) {
            *place = number;
        }
        self.set.step = target;
        true
    }

    fn label(&self, number: usize) -> String {
        self.name.as_ref().map_or_else(
            || format!("rule {number}"),
            |name| format!("rule \"{name}\""),
        )
    }
}

/// # Errors
///
/// The rule's shape is wrong: no value or two, a mask without a source, a
/// condition that is neither `all` nor `any`.
pub(crate) fn build(raw: RawRule) -> Result<Rule, RuleError> {
    let when = raw.when.map(build_condition).transpose()?;
    let set = raw.set;
    let operand = match (set.from, set.value) {
        (None, None) => return Err(RuleError::NoValue),
        (Some(_), Some(_)) => return Err(RuleError::AmbiguousValue),
        (Some(from), None) => Operand::Copy {
            from: build_source(from),
            and: set.and,
            or: set.or,
        },
        (None, Some(value)) => {
            if set.and.is_some() || set.or.is_some() {
                return Err(RuleError::MaskNeedsASource);
            }
            Operand::Constant(value)
        }
    };
    Ok(Rule {
        name: raw.name,
        when,
        set: Assignment {
            step: set.step,
            field: set.field,
            bit: set.bit,
            operand,
        },
    })
}

fn build_condition(raw: RawWhen) -> Result<Condition, RuleError> {
    let (combine, tests) = match (raw.all.is_empty(), raw.any.is_empty()) {
        (false, true) => (Combine::All, raw.all),
        (true, false) => (Combine::Any, raw.any),
        _ => return Err(RuleError::BadCondition),
    };
    Ok(Condition {
        combine,
        tests: tests
            .into_iter()
            .map(|test| Test {
                source: Source {
                    step: test.step,
                    field: test.field,
                    bit: test.bit,
                },
                equals: test.equals,
            })
            .collect(),
    })
}

fn build_source(raw: RawSource) -> Source {
    Source {
        step: raw.step,
        field: raw.field,
        bit: raw.bit,
    }
}

pub(crate) fn lower(rule: &Rule) -> RawRule {
    let when = rule.when.as_ref().map(|condition| {
        let tests = condition
            .tests
            .iter()
            .map(|test| RawTest {
                step: test.source.step,
                field: test.source.field.clone(),
                bit: test.source.bit.clone(),
                equals: test.equals.clone(),
            })
            .collect();
        match condition.combine {
            Combine::All => RawWhen {
                all: tests,
                any: Vec::new(),
            },
            Combine::Any => RawWhen {
                all: Vec::new(),
                any: tests,
            },
        }
    });
    let (from, value, and, or) = match &rule.set.operand {
        Operand::Copy { from, and, or } => (
            Some(RawSource {
                step: from.step,
                field: from.field.clone(),
                bit: from.bit.clone(),
            }),
            None,
            *and,
            *or,
        ),
        Operand::Constant(value) => (None, Some(value.clone()), None, None),
    };
    RawRule {
        name: rule.name.clone(),
        when,
        set: RawSet {
            step: rule.set.step,
            field: rule.set.field.clone(),
            bit: rule.set.bit.clone(),
            from,
            value,
            and,
            or,
        },
    }
}

/// Checks every rule against the steps it names, before anything runs.
///
/// Only what the file alone can tell: which steps exist, which come first,
/// which can be read. Whether a field or a bit exists needs the frame
/// definitions, which a running scenario has and a file being loaded does not.
///
/// # Errors
///
/// The error carries the rule's number, counted from one.
pub(crate) fn check(rules: &[Rule], steps: &[Step]) -> Result<(), (usize, RuleError)> {
    for (index, rule) in rules.iter().enumerate() {
        check_one(rule, steps).map_err(|reason| (index + 1, reason))?;
    }
    Ok(())
}

fn check_one(rule: &Rule, steps: &[Step]) -> Result<(), RuleError> {
    let target = rule.set.step;
    let Some(Action::Send {
        counters,
        from_capture,
        ..
    }) = target
        .checked_sub(1)
        .and_then(|at| steps.get(at))
        .map(|step| &step.action)
    else {
        return Err(if (1..=steps.len()).contains(&target) {
            RuleError::TargetNotASend { step: target }
        } else {
            RuleError::NoSuchStep { step: target }
        });
    };
    let field = &rule.set.field;
    // A fixed value in `with` is only what the step sends when no rule says
    // otherwise. A counter or a capture is a source of its own, and two of
    // those on one field would leave it unsaid which one wins.
    if counters.contains_key(field) || from_capture.contains_key(field) {
        return Err(RuleError::TargetAlreadySet {
            step: target,
            field: field.clone(),
        });
    }

    for source in rule.sources() {
        let Some(read) = source.step.checked_sub(1).and_then(|at| steps.get(at)) else {
            return Err(RuleError::NoSuchStep { step: source.step });
        };
        if source.step >= target {
            return Err(RuleError::SourceNotEarlier {
                source_step: source.step,
                target,
            });
        }
        match &read.action {
            Action::Send { .. } => {}
            Action::WaitFor {
                expect: Expect::Frame { .. },
                ..
            } => {
                if read.targets.len() != 1 {
                    return Err(RuleError::SourceNeedsOneTarget {
                        step: source.step,
                        targets: read.targets.len(),
                    });
                }
            }
            _ => return Err(RuleError::SourceNotReadable { step: source.step }),
        }
    }
    Ok(())
}

/// Whether any rule reads what step `step` sent or received, which is what
/// tells the runner it has to keep the decoded values.
#[must_use]
pub fn reads(rules: &[Rule], step: usize) -> bool {
    rules
        .iter()
        .any(|rule| rule.sources().any(|source| source.step == step))
}

/// Lays every rule that writes into `step` over the values that step is about
/// to send, in the order the file lists them: a later rule on the same field
/// wins, which is how two conditions can pick between two values.
///
/// `seen` holds the decoded values of the steps already run in this pass.
///
/// # Errors
///
/// Names the rule and what it could not read or write: a value no earlier step
/// produced, a field or bit the frame does not have.
pub fn apply(
    rules: &[Rule],
    step: usize,
    frame: &FrameDef,
    seen: &BTreeMap<usize, FieldValues>,
    with: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        if rule.set.step != step {
            continue;
        }
        apply_one(rule, frame, seen, with)
            .map_err(|reason| format!("{}: {reason}", rule.label(index + 1)))?;
    }
    Ok(())
}

fn apply_one(
    rule: &Rule,
    frame: &FrameDef,
    seen: &BTreeMap<usize, FieldValues>,
    with: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    if let Some(condition) = &rule.when {
        if !holds(condition, seen)? {
            return Ok(());
        }
    }
    let value = match &rule.set.operand {
        Operand::Constant(value) => value.clone(),
        Operand::Copy { from, and, or } => masked(read(from, seen)?, from, *and, *or)?,
    };
    write(&rule.set, value, frame, with)
}

/// Every test is read, even once the answer is settled, so a test on a value
/// nothing produced is reported rather than hidden behind one that happened to
/// decide first.
fn holds(condition: &Condition, seen: &BTreeMap<usize, FieldValues>) -> Result<bool, String> {
    let outcomes = condition
        .tests
        .iter()
        .map(|test| Ok(same(&read(&test.source, seen)?, &test.equals)))
        .collect::<Result<Vec<bool>, String>>()?;
    Ok(match condition.combine {
        Combine::All => outcomes.iter().all(|held| *held),
        Combine::Any => outcomes.iter().any(|held| *held),
    })
}

fn describe(source: &Source) -> String {
    match &source.bit {
        Some(bit) => format!("{}.{bit} of step {}", source.field, source.step),
        None => format!("{} of step {}", source.field, source.step),
    }
}

fn read(source: &Source, seen: &BTreeMap<usize, FieldValues>) -> Result<Value, String> {
    let values = seen
        .get(&source.step)
        .ok_or_else(|| format!("step {} has no values to read this pass", source.step))?;
    let field = values
        .get(&source.field)
        .ok_or_else(|| format!("step {} has no field {}", source.step, source.field))?;
    let Some(bit) = &source.bit else {
        return Ok(field.clone());
    };
    match field {
        Value::Bits(bits) => bits.get(bit).map(|held| Value::Uint(*held)).ok_or_else(|| {
            format!(
                "{} has no bit named {bit}",
                describe(&Source {
                    bit: None,
                    ..source.clone()
                })
            )
        }),
        _ => Err(format!(
            "{} is not a bitfield",
            describe(&Source {
                bit: None,
                ..source.clone()
            })
        )),
    }
}

/// Numbers compare as numbers whichever way the file or the decoder typed
/// them, so `equals = 2` matches a decoded unsigned 2.
fn same(read: &Value, wanted: &Value) -> bool {
    match (whole_number(read), whole_number(wanted)) {
        (Some(left), Some(right)) => left == right,
        _ => read == wanted,
    }
}

fn whole_number(value: &Value) -> Option<i128> {
    match value {
        Value::Uint(number) => Some(i128::from(*number)),
        Value::Int(number) => Some(i128::from(*number)),
        _ => None,
    }
}

fn masked(value: Value, from: &Source, and: Option<u64>, or: Option<u64>) -> Result<Value, String> {
    if and.is_none() && or.is_none() {
        return Ok(value);
    }
    let bits = whole_number(&value)
        .and_then(|number| u64::try_from(number).ok())
        .ok_or_else(|| {
            format!(
                "{} is not an unsigned number, `and` and `or` cannot change it",
                describe(from)
            )
        })?;
    Ok(Value::Uint(
        (bits & and.unwrap_or(u64::MAX)) | or.unwrap_or(0),
    ))
}

fn write(
    set: &Assignment,
    value: Value,
    frame: &FrameDef,
    with: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    let Some(bit) = &set.bit else {
        with.insert(set.field.clone(), value);
        return Ok(());
    };
    let field = &set.field;
    let kind = frame
        .fields
        .iter()
        .find(|declared| declared.name == *field)
        .map(|declared| &declared.kind)
        .ok_or_else(|| format!("{} has no field named {field}", frame.name))?;
    let FieldKind::Bits { bits, .. } = kind else {
        return Err(format!("{field} is not a bitfield, it has no bit {bit}"));
    };
    if !bits.iter().any(|declared| declared.name == *bit) {
        return Err(format!("{field} has no bit named {bit}"));
    }
    let number = whole_number(&value)
        .and_then(|number| u64::try_from(number).ok())
        .ok_or_else(|| format!("{field}.{bit} needs an unsigned number"))?;

    let mut held = match with.get(field) {
        Some(Value::Bits(held)) => held.clone(),
        _ => match seed_values(frame).remove(field) {
            Some(Value::Bits(seeded)) => seeded,
            _ => BTreeMap::new(),
        },
    };
    held.insert(bit.clone(), number);
    with.insert(field.clone(), Value::Bits(held));
    Ok(())
}

/// Checks one rule against the frame definitions, which the file alone cannot
/// say: that each field it names exists and that each bit sits in a bitfield.
///
/// # Errors
///
/// Names the field or bit that is missing.
pub fn check_against(rule: &Rule, steps: &[Step], frames: &[FrameDef]) -> Result<(), String> {
    check_place(
        steps,
        frames,
        rule.set.step,
        &rule.set.field,
        rule.set.bit.as_deref(),
    )?;
    for source in rule.sources() {
        check_place(
            steps,
            frames,
            source.step,
            &source.field,
            source.bit.as_deref(),
        )?;
    }
    Ok(())
}

/// The frame a step sends or waits for, which is what its fields belong to.
#[must_use]
pub fn frame_of(step: &Step) -> Option<&str> {
    match &step.action {
        Action::Send { frame, .. }
        | Action::WaitFor {
            expect: Expect::Frame { frame, .. },
            ..
        } => Some(frame),
        _ => None,
    }
}

fn check_place(
    steps: &[Step],
    frames: &[FrameDef],
    step: usize,
    field: &str,
    bit: Option<&str>,
) -> Result<(), String> {
    let name = step
        .checked_sub(1)
        .and_then(|at| steps.get(at))
        .and_then(frame_of)
        .ok_or_else(|| format!("step {step} has no frame to read fields from"))?;
    let frame = frames
        .iter()
        .find(|declared| declared.name == name)
        .ok_or_else(|| format!("step {step} uses {name}, which is not defined"))?;
    let declared = frame
        .fields
        .iter()
        .find(|declared| declared.name == field)
        .ok_or_else(|| format!("{name} has no field named {field}"))?;
    let Some(bit) = bit else {
        return Ok(());
    };
    let FieldKind::Bits { bits, .. } = &declared.kind else {
        return Err(format!(
            "{name}.{field} is not a bitfield, it has no bit {bit}"
        ));
    };
    if bits.iter().any(|candidate| candidate.name == bit) {
        Ok(())
    } else {
        Err(format!("{name}.{field} has no bit named {bit}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(toml: &str) -> FrameDef {
        crate::frame::schema::from_toml(toml).expect("a frame")
    }

    fn flags_frame() -> FrameDef {
        frame(
            r#"
name = "Out"
[[field]]
name = "speed"
type = "u8"
[[field]]
name = "flags"
type = "bits"
repr = "u8"
bits = [
  { name = "alarm", width = 1 },
  { name = "link", width = 1 },
  { name = "spare", width = 6 },
]
"#,
        )
    }

    fn seen(entries: &[(usize, &str, Value)]) -> BTreeMap<usize, FieldValues> {
        let mut seen: BTreeMap<usize, FieldValues> = BTreeMap::new();
        for (step, field, value) in entries {
            seen.entry(*step)
                .or_default()
                .insert((*field).to_owned(), value.clone());
        }
        seen
    }

    fn source(step: usize, field: &str, bit: Option<&str>) -> Source {
        Source {
            step,
            field: field.to_owned(),
            bit: bit.map(str::to_owned),
        }
    }

    fn copy(from: Source, and: Option<u64>, or: Option<u64>) -> Operand {
        Operand::Copy { from, and, or }
    }

    fn rule(
        when: Option<Condition>,
        step: usize,
        field: &str,
        bit: Option<&str>,
        operand: Operand,
    ) -> Rule {
        Rule {
            name: None,
            when,
            set: Assignment {
                step,
                field: field.to_owned(),
                bit: bit.map(str::to_owned),
                operand,
            },
        }
    }

    fn bits(pairs: &[(&str, u64)]) -> Value {
        Value::Bits(pairs.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect())
    }

    #[test]
    fn a_field_is_copied_from_an_earlier_step() {
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            copy(source(1, "status", None), None, None),
        )];
        let mut with = BTreeMap::new();

        apply(
            &rules,
            4,
            &flags_frame(),
            &seen(&[(1, "status", Value::Uint(7))]),
            &mut with,
        )
        .expect("applies");

        assert_eq!(with["speed"], Value::Uint(7));
    }

    #[test]
    fn a_rule_for_another_step_leaves_the_values_alone() {
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            copy(source(1, "status", None), None, None),
        )];
        let mut with = BTreeMap::new();

        apply(
            &rules,
            3,
            &flags_frame(),
            &seen(&[(1, "status", Value::Uint(7))]),
            &mut with,
        )
        .expect("applies");

        assert!(with.is_empty(), "{with:?}");
    }

    #[test]
    fn a_mask_keeps_some_bits_and_raises_others() {
        // 0b1010_0110 AND 0x0F = 0b0110, OR 0x80 = 0b1000_0110.
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            copy(source(1, "raw", None), Some(0x0F), Some(0x80)),
        )];
        let mut with = BTreeMap::new();

        apply(
            &rules,
            4,
            &flags_frame(),
            &seen(&[(1, "raw", Value::Uint(0b1010_0110))]),
            &mut with,
        )
        .expect("applies");

        assert_eq!(with["speed"], Value::Uint(0b1000_0110));
    }

    #[test]
    fn one_bit_is_filled_from_another_and_the_rest_stay_put() {
        let rules = [rule(
            None,
            4,
            "flags",
            Some("alarm"),
            copy(source(1, "status", Some("fault")), None, None),
        )];
        let mut with = BTreeMap::from([(
            "flags".to_owned(),
            bits(&[("alarm", 0), ("link", 1), ("spare", 0)]),
        )]);

        apply(
            &rules,
            4,
            &flags_frame(),
            &seen(&[(1, "status", bits(&[("fault", 1)]))]),
            &mut with,
        )
        .expect("applies");

        assert_eq!(
            with["flags"],
            bits(&[("alarm", 1), ("link", 1), ("spare", 0)])
        );
    }

    #[test]
    fn a_bit_rule_starts_from_the_frames_defaults_when_the_step_sets_nothing() {
        let rules = [rule(
            None,
            4,
            "flags",
            Some("link"),
            Operand::Constant(Value::Uint(1)),
        )];
        let mut with = BTreeMap::new();

        apply(&rules, 4, &flags_frame(), &BTreeMap::new(), &mut with).expect("applies");

        assert_eq!(
            with["flags"],
            bits(&[("alarm", 0), ("link", 1), ("spare", 0)])
        );
    }

    #[test]
    fn two_rules_on_different_bits_of_one_field_both_land() {
        let rules = [
            rule(
                None,
                4,
                "flags",
                Some("alarm"),
                Operand::Constant(Value::Uint(1)),
            ),
            rule(
                None,
                4,
                "flags",
                Some("link"),
                Operand::Constant(Value::Uint(1)),
            ),
        ];
        let mut with = BTreeMap::new();

        apply(&rules, 4, &flags_frame(), &BTreeMap::new(), &mut with).expect("applies");

        assert_eq!(
            with["flags"],
            bits(&[("alarm", 1), ("link", 1), ("spare", 0)])
        );
    }

    fn all(tests: Vec<Test>) -> Condition {
        Condition {
            combine: Combine::All,
            tests,
        }
    }

    fn any(tests: Vec<Test>) -> Condition {
        Condition {
            combine: Combine::Any,
            tests,
        }
    }

    fn is(step: usize, field: &str, bit: Option<&str>, equals: u64) -> Test {
        Test {
            source: source(step, field, bit),
            equals: Value::Uint(equals),
        }
    }

    fn applied(when: Option<Condition>) -> bool {
        let rules = [rule(
            when,
            4,
            "speed",
            None,
            Operand::Constant(Value::Uint(9)),
        )];
        let mut with = BTreeMap::new();
        let seen = seen(&[
            (1, "mode", Value::Uint(2)),
            (1, "status", bits(&[("fault", 1), ("busy", 0)])),
        ]);
        apply(&rules, 4, &flags_frame(), &seen, &mut with).expect("applies");
        with.contains_key("speed")
    }

    #[test]
    fn all_needs_every_test_to_hold() {
        assert!(applied(Some(all(vec![
            is(1, "mode", None, 2),
            is(1, "status", Some("fault"), 1)
        ]))));
        assert!(!applied(Some(all(vec![
            is(1, "mode", None, 2),
            is(1, "status", Some("busy"), 1)
        ]))));
    }

    #[test]
    fn any_needs_one_test_to_hold() {
        assert!(applied(Some(any(vec![
            is(1, "mode", None, 3),
            is(1, "status", Some("fault"), 1)
        ]))));
        assert!(!applied(Some(any(vec![
            is(1, "mode", None, 3),
            is(1, "status", Some("busy"), 1)
        ]))));
    }

    #[test]
    fn a_signed_value_equals_the_same_unsigned_number() {
        let when = Some(Condition {
            combine: Combine::All,
            tests: vec![Test {
                source: source(1, "mode", None),
                equals: Value::Int(2),
            }],
        });
        assert!(applied(when));
    }

    #[test]
    fn a_value_nothing_produced_is_reported_by_step_and_field() {
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            copy(source(1, "missing", None), None, None),
        )];
        let error = apply(
            &rules,
            4,
            &flags_frame(),
            &seen(&[(1, "status", Value::Uint(1))]),
            &mut BTreeMap::new(),
        )
        .expect_err("nothing to read");
        assert!(
            error.contains("step 1") && error.contains("missing"),
            "{error}"
        );
    }

    #[test]
    fn a_condition_on_a_value_nothing_produced_is_reported_too() {
        let rules = [rule(
            Some(all(vec![is(2, "mode", None, 1)])),
            4,
            "speed",
            None,
            Operand::Constant(Value::Uint(9)),
        )];
        let error = apply(
            &rules,
            4,
            &flags_frame(),
            &BTreeMap::new(),
            &mut BTreeMap::new(),
        )
        .expect_err("nothing to test");
        assert!(error.contains("step 2"), "{error}");
    }

    #[test]
    fn a_bit_the_target_does_not_have_is_reported() {
        let rules = [rule(
            None,
            4,
            "flags",
            Some("nope"),
            Operand::Constant(Value::Uint(1)),
        )];
        let error = apply(
            &rules,
            4,
            &flags_frame(),
            &BTreeMap::new(),
            &mut BTreeMap::new(),
        )
        .expect_err("no such bit");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_bit_rule_on_a_field_that_is_not_a_bitfield_is_reported() {
        let rules = [rule(
            None,
            4,
            "speed",
            Some("alarm"),
            Operand::Constant(Value::Uint(1)),
        )];
        let error = apply(
            &rules,
            4,
            &flags_frame(),
            &BTreeMap::new(),
            &mut BTreeMap::new(),
        )
        .expect_err("not bits");
        assert!(error.contains("speed"), "{error}");
    }

    #[test]
    fn a_mask_on_something_that_is_not_a_number_is_reported() {
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            copy(source(1, "name", None), Some(0xF), None),
        )];
        let error = apply(
            &rules,
            4,
            &flags_frame(),
            &seen(&[(1, "name", Value::Text("abc".to_owned()))]),
            &mut BTreeMap::new(),
        )
        .expect_err("text has no bits");
        assert!(error.contains("name"), "{error}");
    }

    #[test]
    fn a_rule_replaces_a_value_the_step_already_fixes() {
        let rules = [rule(
            None,
            4,
            "speed",
            None,
            Operand::Constant(Value::Uint(9)),
        )];
        let mut with = BTreeMap::from([("speed".to_owned(), Value::Uint(1))]);

        apply(&rules, 4, &flags_frame(), &BTreeMap::new(), &mut with).expect("applies");

        assert_eq!(with["speed"], Value::Uint(9));
    }

    #[test]
    fn a_rule_changes_one_bit_of_a_fixed_bitfield_and_keeps_the_rest() {
        let rules = [rule(
            None,
            4,
            "flags",
            Some("alarm"),
            Operand::Constant(Value::Uint(1)),
        )];
        let mut with = BTreeMap::from([(
            "flags".to_owned(),
            bits(&[("alarm", 0), ("link", 1), ("spare", 5)]),
        )]);

        apply(&rules, 4, &flags_frame(), &BTreeMap::new(), &mut with).expect("applies");

        assert_eq!(
            with["flags"],
            bits(&[("alarm", 1), ("link", 1), ("spare", 5)])
        );
    }

    #[test]
    fn only_the_steps_a_rule_reads_are_worth_remembering() {
        let rules = [rule(
            Some(all(vec![is(2, "mode", None, 1)])),
            4,
            "speed",
            None,
            copy(source(1, "status", None), None, None),
        )];
        assert!(reads(&rules, 1));
        assert!(reads(&rules, 2));
        assert!(!reads(&rules, 3));
        assert!(!reads(&rules, 4), "writing is not reading");
    }

    #[test]
    fn building_refuses_a_rule_that_sets_nothing() {
        let raw: RawRule = toml::from_str("set = { step = 2, field = \"a\" }").expect("raw");
        assert!(matches!(build(raw), Err(RuleError::NoValue)));
    }

    #[test]
    fn building_refuses_a_rule_that_sets_two_things() {
        let raw: RawRule = toml::from_str(
            "set = { step = 2, field = \"a\", value = 1, from = { step = 1, field = \"b\" } }",
        )
        .expect("raw");
        assert!(matches!(build(raw), Err(RuleError::AmbiguousValue)));
    }

    #[test]
    fn building_refuses_a_mask_next_to_a_fixed_value() {
        let raw: RawRule =
            toml::from_str("set = { step = 2, field = \"a\", value = 1, and = 3 }").expect("raw");
        assert!(matches!(build(raw), Err(RuleError::MaskNeedsASource)));
    }

    #[test]
    fn building_refuses_a_condition_that_is_both_all_and_any() {
        let raw: RawRule = toml::from_str(
            "set = { step = 2, field = \"a\", value = 1 }\n\
             when = { all = [{ step = 1, field = \"b\", equals = 1 }], any = [{ step = 1, field = \"b\", equals = 1 }] }",
        )
        .expect("raw");
        assert!(matches!(build(raw), Err(RuleError::BadCondition)));
    }

    #[test]
    fn building_refuses_an_empty_condition() {
        let raw: RawRule =
            toml::from_str("set = { step = 2, field = \"a\", value = 1 }\nwhen = { all = [] }")
                .expect("raw");
        assert!(matches!(build(raw), Err(RuleError::BadCondition)));
    }

    fn ruled(rule: &str) -> (Vec<Rule>, Vec<Step>) {
        let text = format!(
            r#"
[[scenario]]
name = "R"
on = "bus"

[[scenario.step]]
wait_for = {{ frame = "Status", match = {{ sync = 1 }}, timeout_ms = 500 }}

[[scenario.step]]
send = "Out"

{rule}
"#
        );
        let scenario = crate::scenario::from_toml(&text)
            .expect("a scenario")
            .remove(0);
        (scenario.rules, scenario.steps)
    }

    fn definitions() -> Vec<FrameDef> {
        vec![
            flags_frame(),
            frame(
                r#"
name = "Status"
[[field]]
name = "sync"
type = "u8"
[[field]]
name = "state"
type = "bits"
repr = "u8"
bits = [{ name = "fault", width = 1 }, { name = "spare", width = 7 }]
"#,
            ),
        ]
    }

    fn judged(rule: &str) -> Result<(), String> {
        let (rules, steps) = ruled(rule);
        check_against(&rules[0], &steps, &definitions())
    }

    #[test]
    fn a_rule_naming_real_fields_and_bits_is_accepted() {
        judged(
            "[[scenario.rule]]\nwhen = { all = [{ step = 1, field = \"state\", bit = \"fault\", equals = 1 }] }\n\
             set = { step = 2, field = \"flags\", bit = \"alarm\", from = { step = 1, field = \"state\", bit = \"fault\" } }",
        )
        .expect("fine");
    }

    #[test]
    fn a_target_field_the_frame_lacks_is_named() {
        let error = judged("[[scenario.rule]]\nset = { step = 2, field = \"nope\", value = 1 }")
            .expect_err("no such field");
        assert!(error.contains("nope") && error.contains("Out"), "{error}");
    }

    #[test]
    fn a_target_bit_the_frame_lacks_is_named() {
        let error = judged(
            "[[scenario.rule]]\nset = { step = 2, field = \"flags\", bit = \"nope\", value = 1 }",
        )
        .expect_err("no such bit");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_bit_on_a_field_that_holds_no_bits_is_named() {
        let error = judged(
            "[[scenario.rule]]\nset = { step = 2, field = \"speed\", bit = \"alarm\", value = 1 }",
        )
        .expect_err("not a bitfield");
        assert!(error.contains("speed"), "{error}");
    }

    #[test]
    fn a_source_field_the_frame_lacks_is_named() {
        let error = judged(
            "[[scenario.rule]]\nset = { step = 2, field = \"speed\", from = { step = 1, field = \"nope\" } }",
        )
        .expect_err("no such field");
        assert!(
            error.contains("nope") && error.contains("Status"),
            "{error}"
        );
    }

    #[test]
    fn a_source_bit_the_frame_lacks_is_named() {
        let error = judged(
            "[[scenario.rule]]\nwhen = { all = [{ step = 1, field = \"state\", bit = \"nope\", equals = 1 }] }\n\
             set = { step = 2, field = \"speed\", value = 1 }",
        )
        .expect_err("no such bit");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_frame_nobody_defined_is_named() {
        let (rules, steps) =
            ruled("[[scenario.rule]]\nset = { step = 2, field = \"speed\", value = 1 }");
        let error = check_against(&rules[0], &steps, &[]).expect_err("no frames at all");
        assert!(error.contains("Out"), "{error}");
    }
}
