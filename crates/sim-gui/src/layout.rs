//! Editing a field list, whether it belongs to a frame or to a shared type that
//! groups fields into one.
//!
//! The two are the same problem: a run of declared fields, some of which stand
//! for several fields on the wire. Sharing it is what lets one editor draw both.

use sim_core::frame::{Endianness, FieldDef, FieldKind, FieldSpan, FrameDef, Stated};
/// Whether the file states this declared field in a way the editor cannot
/// unpick.
///
/// True for a type instance and for a repeat: both stand for wire fields
/// that carry no record of the type or the count that produced them, so the
/// editor can move or remove the whole thing but not reword it.
#[must_use]
pub fn is_expanded(layout: &FrameDef, declared: &str) -> bool {
    layout.field_index(declared).is_none()
}

/// The declared field at `index`, if the editor may reword it.
#[must_use]
pub fn plain_field(layout: &FrameDef, index: usize) -> Option<&FieldDef> {
    let name = layout.declared.get(index)?;
    layout.field(name)
}

pub fn plain_field_mut(layout: &mut FrameDef, index: usize) -> Option<&mut FieldDef> {
    let name = layout.declared.get(index)?.clone();
    let at = layout.field_index(&name)?;
    layout.fields.get_mut(at)
}

/// Renames a declared field.
///
/// Refused for an expanded one. Its wire fields carry the instance name as
/// a prefix, and the writer states it as `type = "Point"` with no room to
/// say the new name, so a rename here could be shown but never saved.
pub fn rename_field(layout: &mut FrameDef, index: usize, name: &str) {
    let Some(old) = layout.declared.get(index).cloned() else {
        return;
    };
    if name == old || name.is_empty() {
        return;
    }
    // Nothing moves, so the ranges stored as indices still hold, whether this
    // renames one field or the twenty a type expanded into.
    for at in layout.expansion_of(&old) {
        // The tail is what expansion appended, and it follows the name it was
        // appended to: `colour.red` under `tint` is `tint.red`.
        let tail = layout.fields[at].name[old.len()..].to_owned();
        layout.fields[at].name = format!("{name}{tail}");
    }
    if let Some(stated) = layout.stated.remove(&old) {
        layout.stated.insert(name.to_owned(), stated);
    }
    name.clone_into(&mut layout.declared[index]);
}

/// Adds a field after the declared one at `index`, or at the end.
pub fn add_field(layout: &mut FrameDef, index: Option<usize>, field: FieldDef) {
    let name = unused_name(layout, &field.name);
    let at = match index.and_then(|index| layout.declared.get(index)) {
        Some(after) => layout.expansion_of(after).end,
        None => layout.fields.len(),
    };
    let declared_at = index.map_or(layout.declared.len(), |index| index + 1);

    with_spans_kept(layout, |layout| {
        layout.fields.insert(at, FieldDef { name, ..field });
        layout
            .declared
            .insert(declared_at, layout.fields[at].name.clone());
    });
}

/// Removes a declared field, and everything it expanded into.
///
/// Refused where it would leave a checksum in front of the frame, which is a
/// checksum with nothing to cover. Better said before the click than produced
/// and complained about afterwards, since the way back is not obvious: fields
/// are only ever added at the end.
pub fn remove_field(layout: &mut FrameDef, index: usize) -> bool {
    if !may_remove(layout, index) {
        return false;
    }
    let Some(name) = layout.declared.get(index).cloned() else {
        return false;
    };
    with_spans_kept(layout, |layout| {
        layout.fields.drain(layout.expansion_of(&name));
        layout.declared.remove(index);
        // How the file wrote it goes with it, or the frame would still claim a
        // field it no longer has and could never be written back.
        layout.stated.remove(&name);
    });
    true
}

/// Whether removing the declared field at `index` would leave a frame that
/// still means something.
#[must_use]
pub fn may_remove(layout: &FrameDef, index: usize) -> bool {
    let Some(name) = layout.declared.get(index) else {
        return false;
    };
    let gone = layout.expansion_of(name);
    // Nothing to check unless the removal reaches the front of the frame.
    if gone.start != 0 {
        return true;
    }
    !matches!(
        layout.fields.get(gone.end).map(|field| &field.kind),
        Some(FieldKind::Checksum { .. })
    )
}

/// Moves a declared field one place up or down, expansion and all.
pub fn move_field(layout: &mut FrameDef, index: usize, down: bool) {
    let Some(other) = (if down {
        index.checked_add(1)
    } else {
        index.checked_sub(1)
    }) else {
        return;
    };
    if index >= layout.declared.len() || other >= layout.declared.len() {
        return;
    }
    let (first, second) = if down { (index, other) } else { (other, index) };
    let (Some(above), Some(below)) = (
        layout.declared.get(first).cloned(),
        layout.declared.get(second).cloned(),
    ) else {
        return;
    };

    if first == 0
        && matches!(
            layout.field(&below).map(|field| &field.kind),
            Some(FieldKind::Checksum { .. })
        )
    {
        return;
    }

    with_spans_kept(layout, |layout| {
        let above = layout.expansion_of(&above);
        let below = layout.expansion_of(&below);
        // Rotating rather than swapping: the two runs need not be the same
        // length, one being a type instance and the other a single byte.
        layout.fields[above.start..below.end].rotate_left(above.len());
        layout.declared.swap(first, second);
    });
}

