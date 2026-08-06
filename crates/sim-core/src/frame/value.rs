use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{FieldKind, ScalarType};

/// A decoded field value, or one supplied by the caller before encoding.
///
/// Written down as the bare value it holds, so a saved frame reads as
/// `speed = 120` rather than as a tagged union. Which shape a bare value comes
/// back as is then a guess, resolved by [`Value::coerced_to`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Uint(u64),
    Int(i64),
    Float(f64),
    /// Ahead of `Bytes` on purpose. A quoted string always reads back as text,
    /// even one that looks like hex, so a serial number of "1234" survives being
    /// written down. Turning it back into bytes is the field's business, not the
    /// parser's.
    Text(String),
    /// Written as hex, read back from a list of byte values.
    #[serde(serialize_with = "hex_bytes::serialize")]
    Bytes(Vec<u8>),
    /// Sub-field name to value, for a bitfield container.
    Bits(BTreeMap<String, u64>),
}

impl Value {
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(v) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(v) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bits(&self) -> Option<&BTreeMap<String, u64>> {
        match self {
            Self::Bits(v) => Some(v),
            _ => None,
        }
    }

    /// Short label used in error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Uint(_) => "unsigned integer",
            Self::Int(_) => "signed integer",
            Self::Float(_) => "float",
            Self::Bytes(_) => "bytes",
            Self::Text(_) => "text",
            Self::Bits(_) => "bitfield",
        }
    }

    /// The same value in the shape `kind` requires, or `None` if it cannot mean
    /// anything there.
    ///
    /// A value read back from a file arrives in whichever shape its written form
    /// suggested, which is not always the one the field declares: `0` is an
    /// integer even for a float field, and `"DEAD"` is text even for a byte
    /// array. The encoder is strict about shapes, so a value coming from outside
    /// is put through here first, against the definition that is actually
    /// loaded. That also absorbs a frame whose type changed since the value was
    /// written down, which is what a file left alone for a month tends to be.
    #[must_use]
    pub fn coerced_to(self, kind: &FieldKind) -> Option<Self> {
        Some(match kind {
            FieldKind::Scalar(ScalarType::F32 | ScalarType::F64) => Self::Float(self.to_float()?),
            FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
                Self::Uint(self.to_uint()?)
            }
            FieldKind::Scalar(_) => Self::Int(self.to_int()?),
            FieldKind::Enum { .. } => Self::Uint(self.to_uint()?),
            FieldKind::Bytes { .. } => Self::Bytes(self.to_bytes()?),
            FieldKind::Text { .. } => Self::Text(self.to_text()?),
            FieldKind::Bits { .. } => Self::Bits(match self {
                Self::Bits(bits) => bits,
                _ => return None,
            }),
            // Computed at encode time, so a written-down one is stale by
            // definition and better dropped than restored.
            FieldKind::Checksum { .. } => return None,
        })
    }

    fn to_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(value) => Some(*value),
            Self::Int(value) => u64::try_from(*value).ok(),
            // Only what a file could not hold as an integer in the first place.
            Self::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    fn to_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Uint(value) => i64::try_from(*value).ok(),
            Self::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "a float field cannot hold more precision than a float"
    )]
    fn to_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            Self::Uint(value) => Some(*value as f64),
            Self::Int(value) => Some(*value as f64),
            Self::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    fn to_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Some(bytes.clone()),
            Self::Text(text) => hex_bytes::decode(text).ok(),
            _ => None,
        }
    }

    fn to_text(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Bytes(bytes) => String::from_utf8(bytes.clone()).ok(),
            _ => None,
        }
    }
}

/// Field name to value, as handed to the encoder or produced by the decoder.
pub type FieldValues = BTreeMap<String, Value>;

/// Byte arrays go out as hex, which is how anyone reading the file thinks of
/// them. They do not come back in through here: see the `Text` variant.
mod hex_bytes {
    use std::fmt::Write as _;

