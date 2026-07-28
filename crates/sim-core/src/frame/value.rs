use std::collections::BTreeMap;

/// A decoded field value, or one supplied by the caller before encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Uint(u64),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Text(String),
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
}

/// Field name to value, as handed to the encoder or produced by the decoder.
pub type FieldValues = BTreeMap<String, Value>;