/// A name no field is using yet, `sample` becoming `sample2` if it is.
pub fn unused_name(layout: &FrameDef, wanted: &str) -> String {
    if layout.field_index(wanted).is_none() && !layout.declared.iter().any(|name| name == wanted) {
        return wanted.to_owned();
    }
    // Bounded by the field count: one of that many suffixes is free, since
    // that is how many names there are to collide with.
    (2..=layout.fields.len() + 2)
        .map(|suffix| format!("{wanted}{suffix}"))
        .find(|name| {
            layout.field_index(name).is_none() && !layout.declared.iter().any(|held| held == name)
        })
        .unwrap_or_else(|| wanted.to_owned())
}

/// Runs a structural change with every checksum still covering the fields
/// it was covering, wherever they have gone.
///
/// Ranges are stored as indices, which is what the encoder needs and what
/// the editor must not let it mean. Inserting a field above a checksum
/// would otherwise quietly shift its range by one and protect the wrong
/// bytes, with nothing on screen to say so.
///
/// What is remembered is the set of covered fields, not the two ends.
/// Reordering can put the field that was last in front of the one that was
/// first, and a range rebuilt from the old ends would come back inside out.
pub fn with_spans_kept(layout: &mut FrameDef, change: impl FnOnce(&mut FrameDef)) {
    let covered: Vec<(String, Vec<String>)> = layout
        .fields
        .iter()
        .filter_map(|field| match &field.kind {
            FieldKind::Checksum { covers, .. } => Some((
                field.name.clone(),
                layout.fields[covers.from..=covers.to]
                    .iter()
                    .map(|held| held.name.clone())
                    .collect(),
            )),
            _ => None,
        })
        .collect();

    change(layout);

    for (owner, was) in covered {
        let Some(at) = layout.field_index(&owner) else {
            continue;
        };
        let survivors: Vec<usize> = was
            .iter()
            .filter_map(|name| layout.field_index(name))
            .collect();
        // A range whose every field went with the deletion falls back to
        // the field in front of the checksum: something has to be covered,
        // and an index past the end would take the encoder with it.
        let span = match (survivors.iter().min(), survivors.iter().max()) {
            (Some(&from), Some(&to)) => FieldSpan { from, to },
            _ => FieldSpan {
                from: at.saturating_sub(1),
                to: at.saturating_sub(1),
            },
        };
        if let FieldKind::Checksum { covers, .. } = &mut layout.fields[at].kind {
            *covers = span;
        }
    }
}

/// Sets what the checksum declared at `index` covers, by naming both ends.
pub fn set_coverage(layout: &mut FrameDef, index: usize, from: &str, to: &str) {
    let (Some(from), Some(to)) = (layout.field_index(from), layout.field_index(to)) else {
        return;
    };
    let Some(at) = layout
        .declared
        .get(index)
        .and_then(|name| layout.field_index(name))
    else {
        return;
    };
    let Some(field) = layout.fields.get_mut(at) else {
        return;
    };
    if let FieldKind::Checksum { covers, .. } = &mut field.kind {
        // Backwards is not a range, and the picker offers both ends freely.
        *covers = FieldSpan {
            from: from.min(to),
            to: from.max(to),
        };
    }
}

/// Changes the order the whole frame is written in.
///
/// A field that was following the frame follows it still; one that had been
/// given an order of its own keeps it. That is what inheriting means, and
/// leaving the fields alone here would turn every one of them into an override
/// the moment the frame changed its mind.
pub fn set_endian(layout: &mut FrameDef, endian: Endianness) {
    let was = layout.endian;
    if was == endian {
        return;
    }
    for field in &mut layout.fields {
        if field.endian == was {
            field.endian = endian;
        }
    }
    layout.endian = endian;
}

/// Whether stating a byte order for this field would mean anything.
///
/// One byte reads the same either way round, so the question is not put.
#[must_use]
pub fn has_byte_order(field: &FieldDef) -> bool {
    match &field.kind {
        FieldKind::Scalar(scalar) | FieldKind::Enum { repr: scalar, .. } => scalar.size() > 1,
        FieldKind::Bits { repr, .. } => repr.size() > 1,
        FieldKind::Checksum { spec, .. } => spec.width_bytes() > 1,
        // Written in the order they are given, whatever the frame says.
        FieldKind::Bytes { .. } | FieldKind::Text { .. } => false,
    }
}

/// Restates a declared field as something else, expansion and all.
///
/// The one operation that changes how many fields a frame has without changing
/// how many it declares: naming a type of three fields where a byte stood puts
/// three on the wire under one heading.
///
/// `expansion` is what the type contributes, worked out by the core so that the
/// names match what a reload would produce.
pub fn state_as(
    layout: &mut FrameDef,
    index: usize,
    stated: Option<&Stated>,
    expansion: Vec<FieldDef>,
) {
    let Some(name) = layout.declared.get(index).cloned() else {
        return;
    };
    with_spans_kept(layout, |layout| {
        let held = layout.expansion_of(&name);
        layout.fields.splice(held, expansion);
        match stated {
            Some(stated) => {
                layout.stated.insert(name.clone(), stated.clone());
            }
            None => {
                layout.stated.remove(&name);
            }
        }
    });
}

/// How the file states this declared field, if a name alone would not do.
#[must_use]
pub fn stated_of(layout: &FrameDef, index: usize) -> Option<&Stated> {
    layout.stated.get(layout.declared.get(index)?)
}
