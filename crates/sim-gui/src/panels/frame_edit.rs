//! Building a frame's fields with a mouse.
//!
//! Everything the file format can say about a field is offered here, and
//! nothing else: the kind picker lists the words `type =` accepts, so a
//! technician cannot name a kind that does not exist. What the editor cannot
//! express, it shows and refuses to touch rather than quietly rewording.

use egui::{collapsing_header::CollapsingState, ComboBox, Id, RichText, Ui};
use egui_phosphor::regular as icons;
use sim_core::frame::checksum::{ChecksumSpec, CrcSpec};
use sim_core::frame::value::Value;
use sim_core::frame::{
    BitDef, EnumVariant, FieldDef, FieldKind, FieldSpan, FrameDef, ScalarType, ValueRange,
};

use crate::layout;
use crate::panels::number;
use crate::state::AppState;

/// The scalars a field may be written as, in the order a datasheet lists them.
const SCALARS: [ScalarType; 10] = [
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

/// What one pass over the editor decided to do, applied once the drawing is
/// over so that nothing is read after it has been changed underneath.
enum Edit {
    Add(Option<usize>),
    Remove(usize),
    Move(usize, bool),
    Rename(usize, String),
    Kind(usize, FieldKind),
}

pub fn fields(ui: &mut Ui, state: &mut AppState) {
    let hex = state.hex_values;
    let Some(draft) = &mut state.frames.draft else {
        return;
    };
    layout(ui, &mut draft.frame, hex);
}

/// One pass over a field list, whoever it belongs to.
pub fn layout(ui: &mut Ui, list: &mut FrameDef, hex: bool) {
    // Drawn from a copy so a row may decide to remove itself without the rest
    // of the pass reading a list that has moved under it.
    let frame = list.clone();
    let mut edit = None;

    for (index, declared) in frame.declared.iter().enumerate() {
        field_row(ui, list, &frame, index, declared, hex, &mut edit);
    }

    ui.horizontal(|ui| {
        if ui
            .button(format!("{} Add field", icons::PLUS))
            .on_hover_text("Add a byte at the end, to be told what it is")
            .clicked()
        {
            edit = Some(Edit::Add(None));
        }
        ui.label(RichText::new(format!("{} bytes", frame.size())).weak());
    });

    match edit {
        Some(Edit::Add(after)) => layout::add_field(list, after, blank_field()),
        Some(Edit::Remove(index)) => layout::remove_field(list, index),
        Some(Edit::Move(index, down)) => layout::move_field(list, index, down),
        Some(Edit::Rename(index, name)) => layout::rename_field(list, index, &name),
        Some(Edit::Kind(index, kind)) => {
            if let Some(field) = layout::plain_field_mut(list, index) {
                // A default that still means something under the new kind is
                // kept, the rest starting clean rather than half converted.
                field.default = field
                    .default
                    .take()
                    .and_then(|value| value.coerced_to(&kind));
                field.range = None;
                field.kind = kind;
            }
        }
        None => {}
    }
}

fn field_row(
    ui: &mut Ui,
    layout: &mut FrameDef,
    frame: &FrameDef,
    index: usize,
    declared: &str,
    hex: bool,
    edit: &mut Option<Edit>,
) {
    let id = Id::new(("frame_field", index, declared));
    let expanded = layout::is_expanded(layout, declared);
    CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            if ui
                .add_enabled(index > 0, egui::Button::new(icons::ARROW_UP))
                .clicked()
            {
                *edit = Some(Edit::Move(index, false));
            }
            if ui
                .add_enabled(
                    index + 1 < frame.declared.len(),
                    egui::Button::new(icons::ARROW_DOWN),
                )
                .clicked()
            {
                *edit = Some(Edit::Move(index, true));
            }
            if ui.button(icons::TRASH).on_hover_text("Remove").clicked() {
                *edit = Some(Edit::Remove(index));
            }

            if expanded {
                ui.label(RichText::new(declared).strong());
                ui.label(
                    RichText::new(format!(
                        "{} fields from a type",
                        frame.expansion_of(declared).len()
                    ))
                    .weak(),
                );
                return;
            }

            let mut name = declared.to_owned();
            if ui.text_edit_singleline(&mut name).changed() {
                *edit = Some(Edit::Rename(index, name));
            }
            kind_picker(ui, layout, frame, index, edit);
        })
        .body(|ui| {
            if expanded {
                for at in frame.expansion_of(declared) {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            frame.fields[at].name,
                            frame.fields[at].kind.type_name()
                        ))
                        .weak(),
                    );
                }
                ui.label(RichText::new("Stated as a type, and edited where the type is.").weak());
                return;
            }
            details(ui, layout, frame, index, hex);
        });
}

