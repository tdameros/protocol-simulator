use sim_core::frame::codec;
use sim_core::frame::value::Value;
use sim_core::frame::{BitDef, EnumVariant, FieldDef, FieldKind, FrameDef, ScalarType, ValueRange};
use sim_core::ConnectionStatus;

use egui::{Color32, ComboBox, RichText, ScrollArea, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::panels::number;
use sim_session::engine_handle::EngineHandle;
use sim_session::kinds::bit_positions;
use sim_session::state::Session;
use sim_session::{frames, hex, tree};

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const WARNING: Color32 = Color32::from_rgb(200, 120, 40);

pub fn show(ui: &mut Ui, state: &mut Session, engine: &EngineHandle) {
    // Taken unconditionally: bytes sent here with no frame to decode them into
    // are dropped now rather than surfacing later against an unrelated frame.
    let handed_over = state.pending_frame_hex.take();

    library_bar(ui, state);

    // The type first: it can be opened from a field of a frame being edited,
    // and that frame has to be waiting underneath when the type is done with.
    if state.frames.type_draft.is_some() {
        super::type_edit::editor(ui, state);
        return;
    }
    // Editing a copy, so what the list and the disk hold is untouched until
    // Save says otherwise.
    if state.frames.draft.is_some() {
        draft_editor(ui, state);
        return;
    }

    // Drawn even with nothing in the folder, both of them: New lives on these
    // rows, and a folder emptied of its last frame has to leave a way to make
    // another one.
    super::type_edit::library_bar(ui, state);
    frame_picker(ui, state);
    show_failures(ui, state);

    if state.frames.is_empty() {
        if state.frames.directory.is_some() && state.frames.failures.is_empty() {
            ui.label("No .toml frame definition in that folder.");
        }
        return;
    }
    ui.separator();

    let Some(frame) = state.frames.selected_frame().cloned() else {
        return;
    };

    if let Some(bytes) = handed_over {
        let typed = hex::spaced(&bytes);
        state.frame_hex_note = frames::apply_hex(state, &frame, &typed);
        state.frame_hex = typed;
    }

    // Read before the values are borrowed, the whole editor sharing one answer
    // rather than each field having its own.
    let hex = state.hex_values;
    let tree = tree::build_tree(&frame.fields);
    ScrollArea::vertical()
        .id_salt("frame_fields")
        .max_height(ui.available_height() * 0.55)
        .show(ui, |ui| {
            let values = state.frames.values_mut(&frame);
            show_entries(ui, &tree, values, hex);
        });

    ui.separator();
    preview_and_send(ui, state, engine, &frame);
}

fn library_bar(ui: &mut Ui, state: &mut Session) {
    ui.horizontal(|ui| {
        // Both throw the draft away, so neither is offered while one is open:
        // losing unsaved work to a stray click is not a trade worth making.
        let idle = state.frames.draft.is_none() && state.frames.type_draft.is_none();
        if ui
            .add_enabled(
                idle,
                egui::Button::new(RichText::new(format!(
                    "{} Frames folder",
                    icons::FOLDER_OPEN
                ))),
            )
            .clicked()
        {
            if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                state.frames.load_from(directory);
            }
        }
        if state.frames.directory.is_some()
            && ui
                .add_enabled(
                    idle,
                    egui::Button::new(RichText::new(format!("{} Reload", icons::ARROWS_CLOCKWISE))),
                )
                .on_hover_text("Re-read the .toml files from disk")
                .clicked()
        {
            state.frames.reload();
        }
    });
    if let Some(directory) = &state.frames.directory {
        ui.label(RichText::new(directory.display().to_string()).weak());
    } else {
        ui.label("Pick the folder holding your frame .toml files.");
    }
}

/// New, Edit and Delete, beside the frame they act on rather than beside the
/// folder, so that the row reads like the one for shared types below it.
fn definition_buttons(ui: &mut Ui, state: &mut Session) {
    let idle = state.frames.draft.is_none() && state.frames.type_draft.is_none();
    if ui
        .add_enabled(
            state.frames.directory.is_some() && idle,
            egui::Button::new(format!("{} New", icons::FILE_PLUS)),
        )
        .on_hover_text("Start a frame from scratch")
        .clicked()
    {
        let name = state.frames.unused_frame_name("New frame");
        state.frames.begin_new(frames::blank_frame(&name));
    }
    let editable = state.frames.selected_entry().is_some() && idle;
    if ui
        .add_enabled(
            editable,
            egui::Button::new(format!("{} Edit", icons::PENCIL_SIMPLE)),
        )
        .on_hover_text("Edit this frame definition")
        .clicked()
    {
        state.frames.begin_edit();
    }
    if ui
        .add_enabled(editable, egui::Button::new(icons::TRASH))
        .on_hover_text("Delete this frame, and the file holding it")
        .clicked()
    {
        if let Err(error) = state.frames.delete_selected() {
            state.last_error = Some(format!("{error:#}"));
        }
    }
}

