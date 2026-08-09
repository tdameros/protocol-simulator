//! Building a scenario without opening a file.
//!
//! Nothing here is typed by hand that could be picked from a list: connections,
//! frames and field names all come from what the project already holds. A step
//! carries one action because the picker offers one, and a connection exists
//! because it was ticked, so most of what the loader knows how to refuse cannot
//! be built in the first place.
//!
//! The drawing lives here and the meaning lives in [`crate::scenarios`], which
//! is what makes the meaning testable without a window.

use std::time::Duration;

use egui::{ComboBox, DragValue, Grid, RichText, TextStyle, Ui};
use egui_phosphor::regular as icons;

use sim_core::frame::FrameDef;
use sim_core::scenario::{Action, Expect, Step};
use sim_core::{Anchor, ConnectionId, HexPattern};

use crate::panels::frame_editor::value_widget;
use crate::panels::hex_inject::parse_hex;
use crate::panels::{column, field_label, widest};
use crate::scenarios::{self, ActionKind};
use crate::state::AppState;

/// A change to the list of steps, which cannot happen while it is being drawn.
enum Edit {
    Add,
    Remove(usize),
    Move { index: usize, down: bool },
    Kind { index: usize, kind: ActionKind },
}

pub fn steps(ui: &mut Ui, state: &mut AppState) {
    let links: Vec<ConnectionId> = state.connections.iter().map(|(id, _)| id.clone()).collect();
    let frames: Vec<FrameDef> = state.frames.frames.clone();

    if links.is_empty() {
        ui.label(RichText::new("No connection configured, so a step can only wait.").weak());
    }

    let mut pending: Option<Edit> = None;
    let numbers = widest(ui, &TextStyle::Body, &["88."]);

    let Some(draft) = state.scenarios.draft.as_mut() else {
        return;
    };
    let count = draft.scenario.steps.len();

    for (index, step) in draft.scenario.steps.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                column(ui, numbers, |ui| {
                    ui.label(RichText::new(format!("{}.", index + 1)).weak());
                });

                let mut kind = ActionKind::of(&step.action);
                ComboBox::from_id_salt("kind")
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for choice in ActionKind::ALL {
                            // Offered only where it could actually be saved.
                            let usable = !choice.needs_a_connection() || !links.is_empty();
                            ui.add_enabled_ui(usable, |ui| {
                                if ui
                                    .selectable_label(kind == choice, choice.label())
                                    .clicked()
                                {
                                    kind = choice;
                                }
                            });
                        }
                    });
                if kind != ActionKind::of(&step.action) {
                    pending = Some(Edit::Kind { index, kind });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(icons::TRASH).on_hover_text("Remove").clicked() {
                        pending = Some(Edit::Remove(index));
                    }
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new(icons::ARROW_DOWN))
                        .clicked()
                    {
                        pending = Some(Edit::Move { index, down: true });
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new(icons::ARROW_UP))
                        .clicked()
                    {
                        pending = Some(Edit::Move { index, down: false });
                    }
                });
            });

            ui.indent("body", |ui| {
                // A delay touches no link, so it is not offered one.
                if ActionKind::of(&step.action).needs_a_connection() {
                    targets(ui, step, &links);
                }
                body(ui, step, &frames);
            });
            ui.separator();
        });
    }

    if ui.button(format!("{} Add a step", icons::PLUS)).clicked() {
        pending = Some(Edit::Add);
    }

    let Some(edit) = pending else {
        return;
    };
    let Some(draft) = state.scenarios.draft.as_mut() else {
        return;
    };
    let first_frame = frames.first().map(|frame| frame.name.as_str());
    match edit {
        Edit::Add => draft.add_step(&links, first_frame),
        Edit::Remove(index) => draft.remove_step(index),
        Edit::Move { index, down } => draft.move_step(index, down),
        Edit::Kind { index, kind } => draft.set_action(index, kind, &links),
    }
}

