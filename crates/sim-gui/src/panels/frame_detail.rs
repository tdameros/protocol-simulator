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

use crate::frames::FrameLibrary;
use crate::panels::{bit_positions, printable, spaced_hex};
use crate::state::LogEntry;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const WARNING: Color32 = Color32::from_rgb(200, 120, 40);
const GOOD: Color32 = Color32::from_rgb(40, 160, 90);

/// What a row can be read as, settled before anything is drawn.
///
/// Held apart from the drawing so the pane can be given a height that suits
/// what it is about to show, which is decided by the caller owning the room.
pub struct Reading<'a> {
    /// Every definition of exactly the row's length.
    candidates: Vec<&'a FrameDef>,
    /// The one being read through, if that is settled.
    chosen: Option<&'a FrameDef>,
    /// Nothing to draw, and why.
    empty: Option<String>,
}

/// Works out what `entry` can be read as, and settles `decode_as`.
pub fn read<'a>(
    frames: &'a FrameLibrary,
    entry: &LogEntry,
    decode_as: &mut Option<String>,
) -> Reading<'a> {
    let candidates: Vec<&FrameDef> = frames
        .frames()
        .filter(|frame| frame.size() == entry.bytes.len())
        .collect();
    pick(&candidates, decode_as);
    let chosen = decode_as
        .as_deref()
        .and_then(|name| candidates.iter().find(|frame| frame.name == name))
        .copied();
    let empty = chosen
        .is_none()
        .then(|| nothing_to_read(frames, &candidates, entry));
    Reading {
        candidates,
        chosen,
        empty,
    }
}

impl Reading<'_> {
    /// How tall the pane would like to be: the header, plus a line for every
    /// row it is about to draw.
    ///
    /// Only ever a starting size. What it asks for is capped by the caller, and
    /// dropped entirely once the pane has been dragged to a size by hand.
    pub fn wanted_height(&self, ui: &Ui) -> f32 {
        let line = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let lines = self.chosen.map_or(1, |frame| {
            frame
                .fields
                .iter()
                .map(|field| match &field.kind {
                    FieldKind::Bits { bits, .. } => 1 + bits.len(),
                    _ => 1,
                })
                .sum()
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "a frame with more fields than an f32 counts exactly is not one anyone has"
        )]
        let rows = (lines + 2) as f32;
        rows * line
    }
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
        picker(ui, &reading.candidates, decode_as);
    });

    let Some(frame) = reading.chosen else {
        if let Some(empty) = &reading.empty {
            ui.label(RichText::new(empty).weak());
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

/// Settles which definition the row is read through.
///
/// A choice made against one row means nothing against a row of another
/// length, so it lapses rather than decoding the wrong thing. And one candidate
/// is not a choice: asking for it would cost a click on every row of a protocol
/// whose messages all have distinct lengths.
fn pick(candidates: &[&FrameDef], decode_as: &mut Option<String>) {
    if decode_as
        .as_deref()
        .is_some_and(|name| !candidates.iter().any(|frame| frame.name == name))
    {
        *decode_as = None;
    }
    if decode_as.is_none() {
        if let [only] = candidates {
            *decode_as = Some(only.name.clone());
        }
    }
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

/// Why there are no fields on screen, in the operator's terms.
fn nothing_to_read(frames: &FrameLibrary, candidates: &[&FrameDef], entry: &LogEntry) -> String {
    if frames.is_empty() {
        return "Open a frames folder to read these bytes as fields.".to_owned();
    }
    if candidates.is_empty() {
        return format!("No frame definition is {} bytes.", entry.bytes.len());
    }
    "Pick the frame these bytes are.".to_owned()
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
                ui.label(RichText::new(spaced_hex(&bytes[offset..end])).monospace());
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

fn describe(field: &FieldDef, value: &Value, hex: bool) -> String {
    let digits = field.kind.size() * 2;
    match (&field.kind, value) {
        (FieldKind::Enum { variants, .. }, Value::Uint(raw)) => {
            let name = variants
                .iter()
                .find(|variant| variant.value == *raw)
                .map_or("unknown", |variant| variant.name.as_str());
            format!("{name} ({})", unsigned(*raw, digits, hex))
        }
        // The packed word, since the rows underneath carry the sub-fields.
        (_, Value::Bits(_)) => String::new(),
        (_, Value::Uint(raw)) => unsigned(*raw, digits, hex),
        (_, Value::Int(raw)) => raw.to_string(),
        (_, Value::Float(raw)) => raw.to_string(),
        (_, Value::Text(text)) => format!("{text:?}"),
        // Whose hex is already in the column before this one.
        (_, Value::Bytes(raw)) => printable(raw),
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

/// Padded to the width of what holds it, so a u16 reads 0x00FF and a column of
/// them lines up.
fn unsigned(value: u64, digits: usize, hex: bool) -> String {
    if hex {
        format!("0x{value:0digits$X}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::frame::{Endianness, FieldDef};

    fn frame(name: &str, bytes: usize) -> FrameDef {
        let fields = (0..bytes)
            .map(|index| FieldDef {
                name: format!("byte{index}"),
                description: None,
                kind: FieldKind::Scalar(ScalarType::U8),
                endian: Endianness::default(),
                default: None,
                range: None,
            })
            .collect();
        FrameDef::flat(name, fields)
    }

    #[test]
    fn the_only_candidate_is_taken_without_asking() {
        let only = frame("Status", 3);
        let mut chosen = None;
        pick(&[&only], &mut chosen);
        assert_eq!(chosen.as_deref(), Some("Status"));
    }

    #[test]
    fn several_candidates_wait_for_an_answer() {
        let (status, limits) = (frame("Status", 3), frame("Limits", 3));
        let mut chosen = None;
        pick(&[&status, &limits], &mut chosen);
        assert_eq!(chosen, None, "picking one of them is the operator's to do");
    }

    #[test]
    fn a_choice_survives_the_next_row_of_the_same_shape() {
        let (status, limits) = (frame("Status", 3), frame("Limits", 3));
        let mut chosen = Some("Limits".to_owned());
        pick(&[&status, &limits], &mut chosen);
        assert_eq!(
            chosen.as_deref(),
            Some("Limits"),
            "stepping down a list of one message type costs no clicks"
        );
    }

    #[test]
    fn a_choice_that_no_longer_fits_lapses() {
        let heartbeat = frame("Heartbeat", 5);
        let mut chosen = Some("Status".to_owned());
        pick(&[&heartbeat], &mut chosen);
        assert_eq!(
            chosen.as_deref(),
            Some("Heartbeat"),
            "the row is read as what it can be, never as what it cannot"
        );
    }

    #[test]
    fn nothing_of_that_length_leaves_nothing_chosen() {
        let mut chosen = Some("Status".to_owned());
        pick(&[], &mut chosen);
        assert_eq!(chosen, None);
    }
}