fn draft_editor(ui: &mut Ui, state: &mut Session) {
    let dirty = state.frames.draft_is_dirty();
    let problem = state.frames.draft_problem();
    let Some(draft) = &mut state.frames.draft else {
        return;
    };

    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.text_edit_singleline(&mut draft.frame.name);
    });
    let mut endian = draft.frame.endian;
    super::frame_edit::byte_order(ui, &mut endian, None);
    // Through the layout rather than by assignment: the fields that were
    // following the frame have to keep following it.
    sim_session::layout::set_endian(&mut draft.frame, endian);
    let mut description = draft.frame.description.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Description:");
        if ui.text_edit_singleline(&mut description).changed() {
            draft.frame.description = (!description.trim().is_empty()).then_some(description);
        }
    });

    ui.separator();
    ScrollArea::vertical()
        .id_salt("draft_fields")
        .max_height(ui.available_height() * 0.6)
        .show(ui, |ui| super::frame_edit::fields(ui, state));

    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                dirty && problem.is_none(),
                egui::Button::new(format!("{} Save", icons::FLOPPY_DISK)),
            )
            .clicked()
        {
            frames::save_draft(state);
        }
        if ui.button("Cancel").clicked() {
            state.frames.cancel_edit();
        }
        // Said here rather than after the click: a half-made frame is a normal
        // state to be in while building one.
        if let Some(reason) = &problem {
            ui.colored_label(ERROR, reason);
        }
    });
}

fn frame_picker(ui: &mut Ui, state: &mut Session) {
    if state.frames.directory.is_none() {
        return;
    }
    let names: Vec<String> = state
        .frames
        .frames()
        .map(|frame| frame.name.clone())
        .collect();
    let selected_label = state
        .frames
        .selected
        .and_then(|index| names.get(index).cloned())
        .unwrap_or_else(|| "none".to_owned());

    ui.horizontal(|ui| {
        super::library_label(ui, "Frame:");
        ComboBox::from_id_salt("frame_pick")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for (index, name) in names.iter().enumerate() {
                    if ui
                        .selectable_label(state.frames.selected == Some(index), name)
                        .clicked()
                    {
                        state.frames.selected = Some(index);
                        // Whatever the note said, it said it about another frame.
                        state.frame_hex_note = None;
                    }
                }
            });
        definition_buttons(ui, state);

        // What follows is about the values being typed in, so it is offered
        // only when there is a frame to type them into.
        let Some(frame) = state.frames.selected_frame().cloned() else {
            return;
        };
        ui.label(RichText::new(format!("{} bytes", frame.size())).weak());
        if ui
            .selectable_label(state.hex_values, "0x")
            .on_hover_text("Show whole-number fields in hexadecimal. They still take decimal.")
            .clicked()
        {
            state.hex_values = !state.hex_values;
        }
        if ui
            .button(icons::ARROW_COUNTER_CLOCKWISE)
            .on_hover_text("Reset every field to its default")
            .clicked()
        {
            state.frames.reset_values(&frame);
        }
    });

    if let Some(description) = state
        .frames
        .selected_frame()
        .and_then(|frame| frame.description.clone())
    {
        ui.label(RichText::new(description).weak());
    }
}

fn show_failures(ui: &mut Ui, state: &Session) {
    for (file, reason) in &state.frames.failures {
        ui.colored_label(ERROR, format!("{file}: {reason}"));
    }
}

fn show_entries(
    ui: &mut Ui,
    entries: &[tree::Entry<'_>],
    values: &mut sim_core::frame::value::FieldValues,
    hex: bool,
) {
    // Consecutive fields share one grid so their columns line up; a group
    // interrupts the run because its rows are indented one level deeper.
    let mut run: Vec<&FieldDef> = Vec::new();
    for entry in entries {
        match entry {
            tree::Entry::Field(field) => run.push(field),
            tree::Entry::Group(group) => {
                field_grid(ui, &mut run, values, hex);
                let header = format!("{}  ·  {} B", group.label, entry.size());
                egui::CollapsingHeader::new(RichText::new(header).strong())
                    .id_salt(group.salt)
                    .default_open(true)
                    .show(ui, |ui| show_entries(ui, &group.entries, values, hex));
            }
        }
    }
    field_grid(ui, &mut run, values, hex);
}

fn field_grid(
    ui: &mut Ui,
    run: &mut Vec<&FieldDef>,
    values: &mut sim_core::frame::value::FieldValues,
    hex: bool,
) {
    let Some(first) = run.first() else {
        return;
    };
    egui::Grid::new(("frame_field_grid", &first.name))
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for field in run.iter() {
                field_row(ui, field, values, hex);
                ui.end_row();
            }
        });
    run.clear();
}