fn kind_picker(
    ui: &mut Ui,
    layout: &FrameDef,
    frame: &FrameDef,
    index: usize,
    edit: &mut Option<Edit>,
) {
    let Some(field) = layout::plain_field(layout, index) else {
        return;
    };
    let current = label_of(&field.kind);
    ComboBox::from_id_salt(("frame_kind", index))
        .selected_text(current.clone())
        .width(ui.spacing().interact_size.x * 3.0)
        .show_ui(ui, |ui| {
            for label in kind_labels() {
                // What cannot exist here is not offered, rather than offered
                // and ignored: a checksum at the top of a frame has nothing in
                // front of it to cover.
                let Some(kind) = kind_named(&label, frame, index) else {
                    continue;
                };
                if ui.selectable_label(current == label, &label).clicked() && current != label {
                    *edit = Some(Edit::Kind(index, kind));
                }
            }
        });
}

/// The kind-specific part of a field, below the row that names it.
fn details(ui: &mut Ui, layout: &mut FrameDef, frame: &FrameDef, index: usize, hex: bool) {
    let Some(field) = layout::plain_field_mut(layout, index) else {
        return;
    };

    let mut description = field.description.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Description:");
        if ui.text_edit_singleline(&mut description).changed() {
            field.description = (!description.trim().is_empty()).then_some(description);
        }
    });

    match &mut field.kind {
        FieldKind::Scalar(scalar) => {
            let scalar = *scalar;
            scalar_details(ui, field, scalar, hex);
        }
        FieldKind::Bytes { len } | FieldKind::Text { len } => {
            ui.horizontal(|ui| {
                ui.label("Length:");
                ui.add(number(len, None).range(1..=4096));
                ui.label(RichText::new("bytes").weak());
            });
        }
        FieldKind::Enum { repr, variants } => enum_details(ui, index, *repr, variants, hex),
        FieldKind::Bits { repr, bits } => bits_details(ui, index, *repr, bits),
        FieldKind::Checksum { covers, .. } => {
            let span = *covers;
            coverage_picker(ui, layout, frame, index, span);
        }
    }
}

fn scalar_details(ui: &mut Ui, field: &mut FieldDef, scalar: ScalarType, hex: bool) {
    let digits = scalar.size() * 2;
    let showing = hex
        .then_some(digits)
        .filter(|_| scalar.is_unsigned_integer());

    ui.horizontal(|ui| {
        let mut has_default = field.default.is_some();
        if ui.checkbox(&mut has_default, "Default:").changed() {
            field.default = has_default.then(|| {
                Value::Uint(0)
                    .coerced_to(&field.kind)
                    .unwrap_or(Value::Uint(0))
            });
        }
        let Some(value) = &mut field.default else {
            ui.label(RichText::new("none").weak());
            return;
        };
        match value {
            Value::Uint(held) => {
                ui.add(number(held, showing));
            }
            Value::Int(held) => {
                ui.add(number(held, None));
            }
            Value::Float(held) => {
                ui.add(number(held, None));
            }
            _ => {
                ui.label(RichText::new("set in the file").weak());
            }
        }
    });

    let representable = scalar.representable();
    ui.horizontal(|ui| {
        let mut narrowed = field.range.is_some();
        if ui
            .checkbox(&mut narrowed, "Range:")
            .on_hover_text("Refuse values the protocol does not allow, as an Ada subtype would")
            .changed()
        {
            field.range = narrowed.then_some(representable);
        }
        match &mut field.range {
            Some(ValueRange::Uint { min, max }) => {
                ui.add(number(min, showing));
                ui.label("to");
                ui.add(number(max, showing));
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
                ui.label(RichText::new(representable.describe()).weak());
            }
        }
    });
}

