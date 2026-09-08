//! The fields behind one traffic row.
//!
//! Bytes are read through a definition the operator picks rather than one the
//! app guesses at. Only definitions of exactly the row's length are offered:
//! anything else cannot be what was captured, and a list of every frame in the
//! folder would make the choice harder than reading the hex.

use egui::{Color32, ComboBox, Grid, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icons;

use sim_core::frame::codec::{self, Decoded};
use sim_core::frame::value::Value;
use sim_core::frame::{BitDef, FieldDef, FieldKind, FrameDef, ScalarType};

use sim_session::hex;
use sim_session::kinds::bit_positions;
use sim_session::reading::{describe, unsigned, Reading};
use sim_session::state::LogEntry;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const WARNING: Color32 = Color32::from_rgb(200, 120, 40);
const GOOD: Color32 = Color32::from_rgb(40, 160, 90);

/// How tall the pane would like to be: the header, plus a line for every row
/// it is about to draw.
///
/// Only ever a starting size. What it asks for is capped by the caller, and
/// dropped entirely once the pane has been dragged to a size by hand.
#[expect(
    clippy::cast_precision_loss,
    reason = "a frame with more fields than an f32 counts exactly is not one anyone has"
)]
pub fn wanted_height(reading: &Reading<'_>, ui: &Ui) -> f32 {
    let line = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    (reading.lines() + 2) as f32 * line
}

/// Draws the fields of `entry`.
///
/// Returns false once the pane has been closed.
pub fn show(
    ui: &mut Ui,
    reading: &Reading<'_>,
    entry: &LogEntry,
    decode_as: &mut Option<String>,
    hex_values: bool,
) -> bool {
    let mut open = true;
    ui.horizontal(|ui| {
        if ui
            .small_button(icons::X)
            .on_hover_text("Stop showing the fields")
            .clicked()
        {
            open = false;
        }
        ui.label(RichText::new(format!("{} bytes", entry.bytes.len())).weak());
        ui.separator();
        picker(ui, reading.candidates(), decode_as);
    });

    let Some(frame) = reading.chosen() else {
        if let Some(nothing) = reading.nothing() {
            ui.label(RichText::new(nothing).weak());
        }
        return open;
    };

    let decoded = match codec::decode(frame, &entry.bytes) {
        Ok(decoded) => decoded,
        // Unreachable while the candidates are filtered by length, which is the
        // only thing decoding refuses outright.
        Err(error) => {
            ui.colored_label(ERROR, error.to_string());
            return open;
        }
    };

    ScrollArea::vertical()
        .id_salt("frame_detail")
        .show(ui, |ui| {
            fields(ui, frame, &entry.bytes, &decoded, hex_values);
        });
    open
}

fn picker(ui: &mut Ui, candidates: &[&FrameDef], decode_as: &mut Option<String>) {
    ui.label("Read as:");
    let label = decode_as.clone().unwrap_or_else(|| "nothing".to_owned());
    ComboBox::from_id_salt("frame_detail_pick")
        .selected_text(label)
        .show_ui(ui, |ui| {
            for frame in candidates {
                let chosen = decode_as.as_deref() == Some(frame.name.as_str());
                if ui.selectable_label(chosen, frame.name.as_str()).clicked() {
                    *decode_as = Some(frame.name.clone());
                }
            }
        });
}

fn fields(ui: &mut Ui, frame: &FrameDef, bytes: &[u8], decoded: &Decoded, hex: bool) {
    Grid::new("frame_detail_fields")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            for (index, field) in frame.fields.iter().enumerate() {
                let offset = frame.offset_of(index);
                let end = offset + field.kind.size();
                let label = ui.label(RichText::new(&field.name).strong());
                if let Some(description) = &field.description {
                    label.on_hover_text(description);
                }
                ui.label(RichText::new(format!("{offset}..{end}")).weak().monospace());
                ui.label(RichText::new(hex::spaced(&bytes[offset..end])).monospace());
                value_cell(ui, field, decoded, hex);
                ui.end_row();

                if let FieldKind::Bits { repr, bits } = &field.kind {
                    bit_rows(ui, field, *repr, bits, decoded, hex);
                }
            }
        });
}

fn value_cell(ui: &mut Ui, field: &FieldDef, decoded: &Decoded, hex: bool) {
    ui.horizontal(|ui| {
        match decoded.values.get(&field.name) {
            Some(value) => ui.label(describe(field, value, hex)),
            None => ui.label(""),
        };
        if let Some((color, note)) = note(field, decoded) {
            ui.colored_label(color, note);
        }
    });
}

/// One row per sub-field, under the word holding them.
///
/// A cleared flag is drawn weak so that a fault standing out of a column of
/// zeroes is the thing the eye lands on.
fn bit_rows(
    ui: &mut Ui,
    field: &FieldDef,
    repr: ScalarType,
    bits: &[BitDef],
    decoded: &Decoded,
    hex: bool,
) {
    let Some(Value::Bits(set)) = decoded.values.get(&field.name) else {
        return;
    };
    for (bit, position) in bits.iter().zip(bit_positions(repr, bits)) {
        ui.label(RichText::new(format!("   {}", bit.name)).weak());
        ui.label(
            RichText::new(position.unwrap_or_default())
                .weak()
                .monospace(),
        );
        ui.label("");
        let held = set.get(&bit.name).copied().unwrap_or_default();
        let text = RichText::new(unsigned(held, bit.width.div_ceil(4) as usize, hex)).monospace();
        ui.label(if held == 0 {
            text.weak()
        } else {
            text.strong()
        });
        ui.end_row();
    }
}

/// What the operator has to know about this field, if anything.
fn note(field: &FieldDef, decoded: &Decoded) -> Option<(Color32, String)> {
    if let Some(mismatch) = decoded
        .checksum_mismatches
        .iter()
        .find(|mismatch| mismatch.field == field.name)
    {
        return Some((
            ERROR,
            format!(
                "expected {}",
                unsigned(mismatch.expected, field.kind.size() * 2, true)
            ),
        ));
    }
    if let Some(violation) = decoded
        .range_violations
        .iter()
        .find(|violation| violation.field == field.name)
    {
        return Some((WARNING, format!("outside {}", violation.range)));
    }
    matches!(field.kind, FieldKind::Checksum { .. }).then(|| (GOOD, "ok".to_owned()))
}
