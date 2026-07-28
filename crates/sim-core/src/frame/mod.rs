pub mod checksum;
pub mod value;

use checksum::ChecksumSpec;
use value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endianness {
    #[default]
    Big,
    Little,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl ScalarType {
    #[must_use]
    pub fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }

    /// Parses the spelling used in frame files, e.g. `u16`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "u8" => Self::U8,
            "i8" => Self::I8,
            "u16" => Self::U16,
            "i16" => Self::I16,
            "u32" => Self::U32,
            "i32" => Self::I32,
            "u64" => Self::U64,
            "i64" => Self::I64,
            "f32" => Self::F32,
            "f64" => Self::F64,
            _ => return None,
        })
    }

    /// Whether a bitfield or enum may be represented by this type.
    #[must_use]
    pub fn is_unsigned_integer(self) -> bool {
        matches!(self, Self::U8 | Self::U16 | Self::U32 | Self::U64)
    }
}

/// One sub-field inside a bitfield container, packed most significant bit first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitDef {
    pub name: String,
    pub width: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    pub name: String,
    pub value: u64,
}

/// Field indices covered by a checksum, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpan {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Scalar(ScalarType),
    Bytes {
        len: usize,
    },
    /// Fixed-width text, padded with NUL on encode and trimmed on decode.
    Text {
        len: usize,
    },
    Enum {
        repr: ScalarType,
        variants: Vec<EnumVariant>,
    },
    Bits {
        repr: ScalarType,
        bits: Vec<BitDef>,
    },
    Checksum {
        spec: ChecksumSpec,
        covers: FieldSpan,
    },
}

impl FieldKind {
    /// Encoded size in bytes. Every kind is fixed width in this version.
    #[must_use]
    pub fn size(&self) -> usize {
        match self {
            Self::Scalar(scalar)
            | Self::Enum { repr: scalar, .. }
            | Self::Bits { repr: scalar, .. } => scalar.size(),
            Self::Bytes { len } | Self::Text { len } => *len,
            Self::Checksum { spec, .. } => spec.width_bytes(),
        }
    }

    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Scalar(scalar) => scalar.name(),
            Self::Bytes { .. } => "bytes",
            Self::Text { .. } => "text",
            Self::Enum { .. } => "enum",
            Self::Bits { .. } => "bits",
            Self::Checksum { .. } => "checksum",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub description: Option<String>,
    pub kind: FieldKind,
    /// Resolved when the frame is loaded: the frame default unless overridden.
    pub endian: Endianness,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDef {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<FieldDef>,
}

impl FrameDef {
    #[must_use]
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|field| field.name == name)
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Total encoded size in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.fields.iter().map(|field| field.kind.size()).sum()
    }

    /// Byte offset at which `index` starts.
    #[must_use]
    pub fn offset_of(&self, index: usize) -> usize {
        self.fields[..index]
            .iter()
            .map(|field| field.kind.size())
            .sum()
    }
}