fn enum_details(
    ui: &mut Ui,
    index: usize,
    repr: ScalarType,
    variants: &mut Vec<EnumVariant>,
    hex: bool,
) {
    ui.label(RichText::new(format!("{} on the wire", repr.name())).weak());
    let digits = repr.size() * 2;
    let mut remove = None;
    for (at, variant) in variants.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut variant.name)
                    .id_salt(("variant", index, at))
                    .desired_width(ui.spacing().interact_size.x * 4.0),
            );
            ui.add(number(&mut variant.value, hex.then_some(digits)));
            if ui.button(icons::MINUS).clicked() {
                remove = Some(at);
            }
        });
    }
    if let Some(at) = remove {
        variants.remove(at);
    }
    if ui.button(format!("{} Add value", icons::PLUS)).clicked() {
        let next = variants.iter().map(|held| held.value).max().unwrap_or(0) + 1;
        variants.push(EnumVariant {
            name: format!("VALUE{next}"),
            value: next,
        });
    }
}

fn bits_details(ui: &mut Ui, index: usize, repr: ScalarType, bits: &mut Vec<BitDef>) {
    // A scalar is eight bytes at most, so the width always fits.
    let room = u32::try_from(repr.size() * 8).unwrap_or(64);
    let used: u32 = bits.iter().map(|bit| bit.width).sum();
    ui.label(
        RichText::new(format!(
            "{used} of {room} bits packed, most significant first"
        ))
        .weak(),
    );

    let mut remove = None;
    for (at, bit) in bits.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut bit.name)
                    .id_salt(("bit", index, at))
                    .desired_width(ui.spacing().interact_size.x * 4.0),
            );
            ui.add(number(&mut bit.width, None).range(1..=room));
            ui.label(RichText::new("bits wide").weak());
            if ui.button(icons::MINUS).clicked() {
                remove = Some(at);
            }
        });
    }
    if let Some(at) = remove {
        bits.remove(at);
    }
    // The widths have to add up exactly, so the button stops offering more once
    // there is nothing left to give.
    if ui
        .add_enabled(
            used < room,
            egui::Button::new(format!("{} Add bit", icons::PLUS)),
        )
        .clicked()
    {
        bits.push(BitDef {
            name: format!("bit{}", bits.len()),
            width: 1,
        });
    }
}

/// Which stretch of the frame a checksum protects, named rather than counted.
fn coverage_picker(
    ui: &mut Ui,
    layout: &mut FrameDef,
    frame: &FrameDef,
    index: usize,
    span: FieldSpan,
) {
    // Only what comes before it: a checksum over bytes that are written after
    // it has nothing to read when its turn comes.
    let at = frame
        .declared
        .get(index)
        .and_then(|name| frame.field_index(name))
        .unwrap_or(frame.fields.len());
    let names: Vec<String> = frame.fields[..at]
        .iter()
        .map(|field| field.name.clone())
        .collect();
    let mut from = names.get(span.from).cloned().unwrap_or_default();
    let mut to = names.get(span.to).cloned().unwrap_or_default();
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("Covers:");
        changed |= end_picker(ui, ("cover_from", index), &names, &mut from);
        ui.label("to");
        changed |= end_picker(ui, ("cover_to", index), &names, &mut to);
        let bytes = frame.fields[span.from.min(span.to)..=span.to.max(span.from)]
            .iter()
            .map(|field| field.kind.size())
            .sum::<usize>();
        ui.label(RichText::new(format!("{bytes} bytes")).weak());
    });

    if changed {
        layout::set_coverage(layout, index, &from, &to);
    }
}