/// Which links the step acts on, as ticks rather than as typed names.
fn targets(ui: &mut Ui, step: &mut Step, links: &[ConnectionId]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("on").weak());
        for link in links {
            let mut on = step.targets.contains(link);
            if ui.checkbox(&mut on, &link.0).changed() {
                if on {
                    step.targets.push(link.clone());
                } else {
                    step.targets.retain(|held| held != link);
                }
            }
        }
        // Leaving none ticked would produce a step the loader refuses, so the
        // last one cannot be unticked away.
        if step.targets.is_empty() {
            if let Some(first) = links.first() {
                step.targets.push(first.clone());
            }
        }
    });
}

fn body(ui: &mut Ui, step: &mut Step, frames: &[FrameDef]) {
    match &mut step.action {
        Action::Wait { delay } => {
            let mut millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
            if ui
                .add(
                    DragValue::new(&mut millis)
                        .range(0..=3_600_000)
                        .suffix(" ms"),
                )
                .changed()
            {
                *delay = Duration::from_millis(millis);
            }
        }
        Action::Raw { bytes } => {
            hex_field(ui, "bytes", bytes);
        }
        Action::Send { .. } => send_body(ui, step, frames),
        Action::WaitFor { .. } => wait_body(ui, step, frames),
    }
}

fn send_body(ui: &mut Ui, step: &mut Step, frames: &[FrameDef]) {
    let Action::Send { frame, .. } = &step.action else {
        return;
    };
    let chosen = frame.clone();
    let mut picked: Option<String> = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("frame").weak());
        ComboBox::from_id_salt("frame")
            .selected_text(if chosen.is_empty() {
                "choose...".to_owned()
            } else {
                chosen.clone()
            })
            .show_ui(ui, |ui| {
                for known in frames {
                    if ui
                        .selectable_label(chosen == known.name, &known.name)
                        .clicked()
                    {
                        picked = Some(known.name.clone());
                    }
                }
            });
    });
    if let Some(name) = picked {
        scenarios::set_frame(step, &name);
    }

    let Some(definition) = frames.iter().find(|known| known.name == chosen).cloned() else {
        ui.label(RichText::new("No frame of that name is loaded.").weak());
        return;
    };

    // Ticked means this scenario decides the value; unticked means the frame's
    // own default goes out, whatever it may become.
    Grid::new("overrides")
        .num_columns(4)
        .min_col_width(0.0)
        .show(ui, |ui| {
            for field in &definition.fields {
                let Action::Send { with, counters, .. } = &mut step.action else {
                    return;
                };
                // Computed on send, so there is nothing here to decide.
                if matches!(field.kind, sim_core::frame::FieldKind::Checksum { .. }) {
                    continue;
                }

                let counted = counters.contains_key(&field.name);
                let mut overridden = with.contains_key(&field.name);
                if ui
                    .add_enabled(!counted, egui::Checkbox::new(&mut overridden, &field.name))
                    .changed()
                {
                    scenarios::set_override(step, &definition, &field.name, overridden);
                }

                let Action::Send { with, counters, .. } = &mut step.action else {
                    return;
                };
                if with.contains_key(&field.name) {
                    value_widget(ui, field, &field.kind, with);
                } else if counted {
                    ui.label(RichText::new("counted").weak());
                } else {
                    ui.label(RichText::new("frame default").weak());
                }

                let mut counting = counters.contains_key(&field.name);
                if ui.checkbox(&mut counting, "count").changed() {
                    scenarios::set_counter(step, &field.name, counting);
                }

                let Action::Send { counters, .. } = &mut step.action else {
                    return;
                };
                if let Some(counter) = counters.get_mut(&field.name) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("from").weak());
                        ui.add(DragValue::new(&mut counter.from));
                        ui.label(RichText::new("by").weak());
                        ui.add(DragValue::new(&mut counter.step).range(1..=u64::MAX));

                        let mut wraps = counter.wrap.is_some();
                        if ui.checkbox(&mut wraps, "wrap at").changed() {
                            counter.wrap = wraps.then_some(255);
                        }
                        if let Some(wrap) = &mut counter.wrap {
                            ui.add(DragValue::new(wrap));
                        }
                    });
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
}

