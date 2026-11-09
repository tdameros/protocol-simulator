//! Rules, drawn under the steps they connect.
//!
//! Same stance as the step editor: nothing is typed that could be picked. A
//! rule reads from steps that come before the one it fills, so those are the
//! only steps offered, and the fields and bits on offer are the ones the chosen
//! step's frame declares. What the loader refuses is mostly not buildable here,
//! and what only the frame definitions can say is shown on the rule itself.
//!
//! The meaning lives in [`sim_session::scenarios`] and [`sim_core::rule`].

use std::collections::HashSet;

use egui::{ComboBox, RichText, Ui};
use egui_phosphor::regular as icons;

use sim_core::frame::value::Value;
use sim_core::frame::{FieldDef, FieldKind, FrameDef, ScalarType};
use sim_core::rule::{Assignment, Combine, Condition, Operand, Rule, Source, Test};
use sim_core::scenario::{Action, Expect, Scenario};
use sim_session::scenarios;
use sim_session::state::Session;

use crate::panels::number;

const ERROR: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);

/// A change to the list of rules, which cannot happen while it is being drawn.
enum Edit {
    Add,
    Remove(usize),
    Move { index: usize, down: bool },
}

/// What the rules need to know about a step, copied out so the rules can be
/// edited while it is read.
struct StepInfo {
    label: String,
    frame: Option<String>,
    /// A rule may write into it: it sends a frame from the library.
    sends: bool,
    /// A rule may read from it, being before the rule's own step.
    readable: bool,
    /// Fields the step already sets by itself, which a rule cannot also set.
    taken: HashSet<String>,
}

fn catalogue(scenario: &Scenario) -> Vec<StepInfo> {
    let readable = scenarios::readable_before(scenario, scenario.steps.len() + 1);
    scenario
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let number = index + 1;
            let (label, taken) = match &step.action {
                Action::Send {
                    frame,
                    with,
                    counters,
                    from_capture,
                    ..
                } => (
                    format!("{number}. send {frame}"),
                    with.keys()
                        .chain(counters.keys())
                        .chain(from_capture.keys())
                        .cloned()
                        .collect(),
                ),
                Action::WaitFor {
                    expect: Expect::Frame { frame, .. },
                    ..
                } => (format!("{number}. wait for {frame}"), HashSet::new()),
                _ => (
                    format!("{number}. {}", scenarios::describe(step)),
                    HashSet::new(),
                ),
            };
            StepInfo {
                label,
                frame: sim_core::rule::frame_of(step).map(str::to_owned),
                sends: matches!(step.action, Action::Send { .. }),
                readable: readable.contains(&number),
                taken,
            }
        })
        .collect()
}

/// Everything a row needs to offer choices, gathered once per frame drawn.
struct Choices<'a> {
    steps: &'a [StepInfo],
    frames: &'a [FrameDef],
    hex: bool,
}

impl Choices<'_> {
    fn label(&self, step: usize) -> String {
        step.checked_sub(1)
            .and_then(|at| self.steps.get(at))
            .map_or_else(|| format!("{step}. (missing)"), |info| info.label.clone())
    }

    fn fields(&self, step: usize) -> &[FieldDef] {
        step.checked_sub(1)
            .and_then(|at| self.steps.get(at))
            .and_then(|info| info.frame.as_deref())
            .and_then(|name| self.frames.iter().find(|frame| frame.name == name))
            .map_or(&[], |frame| frame.fields.as_slice())
    }

    fn field(&self, step: usize, name: &str) -> Option<&FieldDef> {
        self.fields(step).iter().find(|field| field.name == name)
    }

    /// The width of one bit of a bitfield, which bounds what it can be set to.
    fn bit_width(&self, step: usize, field: &str, bit: &str) -> Option<u32> {
        match &self.field(step, field)?.kind {
            FieldKind::Bits { bits, .. } => bits
                .iter()
                .find(|candidate| candidate.name == bit)
                .map(|found| found.width),
            _ => None,
        }
    }
}

