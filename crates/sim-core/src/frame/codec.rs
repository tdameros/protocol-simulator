use std::collections::BTreeMap;

use super::value::{FieldValues, Value};
use super::{Endianness, FieldDef, FieldKind, FrameDef, ScalarType};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("field {field}: no value supplied and no default")]
    MissingValue { field: String },

    #[error("field {field}: expected {expected}, got {got}")]
    TypeMismatch {
        field: String,
        expected: &'static str,
        got: &'static str,
    },

    #[error("field {field}: {value} does not fit in {repr}")]
    OutOfRange {
        field: String,
        value: String,
        repr: &'static str,
    },

    #[error("field {field}: expected {expected} bytes, got {got}")]
    WrongLength {
        field: String,
        expected: usize,
        got: usize,
    },

    #[error("field {field}: unknown variant {variant}")]
    UnknownVariant { field: String, variant: String },

    #[error("field {field}: {value} is outside {range}")]
    OutOfSubrange {
        field: String,
        value: String,
        range: String,
    },

    #[error("frame {frame}: expected {expected} bytes, got {got}")]
    FrameLength {
        frame: String,
        expected: usize,
        got: usize,
    },
}

/// Outcome of decoding a frame.
pub struct Decoded {
    pub values: FieldValues,
    /// Checksum fields whose stored value disagrees with the recomputed one.
    ///
    /// Reported rather than raised: a corrupt frame is still worth showing, and
    /// deciding what to do about it belongs to the caller.
    pub checksum_mismatches: Vec<ChecksumMismatch>,
    /// Fields holding a value their subtype does not allow.
    ///
    /// Reported for the same reason: equipment that sends 120 into a 0..99
    /// field is precisely what you opened the simulator to find out.
    pub range_violations: Vec<RangeViolation>,
}

pub struct ChecksumMismatch {
    pub field: String,
    pub found: u64,
    pub expected: u64,
}

pub struct RangeViolation {
    pub field: String,
    pub found: String,
    pub range: String,
}

/// Encodes `values` according to `frame`.
///
/// Checksum fields are written last, once the bytes they cover exist.
///
/// # Errors
///
/// Returns an error if a field has neither a value nor a default, or if a value
/// does not fit the field it is assigned to.
pub fn encode(frame: &FrameDef, values: &FieldValues) -> Result<Vec<u8>, CodecError> {
    let mut out = vec![0u8; frame.size()];

    for (index, field) in frame.fields.iter().enumerate() {
        if matches!(field.kind, FieldKind::Checksum { .. }) {
            continue;
        }
        let offset = frame.offset_of(index);
        let value = values
            .get(&field.name)
            .or(field.default.as_ref())
            .ok_or_else(|| CodecError::MissingValue {
                field: field.name.clone(),
            })?;
        check_range(field, value)?;
        encode_field(field, value, &mut out[offset..offset + field.kind.size()])?;
    }

    for (index, field) in frame.fields.iter().enumerate() {
        let FieldKind::Checksum { spec, covers } = &field.kind else {
            continue;
        };
        let start = frame.offset_of(covers.from);
        let end = frame.offset_of(covers.to) + frame.fields[covers.to].kind.size();
        let sum = spec.compute(&out[start..end]);
        let offset = frame.offset_of(index);
        write_uint(sum, spec.width_bytes(), field.endian, &mut out[offset..]);
    }

    Ok(out)
}

/// Rejects a value its field's subtype does not allow.
///
/// Refused on the way out, merely reported on the way in: sending a frame your
/// own specification forbids is a mistake, receiving one is a finding.
fn check_range(field: &FieldDef, value: &Value) -> Result<(), CodecError> {
    let Some(range) = &field.range else {
        return Ok(());
    };
    if range.accepts(value) {
        return Ok(());
    }
    Err(CodecError::OutOfSubrange {
        field: field.name.clone(),
        value: describe_value(value),
        range: range.describe(),
    })
}

