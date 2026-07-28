use sim_core::frame::codec;
use sim_core::frame::value::Value;
use sim_core::frame::{BitDef, EnumVariant, FieldDef, FieldKind, FrameDef, ScalarType};
use sim_core::ConnectionStatus;

use egui::{Color32, ComboBox, DragValue, RichText, ScrollArea, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::state::AppState;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);

pub fn show(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    library_bar(ui, state);

    if state.frames.frames.is_empty() {
        if state.frames.directory.is_some() && state.frames.failures.is_empty() {
            ui.label("No .toml frame definition in that folder.");
        }
        show_failures(ui, state);
        return;
    }

    frame_picker(ui, state);
    show_failures(ui, state);
    ui.separator();

    let Some(frame) = state.frames.selected_frame().cloned() else {
        return;
    };

    ScrollArea::vertical()
        .id_salt("frame_fields")
        .max_height(ui.available_height() * 0.55)
        .show(ui, |ui| {
            let values = state.frames.values_mut(&frame);
            egui::Grid::new("frame_field_grid")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    for field in &frame.fields {
                        field_row(ui, field, values);
                        ui.end_row();
                    }
                });
        });

    ui.separator();
    preview_and_send(ui, state, engine, &frame);
}

fn library_bar(ui: &mut Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        if ui
            .button(RichText::new(format!(
                "{} Frames folder",
                icons::FOLDER_OPEN
            )))
            .clicked()
        {
            if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                state.frames.load_from(directory);
            }
        }
        if state.frames.directory.is_some()
            && ui
                .button(RichText::new(format!("{} Reload", icons::ARROWS_CLOCKWISE)))
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