pub fn rules(ui: &mut Ui, state: &mut Session) {
    let frames: Vec<FrameDef> = state.frames.frames().cloned().collect();
    let hex = state.hex_values;
    let Some(draft) = state.scenarios.draft.as_mut() else {
        return;
    };

    ui.label(RichText::new("Rules").strong()).on_hover_text(
        "When what an earlier step sent or received matches, fill a field of a later send",
    );

    let steps = catalogue(&draft.scenario);
    let choices = Choices {
        steps: &steps,
        frames: &frames,
        hex,
    };
    // Worked out before the rules are borrowed mutably.
    let problems: Vec<Option<String>> = draft
        .scenario
        .rules
        .iter()
        .map(|rule| scenarios::rule_problem(&draft.scenario, rule, &frames))
        .collect();
    let can_add = scenarios::add_rule(&mut draft.scenario.clone(), &frames);
    let count = draft.scenario.rules.len();

    let mut pending: Option<Edit> = None;
    for (index, problem) in problems.iter().enumerate() {
        ui.push_id(index, |ui| {
            ui.group(|ui| {
                header(ui, &mut draft.scenario.rules, index, count, &mut pending);
                let rule = &mut draft.scenario.rules[index];
                when_rows(ui, rule, &choices);
                set_rows(ui, rule, &choices);
                if let Some(reason) = problem {
                    ui.colored_label(ERROR, reason);
                }
            });
        });
    }

    if ui
        .add_enabled(
            can_add,
            egui::Button::new(format!("{} Add a rule", icons::PLUS)),
        )
        .on_disabled_hover_text("A rule needs a send step with a frame step before it")
        .clicked()
    {
        pending = Some(Edit::Add);
    }

    let Some(edit) = pending else {
        return;
    };
    match edit {
        Edit::Add => {
            scenarios::add_rule(&mut draft.scenario, &frames);
        }
        Edit::Remove(index) => {
            draft.scenario.rules.remove(index);
        }
        Edit::Move { index, down } => {
            let other = if down {
                index + 1
            } else {
                index.saturating_sub(1)
            };
            if other < draft.scenario.rules.len() {
                draft.scenario.rules.swap(index, other);
            }
        }
    }
}

fn header(ui: &mut Ui, rules: &mut [Rule], index: usize, count: usize, pending: &mut Option<Edit>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Rule {}", index + 1)).weak());
        let mut name = rules[index].name.clone().unwrap_or_default();
        if ui
            .add(egui::TextEdit::singleline(&mut name).hint_text("name (optional)"))
            .changed()
        {
            rules[index].name = (!name.trim().is_empty()).then_some(name);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(icons::TRASH).on_hover_text("Remove").clicked() {
                *pending = Some(Edit::Remove(index));
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new(icons::ARROW_DOWN))
                .on_hover_text("Later rules win over earlier ones on the same field")
                .clicked()
            {
                *pending = Some(Edit::Move { index, down: true });
            }
            if ui
                .add_enabled(index > 0, egui::Button::new(icons::ARROW_UP))
                .clicked()
            {
                *pending = Some(Edit::Move { index, down: false });
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Always,
    All,
    Any,
}

impl Mode {
    fn of(rule: &Rule) -> Self {
        match rule.when.as_ref().map(|condition| condition.combine) {
            None => Self::Always,
            Some(Combine::All) => Self::All,
            Some(Combine::Any) => Self::Any,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::All => "when all of these hold",
            Self::Any => "when any of these holds",
        }
    }
}

fn when_rows(ui: &mut Ui, rule: &mut Rule, choices: &Choices) {
    let target = rule.set.step;
    let candidates = candidates_before(choices, target);
    let current = Mode::of(rule);
    let mut mode = current;

    ui.horizontal(|ui| {
        ComboBox::from_id_salt("mode")
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for option in [Mode::Always, Mode::All, Mode::Any] {
                    let usable = option == Mode::Always || !candidates.is_empty();
                    ui.add_enabled_ui(usable, |ui| {
                        ui.selectable_value(&mut mode, option, option.label());
                    });
                }
            });
    });

    if mode != current {
        rule.when = match mode {
            Mode::Always => None,
            Mode::All | Mode::Any => {
                let combine = if mode == Mode::All {
                    Combine::All
                } else {
                    Combine::Any
                };
                let tests = rule
                    .when
                    .take()
                    .map(|held| held.tests)
                    .filter(|held| !held.is_empty())
                    .or_else(|| default_test(choices, &candidates).map(|test| vec![test]))
                    .unwrap_or_default();
                Some(Condition { combine, tests })
            }
        };
    }

    let Some(condition) = &mut rule.when else {
        return;
    };
    let mut removed: Option<usize> = None;
    ui.indent("tests", |ui| {
        for (at, test) in condition.tests.iter_mut().enumerate() {
            ui.push_id(at, |ui| {
                ui.horizontal_wrapped(|ui| {
                    source_row(ui, &mut test.source, &candidates, choices);
                    ui.label("=");
                    value_editor(ui, choices, &test.source, &mut test.equals);
                    if ui
                        .button(icons::X)
                        .on_hover_text("Remove this test")
                        .clicked()
                    {
                        removed = Some(at);
                    }
                });
            });
        }
        if ui
            .add_enabled(
                !candidates.is_empty(),
                egui::Button::new(format!("{} Add a test", icons::PLUS)),
            )
            .clicked()
        {
            if let Some(test) = default_test(choices, &candidates) {
                condition.tests.push(test);
            }
        }
    });
    // Removing the only test turns the condition off: a condition with no test
    // says nothing, and "always" is how a rule says that.
    if let Some(at) = removed {
        if condition.tests.len() > 1 {
            condition.tests.remove(at);
        } else {
            rule.when = None;
        }
    }
}