fn field_row(
    ui: &mut Ui,
    field: &FieldDef,
    values: &mut sim_core::frame::value::FieldValues,
    hex: bool,
) {
    let mut label = ui.label(RichText::new(tree::leaf_name(&field.name)).strong());
    if let Some(description) = &field.description {
        label = label.on_hover_text(description);
    }
    if field.name.contains('.') {
        label = label.on_hover_text(&field.name);
    }
    let _ = label;

    ui.label(RichText::new(frames::type_label(field)).weak());

    match &field.kind {
        FieldKind::Checksum { .. } => {
            ui.label(RichText::new("computed on send").weak());
        }
        kind => value_widget(ui, field, kind, values, hex),
    }
}

/// Shared with the scenario editor: a field is edited the same way whether it
/// is being sent by hand or written into a step.
pub fn value_widget(
    ui: &mut Ui,
    field: &FieldDef,
    kind: &FieldKind,
    values: &mut sim_core::frame::value::FieldValues,
    hex: bool,
) {
    let entry = values.entry(field.name.clone()).or_insert(Value::Uint(0));

    match kind {
        FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => {
            let mut current = entry.as_float().unwrap_or(0.0);
            // A float has no hexadecimal to show, so it stays as it is.
            let mut widget = number(&mut current, None).speed(0.1);
            // The declared subtype, not the representation, is what the editor
            // lets you reach: a 0..99 field simply will not go to 100.
            if let Some(ValueRange::Float { min, max }) = field.range {
                widget = widget.range(min..=max);
            }
            if ui.add(widget).changed() {
                *entry = Value::Float(current);
            }
        }
        FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
            let mut current = entry.as_uint().unwrap_or(0);
            let (min, max) = match field.range {
                Some(ValueRange::Uint { min, max }) => (min, max),
                _ => (0, frames::max_unsigned(*scalar)),
            };
            // Padded to the width of what holds it, so a u16 reads 0x00FF
            // rather than 0xFF and lines up with the byte preview below.
            let digits = hex.then(|| scalar.size() * 2);
            if ui
                .add(number(&mut current, digits).range(min..=max))
                .changed()
            {
                *entry = Value::Uint(current);
            }
        }
        FieldKind::Scalar(scalar) => {
            let mut current = entry.as_int().unwrap_or(0);
            let bits = scalar.size() * 8;
            let (min, max) = match field.range {
                Some(ValueRange::Int { min, max }) => (min, max),
                _ => (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1),
            };
            let digits = hex.then(|| scalar.size() * 2);
            if ui
                .add(number(&mut current, digits).range(min..=max))
                .changed()
            {
                *entry = Value::Int(current);
            }
        }
        FieldKind::Bytes { len } => {
            let current = entry.as_bytes().unwrap_or(&[]).to_vec();
            let mut text = hex::packed(&current);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .font(TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                if let Ok(mut bytes) = hex::parse(&text) {
                    bytes.resize(*len, 0);
                    *entry = Value::Bytes(bytes);
                }
            }
        }
        FieldKind::Text { len } => {
            let mut current = entry.as_text().unwrap_or("").to_owned();
            if ui.text_edit_singleline(&mut current).changed() {
                current.truncate(*len);
                *entry = Value::Text(current);
            }
        }
        FieldKind::Enum { variants, .. } => enum_widget(ui, &field.name, variants, entry),
        FieldKind::Bits { bits, repr } => bits_widget(ui, &field.name, *repr, bits, entry, hex),
        FieldKind::Checksum { .. } => {}
    }
}

fn enum_widget(ui: &mut Ui, id: &str, variants: &[EnumVariant], entry: &mut Value) {
    let current = entry.as_uint().unwrap_or(0);
    // A value with no matching variant is shown as-is rather than hidden: it may
    // well be what the equipment under test actually sends.
    let label = variants
        .iter()
        .find(|variant| variant.value == current)
        .map_or_else(|| format!("{current} (unnamed)"), |v| v.name.clone());

    ComboBox::from_id_salt(id)
        .selected_text(label)
        .show_ui(ui, |ui| {
            for variant in variants {
                if ui
                    .selectable_label(
                        variant.value == current,
                        format!("{} = {}", variant.name, variant.value),
                    )
                    .clicked()
                {
                    *entry = Value::Uint(variant.value);
                }
            }
        });
}

