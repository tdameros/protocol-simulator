pub mod checksum;
pub mod codec;
pub mod schema;
pub mod value;

use checksum::ChecksumSpec;
use value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endianness {
    Big,
    /// What a file means by saying nothing, most of the hardware this talks to
    /// being little-endian.
    #[default]
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
    pub const ALL: [Self; 10] = [
        Self::U8,
        Self::I8,
        Self::U16,
        Self::I16,
        Self::U32,
        Self::I32,
        Self::U64,
        Self::I64,
        Self::F32,
        Self::F64,
    ];

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

    /// Everything this representation can hold, as a range.
    ///
    /// A declared subtype has to fit inside it.
    #[must_use]
    pub fn representable(self) -> ValueRange {
        match self {
            Self::U8 => ValueRange::Uint {
                min: 0,
                max: u64::from(u8::MAX),
            },
            Self::U16 => ValueRange::Uint {
                min: 0,
                max: u64::from(u16::MAX),
            },
            Self::U32 => ValueRange::Uint {
                min: 0,
                max: u64::from(u32::MAX),
            },
            Self::U64 => ValueRange::Uint {
                min: 0,
                max: u64::MAX,
            },
            Self::I8 => ValueRange::Int {
                min: i64::from(i8::MIN),
                max: i64::from(i8::MAX),
            },
            Self::I16 => ValueRange::Int {
                min: i64::from(i16::MIN),
                max: i64::from(i16::MAX),
            },
            Self::I32 => ValueRange::Int {
                min: i64::from(i32::MIN),
                max: i64::from(i32::MAX),
            },
            Self::I64 => ValueRange::Int {
                min: i64::MIN,
                max: i64::MAX,
            },
            Self::F32 | Self::F64 => ValueRange::Float {
                min: f64::NEG_INFINITY,
                max: f64::INFINITY,
            },
        }
    }

    /// Whether a bitfield or enum may be represented by this type.
    #[must_use]
    pub fn is_unsigned_integer(self) -> bool {
        matches!(self, Self::U8 | Self::U16 | Self::U32 | Self::U64)
    }
}

/// A scalar restricted to part of what its representation can hold.
///
/// The Ada idea: `u8` says how many bytes go on the wire, `0 ..= 99` says which
/// of those values the protocol actually allows. Kept per representation rather
/// than as a pair of floats so a `u64` bound stays exact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueRange {
    Uint { min: u64, max: u64 },
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
}

impl ValueRange {
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Uint { min, max }, Value::Uint(v)) => (min..=max).contains(&v),
            (Self::Int { min, max }, Value::Int(v)) => (min..=max).contains(&v),
            (Self::Float { min, max }, Value::Float(v)) => (min..=max).contains(&v),
            // A value of the wrong shape is the encoder's complaint, not ours.
            _ => true,
        }
    }

    /// Whether every value this range allows is also allowed by `wider`.
    ///
    /// Used to check that a subtype narrowing another stays inside it.
    #[must_use]
    pub fn is_within(&self, wider: &Self) -> bool {
        match (self, wider) {
            (Self::Uint { min, max }, Self::Uint { min: lo, max: hi }) => min >= lo && max <= hi,
            (Self::Int { min, max }, Self::Int { min: lo, max: hi }) => min >= lo && max <= hi,
            (Self::Float { min, max }, Self::Float { min: lo, max: hi }) => min >= lo && max <= hi,
            _ => false,
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Uint { min, max } => format!("{min}..{max}"),
            Self::Int { min, max } => format!("{min}..{max}"),
            Self::Float { min, max } => format!("{min}..{max}"),
        }
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
    /// Narrower than the representation allows, if the field says so.
    pub range: Option<ValueRange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDef {
    pub name: String,
    pub description: Option<String>,
    /// The byte order every field inherits unless it says otherwise.
    ///
    /// Held even though each field already carries its own resolved order: an
    /// editor adding a field has to know what the frame's own answer is, and a
    /// writer has to know which fields are worth stating an order for.
    pub endian: Endianness,
    /// Every field on the wire, types and repeats already expanded.
    pub fields: Vec<FieldDef>,
    /// How the file writes the declared fields that are not plain builtins.
    ///
    /// A name on its own does not say that `zone` was `type = "Zone"` with two
    /// named instances. Without that, a frame can be shown and moved about but
    /// never written back, since twenty-one expanded fields cannot be folded
    /// into the four lines that produced them.
    pub stated: std::collections::BTreeMap<String, Stated>,
    /// The names the file actually writes, in the order it writes them.
    ///
    /// A frame that instantiates a type or repeats a field declares four things
    /// and carries twenty-one, and nothing in `fields` says which is which. An
    /// editor has to know: an expanded field cannot be changed where it sits,
    /// only where it is declared, and rewriting it as a plain field would flatten
    /// what someone took the trouble to factorise.
    pub declared: Vec<String>,
}

/// What a file says about a declared field beyond its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stated {
    /// The type it is written as, builtin or shared.
    pub kind: String,
    /// Repeated this many times, as `name[0]`, `name[1]` and so on.
    pub repeat: Option<usize>,
    /// Repeated once per name, as `name.left`, `name.right`.
    pub instances: Option<Vec<String>>,
}

