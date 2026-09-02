//! What kinds a field can be, and the words a picker offers for them.
//!
//! The list has to agree with what `type =` accepts in a frame file, so it is
//! derived from the model rather than written out again. A scalar added to
//! `ScalarType` reaches every picker without anybody remembering to add it.

use sim_core::frame::checksum::{ChecksumSpec, CrcSpec};
use sim_core::frame::schema::{self, TypeLibrary};
use sim_core::frame::{
    BitDef, Endianness, EnumVariant, FieldDef, FieldKind, FieldSpan, FrameDef, ScalarType, Stated,
};

/// What New starts a field as: one byte, to be told what it is.
///
/// Following the frame's order rather than assuming one, or widening it to a
/// `u16` afterwards would silently put it on the wire the wrong way round.
#[must_use]
pub fn blank_field(endian: Endianness) -> FieldDef {
    FieldDef {
        name: "field".to_owned(),
        description: None,
        kind: FieldKind::Scalar(ScalarType::U8),
        endian,
        default: None,
        range: None,
    }
}

/// Every word the file's `type =` accepts, which is exactly what the picker
/// offers.
#[must_use]
pub fn labels() -> Vec<String> {
    let mut labels: Vec<String> = ScalarType::ALL
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

#[must_use]
pub fn label_of(kind: &FieldKind) -> String {
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
#[must_use]
pub fn named(label: &str, frame: &FrameDef, index: usize) -> Option<FieldKind> {
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

/// The fields a restated declaration puts on the wire.
///
/// A field going back to being plain keeps the one it already had, so that
/// dropping a type does not also drop its name and its byte order.
#[must_use]
pub fn restate(
    list: &FrameDef,
    index: usize,
    stated: Option<&Stated>,
    types: &TypeLibrary,
) -> Vec<FieldDef> {
    let Some(name) = list.declared.get(index) else {
        return Vec::new();
    };
    let endian = list.endian;
    match stated {
        Some(stated) => schema::instantiate(types, name, &stated.kind, stated, endian)
            // An instance the library cannot expand leaves the field as it
            // stands, which the guard will then refuse to save.
            .unwrap_or_else(|_| list.fields[list.expansion_of(name)].to_vec()),
        // Named after the declaration it replaces, or the model would hold a
        // field the frame does not declare.
        None => vec![FieldDef {
            name: name.clone(),
            ..blank_field(endian)
        }],
    }
}