fn bits_widget(
    ui: &mut Ui,
    id: &str,
    repr: ScalarType,
    bits: &[BitDef],
    entry: &mut Value,
    hex: bool,
) {
    let mut current = entry.as_bits().cloned().unwrap_or_default();
    let mut changed = false;
    let positions = bit_positions(repr, bits);

    // A grid, so a bitfield mixing single bits and wider ones keeps its names
    // in one column, its positions in the next and its controls in a third,
    // instead of staggering all three.
    egui::Grid::new(("bits", id))
        .num_columns(3)
        .min_col_width(0.0)
        .show(ui, |ui| {
            for (bit, position) in bits.iter().zip(&positions) {
                let slot = current.entry(bit.name.clone()).or_insert(0);
                let wide = bit.width > 1;

                if wide {
                    ui.label(&bit.name);
                } else {
                    let mut on = *slot != 0;
                    if ui.checkbox(&mut on, &bit.name).changed() {
                        *slot = u64::from(on);
                        changed = true;
                    }
                }

                match position {
                    Some(position) => ui.label(RichText::new(format!("[{position}]")).weak()),
                    None => ui.label(RichText::new("[?]").color(ERROR)),
                };

                if wide {
                    let max = (1u64 << bit.width) - 1;
                    // Four bits to a digit, so a five-bit part still gets two.
                    let digits = hex.then(|| bit.width.div_ceil(4) as usize);
                    changed |= ui.add(number(slot, digits).range(0..=max)).changed();
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });

    if changed {
        *entry = Value::Bits(current);
    }
}

fn preview_and_send(ui: &mut Ui, state: &mut Session, engine: &EngineHandle, frame: &FrameDef) {
    let encoded = {
        let values = state.frames.values_mut(frame);
        codec::encode(frame, values)
    };

    ui.heading("Preview");
    // Shown even when the fields do not encode: a frame refused because a value
    // sits outside its subtype is precisely the one you want to keep looking at.
    hex_preview(ui, state, frame, encoded.as_deref().ok());
    if let Err(error) = &encoded {
        ui.colored_label(ERROR, error.to_string());
    }
    if let Some(note) = &state.frame_hex_note {
        ui.colored_label(WARNING, note);
    }

    let connected: Vec<_> = state
        .connections
        .iter()
        .filter(|(_, entry)| entry.status == ConnectionStatus::Connected)
        .map(|(id, _)| id.clone())
        .collect();

    let selected_label = state
        .frame_target
        .as_ref()
        .map_or_else(|| "choose...".to_owned(), |id| id.0.clone());

    ui.horizontal(|ui| {
        ui.label("Target connection:");
        ComboBox::from_id_salt("frame_target")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for id in &connected {
                    if ui
                        .selectable_label(state.frame_target.as_ref() == Some(id), &id.0)
                        .clicked()
                    {
                        state.frame_target = Some(id.clone());
                    }
                }
            });
    });

    let target_ready = state
        .frame_target
        .as_ref()
        .and_then(|id| state.status_of(id))
        == Some(ConnectionStatus::Connected);
    if let (Some(id), false) = (state.frame_target.as_ref(), target_ready) {
        ui.colored_label(
            WARNING,
            format!("\"{}\" is not connected. Reconnect it to send.", id.0),
        );
    }

    let can_send = target_ready && encoded.is_ok();
    if ui
        .add_enabled(
            can_send,
            egui::Button::new(RichText::new(format!("{} Send", icons::PAPER_PLANE_TILT))),
        )
        .clicked()
    {
        if let (Some(id), Ok(bytes)) = (state.frame_target.clone(), encoded) {
            engine.send_raw(id, bytes);
        }
    }
}

/// The encoded frame, editable: typing bytes drives the fields above.
///
/// The box only mirrors the encoder while it is not focused. Once it is, the
/// text is whatever was typed, and the fields follow it instead.
fn hex_preview(ui: &mut Ui, state: &mut Session, frame: &FrameDef, bytes: Option<&[u8]>) {
    let id = egui::Id::new(("frame_hex", &frame.name));
    // With nothing to mirror, the typed text stays put rather than being wiped.
    if let (false, Some(bytes)) = (ui.memory(|memory| memory.has_focus(id)), bytes) {
        state.frame_hex = hex::spaced(bytes);
    }

    let response = ui.add(
        egui::TextEdit::multiline(&mut state.frame_hex)
            .id(id)
            .font(TextStyle::Monospace)
            .desired_rows(2),
    );

    if response.changed() {
        let typed = state.frame_hex.clone();
        state.frame_hex_note = frames::apply_hex(state, frame, &typed);
    }
}