fn wait_body(ui: &mut Ui, step: &mut Step, frames: &[FrameDef]) {
    let Action::WaitFor { expect, timeout } = &mut step.action else {
        return;
    };

    let mut by_frame = matches!(expect, Expect::Frame { .. });
    let before = by_frame;
    ui.horizontal(|ui| {
        ui.radio_value(&mut by_frame, true, "a frame");
        ui.radio_value(&mut by_frame, false, "these bytes");

        ui.separator();
        let mut limited = timeout.is_some();
        if ui.checkbox(&mut limited, "give up after").changed() {
            *timeout = limited.then(|| Duration::from_millis(500));
        }
        if let Some(limit) = timeout {
            let mut millis = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
            if ui
                .add(
                    DragValue::new(&mut millis)
                        .range(1..=3_600_000)
                        .suffix(" ms"),
                )
                .changed()
            {
                *limit = Duration::from_millis(millis);
            }
        }
    });
    if by_frame != before {
        scenarios::set_wait_by_frame(step, by_frame, frames.first().map(|f| f.name.as_str()));
    }

    let Action::WaitFor { expect, .. } = &mut step.action else {
        return;
    };
    if matches!(expect, Expect::Pattern { .. }) {
        pattern_body(ui, expect);
        return;
    }
    frame_body(ui, step, frames);
}

fn pattern_body(ui: &mut Ui, expect: &mut Expect) {
    match expect {
        Expect::Pattern { pattern, anchor } => {
            ui.horizontal(|ui| {
                // The same treatment as a plain hex field, `??` aside: a
                // pattern is no more typeable one keystroke at a time.
                if let Some(text) = kept_text(ui, "pattern", &pattern.to_hex(), "AA 55 ?? 01") {
                    if let Some(parsed) = HexPattern::parse(&text) {
                        *pattern = parsed;
                    }
                }

                let mut anchored = anchor.offset().is_some();
                if ui.checkbox(&mut anchored, "at offset").changed() {
                    *anchor = if anchored {
                        Anchor::At(0)
                    } else {
                        Anchor::Anywhere
                    };
                }
                if let Anchor::At(offset) = anchor {
                    ui.add(DragValue::new(offset));
                }
            });
        }
        Expect::Frame { .. } => {}
    }
}

fn frame_body(ui: &mut Ui, step: &mut Step, frames: &[FrameDef]) {
    let Action::WaitFor {
        expect: Expect::Frame { frame, fields },
        ..
    } = &mut step.action
    else {
        return;
    };
    {
        {
            let chosen = frame.clone();
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("wait_frame")
                    .selected_text(if chosen.is_empty() {
                        "choose...".to_owned()
                    } else {
                        chosen.clone()
                    })
                    .show_ui(ui, |ui| {
                        for known in frames {
                            if ui
                                .selectable_label(chosen == known.name, &known.name)
                                .clicked()
                            {
                                frame.clone_from(&known.name);
                                fields.clear();
                            }
                        }
                    });
                ui.label(RichText::new("matching, at its declared value:").weak())
                    .on_hover_text(
                        "A ticked field has to carry the value the frame declares as its default.                          There is no way to ask for another value yet.",
                    );
            });

            let Some(definition) = frames.iter().find(|known| known.name == chosen) else {
                ui.label(RichText::new("No frame of that name is loaded.").weak());
                return;
            };
            // Checksums are left out for the same reason the send editor leaves
            // them out: computed rather than chosen, and matching one would pin
            // the wait to a frame carrying nothing but defaults.
            let names: Vec<String> = definition
                .fields
                .iter()
                .filter(|field| !matches!(field.kind, sim_core::frame::FieldKind::Checksum { .. }))
                .map(|field| field.name.clone())
                .collect();

            let mut toggled: Option<(String, bool)> = None;
            ui.horizontal_wrapped(|ui| {
                for name in names {
                    let mut on = fields.iter().any(|held| held == &name);
                    if ui.checkbox(&mut on, &name).changed() {
                        toggled = Some((name, on));
                    }
                }
            });
            if let Some((name, on)) = toggled {
                scenarios::set_match(step, &name, on);
            }
        }
    }
}