fn frame_picker(ui: &mut Ui, state: &mut AppState) {
    let names: Vec<String> = state
        .frames
        .frames
        .iter()
        .map(|frame| frame.name.clone())
        .collect();
    let selected_label = state
        .frames
        .selected
        .and_then(|index| names.get(index).cloned())
        .unwrap_or_else(|| "choose...".to_owned());

    ui.horizontal(|ui| {
        ui.label("Frame:");
        ComboBox::from_id_salt("frame_pick")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for (index, name) in names.iter().enumerate() {
                    if ui
                        .selectable_label(state.frames.selected == Some(index), name)
                        .clicked()
                    {
                        state.frames.selected = Some(index);
                    }
                }
            });
        if let Some(frame) = state.frames.selected_frame() {
            ui.label(RichText::new(format!("{} bytes", frame.size())).weak());
        }
        if ui
            .button(icons::ARROW_COUNTER_CLOCKWISE)
            .on_hover_text("Reset every field to its default")
            .clicked()
        {
            if let Some(frame) = state.frames.selected_frame().cloned() {
                state.frames.reset_values(&frame);
            }
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

fn show_failures(ui: &mut Ui, state: &AppState) {
    for (file, reason) in &state.frames.failures {
        ui.colored_label(ERROR, format!("{file}: {reason}"));
    }
}

fn field_row(ui: &mut Ui, field: &FieldDef, values: &mut sim_core::frame::value::FieldValues) {
    let mut label = ui.label(RichText::new(&field.name).strong());
    if let Some(description) = &field.description {
        label = label.on_hover_text(description);
    }
    let _ = label;

    ui.label(RichText::new(type_label(field)).weak());

    match &field.kind {
        FieldKind::Checksum { .. } => {
            ui.label(RichText::new("computed on send").weak());
        }
        kind => value_widget(ui, field, kind, values),
    }
}

/// The declared type, shown next to every field so the layout is readable
/// without opening the TOML.
fn type_label(field: &FieldDef) -> String {
    let endian = match field.endian {
        sim_core::frame::Endianness::Big => "be",
        sim_core::frame::Endianness::Little => "le",
    };
    match &field.kind {
        FieldKind::Scalar(scalar) if scalar.size() > 1 => format!("{} {endian}", scalar.name()),
        FieldKind::Scalar(scalar) => scalar.name().to_owned(),
        FieldKind::Bytes { len } => format!("bytes[{len}]"),
        FieldKind::Text { len } => format!("text[{len}]"),
        FieldKind::Enum { repr, .. } => format!("enum {}", repr.name()),
        FieldKind::Bits { repr, .. } => format!("bits {}", repr.name()),
        FieldKind::Checksum { spec, .. } => match spec {
            sim_core::frame::checksum::ChecksumSpec::Crc(crc) => crc
                .preset_name()
                .map_or_else(|| format!("crc{}", crc.width_bits), ToOwned::to_owned),
            sim_core::frame::checksum::ChecksumSpec::Xor8 => "xor8".to_owned(),
            sim_core::frame::checksum::ChecksumSpec::Sum { width_bytes } => {
                format!("sum{}", width_bytes * 8)
            }
        },
    }
}

fn value_widget(
    ui: &mut Ui,
    field: &FieldDef,
    kind: &FieldKind,
    values: &mut sim_core::frame::value::FieldValues,
) {
    let entry = values.entry(field.name.clone()).or_insert(Value::Uint(0));

    match kind {
        FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => {
            let mut current = entry.as_float().unwrap_or(0.0);
            if ui.add(DragValue::new(&mut current).speed(0.1)).changed() {
                *entry = Value::Float(current);
            }
        }
        FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
            let mut current = entry.as_uint().unwrap_or(0);
            let max = max_unsigned(*scalar);
            // Decimal rather than hex: egui's hex mode shows no 0x prefix, so
            // typing "10" would silently mean 16. The byte preview below already
            // gives the hexadecimal view.
            if ui
                .add(DragValue::new(&mut current).range(0..=max))
                .changed()
            {
                *entry = Value::Uint(current);
            }
        }
        FieldKind::Scalar(scalar) => {
            let mut current = entry.as_int().unwrap_or(0);
            let bits = scalar.size() * 8;
            let min = -(1i64 << (bits - 1));
            let max = (1i64 << (bits - 1)) - 1;
            if ui
                .add(DragValue::new(&mut current).range(min..=max))
                .changed()
            {
                *entry = Value::Int(current);
            }
        }
        FieldKind::Bytes { len } => {
            let current = entry.as_bytes().unwrap_or(&[]).to_vec();
            let mut text = to_hex(&current);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .font(TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                if let Some(mut bytes) = parse_hex(&text) {
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
        FieldKind::Bits { bits, .. } => bits_widget(ui, bits, entry),
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

fn bits_widget(ui: &mut Ui, bits: &[BitDef], entry: &mut Value) {
    let mut current = entry.as_bits().cloned().unwrap_or_default();
    let mut changed = false;

    ui.vertical(|ui| {
        for bit in bits {
            let slot = current.entry(bit.name.clone()).or_insert(0);
            ui.horizontal(|ui| {
                if bit.width == 1 {
                    let mut on = *slot != 0;
                    if ui.checkbox(&mut on, &bit.name).changed() {
                        *slot = u64::from(on);
                        changed = true;
                    }
                } else {
                    ui.label(format!("{} ({} b)", bit.name, bit.width));
                    let max = (1u64 << bit.width) - 1;
                    changed |= ui.add(DragValue::new(slot).range(0..=max)).changed();
                }
            });
        }
    });

    if changed {
        *entry = Value::Bits(current);
    }
}

fn preview_and_send(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle, frame: &FrameDef) {
    let encoded = {
        let values = state.frames.values_mut(frame);
        codec::encode(frame, values)
    };

    ui.heading("Preview");
    match &encoded {
        Ok(bytes) => {
            ui.label(
                RichText::new(to_hex_spaced(bytes))
                    .text_style(TextStyle::Monospace)
                    .strong(),
            );
        }
        Err(error) => {
            ui.colored_label(ERROR, error.to_string());
        }
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
            Color32::from_rgb(200, 120, 40),
            format!("\"{}\" is not connected — reconnect it to send.", id.0),
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

fn max_unsigned(scalar: ScalarType) -> u64 {
    let bits = scalar.size() * 8;
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02X}");
        out
    })
}

fn to_hex_spaced(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02X}");
        out
    })
}

fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}