fn describe_value(value: &Value) -> String {
    match value {
        Value::Uint(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        other => other.type_name().to_owned(),
    }
}

/// Decodes `bytes` according to `frame`.
///
/// # Errors
///
/// Returns an error if `bytes` is not exactly the frame's size.
pub fn decode(frame: &FrameDef, bytes: &[u8]) -> Result<Decoded, CodecError> {
    if bytes.len() != frame.size() {
        return Err(CodecError::FrameLength {
            frame: frame.name.clone(),
            expected: frame.size(),
            got: bytes.len(),
        });
    }

    let mut values = FieldValues::new();
    let mut checksum_mismatches = Vec::new();
    let mut range_violations = Vec::new();

    for (index, field) in frame.fields.iter().enumerate() {
        let offset = frame.offset_of(index);
        let raw = &bytes[offset..offset + field.kind.size()];

        if let FieldKind::Checksum { spec, covers } = &field.kind {
            let found = read_uint(raw, field.endian);
            let start = frame.offset_of(covers.from);
            let end = frame.offset_of(covers.to) + frame.fields[covers.to].kind.size();
            let expected = spec.compute(&bytes[start..end]);
            if found != expected {
                checksum_mismatches.push(ChecksumMismatch {
                    field: field.name.clone(),
                    found,
                    expected,
                });
            }
            values.insert(field.name.clone(), Value::Uint(found));
            continue;
        }

        let value = decode_field(field, raw);
        if let Some(range) = &field.range {
            if !range.accepts(&value) {
                range_violations.push(RangeViolation {
                    field: field.name.clone(),
                    found: describe_value(&value),
                    range: range.describe(),
                });
            }
        }
        values.insert(field.name.clone(), value);
    }

    Ok(Decoded {
        values,
        checksum_mismatches,
        range_violations,
    })
}

fn encode_field(field: &FieldDef, value: &Value, out: &mut [u8]) -> Result<(), CodecError> {
    match &field.kind {
        FieldKind::Scalar(scalar) => encode_scalar(field, *scalar, value, out),
        FieldKind::Bytes { len } => {
            let bytes = value.as_bytes().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: "bytes",
                got: value.type_name(),
            })?;
            if bytes.len() != *len {
                return Err(CodecError::WrongLength {
                    field: field.name.clone(),
                    expected: *len,
                    got: bytes.len(),
                });
            }
            out.copy_from_slice(bytes);
            Ok(())
        }
        FieldKind::Text { len } => {
            let text = value.as_text().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: "text",
                got: value.type_name(),
            })?;
            if text.len() > *len {
                return Err(CodecError::WrongLength {
                    field: field.name.clone(),
                    expected: *len,
                    got: text.len(),
                });
            }
            out[..text.len()].copy_from_slice(text.as_bytes());
            out[text.len()..].fill(0);
            Ok(())
        }
        FieldKind::Enum { repr, variants } => {
            let numeric = match value {
                Value::Uint(raw) => *raw,
                Value::Text(name) => {
                    variants
                        .iter()
                        .find(|variant| variant.name == *name)
                        .ok_or_else(|| CodecError::UnknownVariant {
                            field: field.name.clone(),
                            variant: name.clone(),
                        })?
                        .value
                }
                other => {
                    return Err(CodecError::TypeMismatch {
                        field: field.name.clone(),
                        expected: "variant name or unsigned integer",
                        got: other.type_name(),
                    })
                }
            };
            check_fits(field, numeric, *repr)?;
            write_uint(numeric, repr.size(), field.endian, out);
            Ok(())
        }
        FieldKind::Bits { repr, bits } => {
            let supplied = value.as_bits().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: "bitfield",
                got: value.type_name(),
            })?;
            let mut packed = 0u64;
            let mut remaining = u32::try_from(repr.size() * 8).unwrap_or(u32::MAX);
            for bit in bits {
                remaining -= bit.width;
                let raw = supplied.get(&bit.name).copied().unwrap_or(0);
                let max = if bit.width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << bit.width) - 1
                };
                if raw > max {
                    return Err(CodecError::OutOfRange {
                        field: format!("{}.{}", field.name, bit.name),
                        value: raw.to_string(),
                        repr: "bit width",
                    });
                }
                packed |= raw << remaining;
            }
            write_uint(packed, repr.size(), field.endian, out);
            Ok(())
        }
        // Written in the second pass, once the covered bytes exist.
        FieldKind::Checksum { .. } => Ok(()),
    }
}