/// A text box whose content survives being half typed, returning what was typed
/// whenever it changes.
///
/// Rebuilding the text from the model on every repaint would fight whoever is
/// typing: `AA 5` is not a byte yet, so the model does not take it, so the box
/// would snap back to the last good value between two keystrokes. The text is
/// therefore kept aside while the box has focus, the same trick the frame
/// editor's hex preview uses.
fn kept_text(ui: &mut Ui, salt: &str, mirror: &str, hint: &str) -> Option<String> {
    let id = ui.make_persistent_id(salt);
    let focused = ui.memory(|memory| memory.has_focus(id));
    let held: Option<String> = ui.memory(|memory| memory.data.get_temp(id));

    let mut text = match (held, focused) {
        (Some(text), true) => text,
        _ => mirror.to_owned(),
    };

    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .font(TextStyle::Monospace)
            .hint_text(hint),
    );
    let changed = response.changed().then(|| text.clone());
    ui.memory_mut(|memory| memory.data.insert_temp(id, text));
    changed
}

/// Bytes, taken from the box only once they parse.
fn hex_field(ui: &mut Ui, salt: &str, bytes: &mut Vec<u8>) {
    if let Some(text) = kept_text(ui, salt, &spaced(bytes), "DE AD BE EF") {
        if let Ok(parsed) = parse_hex(&text) {
            *bytes = parsed;
        }
    }
}

fn spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The settings a scenario has beyond its steps.
pub fn header(ui: &mut Ui, state: &mut AppState) {
    let labels = widest(ui, &TextStyle::Body, &["Name:", "Description:", "Repeat:"]);
    let dirty = state.scenarios.draft_is_dirty();

    let Some(draft) = state.scenarios.draft.as_mut() else {
        return;
    };
    let scenario = &mut draft.scenario;

    ui.horizontal(|ui| {
        field_label(ui, "Name:", labels);
        ui.text_edit_singleline(&mut scenario.name);
        if dirty {
            ui.label(RichText::new("edited").weak());
        }
    });

    ui.horizontal(|ui| {
        field_label(ui, "Description:", labels);
        let mut text = scenario.description.clone().unwrap_or_default();
        if ui.text_edit_singleline(&mut text).changed() {
            scenario.description = (!text.trim().is_empty()).then_some(text);
        }
    });

    ui.horizontal(|ui| {
        field_label(ui, "Repeat:", labels);
        let mut repeats = scenario.repeat.is_some();
        if ui.checkbox(&mut repeats, "every").changed() {
            scenario.repeat = repeats.then(|| sim_core::scenario::Repeat {
                every: Duration::from_millis(100),
                times: None,
            });
        }
        if let Some(repeat) = &mut scenario.repeat {
            // Floored at one: a period of zero is no period, and the timer the
            // engine would build from it refuses to exist.
            let mut period = u64::try_from(repeat.every.as_millis()).unwrap_or(u64::MAX);
            if ui
                .add(
                    DragValue::new(&mut period)
                        .range(1..=3_600_000)
                        .suffix(" ms"),
                )
                .changed()
            {
                repeat.every = Duration::from_millis(period.max(1));
            }

            let mut counted = repeat.times.is_some();
            if ui.checkbox(&mut counted, "stop after").changed() {
                repeat.times = counted.then_some(10);
            }
            if let Some(times) = &mut repeat.times {
                ui.add(DragValue::new(times).range(1..=u32::MAX).suffix(" passes"));
            }
        }
    });
}