impl FrameDef {
    /// A frame whose every field is written out as its own entry.
    ///
    /// What a file with no types and no repeats produces, and what anything
    /// building a frame by hand means.
    #[must_use]
    pub fn flat(name: impl Into<String>, fields: Vec<FieldDef>) -> Self {
        Self {
            name: name.into(),
            description: None,
            endian: Endianness::default(),
            stated: std::collections::BTreeMap::new(),
            declared: fields.iter().map(|field| field.name.clone()).collect(),
            fields,
        }
    }

    /// The declared field a wire field came from, or `None` where the file
    /// writes it directly.
    ///
    /// Matched on the longest declared name it starts at, since expansion only
    /// ever appends: `zone` produces `zone.left.led[0].mode`, and a repeat of
    /// `led` produces `led[0]`. The separator has to be there, or `zone_count`
    /// would look like it came from `zone`.
    #[must_use]
    pub fn generated_by(&self, field: &str) -> Option<&str> {
        if self.declared.iter().any(|name| name == field) {
            return None;
        }
        self.declared
            .iter()
            .filter(|name| {
                field
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with(['.', '[']))
            })
            .max_by_key(|name| name.len())
            .map(String::as_str)
    }

    /// The wire fields a declared field stands for, as a contiguous range.
    ///
    /// A plain field stands for itself and the range holds one. A type instance
    /// or a repeat stands for everything it expanded into, which is what makes
    /// it movable and removable as one thing rather than as twenty.
    ///
    /// Empty for a name nothing declared.
    #[must_use]
    pub fn expansion_of(&self, declared: &str) -> std::ops::Range<usize> {
        let mut covered = self.fields.iter().enumerate().filter(|(_, field)| {
            field.name == declared || self.generated_by(&field.name) == Some(declared)
        });
        let Some((first, _)) = covered.next() else {
            return 0..0;
        };
        let last = covered.next_back().map_or(first, |(at, _)| at);
        first..last + 1
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(declared: &[&str], wire: &[&str]) -> FrameDef {
        FrameDef {
            name: "F".to_owned(),
            description: None,
            endian: Endianness::Big,
            stated: std::collections::BTreeMap::new(),
            declared: declared.iter().map(|name| (*name).to_owned()).collect(),
            fields: wire
                .iter()
                .map(|name| FieldDef {
                    name: (*name).to_owned(),
                    description: None,
                    kind: FieldKind::Scalar(ScalarType::U8),
                    endian: Endianness::Big,
                    default: None,
                    range: None,
                })
                .collect(),
        }
    }

    #[test]
    fn an_expanded_field_is_traced_back_to_the_one_that_declared_it() {
        let frame = frame(
            &["header", "zone", "zone_count", "led"],
            &[
                "header",
                "zone.left.accent.red",
                "zone_count",
                "led[0]",
                "led[1]",
            ],
        );

        // A type instance, however deeply nested, and a repeat.
        assert_eq!(frame.generated_by("zone.left.accent.red"), Some("zone"));
        assert_eq!(frame.generated_by("led[0]"), Some("led"));

        // The trap the separator check exists for: `zone_count` starts with
        // `zone` and has nothing to do with it.
        assert_eq!(frame.generated_by("zone_count"), None);
        assert_eq!(frame.generated_by("header"), None);
    }

    #[test]
    fn the_longest_declared_name_wins() {
        // A field declared inside what looks like another one's territory is
        // still its own, so its expansions belong to it and not to the shorter
        // name it happens to sit under.
        let frame = frame(&["a", "a.b"], &["a.x", "a.b.y"]);
        assert_eq!(frame.generated_by("a.x"), Some("a"));
        assert_eq!(frame.generated_by("a.b.y"), Some("a.b"));
    }

    #[test]
    fn a_flat_frame_declares_everything_it_carries() {
        let built = FrameDef::flat(
            "F",
            vec![FieldDef {
                name: "only".to_owned(),
                description: None,
                kind: FieldKind::Scalar(ScalarType::U8),
                endian: Endianness::Big,
                default: None,
                range: None,
            }],
        );
        assert_eq!(built.declared, ["only"]);
        assert_eq!(built.generated_by("only"), None);
    }
}
