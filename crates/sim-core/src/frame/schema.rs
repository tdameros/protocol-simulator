//! Reading and writing frame definitions as TOML.
//!
//! The file format is mirrored by plain `Raw*` structs and then converted into
//! the domain model. Going through an intermediate representation keeps serde
//! attributes out of the model and, more importantly, lets validation report
//! what is wrong in the file rather than failing with a deserialiser message.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::checksum::{ChecksumSpec, CrcSpec};
use super::value::Value;
use super::{
    BitDef, Endianness, EnumVariant, FieldDef, FieldKind, FieldSpan, FrameDef, ScalarType,
};

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid toml: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("cannot serialise frame: {0}")]
    Serialise(#[from] toml::ser::Error),

    #[error("field {field}: unknown type {kind}")]
    UnknownType { field: String, kind: String },

    #[error("field {field}: {kind} needs {missing}")]
    MissingAttribute {
        field: String,
        kind: String,
        missing: &'static str,
    },

    #[error("field {field}: repr {repr} must be an unsigned integer")]
    BadRepr { field: String, repr: String },

    #[error("field {field}: bit widths total {total} bits but repr {repr} holds {capacity}")]
    BitWidthMismatch {
        field: String,
        total: u32,
        repr: String,
        capacity: u32,
    },

    #[error("field {field}: covers unknown field {target}")]
    UnknownCoverTarget { field: String, target: String },

    #[error("field {field}: covers runs backwards, from {from} to {to}")]
    BackwardsSpan {
        field: String,
        from: String,
        to: String,
    },

    #[error("field {field}: a checksum cannot cover itself")]
    SelfCoveringChecksum { field: String },

    #[error("field {field}: unknown crc preset {algo}, expected one of {known}")]
    UnknownCrcPreset {
        field: String,
        algo: String,
        known: String,
    },

    #[error("duplicate field name {name}")]
    DuplicateField { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawEndian {
    Big,
    Little,
}

impl From<RawEndian> for Endianness {
    fn from(raw: RawEndian) -> Self {
        match raw {
            RawEndian::Big => Self::Big,
            RawEndian::Little => Self::Little,
        }
    }
}

impl From<Endianness> for RawEndian {
    fn from(value: Endianness) -> Self {
        match value {
            Endianness::Big => Self::Big,
            Endianness::Little => Self::Little,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RawBit {
    name: String,
    width: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawCovers {
    from: String,
    to: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawField {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endian: Option<RawEndian>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    variants: Option<BTreeMap<String, i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bits: Option<Vec<RawBit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    algo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    covers: Option<RawCovers>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawFrame {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endian: Option<RawEndian>,
    #[serde(default, rename = "field")]
    fields: Vec<RawField>,
}

/// Parses a frame definition from TOML text.
///
/// # Errors
///
/// Returns an error if the text is not valid TOML or describes an inconsistent
/// frame (unknown type, bit widths that do not fill the representation, a
/// checksum covering a field that does not exist, ...).
pub fn from_toml(text: &str) -> Result<FrameDef, SchemaError> {
    let raw: RawFrame = toml::from_str(text)?;
    build(raw)
}

/// Renders a frame definition back to TOML.
///
/// # Errors
///
/// Returns an error if the frame cannot be serialised.
pub fn to_toml(frame: &FrameDef) -> Result<String, SchemaError> {
    Ok(toml::to_string_pretty(&lower(frame))?)
}

/// Loads a frame definition from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not a valid frame.
pub fn load(path: &Path) -> Result<FrameDef, SchemaError> {
    let text = std::fs::read_to_string(path).map_err(|source| SchemaError::Read {
        path: path.display().to_string(),
        source,
    })?;
    from_toml(&text)
}

/// Writes a frame definition to disk.
///
/// # Errors
///
/// Returns an error if the frame cannot be serialised or the file written.
pub fn save(frame: &FrameDef, path: &Path) -> Result<(), SchemaError> {
    let text = to_toml(frame)?;
    std::fs::write(path, text).map_err(|source| SchemaError::Write {
        path: path.display().to_string(),
        source,
    })
}

fn build(raw: RawFrame) -> Result<FrameDef, SchemaError> {
    let frame_endian: Endianness = raw.endian.map(Into::into).unwrap_or_default();

    let mut seen: Vec<&str> = Vec::new();
    for field in &raw.fields {
        if seen.contains(&field.name.as_str()) {
            return Err(SchemaError::DuplicateField {
                name: field.name.clone(),
            });
        }
        seen.push(&field.name);
    }
    let names: Vec<String> = raw.fields.iter().map(|field| field.name.clone()).collect();

    let mut fields = Vec::with_capacity(raw.fields.len());
    for (index, field) in raw.fields.iter().enumerate() {
        let kind = build_kind(field, index, &names)?;
        fields.push(FieldDef {
            name: field.name.clone(),
            description: field.description.clone(),
            endian: field.endian.as_ref().map_or(frame_endian, |raw| match raw {
                RawEndian::Big => Endianness::Big,
                RawEndian::Little => Endianness::Little,
            }),
            default: field
                .default
                .as_ref()
                .and_then(|value| default_value(value, &kind)),
            kind,
        });
    }

    Ok(FrameDef {
        name: raw.name,
        description: raw.description,
        fields,
    })
}

fn build_kind(field: &RawField, index: usize, names: &[String]) -> Result<FieldKind, SchemaError> {
    if let Some(scalar) = ScalarType::parse(&field.kind) {
        return Ok(FieldKind::Scalar(scalar));
    }

    match field.kind.as_str() {
        "bytes" | "text" => {
            let len = field.len.ok_or_else(|| SchemaError::MissingAttribute {
                field: field.name.clone(),
                kind: field.kind.clone(),
                missing: "len",
            })?;
            Ok(if field.kind == "bytes" {
                FieldKind::Bytes { len }
            } else {
                FieldKind::Text { len }
            })
        }
        "enum" => {
            let repr = unsigned_repr(field)?;
            let variants =
                field
                    .variants
                    .as_ref()
                    .ok_or_else(|| SchemaError::MissingAttribute {
                        field: field.name.clone(),
                        kind: field.kind.clone(),
                        missing: "variants",
                    })?;
            let mut variants: Vec<EnumVariant> = variants
                .iter()
                .map(|(name, value)| EnumVariant {
                    name: name.clone(),
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "toml integers are signed; a negative discriminant is caught by the range check at encode time"
                    )]
                    value: *value as u64,
                })
                .collect();
            // A TOML inline table arrives alphabetically sorted by name, which is
            // not how a protocol specification reads. Order by value instead, so
            // listings match the spec and the lowest value comes first.
            variants.sort_by_key(|variant| variant.value);
            Ok(FieldKind::Enum { repr, variants })
        }
        "bits" => {
            let repr = unsigned_repr(field)?;
            let bits = field
                .bits
                .as_ref()
                .ok_or_else(|| SchemaError::MissingAttribute {
                    field: field.name.clone(),
                    kind: field.kind.clone(),
                    missing: "bits",
                })?;
            let total: u32 = bits.iter().map(|bit| bit.width).sum();
            let capacity = u32::try_from(repr.size() * 8).unwrap_or(u32::MAX);
            if total != capacity {
                return Err(SchemaError::BitWidthMismatch {
                    field: field.name.clone(),
                    total,
                    repr: repr.name().to_owned(),
                    capacity,
                });
            }
            Ok(FieldKind::Bits {
                repr,
                bits: bits
                    .iter()
                    .map(|bit| BitDef {
                        name: bit.name.clone(),
                        width: bit.width,
                    })
                    .collect(),
            })
        }
        other => build_checksum(field, other, index, names),
    }
}

fn build_checksum(
    field: &RawField,
    kind: &str,
    index: usize,
    names: &[String],
) -> Result<FieldKind, SchemaError> {
    let spec = match kind {
        "xor8" => ChecksumSpec::Xor8,
        "sum8" => ChecksumSpec::Sum { width_bytes: 1 },
        "sum16" => ChecksumSpec::Sum { width_bytes: 2 },
        "crc8" | "crc16" | "crc32" => {
            // `crc8` and `crc32` have one dominant variant, so the preset name
            // doubles as the type; `crc16` has too many to guess at.
            let algo = field.algo.clone().unwrap_or_else(|| kind.to_owned());
            let spec = CrcSpec::preset(&algo).ok_or_else(|| SchemaError::UnknownCrcPreset {
                field: field.name.clone(),
                algo,
                known: CrcSpec::preset_names().join(", "),
            })?;
            ChecksumSpec::Crc(spec)
        }
        _ => {
            return Err(SchemaError::UnknownType {
                field: field.name.clone(),
                kind: kind.to_owned(),
            })
        }
    };

    let covers = field
        .covers
        .as_ref()
        .ok_or_else(|| SchemaError::MissingAttribute {
            field: field.name.clone(),
            kind: kind.to_owned(),
            missing: "covers",
        })?;
    let resolve = |target: &String| {
        names.iter().position(|name| name == target).ok_or_else(|| {
            SchemaError::UnknownCoverTarget {
                field: field.name.clone(),
                target: target.clone(),
            }
        })
    };
    let from = resolve(&covers.from)?;
    let to = resolve(&covers.to)?;
    if from > to {
        return Err(SchemaError::BackwardsSpan {
            field: field.name.clone(),
            from: covers.from.clone(),
            to: covers.to.clone(),
        });
    }
    if (from..=to).contains(&index) {
        return Err(SchemaError::SelfCoveringChecksum {
            field: field.name.clone(),
        });
    }

    Ok(FieldKind::Checksum {
        spec,
        covers: FieldSpan { from, to },
    })
}

fn unsigned_repr(field: &RawField) -> Result<ScalarType, SchemaError> {
    let repr = field
        .repr
        .as_ref()
        .ok_or_else(|| SchemaError::MissingAttribute {
            field: field.name.clone(),
            kind: field.kind.clone(),
            missing: "repr",
        })?;
    let scalar = ScalarType::parse(repr).ok_or_else(|| SchemaError::BadRepr {
        field: field.name.clone(),
        repr: repr.clone(),
    })?;
    if !scalar.is_unsigned_integer() {
        return Err(SchemaError::BadRepr {
            field: field.name.clone(),
            repr: repr.clone(),
        });
    }
    Ok(scalar)
}

/// Interprets a `default =` entry against the field it belongs to.
///
/// A value that does not suit the field is dropped rather than rejected: the
/// encoder reports a far more precise error when the field is actually used.
fn default_value(raw: &toml::Value, kind: &FieldKind) -> Option<Value> {
    match kind {
        FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
            u64::try_from(raw.as_integer()?).ok().map(Value::Uint)
        }
        FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => raw.as_float().map(Value::Float),
        FieldKind::Scalar(_) => raw.as_integer().map(Value::Int),
        FieldKind::Enum { .. } => match raw {
            toml::Value::String(name) => Some(Value::Text(name.clone())),
            other => u64::try_from(other.as_integer()?).ok().map(Value::Uint),
        },
        FieldKind::Text { .. } => raw.as_str().map(|text| Value::Text(text.to_owned())),
        _ => None,
    }
}

/// Renders a default back to TOML.
///
/// Mirrors [`default_value`]: kinds that cannot carry a default there produce
/// nothing here, so a round trip neither invents nor drops one.
fn default_to_toml(value: &Value) -> Option<toml::Value> {
    match value {
        Value::Uint(raw) => i64::try_from(*raw).ok().map(toml::Value::Integer),
        Value::Int(raw) => Some(toml::Value::Integer(*raw)),
        Value::Float(raw) => Some(toml::Value::Float(*raw)),
        Value::Text(raw) => Some(toml::Value::String(raw.clone())),
        Value::Bytes(_) | Value::Bits(_) => None,
    }
}

fn lower(frame: &FrameDef) -> RawFrame {
    RawFrame {
        name: frame.name.clone(),
        description: frame.description.clone(),
        endian: None,
        fields: frame
            .fields
            .iter()
            .map(|field| lower_field(field, frame))
            .collect(),
    }
}

fn lower_field(field: &FieldDef, frame: &FrameDef) -> RawField {
    let mut raw = RawField {
        name: field.name.clone(),
        kind: field.kind.type_name().to_owned(),
        description: field.description.clone(),
        // Written per field rather than hoisted to a frame default, so a
        // round trip cannot silently change a field's byte order.
        endian: Some(field.endian.into()),
        default: field.default.as_ref().and_then(default_to_toml),
        repr: None,
        variants: None,
        bits: None,
        len: None,
        algo: None,
        covers: None,
    };

    match &field.kind {
        FieldKind::Scalar(_) => {}
        FieldKind::Bytes { len } | FieldKind::Text { len } => raw.len = Some(*len),
        FieldKind::Enum { repr, variants } => {
            raw.repr = Some(repr.name().to_owned());
            raw.variants = Some(
                variants
                    .iter()
                    .map(|variant| {
                        #[expect(
                            clippy::cast_possible_wrap,
                            reason = "toml has no unsigned integer type"
                        )]
                        (variant.name.clone(), variant.value as i64)
                    })
                    .collect(),
            );
        }
        FieldKind::Bits { repr, bits } => {
            raw.repr = Some(repr.name().to_owned());
            raw.bits = Some(
                bits.iter()
                    .map(|bit| RawBit {
                        name: bit.name.clone(),
                        width: bit.width,
                    })
                    .collect(),
            );
        }
        FieldKind::Checksum { spec, covers } => {
            match spec {
                ChecksumSpec::Xor8 => "xor8".clone_into(&mut raw.kind),
                ChecksumSpec::Sum { width_bytes } => raw.kind = format!("sum{}", width_bytes * 8),
                ChecksumSpec::Crc(crc) => {
                    raw.kind = format!("crc{}", crc.width_bits);
                    raw.algo = crc.preset_name().map(ToOwned::to_owned);
                }
            }
            raw.covers = Some(RawCovers {
                from: frame.fields[covers.from].name.clone(),
                to: frame.fields[covers.to].name.clone(),
            });
        }
    }

    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::codec;
    use crate::frame::value::FieldValues;

    /// The layout agreed for the file format, used as the reference case.
    const TELEMETRY: &str = r#"
name = "Telemetry"
endian = "big"

[[field]]
name = "sync"
type = "u16"
default = 0xAA55

[[field]]
name = "timestamp"
type = "u32"

[[field]]
name = "mode"
type = "enum"
repr = "u8"
variants = { IDLE = 0, RUN = 1, FAULT = 2 }

[[field]]
name = "flags"
type = "bits"
repr = "u8"
bits = [
  { name = "armed",  width = 1 },
  { name = "heater", width = 1 },
  { name = "spare",  width = 6 },
]

[[field]]
name = "payload"
type = "bytes"
len = 16

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "sync", to = "payload" }
"#;

    #[test]
    fn parses_the_reference_frame() {
        let frame = from_toml(TELEMETRY).expect("reference frame should parse");
        assert_eq!(frame.name, "Telemetry");
        assert_eq!(frame.fields.len(), 6);
        // 2 + 4 + 1 + 1 + 16 + 2
        assert_eq!(frame.size(), 26);
        assert_eq!(
            frame.field("sync").unwrap().default,
            Some(Value::Uint(0xAA55))
        );
        assert!(matches!(
            frame.field("crc").unwrap().kind,
            FieldKind::Checksum {
                covers: FieldSpan { from: 0, to: 4 },
                ..
            }
        ));
    }

    #[test]
    fn enum_variants_are_ordered_by_value_not_by_name() {
        let frame = from_toml(TELEMETRY).unwrap();
        let FieldKind::Enum { variants, .. } = &frame.field("mode").unwrap().kind else {
            panic!("mode should be an enum");
        };
        // Declared as an inline table, which arrives sorted as FAULT, IDLE, RUN.
        // Ordering by value keeps the lowest first, so a frame does not default
        // to whatever variant happens to sort first alphabetically.
        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["IDLE", "RUN", "FAULT"]);
        assert_eq!(variants[0].value, 0);
    }

    #[test]
    fn frame_endian_is_inherited_and_overridable() {
        let text = r#"
name = "mixed"
endian = "little"

[[field]]
name = "a"
type = "u16"

[[field]]
name = "b"
type = "u16"
endian = "big"
"#;
        let frame = from_toml(text).unwrap();
        assert_eq!(frame.field("a").unwrap().endian, Endianness::Little);
        assert_eq!(frame.field("b").unwrap().endian, Endianness::Big);
    }

    #[test]
    fn the_reference_frame_encodes_and_decodes() {
        let frame = from_toml(TELEMETRY).unwrap();
        let mut values = FieldValues::new();
        values.insert("timestamp".to_owned(), Value::Uint(0x1234_5678));
        values.insert("mode".to_owned(), Value::Text("RUN".to_owned()));
        let mut bits = BTreeMap::new();
        bits.insert("armed".to_owned(), 1);
        values.insert("flags".to_owned(), Value::Bits(bits));
        values.insert("payload".to_owned(), Value::Bytes(vec![0; 16]));

        let bytes = codec::encode(&frame, &values).unwrap();
        assert_eq!(bytes.len(), 26);
        // `sync` came from the default declared in the file.
        assert_eq!(&bytes[..2], &[0xAA, 0x55]);

        let decoded = codec::decode(&frame, &bytes).unwrap();
        assert!(decoded.checksum_mismatches.is_empty());
        assert_eq!(decoded.values["timestamp"], Value::Uint(0x1234_5678));
        assert_eq!(decoded.values["mode"], Value::Uint(1));
    }

    #[test]
    fn a_round_trip_through_toml_preserves_the_frame() {
        let frame = from_toml(TELEMETRY).unwrap();
        let reparsed = from_toml(&to_toml(&frame).unwrap()).expect("rendered toml should parse");
        assert_eq!(frame, reparsed);
    }

    #[test]
    fn bit_widths_must_fill_the_representation() {
        let text = r#"
name = "bad"
[[field]]
name = "flags"
type = "bits"
repr = "u8"
bits = [{ name = "only", width = 3 }]
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(
                err,
                SchemaError::BitWidthMismatch {
                    total: 3,
                    capacity: 8,
                    ..
                }
            ),
            "got {err}"
        );
    }

    #[test]
    fn a_checksum_cannot_reference_a_missing_field() {
        let text = r#"
name = "bad"
[[field]]
name = "sync"
type = "u16"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "sinc", to = "sync" }
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::UnknownCoverTarget { target, .. } if target == "sinc"),
            "got {err}"
        );
    }

    #[test]
    fn a_checksum_cannot_cover_itself() {
        let text = r#"
name = "bad"
[[field]]
name = "sync"
type = "u16"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "sync", to = "crc" }
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::SelfCoveringChecksum { .. }
        ));
    }

    #[test]
    fn an_unknown_crc_preset_lists_the_known_ones() {
        let text = r#"
name = "bad"
[[field]]
name = "sync"
type = "u16"

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-nope"
covers = { from = "sync", to = "sync" }
"#;
        let err = from_toml(text).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("crc16-nope"), "got {message}");
        assert!(message.contains("crc16-modbus"), "got {message}");
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        let text = r#"
name = "bad"
[[field]]
name = "x"
type = "u8"

[[field]]
name = "x"
type = "u8"
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::DuplicateField { .. }
        ));
    }

    #[test]
    fn a_missing_length_is_reported_against_the_field() {
        let text = r#"
name = "bad"
[[field]]
name = "payload"
type = "bytes"
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::MissingAttribute { field, missing, .. }
                if field == "payload" && *missing == "len"),
            "got {err}"
        );
    }

    #[test]
    fn an_enum_repr_must_be_unsigned() {
        let text = r#"
name = "bad"
[[field]]
name = "mode"
type = "enum"
repr = "i8"
variants = { A = 0 }
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::BadRepr { .. }
        ));
    }

    #[test]
    fn saving_and_loading_uses_the_filesystem() {
        let dir = std::env::temp_dir().join(format!("sim-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("telemetry.toml");

        let frame = from_toml(TELEMETRY).unwrap();
        save(&frame, &path).unwrap();
        assert_eq!(load(&path).unwrap(), frame);

        std::fs::remove_dir_all(&dir).ok();
    }
}
