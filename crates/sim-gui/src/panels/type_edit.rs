//! Editing the types every frame in a folder shares.
//!
//! A shared type is one of two things, and the file format will not have both:
//! a group of fields, edited with the very editor a frame's fields are edited
//! with, or a scalar narrowed to the values the protocol allows. Which of the
//! two is a setting inside the type rather than a choice to be made before
//! there is one, both being a `[[type]]` in the same folder.
//!
//! Changing a type reaches every frame naming it, so what it would do to them
//! is shown beside Save rather than discovered at the next Reload.

use egui::{ComboBox, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icons;
use sim_core::frame::schema::{Subtype, TypeDef};
use sim_core::frame::{FrameDef, ScalarType, ValueRange};

use crate::frames::Effect;
use crate::panels::number;
use crate::state::AppState;

const ERROR: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);
const WARNING: egui::Color32 = egui::Color32::from_rgb(200, 120, 40);

/// The scalars a subtype may narrow.
const BASES: [ScalarType; 10] = [
    ScalarType::U8,
    ScalarType::I8,
    ScalarType::U16,
    ScalarType::I16,
    ScalarType::U32,
    ScalarType::I32,
    ScalarType::U64,
    ScalarType::I64,
    ScalarType::F32,
    ScalarType::F64,
];

/// The shared types, listed under the frames with the same three buttons.
pub fn library_bar(ui: &mut Ui, state: &mut AppState) {
    if state.frames.directory.is_none() {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Shared types:").weak());
        let names: Vec<String> = state
            .frames
            .type_entries
            .iter()
            .map(|entry| entry.definition.name().to_owned())
            .collect();
        let chosen = state
            .frames
            .type_selected
            .and_then(|at| names.get(at).cloned())
            .unwrap_or_else(|| "none".to_owned());
        ComboBox::from_id_salt("shared_type_pick")
            .selected_text(chosen)
            .show_ui(ui, |ui| {
                for (at, name) in names.iter().enumerate() {
                    if ui
                        .selectable_label(state.frames.type_selected == Some(at), name)
                        .clicked()
                    {
                        state.frames.type_selected = Some(at);
                    }
                }
            });

        if ui
            .add_enabled(
                state.frames.type_draft.is_none(),
                egui::Button::new(format!("{} New", icons::FILE_PLUS)),
            )
            .on_hover_text("A type every frame in this folder can name")
            .clicked()
        {
            // Started as a group with nothing in it, which Save refuses until
            // it is told what it is. A subtype would be savable straight away,
            // and an empty one nobody meant to make is worse than a prompt.
            state.frames.begin_new_type(TypeDef {
                layout: FrameDef::flat("NewType", Vec::new()),
                narrows: None,
            });
        }
        let chosen = state.frames.selected_type().is_some();
        if ui
            .add_enabled(
                chosen,
                egui::Button::new(format!("{} Edit", icons::PENCIL_SIMPLE)),
            )
            .clicked()
        {
            state.frames.begin_type_edit();
        }
        if ui
            .add_enabled(chosen, egui::Button::new(icons::TRASH))
            .on_hover_text("Delete this type, leaving the rest of its file alone")
            .clicked()
        {
            if let Err(error) = state.frames.delete_selected_type() {
                state.last_error = Some(format!("{error:#}"));
            }
        }
    });
}