    use serde::Serializer;

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02X}");
        }
        serializer.serialize_str(&text)
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, ()> {
        let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if !cleaned.len().is_multiple_of(2) || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(());
        }
        (0..cleaned.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&cleaned[index..index + 2], 16).map_err(|_| ()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values only travel inside a table, TOML having nowhere to put a bare one.
    fn through_toml(values: &FieldValues) -> FieldValues {
        let text = toml::to_string(values).expect("values should serialise");
        toml::from_str(&text).expect("values should parse back")
    }

    fn one(value: Value) -> FieldValues {
        FieldValues::from([("field".to_owned(), value)])
    }

    fn read_back(value: Value, kind: &FieldKind) -> Option<Value> {
        through_toml(&one(value))
            .remove("field")
            .expect("the entry should survive")
            .coerced_to(kind)
    }

    #[test]
    fn a_written_value_comes_back_in_the_shape_its_field_declares() {
        // Each pair is what a file holds, and what the field it belongs to needs.
        let cases = [
            (Value::Uint(0xAA55), FieldKind::Scalar(ScalarType::U16)),
            (Value::Int(-40), FieldKind::Scalar(ScalarType::I8)),
            (Value::Float(1.5), FieldKind::Scalar(ScalarType::F32)),
            (Value::Text("ready".to_owned()), FieldKind::Text { len: 8 }),
            (Value::Bytes(vec![0xDE, 0xAD]), FieldKind::Bytes { len: 2 }),
            (
                Value::Bits(BTreeMap::from([("armed".to_owned(), 1)])),
                FieldKind::Bits {
                    repr: ScalarType::U8,
                    bits: Vec::new(),
                },
            ),
        ];

        for (value, kind) in cases {
            assert_eq!(
                read_back(value.clone(), &kind),
                Some(value.clone()),
                "{value:?} should come back unchanged for a {}",
                kind.type_name()
            );
        }
    }

    #[test]
    fn text_that_looks_like_hex_stays_text() {
        // The trap this ordering exists for: a serial number of "1234" read back
        // as two bytes would be silent corruption.
        assert_eq!(
            read_back(Value::Text("1234".to_owned()), &FieldKind::Text { len: 4 }),
            Some(Value::Text("1234".to_owned()))
        );
    }

    #[test]
    fn bytes_are_written_as_hex_and_read_back_from_it() {
        let text = toml::to_string(&one(Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])))
            .expect("should serialise");
        assert!(text.contains("\"DEADBEEF\""), "{text}");
        // Which arrives as text, and only the field definition says otherwise.
        assert_eq!(
            read_back(
                Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
                &FieldKind::Bytes { len: 4 }
            ),
            Some(Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]))
        );
    }

    #[test]
    fn a_whole_number_written_for_a_float_field_is_taken_as_one() {
        // What a hand-edited file looks like, and what the encoder refuses.
        assert_eq!(
            Value::Uint(0).coerced_to(&FieldKind::Scalar(ScalarType::F32)),
            Some(Value::Float(0.0))
        );
        assert_eq!(
            Value::Int(-3).coerced_to(&FieldKind::Scalar(ScalarType::F64)),
            Some(Value::Float(-3.0))
        );
    }

    #[test]
    fn a_value_that_cannot_mean_anything_there_is_dropped() {
        // Negative into unsigned, and text into a number.
        assert!(Value::Int(-1)
            .coerced_to(&FieldKind::Scalar(ScalarType::U8))
            .is_none());
        assert!(Value::Text("nope".to_owned())
            .coerced_to(&FieldKind::Scalar(ScalarType::U8))
            .is_none());
        // A bitfield is the one shape nothing else converts into.
        assert!(Value::Uint(3)
            .coerced_to(&FieldKind::Bits {
                repr: ScalarType::U8,
                bits: Vec::new(),
            })
            .is_none());
    }

    #[test]
    fn a_stale_checksum_is_never_restored() {
        let kind = FieldKind::Checksum {
            spec: crate::frame::checksum::ChecksumSpec::Xor8,
            covers: crate::frame::FieldSpan { from: 0, to: 1 },
        };
        assert!(Value::Uint(0x42).coerced_to(&kind).is_none());
    }

    #[test]
    fn an_unsigned_value_too_large_for_toml_still_survives() {
        // TOML integers are signed, so anything past i64::MAX has to go out as
        // text and be read back by the field that knows it is a u64.
        let values = one(Value::Uint(u64::MAX));
        match toml::to_string(&values) {
            Ok(text) => {
                let back = toml::from_str::<FieldValues>(&text)
                    .expect("should parse back")
                    .remove("field")
                    .expect("entry should survive")
                    .coerced_to(&FieldKind::Scalar(ScalarType::U64));
                assert_eq!(back, Some(Value::Uint(u64::MAX)), "through:\n{text}");
            }
            Err(error) => panic!("toml refused a u64 field value: {error}"),
        }
    }
}
