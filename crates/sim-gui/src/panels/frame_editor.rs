use sim_core::frame::codec;
use sim_core::frame::value::Value;
use sim_core::frame::{BitDef, EnumVariant, FieldDef, FieldKind, FrameDef, ScalarType, ValueRange};
use sim_core::ConnectionStatus;

use egui::{Color32, ComboBox, RichText, ScrollArea, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::panels::number;
use crate::state::AppState;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const WARNING: Color32 = Color32::from_rgb(200, 120, 40);

pub fn show(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    // Taken unconditionally: bytes sent here with no frame to decode them into
    // are dropped now rather than surfacing later against an unrelated frame.
    let handed_over = state.pending_frame_hex.take();

    library_bar(ui, state);

    // Editing a copy, so what the list and the disk hold is untouched until
    // Save says otherwise.
    if state.frames.draft.is_some() {
        draft_editor(ui, state);
        return;
    }
    if state.frames.type_draft.is_some() {
        super::type_edit::editor(ui, state);
        return;
    }

    let empty = state.frames.is_empty();
    if empty {
        if state.frames.directory.is_some() && state.frames.failures.is_empty() {
            ui.label("No .toml frame definition in that folder.");
        }
    } else {
        frame_picker(ui, state);
    }
    // Offered even with no frame yet: a folder often starts with the types
    // everything in it is going to be built from.
    super::type_edit::library_bar(ui, state);
    show_failures(ui, state);
    if empty {
        return;
    }
    ui.separator();

    let Some(frame) = state.frames.selected_frame().cloned() else {
        return;
    };

    if let Some(bytes) = handed_over {
        let typed = to_hex_spaced(&bytes);
        state.frame_hex_note = apply_hex(state, &frame, &typed);
        state.frame_hex = typed;
    }

    // Read before the values are borrowed, the whole editor sharing one answer
    // rather than each field having its own.
    let hex = state.hex_values;
    let tree = build_tree(&frame.fields);
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

fn library_bar(ui: &mut Ui, state: &mut AppState) {
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

        ui.separator();

        if ui
            .add_enabled(
                state.frames.directory.is_some() && idle,
                egui::Button::new(format!("{} New", icons::FILE_PLUS)),
            )
            .on_hover_text("Start a frame from scratch")
            .clicked()
        {
            state.frames.begin_new(blank());
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
    });
    if let Some(directory) = &state.frames.directory {
        ui.label(RichText::new(directory.display().to_string()).weak());
    } else {
        ui.label("Pick the folder holding your frame .toml files.");
    }
}

/// What New starts from: one byte, the smallest thing that is still a frame.
fn blank() -> FrameDef {
    FrameDef::flat(
        "New frame",
        vec![FieldDef {
            name: "id".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: sim_core::frame::Endianness::default(),
            default: None,
            range: None,
        }],
    )
}

fn draft_editor(ui: &mut Ui, state: &mut AppState) {
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
    crate::layout::set_endian(&mut draft.frame, endian);
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
            save_draft(state);
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

/// Writes the draft out, choosing a file for one that has never had a home.
fn save_draft(state: &mut AppState) {
    let Some(directory) = state.frames.directory.clone() else {
        state.last_error = Some("No frames folder to save into.".to_owned());
        return;
    };
    let name = state
        .frames
        .draft
        .as_ref()
        .map(|draft| draft.frame.name.clone())
        .unwrap_or_default();

    let into = crate::frames::suggested_file(&directory, &name);
    if let Err(error) = state.frames.save_draft(&into) {
        state.last_error = Some(format!("{error:#}"));
    }
}

fn frame_picker(ui: &mut Ui, state: &mut AppState) {
    let names: Vec<String> = state
        .frames
        .frames()
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
                        // Whatever the note said, it said it about another frame.
                        state.frame_hex_note = None;
                    }
                }
            });
        if let Some(frame) = state.frames.selected_frame() {
            ui.label(RichText::new(format!("{} bytes", frame.size())).weak());
        }
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

/// One level of the field tree rebuilt from the dotted names an instantiated
/// type produces, so `zone.left` folds away with everything under it.
enum Entry<'a> {
    Field(&'a FieldDef),
    Group(Group<'a>),
}

struct Group<'a> {
    /// The last path segment, which is what the header shows.
    label: &'a str,
    /// The whole path, used to decide what belongs to this group.
    path: &'a str,
    /// Name of the first field inside, which unlike the path is always unique
    /// even when hand-written names interleave two blocks.
    salt: &'a str,
    entries: Vec<Entry<'a>>,
}