pub fn editor(ui: &mut Ui, state: &mut AppState) {
    let dirty = state.frames.type_draft_is_dirty();
    let problem = state.frames.type_draft_problem();
    let impact = state.frames.type_draft_impact();
    // Its own name left out: a type naming itself is the one thing the loader
    // refuses outright.
    let editing = state
        .frames
        .type_draft
        .as_ref()
        .map(|draft| draft.definition.name().to_owned());
    let shared = state.frames.shared_choices(editing.as_deref());
    let types = state.frames.types().clone();
    let Some(draft) = &mut state.frames.type_draft else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label("Type:");
        ui.text_edit_singleline(&mut draft.definition.layout.name);
    });
    shape(ui, &mut draft.definition);
    let mut description = draft
        .definition
        .layout
        .description
        .clone()
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Description:");
        if ui.text_edit_singleline(&mut description).changed() {
            draft.definition.layout.description =
                (!description.trim().is_empty()).then_some(description);
        }
    });
    ui.separator();

    ScrollArea::vertical()
        .id_salt("type_body")
        .max_height(ui.available_height() * 0.5)
        .show(ui, |ui| match &mut draft.definition.narrows {
            Some(narrows) => narrowing(ui, narrows),
            None => super::frame_edit::layout(
                ui,
                &mut draft.definition.layout,
                state.hex_values,
                &shared,
                &types,
            ),
        });

    ui.separator();
    // Said before the click, not after: reaching every frame naming the type is
    // what a shared type is for, and the only surprise worth avoiding is not
    // knowing which.
    for (frame, effect) in &impact {
        let (colour, said) = match effect {
            Effect::Broken(reason) => (ERROR, format!("{frame} would stop loading: {reason}")),
            Effect::Resized { was, now } => {
                (WARNING, format!("{frame} goes from {was} to {now} bytes"))
            }
            Effect::Reshaped => (WARNING, format!("{frame} changes shape")),
        };
        ui.colored_label(colour, said);
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                dirty && problem.is_none(),
                egui::Button::new(format!("{} Save", icons::FLOPPY_DISK)),
            )
            .clicked()
        {
            if let Err(error) = state.frames.save_type_draft() {
                state.last_error = Some(format!("{error:#}"));
            }
        }
        if ui.button("Cancel").clicked() {
            state.frames.cancel_type_edit();
        }
        if let Some(reason) = &problem {
            ui.colored_label(ERROR, reason);
        }
    });
}

/// Which of the two things a type is.
///
/// The format will not have both at once, so switching empties the other side.
/// Deliberate rather than clever: keeping the fields of a type that is no
/// longer a group would write a file the loader refuses, and hiding them would
/// leave the editor showing something the file does not say.
fn shape(ui: &mut Ui, definition: &mut TypeDef) {
    let subtype = definition.narrows.is_some();
    ui.horizontal(|ui| {
        ui.label("Holds:");
        if ui.selectable_label(!subtype, "fields").clicked() && subtype {
            definition.narrows = None;
        }
        if ui
            .selectable_label(subtype, "one narrowed scalar")
            .clicked()
            && !subtype
        {
            definition.layout.fields.clear();
            definition.layout.declared.clear();
            definition.layout.stated.clear();
            definition.narrows = Some(Subtype {
                base: ScalarType::U8.name().to_owned(),
                range: Some(ScalarType::U8.representable()),
            });
        }
    });
}

/// The whole of a subtype: what it narrows, and to what.
fn narrowing(ui: &mut Ui, narrows: &mut Subtype) {
    let scalar = ScalarType::parse(&narrows.base);
    ui.horizontal(|ui| {
        ui.label("Narrows:");
        ComboBox::from_id_salt("subtype_base")
            .selected_text(narrows.base.clone())
            .show_ui(ui, |ui| {
                for base in BASES {
                    if ui
                        .selectable_label(scalar == Some(base), base.name())
                        .clicked()
                        && scalar != Some(base)
                    {
                        base.name().clone_into(&mut narrows.base);
                        // The bounds belonged to the old scalar, and may not
                        // even be the same sort of number as the new one.
                        narrows.range = Some(base.representable());
                    }
                }
            });
    });

    let representable = scalar.map(ScalarType::representable);
    ui.horizontal(|ui| {
        let mut narrowed = narrows.range.is_some();
        if ui.checkbox(&mut narrowed, "Allowed:").changed() {
            narrows.range =
                narrowed.then(|| representable.unwrap_or(ValueRange::Uint { min: 0, max: 0 }));
        }
        match &mut narrows.range {
            Some(ValueRange::Uint { min, max }) => {
                ui.add(number(min, None));
                ui.label("to");
                ui.add(number(max, None));
            }
            Some(ValueRange::Int { min, max }) => {
                ui.add(number(min, None));
                ui.label("to");
                ui.add(number(max, None));
            }
            Some(ValueRange::Float { min, max }) => {
                ui.add(number(min, None));
                ui.label("to");
                ui.add(number(max, None));
            }
            None => {
                ui.label(
                    RichText::new(representable.map_or_else(String::new, |range| range.describe()))
                        .weak(),
                );
            }
        }
    });
    ui.label(RichText::new("A field of this type is refused any value outside that.").weak());
}