fn set_rows(ui: &mut Ui, rule: &mut Rule, choices: &Choices) {
    let sends: Vec<usize> = (1..=choices.steps.len())
        .filter(|number| choices.steps[number - 1].sends)
        .collect();

    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("set").weak());
        target_row(ui, &mut rule.set, &sends, choices);
    });

    let candidates = candidates_before(choices, rule.set.step);
    let copying = matches!(rule.set.operand, Operand::Copy { .. });
    let mut wants_copy = copying;
    ui.horizontal(|ui| {
        ui.label(RichText::new("to").weak());
        ComboBox::from_id_salt("operand")
            .selected_text(if copying {
                "a value read from a step"
            } else {
                "a fixed value"
            })
            .show_ui(ui, |ui| {
                ui.add_enabled_ui(!candidates.is_empty(), |ui| {
                    ui.selectable_value(&mut wants_copy, true, "a value read from a step");
                });
                ui.selectable_value(&mut wants_copy, false, "a fixed value");
            });
    });
    if wants_copy != copying {
        rule.set.operand = if wants_copy {
            default_source(choices, &candidates).map_or(Operand::Constant(Value::Uint(0)), |from| {
                Operand::Copy {
                    from,
                    and: None,
                    or: None,
                }
            })
        } else {
            Operand::Constant(Value::Uint(0))
        };
    }

    let target = Source {
        step: rule.set.step,
        field: rule.set.field.clone(),
        bit: rule.set.bit.clone(),
    };
    match &mut rule.set.operand {
        Operand::Constant(value) => {
            ui.indent("constant", |ui| value_editor(ui, choices, &target, value));
        }
        Operand::Copy { from, and, or } => {
            ui.indent("copy", |ui| {
                ui.horizontal_wrapped(|ui| {
                    source_row(ui, from, &candidates, choices);
                });
                ui.horizontal(|ui| {
                    mask(ui, "and", and);
                    mask(ui, "or", or);
                });
            });
        }
    }
}

/// The steps before `target` a rule could read, numbered from one.
fn candidates_before(choices: &Choices, target: usize) -> Vec<usize> {
    (1..target)
        .filter(|number| {
            choices
                .steps
                .get(number - 1)
                .is_some_and(|info| info.readable)
        })
        .collect()
}

fn default_source(choices: &Choices, candidates: &[usize]) -> Option<Source> {
    candidates.iter().rev().find_map(|step| {
        let field = choices.fields(*step).first()?;
        Some(Source {
            step: *step,
            field: field.name.clone(),
            bit: None,
        })
    })
}

fn default_test(choices: &Choices, candidates: &[usize]) -> Option<Test> {
    default_source(choices, candidates).map(|source| Test {
        source,
        equals: Value::Uint(0),
    })
}

/// Step, field and, for a bitfield, bit: three lists that narrow one another.
fn source_row(ui: &mut Ui, source: &mut Source, candidates: &[usize], choices: &Choices) {
    ComboBox::from_id_salt("step")
        .selected_text(choices.label(source.step))
        .show_ui(ui, |ui| {
            for step in candidates {
                if ui
                    .selectable_label(source.step == *step, choices.label(*step))
                    .clicked()
                    && source.step != *step
                {
                    source.step = *step;
                    source.field = choices
                        .fields(*step)
                        .first()
                        .map(|field| field.name.clone())
                        .unwrap_or_default();
                    source.bit = None;
                }
            }
        });
    field_and_bit(
        ui,
        source.step,
        &mut source.field,
        &mut source.bit,
        choices,
        &HashSet::new(),
    );
}

fn target_row(ui: &mut Ui, set: &mut Assignment, sends: &[usize], choices: &Choices) {
    ComboBox::from_id_salt("target_step")
        .selected_text(choices.label(set.step))
        .show_ui(ui, |ui| {
            for step in sends {
                if ui
                    .selectable_label(set.step == *step, choices.label(*step))
                    .clicked()
                    && set.step != *step
                {
                    set.step = *step;
                    let taken = &choices.steps[*step - 1].taken;
                    set.field = choices
                        .fields(*step)
                        .iter()
                        .find(|field| !taken.contains(&field.name))
                        .map(|field| field.name.clone())
                        .unwrap_or_default();
                    set.bit = None;
                }
            }
        });
    let taken = choices
        .steps
        .get(set.step.saturating_sub(1))
        .map(|info| info.taken.clone())
        .unwrap_or_default();
    field_and_bit(ui, set.step, &mut set.field, &mut set.bit, choices, &taken);
}