impl Entry<'_> {
    fn size(&self) -> usize {
        match self {
            Self::Field(field) => field.kind.size(),
            Self::Group(group) => group.entries.iter().map(Self::size).sum(),
        }
    }
}

fn build_tree(fields: &[FieldDef]) -> Vec<Entry<'_>> {
    let mut root = Vec::new();
    for field in fields {
        insert(&mut root, field, 0);
    }
    root
}

/// Files declare fields in wire order, so a group only ever extends the entry
/// that precedes it: display order can never drift from the byte order.
fn insert<'a>(entries: &mut Vec<Entry<'a>>, field: &'a FieldDef, at: usize) {
    let Some(dot) = field.name[at..].find('.') else {
        entries.push(Entry::Field(field));
        return;
    };
    let path = &field.name[..at + dot];
    let next = at + dot + 1;

    if let Some(Entry::Group(group)) = entries.last_mut() {
        if group.path == path {
            insert(&mut group.entries, field, next);
            return;
        }
    }
    let mut group = Group {
        label: &field.name[at..at + dot],
        path,
        salt: &field.name,
        entries: Vec::new(),
    };
    insert(&mut group.entries, field, next);
    entries.push(Entry::Group(group));
}

fn leaf_name(name: &str) -> &str {
    name.rfind('.').map_or(name, |at| &name[at + 1..])
}