fn encode_scalar(
    field: &FieldDef,
    scalar: ScalarType,
    value: &Value,
    out: &mut [u8],
) -> Result<(), CodecError> {
    match scalar {
        ScalarType::F32 => {
            let raw = value.as_float().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: "float",
                got: value.type_name(),
            })?;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "narrowing to f32 is the point of the field type"
            )]
            let bits = (raw as f32).to_bits();
            write_uint(u64::from(bits), 4, field.endian, out);
            Ok(())
        }
        ScalarType::F64 => {
            let raw = value.as_float().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: "float",
                got: value.type_name(),
            })?;
            write_uint(raw.to_bits(), 8, field.endian, out);
            Ok(())
        }
        _ if scalar.is_unsigned_integer() => {
            let raw = value.as_uint().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: scalar.name(),
                got: value.type_name(),
            })?;
            check_fits(field, raw, scalar)?;
            write_uint(raw, scalar.size(), field.endian, out);
            Ok(())
        }
        _ => {
            let raw = value.as_int().ok_or_else(|| CodecError::TypeMismatch {
                field: field.name.clone(),
                expected: scalar.name(),
                got: value.type_name(),
            })?;
            let bits = scalar.size() * 8;
            let min = -(1i64 << (bits - 1));
            let max = (1i64 << (bits - 1)) - 1;
            if raw < min || raw > max {
                return Err(CodecError::OutOfRange {
                    field: field.name.clone(),
                    value: raw.to_string(),
                    repr: scalar.name(),
                });
            }
            #[expect(
                clippy::cast_sign_loss,
                reason = "two's complement bit pattern is what goes on the wire"
            )]
            let unsigned = raw as u64;
            write_uint(unsigned, scalar.size(), field.endian, out);
            Ok(())
        }
    }
}

fn check_fits(field: &FieldDef, value: u64, repr: ScalarType) -> Result<(), CodecError> {
    let bits = repr.size() * 8;
    if bits < 64 && value > (1u64 << bits) - 1 {
        return Err(CodecError::OutOfRange {
            field: field.name.clone(),
            value: value.to_string(),
            repr: repr.name(),
        });
    }
    Ok(())
}

fn decode_field(field: &FieldDef, raw: &[u8]) -> Value {
    match &field.kind {
        FieldKind::Scalar(scalar) => decode_scalar(*scalar, raw, field.endian),
        FieldKind::Bytes { .. } => Value::Bytes(raw.to_vec()),
        FieldKind::Text { .. } => {
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
            Value::Text(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
        // Both keep the raw number: an enum may legitimately carry a value with
        // no matching variant, and naming it is a display concern.
        FieldKind::Enum { .. } | FieldKind::Checksum { .. } => {
            Value::Uint(read_uint(raw, field.endian))
        }
        FieldKind::Bits { repr, bits } => {
            let packed = read_uint(raw, field.endian);
            let mut remaining = u32::try_from(repr.size() * 8).unwrap_or(u32::MAX);
            let mut out = BTreeMap::new();
            for bit in bits {
                remaining -= bit.width;
                let mask = if bit.width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << bit.width) - 1
                };
                out.insert(bit.name.clone(), (packed >> remaining) & mask);
            }
            Value::Bits(out)
        }
    }
}