/// `unavailable` are fields shown greyed out: they are named so that it is
/// clear why they cannot be picked, not hidden.
fn field_and_bit(
    ui: &mut Ui,
    step: usize,
    field: &mut String,
    bit: &mut Option<String>,
    choices: &Choices,
    unavailable: &HashSet<String>,
) {
    ComboBox::from_id_salt("field")
        .selected_text(if field.is_empty() {
            "choose...".to_owned()
        } else {
            field.clone()
        })
        .show_ui(ui, |ui| {
            for candidate in choices.fields(step) {
                if matches!(candidate.kind, FieldKind::Checksum { .. }) {
                    continue;
                }
                let free = !unavailable.contains(&candidate.name);
                ui.add_enabled_ui(free, |ui| {
                    if ui
                        .selectable_label(*field == candidate.name, &candidate.name)
                        .on_disabled_hover_text("The step sets this field itself")
                        .clicked()
                        && *field != candidate.name
                    {
                        field.clone_from(&candidate.name);
                        *bit = None;
                    }
                });
            }
        });

    let Some(FieldKind::Bits { bits, .. }) = choices.field(step, field).map(|held| &held.kind)
    else {
        *bit = None;
        return;
    };
    ComboBox::from_id_salt("bit")
        .selected_text(bit.clone().unwrap_or_else(|| "whole field".to_owned()))
        .show_ui(ui, |ui| {
            ui.selectable_value(bit, None, "whole field");
            for candidate in bits {
                ui.selectable_value(bit, Some(candidate.name.clone()), &candidate.name);
            }
        });
}

/// A number, a variant or a bit's worth, whichever the place it is compared
/// with or written into holds.
fn value_editor(ui: &mut Ui, choices: &Choices, place: &Source, value: &mut Value) {
    if let Some(bit) = &place.bit {
        let width = choices
            .bit_width(place.step, &place.field, bit)
            .unwrap_or(1)
            .min(63);
        let mut current = value.as_uint().unwrap_or(0);
        if ui
            .add(number(&mut current, None).range(0..=((1u64 << width) - 1)))
            .changed()
        {
            *value = Value::Uint(current);
        }
        return;
    }

    let Some(field) = choices.field(place.step, &place.field) else {
        ui.label(RichText::new("choose a field").weak());
        return;
    };
    match &field.kind {
        FieldKind::Enum { variants, .. } => {
            let held = value.as_uint().unwrap_or(0);
            let shown = variants
                .iter()
                .find(|variant| variant.value == held)
                .map_or_else(|| held.to_string(), |variant| variant.name.clone());
            ComboBox::from_id_salt("variant")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for variant in variants {
                        if ui
                            .selectable_label(held == variant.value, &variant.name)
                            .clicked()
                        {
                            *value = Value::Uint(variant.value);
                        }
                    }
                });
        }
        FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => {
            let mut current = value.as_float().unwrap_or(0.0);
            if ui.add(number(&mut current, None).speed(0.1)).changed() {
                *value = Value::Float(current);
            }
        }
        FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
            let mut current = value.as_uint().unwrap_or(0);
            let digits = choices.hex.then(|| scalar.size() * 2);
            if ui.add(number(&mut current, digits)).changed() {
                *value = Value::Uint(current);
            }
        }
        FieldKind::Scalar(scalar) => {
            let mut current = value.as_int().unwrap_or(0);
            let digits = choices.hex.then(|| scalar.size() * 2);
            if ui.add(number(&mut current, digits)).changed() {
                *value = Value::Int(current);
            }
        }
        FieldKind::Bytes { .. }
        | FieldKind::Text { .. }
        | FieldKind::Bits { .. }
        | FieldKind::Checksum { .. } => {
            ui.label(RichText::new("pick a bit or a numeric field").weak());
        }
    }
}

/// An optional mask, ticked in to apply it.
fn mask(ui: &mut Ui, label: &str, held: &mut Option<u64>) {
    let mut on = held.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *held = on.then_some(0xFF);
    }
    if let Some(bits) = held {
        // Always shown in hex: a mask is read bit by bit, whatever base the
        // frame editor is set to.
        ui.add(number(bits, Some(2)));
    }
}