fn show_entries(
    ui: &mut Ui,
    entries: &[Entry<'_>],
    values: &mut sim_core::frame::value::FieldValues,
    hex: bool,
) {
    // Consecutive fields share one grid so their columns line up; a group
    // interrupts the run because its rows are indented one level deeper.
    let mut run: Vec<&FieldDef> = Vec::new();
    for entry in entries {
        match entry {
            Entry::Field(field) => run.push(field),
            Entry::Group(group) => {
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
    let mut label = ui.label(RichText::new(leaf_name(&field.name)).strong());
    if let Some(description) = &field.description {
        label = label.on_hover_text(description);
    }
    if field.name.contains('.') {
        label = label.on_hover_text(&field.name);
    }
    let _ = label;

    ui.label(RichText::new(type_label(field)).weak());

    match &field.kind {
        FieldKind::Checksum { .. } => {
            ui.label(RichText::new("computed on send").weak());
        }
        kind => value_widget(ui, field, kind, values, hex),
    }
}

/// The declared type, shown next to every field so the layout is readable
/// without opening the TOML.
fn type_label(field: &FieldDef) -> String {
    let endian = match field.endian {
        sim_core::frame::Endianness::Big => "be",
        sim_core::frame::Endianness::Little => "le",
    };
    let constraint = field
        .range
        .as_ref()
        .map(|range| format!(" {}", range.describe()))
        .unwrap_or_default();
    match &field.kind {
        FieldKind::Scalar(scalar) if scalar.size() > 1 => {
            format!("{} {endian}{constraint}", scalar.name())
        }
        FieldKind::Scalar(scalar) => format!("{}{constraint}", scalar.name()),
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
                _ => (0, max_unsigned(*scalar)),
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

/// Where each sub-field sits in the word, written as a datasheet writes it.
///
/// The file lists them in packing order from the top of the word, which is what
/// the codec relies on, so the positions fall straight out of the widths. Shown
/// because nothing else on screen says whether the first row is the top bit or
/// the bottom one, and that is the first thing anyone checks against a
/// datasheet.
///
/// A width that does not fit what is left gives `None` rather than a wrong
/// number. The schema refuses such a frame at load, so this is a guard, not a
/// case anyone should see.
fn bit_positions(repr: ScalarType, bits: &[BitDef]) -> Vec<Option<String>> {
    let mut remaining = u32::try_from(repr.size()).unwrap_or(0) * 8;
    bits.iter()
        .map(|bit| {
            remaining = remaining.checked_sub(bit.width)?;
            let high = remaining + bit.width - 1;
            Some(if bit.width == 1 {
                high.to_string()
            } else {
                format!("{high}:{remaining}")
            })
        })
        .collect()
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

fn preview_and_send(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle, frame: &FrameDef) {
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
fn hex_preview(ui: &mut Ui, state: &mut AppState, frame: &FrameDef, bytes: Option<&[u8]>) {
    let id = egui::Id::new(("frame_hex", &frame.name));
    // With nothing to mirror, the typed text stays put rather than being wiped.
    if let (false, Some(bytes)) = (ui.memory(|memory| memory.has_focus(id)), bytes) {
        state.frame_hex = to_hex_spaced(bytes);
    }

    let response = ui.add(
        egui::TextEdit::multiline(&mut state.frame_hex)
            .id(id)
            .font(TextStyle::Monospace)
            .desired_rows(2),
    );

    if response.changed() {
        let typed = state.frame_hex.clone();
        state.frame_hex_note = apply_hex(state, frame, &typed);
    }
}

/// Decodes typed hex back into the field values.
///
/// Returns what the operator should know about: why nothing was applied, or
/// what the frame will not keep.
fn apply_hex(state: &mut AppState, frame: &FrameDef, typed: &str) -> Option<String> {
    let cleaned: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some("Not hexadecimal.".to_owned());
    }
    // Half a byte typed is someone mid-keystroke, not a mistake to point at.
    let bytes = parse_hex(&cleaned)?;
    if bytes.len() != frame.size() {
        return Some(format!(
            "{} bytes typed, the frame is {}.",
            bytes.len(),
            frame.size()
        ));
    }

    let decoded = match codec::decode(frame, &bytes) {
        Ok(decoded) => decoded,
        Err(error) => return Some(error.to_string()),
    };

    let values = state.frames.values_mut(frame);
    for field in &frame.fields {
        // Checksums are recomputed on encode, so writing one back would be
        // overwritten anyway; the mismatch below is the honest report.
        if matches!(field.kind, FieldKind::Checksum { .. }) {
            continue;
        }
        if let Some(value) = decoded.values.get(&field.name) {
            values.insert(field.name.clone(), value.clone());
        }
    }

    let mut notes = Vec::new();
    // Worth saying out loud: paste a capture with a bad checksum and the
    // preview will quietly show the corrected one a moment later.
    if !decoded.checksum_mismatches.is_empty() {
        let fields: Vec<&str> = decoded
            .checksum_mismatches
            .iter()
            .map(|mismatch| mismatch.field.as_str())
            .collect();
        notes.push(format!(
            "{} did not match; the preview will show the recomputed value.",
            fields.join(", ")
        ));
    }
    for violation in &decoded.range_violations {
        notes.push(format!(
            "{} is {}, outside {}.",
            violation.field, violation.found, violation.range
        ));
    }
    (!notes.is_empty()).then(|| notes.join(" "))
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

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::frame::Endianness;

    /// Two bytes each, so a group's reported size is unambiguous.
    fn field(name: &str) -> FieldDef {
        FieldDef {
            name: name.to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U16),
            endian: Endianness::Big,
            default: None,
            range: None,
        }
    }

    fn shape(entries: &[Entry<'_>]) -> String {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Field(field) => field.name.clone(),
                Entry::Group(group) => format!("{}({})", group.path, shape(&group.entries)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn nested_instances_nest_in_the_editor_too() {
        let fields = [
            field("header"),
            field("zone.left.led[0].mode"),
            field("zone.left.led[1].mode"),
            field("zone.left.accent.red"),
            field("zone.right.led[0].mode"),
            field("crc"),
        ];
        let tree = build_tree(&fields);

        assert_eq!(
            shape(&tree),
            "header \
             zone(\
             zone.left(\
             zone.left.led[0](zone.left.led[0].mode) \
             zone.left.led[1](zone.left.led[1].mode) \
             zone.left.accent(zone.left.accent.red)\
             ) \
             zone.right(zone.right.led[0](zone.right.led[0].mode))\
             ) \
             crc"
        );

        // Folding `zone` hides four fields, whatever the depth they sit at.
        assert_eq!(tree[1].size(), 8);
    }

    /// Four bytes, the last one a checksum, so the round trip is easy to read.
    const GUARDED: &str = r#"
name = "Guarded"
endian = "big"

[[field]]
name = "sync"
type = "u16"
default = 0xAA55

[[field]]
name = "mode"
type = "enum"
repr = "u8"
variants = { IDLE = 0, RUN = 1, FAULT = 2 }

[[field]]
name = "check"
type = "xor8"
covers = { from = "sync", to = "mode" }
"#;

    fn guarded() -> FrameDef {
        sim_core::frame::schema::from_toml(GUARDED).expect("fixture should parse")
    }

    #[test]
    fn typing_hex_drives_the_fields() {
        let frame = guarded();
        let mut state = AppState::default();

        // 0xAA ^ 0x55 ^ 0x02 = 0xFD
        assert_eq!(apply_hex(&mut state, &frame, "AA 55 02 FD"), None);

        let values = state.frames.values_mut(&frame);
        assert_eq!(values["sync"], Value::Uint(0xAA55));
        assert_eq!(values["mode"], Value::Uint(2));
        // Recomputed on encode, so it is never written back as a value.
        assert!(!values.contains_key("check"));
    }

    #[test]
    fn an_incomplete_byte_is_not_worth_complaining_about() {
        let frame = guarded();
        let mut state = AppState::default();

        state
            .frames
            .values_mut(&frame)
            .insert("mode".to_owned(), Value::Uint(2));

        // Mid-keystroke: silent, and what is already there is left alone.
        assert_eq!(apply_hex(&mut state, &frame, "AA 5"), None);
        assert_eq!(state.frames.values_mut(&frame)["mode"], Value::Uint(2));

        assert_eq!(
            apply_hex(&mut state, &frame, "AA ZZ"),
            Some("Not hexadecimal.".to_owned())
        );
    }

    #[test]
    fn a_short_frame_says_how_short() {
        let frame = guarded();
        let mut state = AppState::default();
        let note = apply_hex(&mut state, &frame, "AA 55").expect("should be reported");
        assert!(note.contains('2') && note.contains('4'), "got {note}");
    }

    #[test]
    fn a_wrong_checksum_is_applied_but_flagged() {
        let frame = guarded();
        let mut state = AppState::default();

        // Right bytes, deliberately wrong check byte.
        let note = apply_hex(&mut state, &frame, "AA 55 02 00").expect("should be reported");
        assert!(note.contains("check"), "got {note}");

        // The fields still took the pasted values: a capture with a bad
        // checksum is exactly what you want to look at.
        assert_eq!(state.frames.values_mut(&frame)["mode"], Value::Uint(2));
    }

    #[test]
    fn a_bitfield_says_where_each_of_its_parts_sits() {
        let frame = sim_core::frame::schema::from_toml(
            r#"
name = "Status"
[[field]]
name = "flags"
type = "bits"
repr = "u8"
bits = [
  { name = "armed",       width = 1 },
  { name = "heater_on",   width = 1 },
  { name = "link_up",     width = 1 },
  { name = "power_level", width = 2 },
  { name = "spare",       width = 3 },
]
"#,
        )
        .expect("should parse");
        let FieldKind::Bits { repr, bits } = &frame.fields[0].kind else {
            panic!("expected a bitfield");
        };

        // Listed from the top of the word, which is the order the codec packs
        // them in and the order the file declares them in.
        assert_eq!(
            bit_positions(*repr, bits),
            [
                Some("7".to_owned()),
                Some("6".to_owned()),
                Some("5".to_owned()),
                Some("4:3".to_owned()),
                Some("2:0".to_owned()),
            ]
        );
    }

    #[test]
    fn a_bitfield_wider_than_its_word_says_so_rather_than_lying() {
        // The schema refuses this at load, so it is a guard rather than a case
        // anyone should meet, but a wrong number would be worse than a question
        // mark.
        let bits = [
            BitDef {
                name: "big".to_owned(),
                width: 6,
            },
            BitDef {
                name: "too_big".to_owned(),
                width: 6,
            },
        ];
        assert_eq!(
            bit_positions(ScalarType::U8, &bits),
            [Some("7:2".to_owned()), None]
        );
    }

    #[test]
    fn a_repeated_builtin_stays_a_plain_row() {
        let fields = [field("sample[0]"), field("sample[1]")];
        assert_eq!(shape(&build_tree(&fields)), "sample[0] sample[1]");
    }

    #[test]
    fn display_order_never_drifts_from_wire_order() {
        // Hand-written names can interleave. Reuniting the two `a` blocks would
        // move `b.y` in the listing while it stays put in the bytes.
        let fields = [field("a.x"), field("b.y"), field("a.z")];
        assert_eq!(shape(&build_tree(&fields)), "a(a.x) b(b.y) a(a.z)");
    }
}