fn end_picker(ui: &mut Ui, salt: (&str, usize), names: &[String], chosen: &mut String) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt(salt)
        .selected_text(chosen.clone())
        .show_ui(ui, |ui| {
            for name in names {
                if ui.selectable_label(chosen == name, name).clicked() {
                    chosen.clone_from(name);
                    changed = true;
                }
            }
        });
    changed
}

/// What New starts a field as: one byte, to be told what it is.
fn blank_field() -> FieldDef {
    FieldDef {
        name: "field".to_owned(),
        description: None,
        kind: FieldKind::Scalar(ScalarType::U8),
        endian: sim_core::frame::Endianness::Big,
        default: None,
        range: None,
    }
}

/// Every word the file's `type =` accepts, which is exactly what the picker
/// offers.
fn kind_labels() -> Vec<String> {
    let mut labels: Vec<String> = SCALARS
        .iter()
        .map(|scalar| scalar.name().to_owned())
        .collect();
    labels.extend(["bytes", "text", "enum", "bits", "xor8"].map(ToOwned::to_owned));
    labels.extend([8, 16, 32].map(|width| format!("sum{width}")));
    labels.extend(
        CrcSpec::preset_names()
            .iter()
            .map(|name| (*name).to_owned()),
    );
    labels
}

fn label_of(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Scalar(scalar) => scalar.name().to_owned(),
        FieldKind::Bytes { .. } => "bytes".to_owned(),
        FieldKind::Text { .. } => "text".to_owned(),
        FieldKind::Enum { .. } => "enum".to_owned(),
        FieldKind::Bits { .. } => "bits".to_owned(),
        FieldKind::Checksum { spec, .. } => match spec {
            ChecksumSpec::Xor8 => "xor8".to_owned(),
            ChecksumSpec::Sum { width_bytes } => format!("sum{}", width_bytes * 8),
            ChecksumSpec::Crc(crc) => crc
                .preset_name()
                .map_or_else(|| format!("crc{}", crc.width_bits), ToOwned::to_owned),
        },
    }
}

/// A field of the named kind, starting from something that already encodes.
///
/// A checksum starts covering everything in front of it, which is both the
/// commonest answer and the only one that is certainly a valid range.
fn kind_named(label: &str, frame: &FrameDef, index: usize) -> Option<FieldKind> {
    if let Some(scalar) = ScalarType::parse(label) {
        return Some(FieldKind::Scalar(scalar));
    }
    let spec = match label {
        "bytes" => return Some(FieldKind::Bytes { len: 1 }),
        "text" => return Some(FieldKind::Text { len: 8 }),
        "enum" => {
            return Some(FieldKind::Enum {
                repr: ScalarType::U8,
                variants: vec![EnumVariant {
                    name: "VALUE0".to_owned(),
                    value: 0,
                }],
            })
        }
        "bits" => {
            return Some(FieldKind::Bits {
                repr: ScalarType::U8,
                bits: vec![BitDef {
                    name: "value".to_owned(),
                    width: 8,
                }],
            })
        }
        "xor8" => ChecksumSpec::Xor8,
        "sum8" => ChecksumSpec::Sum { width_bytes: 1 },
        "sum16" => ChecksumSpec::Sum { width_bytes: 2 },
        "sum32" => ChecksumSpec::Sum { width_bytes: 4 },
        preset => ChecksumSpec::Crc(CrcSpec::preset(preset)?),
    };
    // Its own position among the wire fields, which is where the run in front
    // of it ends.
    let at = frame.field_index(frame.declared.get(index)?)?;
    let to = at.checked_sub(1)?;
    Some(FieldKind::Checksum {
        spec,
        covers: FieldSpan { from: 0, to },
    })
}