fn decode_scalar(scalar: ScalarType, raw: &[u8], endian: Endianness) -> Value {
    let bits = read_uint(raw, endian);
    match scalar {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "read_uint yields exactly the 4 bytes of an f32"
        )]
        ScalarType::F32 => Value::Float(f64::from(f32::from_bits(bits as u32))),
        ScalarType::F64 => Value::Float(f64::from_bits(bits)),
        _ if scalar.is_unsigned_integer() => Value::Uint(bits),
        _ => {
            // Sign-extend from the field's width.
            let shift = 64 - scalar.size() * 8;
            #[expect(
                clippy::cast_possible_wrap,
                reason = "reinterpreting the bit pattern is the sign extension"
            )]
            let signed = ((bits << shift) as i64) >> shift;
            Value::Int(signed)
        }
    }
}

fn write_uint(value: u64, width: usize, endian: Endianness, out: &mut [u8]) {
    for (i, slot) in out.iter_mut().take(width).enumerate() {
        let shift = match endian {
            Endianness::Big => (width - 1 - i) * 8,
            Endianness::Little => i * 8,
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "masking to one byte is the intent"
        )]
        let byte = (value >> shift) as u8;
        *slot = byte;
    }
}

fn read_uint(raw: &[u8], endian: Endianness) -> u64 {
    raw.iter().enumerate().fold(0u64, |acc, (i, byte)| {
        let shift = match endian {
            Endianness::Big => (raw.len() - 1 - i) * 8,
            Endianness::Little => i * 8,
        };
        acc | (u64::from(*byte) << shift)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::checksum::{ChecksumSpec, CrcSpec};
    use crate::frame::{BitDef, EnumVariant, FieldSpan, ValueRange};

    fn field(name: &str, kind: FieldKind, endian: Endianness) -> FieldDef {
        FieldDef {
            name: name.to_owned(),
            description: None,
            kind,
            endian,
            default: None,
            range: None,
        }
    }

    fn constrained(name: &str, scalar: ScalarType, range: ValueRange) -> FieldDef {
        FieldDef {
            range: Some(range),
            ..field(name, FieldKind::Scalar(scalar), Endianness::Big)
        }
    }

    #[test]
    fn a_value_outside_its_subtype_is_refused_on_the_way_out() {
        let frame = FrameDef::flat(
            "duty".to_owned(),
            vec![constrained(
                "percent",
                ScalarType::U8,
                ValueRange::Uint { min: 0, max: 99 },
            )],
        );

        let mut values = FieldValues::new();
        values.insert("percent".to_owned(), Value::Uint(99));
        assert!(encode(&frame, &values).is_ok());

        values.insert("percent".to_owned(), Value::Uint(100));
        let error = encode(&frame, &values).unwrap_err();
        assert!(
            matches!(&error, CodecError::OutOfSubrange { field, .. } if field == "percent"),
            "got {error}"
        );
        assert!(error.to_string().contains("0..99"), "got {error}");
    }

    #[test]
    fn a_value_outside_its_subtype_is_only_reported_on_the_way_in() {
        let frame = FrameDef::flat(
            "duty".to_owned(),
            vec![constrained(
                "percent",
                ScalarType::U8,
                ValueRange::Uint { min: 0, max: 99 },
            )],
        );

        // What the equipment actually sent, out of range and all.
        let decoded = decode(&frame, &[120]).expect("a bad value is still a readable frame");
        assert_eq!(decoded.values["percent"], Value::Uint(120));
        assert_eq!(decoded.range_violations.len(), 1);
        assert_eq!(decoded.range_violations[0].field, "percent");
        assert_eq!(decoded.range_violations[0].found, "120");

        assert!(decode(&frame, &[99]).unwrap().range_violations.is_empty());
    }

    #[test]
    fn endianness_applies_per_field() {
        let frame = FrameDef::flat(
            "mixed".to_owned(),
            vec![
                field("be", FieldKind::Scalar(ScalarType::U16), Endianness::Big),
                field("le", FieldKind::Scalar(ScalarType::U16), Endianness::Little),
            ],
        );
        let mut values = FieldValues::new();
        values.insert("be".to_owned(), Value::Uint(0x1234));
        values.insert("le".to_owned(), Value::Uint(0x1234));

        let bytes = encode(&frame, &values).unwrap();
        assert_eq!(bytes, vec![0x12, 0x34, 0x34, 0x12]);
    }

    #[test]
    fn signed_scalars_round_trip_through_twos_complement() {
        let frame = FrameDef::flat(
            "signed".to_owned(),
            vec![field(
                "temp",
                FieldKind::Scalar(ScalarType::I16),
                Endianness::Big,
            )],
        );
        let mut values = FieldValues::new();
        values.insert("temp".to_owned(), Value::Int(-40));

        let bytes = encode(&frame, &values).unwrap();
        assert_eq!(bytes, vec![0xFF, 0xD8]);

        let decoded = decode(&frame, &bytes).unwrap();
        assert_eq!(decoded.values["temp"], Value::Int(-40));
    }

    #[test]
    fn floats_round_trip() {
        let frame = FrameDef::flat(
            "floats".to_owned(),
            vec![
                field("a", FieldKind::Scalar(ScalarType::F32), Endianness::Big),
                field("b", FieldKind::Scalar(ScalarType::F64), Endianness::Little),
            ],
        );
        let mut values = FieldValues::new();
        values.insert("a".to_owned(), Value::Float(1.5));
        values.insert("b".to_owned(), Value::Float(-2.25));

        let decoded = decode(&frame, &encode(&frame, &values).unwrap()).unwrap();
        assert_eq!(decoded.values["a"], Value::Float(1.5));
        assert_eq!(decoded.values["b"], Value::Float(-2.25));
    }

    #[test]
    fn bitfields_pack_most_significant_first() {
        let frame = FrameDef::flat(
            "flags".to_owned(),
            vec![field(
                "f",
                FieldKind::Bits {
                    repr: ScalarType::U8,
                    bits: vec![
                        BitDef {
                            name: "armed".to_owned(),
                            width: 1,
                        },
                        BitDef {
                            name: "heater".to_owned(),
                            width: 1,
                        },
                        BitDef {
                            name: "level".to_owned(),
                            width: 6,
                        },
                    ],
                },
                Endianness::Big,
            )],
        );
        let mut bits = BTreeMap::new();
        bits.insert("armed".to_owned(), 1);
        bits.insert("heater".to_owned(), 0);
        bits.insert("level".to_owned(), 0b10_1010);
        let mut values = FieldValues::new();
        values.insert("f".to_owned(), Value::Bits(bits.clone()));

        let bytes = encode(&frame, &values).unwrap();
        assert_eq!(bytes, vec![0b1010_1010]);

        let decoded = decode(&frame, &bytes).unwrap();
        assert_eq!(decoded.values["f"], Value::Bits(bits));
    }

    #[test]
    fn enum_accepts_a_variant_name() {
        let frame = FrameDef::flat(
            "modes".to_owned(),
            vec![field(
                "mode",
                FieldKind::Enum {
                    repr: ScalarType::U8,
                    variants: vec![
                        EnumVariant {
                            name: "IDLE".to_owned(),
                            value: 0,
                        },
                        EnumVariant {
                            name: "RUN".to_owned(),
                            value: 7,
                        },
                    ],
                },
                Endianness::Big,
            )],
        );
        let mut values = FieldValues::new();
        values.insert("mode".to_owned(), Value::Text("RUN".to_owned()));
        assert_eq!(encode(&frame, &values).unwrap(), vec![7]);

        values.insert("mode".to_owned(), Value::Text("NOPE".to_owned()));
        assert!(matches!(
            encode(&frame, &values),
            Err(CodecError::UnknownVariant { .. })
        ));
    }

    fn crc_frame() -> FrameDef {
        FrameDef::flat(
            "telemetry".to_owned(),
            vec![
                field("sync", FieldKind::Scalar(ScalarType::U16), Endianness::Big),
                field("payload", FieldKind::Bytes { len: 3 }, Endianness::Big),
                field(
                    "crc",
                    FieldKind::Checksum {
                        spec: ChecksumSpec::Crc(CrcSpec::preset("crc16-ccitt").unwrap()),
                        covers: FieldSpan { from: 0, to: 1 },
                    },
                    Endianness::Big,
                ),
            ],
        )
    }

    #[test]
    fn checksum_covers_the_named_span_and_verifies_on_decode() {
        let frame = crc_frame();
        let mut values = FieldValues::new();
        values.insert("sync".to_owned(), Value::Uint(0xAA55));
        values.insert("payload".to_owned(), Value::Bytes(vec![1, 2, 3]));

        let bytes = encode(&frame, &values).unwrap();
        let expected = CrcSpec::preset("crc16-ccitt").unwrap().compute(&bytes[..5]);
        assert_eq!(
            u64::from(u16::from_be_bytes([bytes[5], bytes[6]])),
            expected
        );

        let decoded = decode(&frame, &bytes).unwrap();
        assert!(decoded.checksum_mismatches.is_empty());
    }

    #[test]
    fn a_corrupt_payload_is_reported_not_rejected() {
        let frame = crc_frame();
        let mut values = FieldValues::new();
        values.insert("sync".to_owned(), Value::Uint(0xAA55));
        values.insert("payload".to_owned(), Value::Bytes(vec![1, 2, 3]));

        let mut bytes = encode(&frame, &values).unwrap();
        // sync occupies bytes 0..2, so byte 3 is the second payload byte.
        bytes[3] ^= 0xFF;

        let decoded = decode(&frame, &bytes).unwrap();
        assert_eq!(decoded.checksum_mismatches.len(), 1);
        assert_eq!(decoded.checksum_mismatches[0].field, "crc");
        // The frame still decodes so the operator can see what arrived.
        assert_eq!(
            decoded.values["payload"],
            Value::Bytes(vec![1, 2 ^ 0xFF, 3])
        );
    }

    #[test]
    fn defaults_fill_in_omitted_fields() {
        let mut sync = field("sync", FieldKind::Scalar(ScalarType::U16), Endianness::Big);
        sync.default = Some(Value::Uint(0xAA55));
        let frame = FrameDef::flat("defaulted".to_owned(), vec![sync]);
        assert_eq!(
            encode(&frame, &FieldValues::new()).unwrap(),
            vec![0xAA, 0x55]
        );
    }

    #[test]
    fn a_field_with_neither_value_nor_default_is_an_error() {
        let frame = FrameDef::flat(
            "bare".to_owned(),
            vec![field(
                "x",
                FieldKind::Scalar(ScalarType::U8),
                Endianness::Big,
            )],
        );
        assert!(matches!(
            encode(&frame, &FieldValues::new()),
            Err(CodecError::MissingValue { .. })
        ));
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let frame = FrameDef::flat(
            "narrow".to_owned(),
            vec![field(
                "x",
                FieldKind::Scalar(ScalarType::U8),
                Endianness::Big,
            )],
        );
        let mut values = FieldValues::new();
        values.insert("x".to_owned(), Value::Uint(256));
        assert!(matches!(
            encode(&frame, &values),
            Err(CodecError::OutOfRange { .. })
        ));
    }

    #[test]
    fn decoding_the_wrong_length_is_an_error() {
        let frame = crc_frame();
        assert!(matches!(
            decode(&frame, &[0, 1, 2]),
            Err(CodecError::FrameLength { .. })
        ));
    }

    #[test]
    fn text_is_nul_padded_and_trimmed() {
        let frame = FrameDef::flat(
            "label".to_owned(),
            vec![field("tag", FieldKind::Text { len: 6 }, Endianness::Big)],
        );
        let mut values = FieldValues::new();
        values.insert("tag".to_owned(), Value::Text("ok".to_owned()));

        let bytes = encode(&frame, &values).unwrap();
        assert_eq!(bytes, b"ok\0\0\0\0");

        let decoded = decode(&frame, &bytes).unwrap();
        assert_eq!(decoded.values["tag"], Value::Text("ok".to_owned()));
    }
}
