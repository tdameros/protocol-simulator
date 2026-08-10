//! Reading and writing frame definitions as TOML.
//!
//! The file format is mirrored by plain `Raw*` structs and then converted into
//! the domain model. Going through an intermediate representation keeps serde
//! attributes out of the model and, more importantly, lets validation report
//! what is wrong in the file rather than failing with a deserialiser message.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::Item;

use crate::document;

use super::checksum::{ChecksumSpec, CrcSpec};
use super::value::Value;
use super::{
    BitDef, Endianness, EnumVariant, FieldDef, FieldKind, FieldSpan, FrameDef, ScalarType,
    ValueRange,
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

    #[error("invalid toml: {0}")]
    Edit(toml_edit::TomlError),

    #[error("cannot rewrite frame: {0}")]
    Rewrite(toml_edit::ser::Error),

    #[error("field {field}: unknown type {kind}, expected one of {known}")]
    UnknownType {
        field: String,
        kind: String,
        known: String,
    },

    #[error("duplicate type name {name}")]
    DuplicateType { name: String },

    #[error("type {name} has no fields")]
    EmptyType { name: String },

    #[error("type {name} contains itself, through {path}")]
    RecursiveType { name: String, path: String },

    #[error("field {field}: repeat and instances cannot both be set")]
    RepeatAndInstances { field: String },

    #[error("field {field}: repeat must be at least 1")]
    EmptyRepeat { field: String },

    #[error("field {field}: instances must name at least one instance")]
    EmptyInstances { field: String },

    #[error("field {field}: duplicate instance name {instance}")]
    DuplicateInstance { field: String, instance: String },

    #[error("field {field}: {attribute} does not apply to type {kind}")]
    UnexpectedAttribute {
        field: String,
        kind: String,
        attribute: &'static str,
    },

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

    #[error("field {field}: range does not apply to {kind}")]
    RangeOnNonScalar { field: String, kind: String },

    #[error("field {field}: range {range} runs backwards")]
    BackwardsRange { field: String, range: String },

    #[error("field {field}: range {range} does not fit in {repr}")]
    RangeOutOfType {
        field: String,
        range: String,
        repr: String,
    },

    #[error("{owner}: range {inner} is not inside {outer}")]
    RangeNotWithin {
        owner: String,
        inner: String,
        outer: String,
    },

    #[error("type {name}: base and field cannot both be set")]
    TypeIsBothRecordAndSubtype { name: String },

    #[error("type {name}: base {base} must be a scalar or another subtype")]
    BadSubtypeBase { name: String, base: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawBit {
    name: String,
    width: u32,
}

/// The `range = { min = .., max = .. }` attribute, still as written.
///
/// Held as raw TOML because what the bounds mean depends on the scalar they
/// constrain, which is only known once the type reference is resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawRange {
    min: toml::Value,
    max: toml::Value,
}

impl RawRange {
    fn describe(&self) -> String {
        format!("{}..{}", self.min, self.max)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawCovers {
    from: String,
    to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instances: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<RawRange>,
}

/// A reusable named type: a record when it lists fields, a scalar subtype when
/// it gives a `base` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawType {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The scalar, or the subtype, this one narrows.
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<RawRange>,
    #[serde(default, rename = "field")]
    fields: Vec<RawField>,
}

impl RawType {
    fn is_subtype(&self) -> bool {
        self.base.is_some()
    }
}

/// A file holding nothing but shared type definitions.
#[derive(Debug, Default, Deserialize)]
struct RawTypeFile {
    #[serde(default, rename = "type")]
    types: Vec<RawType>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawFrame {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endian: Option<RawEndian>,
    #[serde(default, rename = "type", skip_serializing_if = "Vec::is_empty")]
    types: Vec<RawType>,
    #[serde(default, rename = "field")]
    fields: Vec<RawField>,
}

/// Type definitions shared by every frame in a directory.
///
/// A frame may also declare types inline; those win over the library when the
/// names collide, so a frame can specialise a shared type without renaming it.
#[derive(Debug, Default, Clone)]
pub struct TypeLibrary {
    types: BTreeMap<String, RawType>,
}

impl TypeLibrary {
    /// Adds the `[[type]]` blocks found in `text`.
    ///
    /// # Errors
    ///
    /// Returns an error if the text is not valid TOML or redefines a type the
    /// library already holds.
    pub fn merge_toml(&mut self, text: &str) -> Result<(), SchemaError> {
        let file: RawTypeFile = toml::from_str(text)?;
        for raw in file.types {
            if self.types.contains_key(&raw.name) {
                return Err(SchemaError::DuplicateType { name: raw.name });
            }
            self.types.insert(raw.name.clone(), raw);
        }
        Ok(())
    }

    /// Adds the type definitions held in one file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or is not valid.
    pub fn merge_file(&mut self, path: &Path) -> Result<(), SchemaError> {
        let text = std::fs::read_to_string(path).map_err(|source| SchemaError::Read {
            path: path.display().to_string(),
            source,
        })?;
        self.merge_toml(&text)
    }

    /// Loads every `.toml` file in `directory`, in name order.
    ///
    /// A directory that does not exist yields an empty library: sharing types is
    /// opt-in, so its absence is the normal case rather than a failure.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be listed or a file is invalid.
    pub fn load_dir(directory: &Path) -> Result<Self, SchemaError> {
        let mut library = Self::default();
        if !directory.is_dir() {
            return Ok(library);
        }
        for path in toml_files(directory)? {
            library.merge_file(&path)?;
        }
        Ok(library)
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.types.keys().map(String::as_str).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Lists the `.toml` files directly inside `directory`, in name order.
///
/// # Errors
///
/// Returns an error if the directory cannot be listed.
pub fn toml_files(directory: &Path) -> Result<Vec<std::path::PathBuf>, SchemaError> {
    let entries = std::fs::read_dir(directory).map_err(|source| SchemaError::Read {
        path: directory.display().to_string(),
        source,
    })?;
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Parses a frame definition from TOML text.
///
/// # Errors
///
/// Returns an error if the text is not valid TOML or describes an inconsistent
/// frame (unknown type, bit widths that do not fill the representation, a
/// checksum covering a field that does not exist, ...).
pub fn from_toml(text: &str) -> Result<FrameDef, SchemaError> {
    from_toml_with(text, &TypeLibrary::default())
}

/// Parses a frame definition that may instantiate types from a shared library.
///
/// # Errors
///
/// As [`from_toml`], plus an error if a field names a type that is neither a
/// builtin nor defined inline or in `library`.
pub fn from_toml_with(text: &str, library: &TypeLibrary) -> Result<FrameDef, SchemaError> {
    let raw: RawFrame = toml::from_str(text)?;
    build(raw, library)
}

/// Renders a frame definition back to TOML.
///
/// # Errors
///
/// Returns an error if the frame cannot be serialised.
pub fn to_toml(frame: &FrameDef) -> Result<String, SchemaError> {
    Ok(toml::to_string_pretty(&lower(frame))?)
}

/// Writes a frame back into the text it was loaded from.
///
/// Unlike [`to_toml`], which serialises the expanded layout and so loses both
/// the comments and the factorisation, this copies the frame over the document
/// key by key. What the file says stays where it was written.
///
/// Only the fields the file wrote are touched, and among those only the ones
/// the model can express on its own: a field standing for a type instance or a
/// repeat is left exactly as typed, because the expansion it produced cannot be
/// folded back into it. Editing such a field means editing its type.
///
/// # Errors
///
/// Returns an error if the text is not valid TOML, or the frame not
/// serialisable.
pub fn update_in(text: &str, frame: &FrameDef) -> Result<String, SchemaError> {
    let mut document: toml_edit::DocumentMut = text.parse().map_err(SchemaError::Edit)?;
    let fresh = as_document(frame)?;

    // `endian` is spent at load time, folded into each field's scalar type, so
    // the model has nothing left to say about it and the file keeps the last
    // word. `type` likewise: the expansion is downstream of it.
    document::merge(
        document.as_table_mut(),
        fresh.as_table(),
        &["endian", "type", "field"],
    );
    merge_fields(&mut document, frame, fresh.as_table());
    Ok(document.to_string())
}

fn merge_fields(document: &mut toml_edit::DocumentMut, frame: &FrameDef, fresh: &toml_edit::Table) {
    let written: BTreeMap<String, toml_edit::Table> = sections(fresh, "field")
        .into_iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Item::as_str)?.to_owned();
            Some((name, entry))
        })
        .collect();

    if document.get("field").is_none() {
        document.insert(
            "field",
            Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let mut next = document::last_position(document);
    let Some(entries) = document
        .get_mut("field")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return;
    };

    entries.retain(|entry| {
        entry
            .get("name")
            .and_then(Item::as_str)
            .is_some_and(|name| frame.declared.iter().any(|declared| declared == name))
    });

    let mut present: Vec<String> = Vec::new();
    for entry in entries.iter_mut() {
        let Some(name) = entry.get("name").and_then(Item::as_str).map(str::to_owned) else {
            continue;
        };
        if let Some(replacement) = written.get(&name) {
            document::merge(entry, replacement, kept_from_the_file(entry));
        }
        present.push(name);
    }

    for name in &frame.declared {
        if present.iter().any(|seen| seen == name) {
            continue;
        }
        let Some(fresh) = written.get(name) else {
            continue;
        };
        let mut appended = fresh.clone();
        document::place_after(&mut appended, &mut next);
        entries.push(appended);
    }
}

/// What the model is not allowed to overwrite on a field the file already
/// wrote.
///
/// Byte order is always the file's, being spent at load time. A field naming a
/// type from the library gives up more: the range, the representation, the
/// variants and the bits all came from that type, and writing them back would
/// replace `type = "Percent"` with the byte and the bounds it stands for. What
/// belongs to the field itself, its default and the range a checksum covers,
/// stays editable.
fn kept_from_the_file(entry: &toml_edit::Table) -> &'static [&'static str] {
    let names_a_type = entry
        .get("type")
        .and_then(Item::as_str)
        .is_some_and(|kind| !is_builtin_kind(kind));
    if names_a_type {
        &["endian", "type", "range", "repr", "variants", "bits"]
    } else {
        &["endian"]
    }
}

/// How a checksum range names one of its ends.
///
/// A range written against a declared field means every byte that field
/// expanded into, so naming it back is not only shorter, it keeps the range
/// following the type: add an instance and the checksum still covers all of
/// them. Naming the expanded field instead would pin the range where it stands
/// today. The expanded name is used only when the end falls inside a type
/// rather than on its edge.
fn cover_name(frame: &FrameDef, index: usize, edge: Edge) -> String {
    let field = &frame.fields[index];
    let Some(declared) = frame.generated_by(&field.name) else {
        return field.name.clone();
    };
    let mut expansion = frame
        .fields
        .iter()
        .enumerate()
        .filter(|(_, other)| frame.generated_by(&other.name) == Some(declared))
        .map(|(at, _)| at);
    let edge_of_expansion = match edge {
        Edge::First => expansion.next(),
        Edge::Last => expansion.next_back(),
    };
    if edge_of_expansion == Some(index) {
        declared.to_owned()
    } else {
        field.name.clone()
    }
}

/// The entries under `key`, however they were written.
///
/// Serialising a frame produces one array of inline tables where a person would
/// have written a run of `[[field]]` sections. Both mean the same thing, and
/// the writer has to read the first while editing the second.
fn sections(table: &toml_edit::Table, key: &str) -> Vec<toml_edit::Table> {
    match table.get(key) {
        Some(Item::ArrayOfTables(entries)) => entries.iter().cloned().collect(),
        Some(Item::Value(toml_edit::Value::Array(entries))) => entries
            .iter()
            .filter_map(toml_edit::Value::as_inline_table)
            .map(|entry| entry.clone().into_table())
            .collect(),
        _ => Vec::new(),
    }
}

fn as_document(frame: &FrameDef) -> Result<toml_edit::DocumentMut, SchemaError> {
    let mut raw = lower(frame);
    // A field the file did not write is one this frame cannot describe on its
    // own, so it has no business being written back.
    raw.fields
        .retain(|field| frame.declared.contains(&field.name));
    // Every field carries a resolved endianness, most of them inherited from
    // the frame. Writing that back would stamp `endian` onto fields that never
    // asked for it, so byte order stays the file's to state.
    for field in &mut raw.fields {
        field.endian = None;
    }
    toml_edit::ser::to_document(&raw).map_err(SchemaError::Rewrite)
}

/// Loads a frame definition from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not a valid frame.
pub fn load(path: &Path) -> Result<FrameDef, SchemaError> {
    load_with(path, &TypeLibrary::default())
}

/// Loads a frame definition that may instantiate types from a shared library.
///
/// # Errors
///
/// As [`load`], plus an error if a field names an unknown type.
pub fn load_with(path: &Path, library: &TypeLibrary) -> Result<FrameDef, SchemaError> {
    let text = std::fs::read_to_string(path).map_err(|source| SchemaError::Read {
        path: path.display().to_string(),
        source,
    })?;
    from_toml_with(&text, library)
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

fn build(raw: RawFrame, library: &TypeLibrary) -> Result<FrameDef, SchemaError> {
    let frame_endian: Endianness = raw.endian.map(Into::into).unwrap_or_default();

    let mut types = library.types.clone();
    let mut declared: Vec<&str> = Vec::new();
    for local in &raw.types {
        if declared.contains(&local.name.as_str()) {
            return Err(SchemaError::DuplicateType {
                name: local.name.clone(),
            });
        }
        declared.push(&local.name);
        // An inline definition shadows the shared one, so a frame can specialise
        // a library type without having to invent a new name for it.
        types.insert(local.name.clone(), local.clone());
    }

    // Captured before expansion, which is the only moment the two are still
    // distinguishable.
    let written: Vec<String> = raw.fields.iter().map(|field| field.name.clone()).collect();
    let expanded = expand(&raw.fields, &types)?;

    let mut seen: Vec<&str> = Vec::new();
    for field in &expanded {
        if seen.contains(&field.name.as_str()) {
            return Err(SchemaError::DuplicateField {
                name: field.name.clone(),
            });
        }
        seen.push(&field.name);
    }
    let names: Vec<String> = expanded.iter().map(|field| field.name.clone()).collect();

    let mut fields = Vec::with_capacity(expanded.len());
    for (index, field) in expanded.iter().enumerate() {
        let kind = build_kind(field, index, &names)?;
        let range = build_range(field, &kind)?;
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
            range,
            kind,
        });
    }

    Ok(FrameDef {
        name: raw.name,
        description: raw.description,
        fields,
        declared: written,
    })
}

/// Type names that are not scalars, kept in one place so [`is_builtin_kind`]
/// and [`build_kind`] cannot drift apart.
const BUILTIN_KINDS: &[&str] = &[
    "bytes", "text", "enum", "bits", "xor8", "sum8", "sum16", "crc8", "crc16", "crc32",
];

fn is_builtin_kind(kind: &str) -> bool {
    ScalarType::parse(kind).is_some() || BUILTIN_KINDS.contains(&kind)
}

fn known_kinds(types: &BTreeMap<String, RawType>) -> String {
    ScalarType::ALL
        .iter()
        .map(|scalar| scalar.name())
        .chain(BUILTIN_KINDS.iter().copied())
        .chain(types.keys().map(String::as_str))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Replaces every type instantiation with the fields it stands for.
///
/// The rest of the crate only ever sees a flat list, so templates cost the
/// codec, the checksums and the editor nothing.
fn expand(
    fields: &[RawField],
    types: &BTreeMap<String, RawType>,
) -> Result<Vec<RawField>, SchemaError> {
    let mut out = Vec::with_capacity(fields.len());
    let mut stack: Vec<&str> = Vec::new();
    expand_into(fields, types, None, None, &mut stack, &mut out)?;
    Ok(out)
}

fn expand_into<'a>(
    fields: &'a [RawField],
    types: &'a BTreeMap<String, RawType>,
    prefix: Option<&str>,
    inherited_endian: Option<RawEndian>,
    stack: &mut Vec<&'a str>,
    out: &mut Vec<RawField>,
) -> Result<(), SchemaError> {
    for field in fields {
        let endian = field.endian.or(inherited_endian);
        let paths = instance_paths(field, prefix)?;

        let Some(definition) = types.get(&field.kind) else {
            if !is_builtin_kind(&field.kind) {
                return Err(SchemaError::UnknownType {
                    field: field.name.clone(),
                    kind: field.kind.clone(),
                    known: known_kinds(types),
                });
            }
            for path in paths {
                out.push(instantiate_builtin(field, path, endian, prefix));
            }
            continue;
        };

        // A subtype is one scalar with a narrower range, not a group of fields,
        // so it replaces the field rather than expanding under it.
        if definition.is_subtype() {
            let (scalar, inherited) = resolve_subtype(definition, types, &mut Vec::new())?;
            let range = narrowest(field, inherited, &definition.name)?;
            for path in paths {
                let mut copy = instantiate_builtin(field, path, endian, prefix);
                copy.kind.clone_from(&scalar);
                copy.range.clone_from(&range);
                out.push(copy);
            }
            continue;
        }

        reject_type_only_attributes(field)?;
        if definition.fields.is_empty() {
            return Err(SchemaError::EmptyType {
                name: definition.name.clone(),
            });
        }
        if stack.contains(&definition.name.as_str()) {
            return Err(SchemaError::RecursiveType {
                name: definition.name.clone(),
                path: stack.join(" -> "),
            });
        }

        stack.push(&definition.name);
        for path in paths {
            expand_into(&definition.fields, types, Some(&path), endian, stack, out)?;
        }
        stack.pop();
    }
    Ok(())
}

/// The fully qualified name of every copy this field asks for.
fn instance_paths(field: &RawField, prefix: Option<&str>) -> Result<Vec<String>, SchemaError> {
    let labels = match (field.repeat, field.instances.as_ref()) {
        (Some(_), Some(_)) => {
            return Err(SchemaError::RepeatAndInstances {
                field: field.name.clone(),
            })
        }
        (Some(0), None) => {
            return Err(SchemaError::EmptyRepeat {
                field: field.name.clone(),
            })
        }
        (Some(count), None) => (0..count)
            .map(|index| format!("{}[{index}]", field.name))
            .collect(),
        (None, Some(instances)) if instances.is_empty() => {
            return Err(SchemaError::EmptyInstances {
                field: field.name.clone(),
            })
        }
        (None, Some(instances)) => {
            let mut seen: Vec<&str> = Vec::new();
            for instance in instances {
                if seen.contains(&instance.as_str()) {
                    return Err(SchemaError::DuplicateInstance {
                        field: field.name.clone(),
                        instance: instance.clone(),
                    });
                }
                seen.push(instance);
            }
            instances
                .iter()
                .map(|instance| format!("{}.{instance}", field.name))
                .collect()
        }
        (None, None) => vec![field.name.clone()],
    };

    Ok(match prefix {
        Some(prefix) => labels
            .into_iter()
            .map(|label| format!("{prefix}.{label}"))
            .collect(),
        None => labels,
    })
}

fn instantiate_builtin(
    field: &RawField,
    path: String,
    endian: Option<RawEndian>,
    prefix: Option<&str>,
) -> RawField {
    let mut copy = field.clone();
    copy.name = path;
    copy.endian = endian;
    copy.repeat = None;
    copy.instances = None;
    // A checksum declared inside a type covers its own siblings, so its bounds
    // move with the instance rather than pointing at the first copy.
    if let (Some(prefix), Some(covers)) = (prefix, copy.covers.as_mut()) {
        covers.from = format!("{prefix}.{}", covers.from);
        covers.to = format!("{prefix}.{}", covers.to);
    }
    copy
}

/// Turns a declared range into one typed by the scalar it constrains.
fn build_range(field: &RawField, kind: &FieldKind) -> Result<Option<ValueRange>, SchemaError> {
    let Some(raw) = &field.range else {
        return Ok(None);
    };
    let FieldKind::Scalar(scalar) = kind else {
        return Err(SchemaError::RangeOnNonScalar {
            field: field.name.clone(),
            kind: kind.type_name().to_owned(),
        });
    };

    let out_of_type = || SchemaError::RangeOutOfType {
        field: field.name.clone(),
        range: raw.describe(),
        repr: scalar.name().to_owned(),
    };
    let range = if scalar.is_unsigned_integer() {
        ValueRange::Uint {
            min: unsigned_bound(&raw.min).ok_or_else(out_of_type)?,
            max: unsigned_bound(&raw.max).ok_or_else(out_of_type)?,
        }
    } else if matches!(scalar, ScalarType::F32 | ScalarType::F64) {
        ValueRange::Float {
            min: as_float(&raw.min).ok_or_else(out_of_type)?,
            max: as_float(&raw.max).ok_or_else(out_of_type)?,
        }
    } else {
        ValueRange::Int {
            min: raw.min.as_integer().ok_or_else(out_of_type)?,
            max: raw.max.as_integer().ok_or_else(out_of_type)?,
        }
    };

    if compare(&raw.min, &raw.max) == Some(Ordering::Greater) {
        return Err(SchemaError::BackwardsRange {
            field: field.name.clone(),
            range: raw.describe(),
        });
    }
    if !range.is_within(&scalar.representable()) {
        return Err(out_of_type());
    }
    Ok(Some(range))
}

fn unsigned_bound(value: &toml::Value) -> Option<u64> {
    u64::try_from(value.as_integer()?).ok()
}

/// Follows a subtype down to the scalar it ultimately narrows.
///
/// Returns that scalar's spelling and the tightest range met on the way, each
/// level having to stay inside the one above it.
fn resolve_subtype<'a>(
    definition: &'a RawType,
    types: &'a BTreeMap<String, RawType>,
    stack: &mut Vec<&'a str>,
) -> Result<(String, Option<RawRange>), SchemaError> {
    if !definition.fields.is_empty() {
        return Err(SchemaError::TypeIsBothRecordAndSubtype {
            name: definition.name.clone(),
        });
    }
    let base = definition.base.as_ref().expect("caller checked is_subtype");

    if ScalarType::parse(base).is_some() {
        return Ok((base.clone(), definition.range.clone()));
    }

    let parent = types
        .get(base)
        .filter(|parent| parent.is_subtype())
        .ok_or_else(|| SchemaError::BadSubtypeBase {
            name: definition.name.clone(),
            base: base.clone(),
        })?;
    if stack.contains(&definition.name.as_str()) {
        return Err(SchemaError::RecursiveType {
            name: definition.name.clone(),
            path: stack.join(" -> "),
        });
    }

    stack.push(&definition.name);
    let (scalar, inherited) = resolve_subtype(parent, types, stack)?;
    stack.pop();

    let range = tighten(
        definition.range.clone(),
        inherited,
        &format!("type {}", definition.name),
    )?;
    Ok((scalar, range))
}

/// Combines a field's own range with the one its type brings.
fn narrowest(
    field: &RawField,
    inherited: Option<RawRange>,
    type_name: &str,
) -> Result<Option<RawRange>, SchemaError> {
    tighten(
        field.range.clone(),
        inherited,
        &format!("field {} of type {type_name}", field.name),
    )
}

/// Keeps `own` when it is given, having checked it stays inside `outer`.
fn tighten(
    own: Option<RawRange>,
    outer: Option<RawRange>,
    owner: &str,
) -> Result<Option<RawRange>, SchemaError> {
    let (Some(own), Some(outer)) = (own.clone(), outer.clone()) else {
        return Ok(own.or(outer));
    };
    let inside = matches!(
        compare(&own.min, &outer.min),
        Some(Ordering::Greater | Ordering::Equal)
    ) && matches!(
        compare(&own.max, &outer.max),
        Some(Ordering::Less | Ordering::Equal)
    );
    if !inside {
        return Err(SchemaError::RangeNotWithin {
            owner: owner.to_owned(),
            inner: own.describe(),
            outer: outer.describe(),
        });
    }
    Ok(Some(own))
}

/// Orders two TOML numbers, exactly when both are integers.
fn compare(a: &toml::Value, b: &toml::Value) -> Option<Ordering> {
    match (a, b) {
        (toml::Value::Integer(a), toml::Value::Integer(b)) => Some(a.cmp(b)),
        _ => as_float(a)?.partial_cmp(&as_float(b)?),
    }
}

fn as_float(value: &toml::Value) -> Option<f64> {
    match value {
        #[expect(
            clippy::cast_precision_loss,
            reason = "only reached when one side is a float, where the bound is approximate anyway"
        )]
        toml::Value::Integer(v) => Some(*v as f64),
        toml::Value::Float(v) => Some(*v),
        _ => None,
    }
}

/// Attributes that describe a builtin field cannot mean anything on a type
/// instantiation, and silently ignoring them would hide a real mistake.
fn reject_type_only_attributes(field: &RawField) -> Result<(), SchemaError> {
    let offender = [
        ("default", field.default.is_some()),
        ("repr", field.repr.is_some()),
        ("variants", field.variants.is_some()),
        ("bits", field.bits.is_some()),
        ("len", field.len.is_some()),
        ("algo", field.algo.is_some()),
        ("covers", field.covers.is_some()),
        ("range", field.range.is_some()),
    ]
    .into_iter()
    .find_map(|(name, present)| present.then_some(name));

    match offender {
        Some(attribute) => Err(SchemaError::UnexpectedAttribute {
            field: field.name.clone(),
            kind: field.kind.clone(),
            attribute,
        }),
        None => Ok(()),
    }
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
                known: known_kinds(&BTreeMap::new()),
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
    let resolve = |target: &String, edge: Edge| {
        resolve_cover_target(names, target, edge).ok_or_else(|| SchemaError::UnknownCoverTarget {
            field: field.name.clone(),
            target: target.clone(),
        })
    };
    let from = resolve(&covers.from, Edge::First)?;
    let to = resolve(&covers.to, Edge::Last)?;
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

#[derive(Clone, Copy)]
enum Edge {
    First,
    Last,
}

/// Resolves one end of a `covers` range, by field name or by group name.
///
/// Naming an instantiated type, `led` or `led[2]`, selects the whole block it
/// expanded into, so a checksum stays correct when the repeat count changes.
fn resolve_cover_target(names: &[String], target: &str, edge: Edge) -> Option<usize> {
    if let Some(index) = names.iter().position(|name| name == target) {
        return Some(index);
    }
    let within_group = |name: &String| {
        name.strip_prefix(target)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    };
    match edge {
        Edge::First => names.iter().position(within_group),
        Edge::Last => names.iter().rposition(within_group),
    }
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

fn range_to_raw(range: &ValueRange) -> RawRange {
    let integer = |value: u64| toml::Value::Integer(i64::try_from(value).unwrap_or(i64::MAX));
    match range {
        ValueRange::Uint { min, max } => RawRange {
            min: integer(*min),
            max: integer(*max),
        },
        ValueRange::Int { min, max } => RawRange {
            min: toml::Value::Integer(*min),
            max: toml::Value::Integer(*max),
        },
        ValueRange::Float { min, max } => RawRange {
            min: toml::Value::Float(*min),
            max: toml::Value::Float(*max),
        },
    }
}

fn lower(frame: &FrameDef) -> RawFrame {
    RawFrame {
        name: frame.name.clone(),
        description: frame.description.clone(),
        endian: None,
        // Types are resolved at load time, so what is written back is the
        // expanded layout: identical on the wire, no longer factorised.
        types: Vec::new(),
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
        range: field.range.as_ref().map(range_to_raw),
        repr: None,
        variants: None,
        bits: None,
        len: None,
        algo: None,
        covers: None,
        repeat: None,
        instances: None,
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
                from: cover_name(frame, covers.from, Edge::First),
                to: cover_name(frame, covers.to, Edge::Last),
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

    /// The case templates exist for: one structure, many identical instances.
    const LED_BANK: &str = r#"
name = "LedBank"
endian = "big"

[[type]]
name = "LedConfig"

[[type.field]]
name = "mode"
type = "enum"
repr = "u8"
variants = { OFF = 0, ON = 1, BLINK = 2 }

[[type.field]]
name = "brightness"
type = "u8"
default = 128

[[type.field]]
name = "period_ms"
type = "u16"

[[field]]
name = "header"
type = "u8"
default = 0x10

[[field]]
name = "led"
type = "LedConfig"
repeat = 4

[[field]]
name = "crc"
type = "crc16"
algo = "crc16-ccitt"
covers = { from = "header", to = "led" }
"#;

    fn names_of(frame: &FrameDef) -> Vec<&str> {
        frame
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect()
    }

    #[test]
    fn a_repeated_type_expands_into_indexed_fields() {
        let frame = from_toml(LED_BANK).expect("led bank should parse");
        assert_eq!(
            names_of(&frame),
            [
                "header",
                "led[0].mode",
                "led[0].brightness",
                "led[0].period_ms",
                "led[1].mode",
                "led[1].brightness",
                "led[1].period_ms",
                "led[2].mode",
                "led[2].brightness",
                "led[2].period_ms",
                "led[3].mode",
                "led[3].brightness",
                "led[3].period_ms",
                "crc",
            ]
        );
        // 1 + 4 * (1 + 1 + 2) + 2
        assert_eq!(frame.size(), 19);
        // Defaults belong to the type and reach every copy.
        assert_eq!(
            frame.field("led[2].brightness").unwrap().default,
            Some(Value::Uint(128))
        );
    }

    #[test]
    fn naming_a_type_instance_in_covers_selects_the_whole_block() {
        let frame = from_toml(LED_BANK).unwrap();
        let FieldKind::Checksum { covers, .. } = &frame.field("crc").unwrap().kind else {
            panic!("crc should be a checksum");
        };
        assert_eq!(frame.fields[covers.from].name, "header");
        assert_eq!(frame.fields[covers.to].name, "led[3].period_ms");
    }

    #[test]
    fn an_expanded_frame_encodes_and_decodes() {
        let frame = from_toml(LED_BANK).unwrap();
        let mut values = FieldValues::new();
        for index in 0..4 {
            values.insert(
                format!("led[{index}].mode"),
                Value::Text("BLINK".to_owned()),
            );
            values.insert(format!("led[{index}].brightness"), Value::Uint(200));
            values.insert(format!("led[{index}].period_ms"), Value::Uint(500));
        }

        let bytes = codec::encode(&frame, &values).unwrap();
        assert_eq!(bytes.len(), 19);
        let decoded = codec::decode(&frame, &bytes).unwrap();
        assert!(decoded.checksum_mismatches.is_empty());
        assert_eq!(decoded.values["led[3].brightness"], Value::Uint(200));
    }

    #[test]
    fn named_instances_read_better_than_indices() {
        let text = r#"
name = "Rgb"

[[type]]
name = "Channel"
[[type.field]]
name = "level"
type = "u8"

[[field]]
name = "led"
type = "Channel"
instances = ["red", "green", "blue"]
"#;
        let frame = from_toml(text).unwrap();
        assert_eq!(
            names_of(&frame),
            ["led.red.level", "led.green.level", "led.blue.level"]
        );
    }

    #[test]
    fn repeat_also_applies_to_a_builtin_field() {
        let text = r#"
name = "Samples"
[[field]]
name = "sample"
type = "u16"
repeat = 3
"#;
        let frame = from_toml(text).unwrap();
        assert_eq!(names_of(&frame), ["sample[0]", "sample[1]", "sample[2]"]);
        assert_eq!(frame.size(), 6);
    }

    #[test]
    fn types_nest_and_the_endian_override_reaches_the_leaves() {
        let text = r#"
name = "Nested"
endian = "big"

[[type]]
name = "Inner"
[[type.field]]
name = "value"
type = "u16"

[[type]]
name = "Outer"
[[type.field]]
name = "pair"
type = "Inner"
repeat = 2

[[field]]
name = "block"
type = "Outer"
endian = "little"
"#;
        let frame = from_toml(text).unwrap();
        assert_eq!(
            names_of(&frame),
            ["block.pair[0].value", "block.pair[1].value"]
        );
        // The override sits on the instantiation, three levels above the field.
        assert!(frame
            .fields
            .iter()
            .all(|field| field.endian == Endianness::Little));
    }

    #[test]
    fn a_checksum_inside_a_type_covers_its_own_instance() {
        let text = r#"
name = "Blocks"

[[type]]
name = "Block"
[[type.field]]
name = "payload"
type = "bytes"
len = 4
[[type.field]]
name = "check"
type = "xor8"
covers = { from = "payload", to = "payload" }

[[field]]
name = "block"
type = "Block"
repeat = 2
"#;
        let frame = from_toml(text).unwrap();
        let FieldKind::Checksum { covers, .. } = &frame.field("block[1].check").unwrap().kind
        else {
            panic!("check should be a checksum");
        };
        // Not block[0].payload: the bounds followed the instance.
        assert_eq!(frame.fields[covers.from].name, "block[1].payload");
        assert_eq!(frame.fields[covers.to].name, "block[1].payload");
    }

    #[test]
    fn a_shared_type_can_be_shadowed_inline() {
        let mut library = TypeLibrary::default();
        library
            .merge_toml(
                r#"
[[type]]
name = "Header"
[[type.field]]
name = "version"
type = "u8"
"#,
            )
            .unwrap();
        assert_eq!(library.names(), ["Header"]);

        let shared = from_toml_with(
            "name = \"A\"\n[[field]]\nname = \"h\"\ntype = \"Header\"\n",
            &library,
        )
        .unwrap();
        assert_eq!(names_of(&shared), ["h.version"]);

        let text = r#"
name = "B"

[[type]]
name = "Header"
[[type.field]]
name = "version"
type = "u16"
[[type.field]]
name = "length"
type = "u16"

[[field]]
name = "h"
type = "Header"
"#;
        let local = from_toml_with(text, &library).unwrap();
        assert_eq!(names_of(&local), ["h.version", "h.length"]);
        assert_eq!(local.size(), 4);
    }

    #[test]
    fn a_recursive_type_is_rejected() {
        let text = r#"
name = "Loop"

[[type]]
name = "A"
[[type.field]]
name = "b"
type = "B"

[[type]]
name = "B"
[[type.field]]
name = "a"
type = "A"

[[field]]
name = "root"
type = "A"
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::RecursiveType { name, .. } if name == "A"),
            "got {err}"
        );
    }

    #[test]
    fn an_unknown_type_lists_what_is_available() {
        let text = r#"
name = "Typo"

[[type]]
name = "LedConfig"
[[type.field]]
name = "mode"
type = "u8"

[[field]]
name = "led"
type = "LedConfg"
"#;
        let message = from_toml(text).unwrap_err().to_string();
        assert!(message.contains("LedConfg"), "got {message}");
        assert!(message.contains("LedConfig"), "got {message}");
        assert!(message.contains("crc16"), "got {message}");
    }

    #[test]
    fn a_type_instance_rejects_attributes_that_cannot_apply() {
        let text = r#"
name = "Bad"

[[type]]
name = "Thing"
[[type.field]]
name = "value"
type = "u8"

[[field]]
name = "thing"
type = "Thing"
len = 4
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::UnexpectedAttribute { attribute, .. } if *attribute == "len"),
            "got {err}"
        );
    }

    #[test]
    fn repetition_counts_must_make_sense() {
        let base = |extra: &str| {
            format!("name = \"x\"\n[[field]]\nname = \"f\"\ntype = \"u8\"\n{extra}\n")
        };
        assert!(matches!(
            from_toml(&base("repeat = 0")).unwrap_err(),
            SchemaError::EmptyRepeat { .. }
        ));
        assert!(matches!(
            from_toml(&base("instances = []")).unwrap_err(),
            SchemaError::EmptyInstances { .. }
        ));
        assert!(matches!(
            from_toml(&base("repeat = 2\ninstances = [\"a\"]")).unwrap_err(),
            SchemaError::RepeatAndInstances { .. }
        ));
        assert!(matches!(
            from_toml(&base("instances = [\"a\", \"a\"]")).unwrap_err(),
            SchemaError::DuplicateInstance { .. }
        ));
    }

    #[test]
    fn an_empty_type_is_rejected_rather_than_dropping_the_field() {
        let text = r#"
name = "Bad"

[[type]]
name = "Nothing"

[[field]]
name = "gap"
type = "Nothing"
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::EmptyType { .. }
        ));
    }

    #[test]
    fn two_instances_of_the_same_name_collide() {
        let text = r#"
name = "Bad"

[[type]]
name = "Thing"
[[type.field]]
name = "value"
type = "u8"

[[field]]
name = "thing"
type = "Thing"

[[field]]
name = "thing"
type = "Thing"
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::DuplicateField { .. }
        ));
    }

    /// Named subtypes, chained, plus an anonymous constraint.
    const SUBTYPES: &str = r#"
name = "Constrained"
endian = "big"

[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 99 }

[[type]]
name = "LowPercent"
base = "Percent"
range = { min = 0, max = 9 }

[[field]]
name = "duty"
type = "Percent"
default = 50

[[field]]
name = "trim"
type = "LowPercent"

[[field]]
name = "gain"
type = "i16"
range = { min = -500, max = 500 }
"#;

    #[test]
    fn a_subtype_is_its_base_scalar_plus_a_range() {
        let frame = from_toml(SUBTYPES).expect("subtypes should parse");
        // Three scalars, nothing added to the wire.
        assert_eq!(frame.size(), 4);

        let duty = frame.field("duty").unwrap();
        assert_eq!(duty.kind, FieldKind::Scalar(ScalarType::U8));
        assert_eq!(duty.range, Some(ValueRange::Uint { min: 0, max: 99 }));
        assert_eq!(duty.default, Some(Value::Uint(50)));

        // A subtype of a subtype keeps the tighter bound.
        assert_eq!(
            frame.field("trim").unwrap().range,
            Some(ValueRange::Uint { min: 0, max: 9 })
        );

        // And a constraint needs no name at all.
        let gain = frame.field("gain").unwrap();
        assert_eq!(gain.kind, FieldKind::Scalar(ScalarType::I16));
        assert_eq!(
            gain.range,
            Some(ValueRange::Int {
                min: -500,
                max: 500
            })
        );
    }

    #[test]
    fn subtypes_survive_a_round_trip_through_toml() {
        let frame = from_toml(SUBTYPES).unwrap();
        let reparsed = from_toml(&to_toml(&frame).unwrap()).expect("rendered toml should parse");
        assert_eq!(frame, reparsed);
    }

    #[test]
    fn a_field_may_narrow_the_subtype_it_uses_but_not_widen_it() {
        let narrow = r#"
name = "x"
[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 99 }
[[field]]
name = "duty"
type = "Percent"
range = { min = 10, max = 20 }
"#;
        assert_eq!(
            from_toml(narrow).unwrap().field("duty").unwrap().range,
            Some(ValueRange::Uint { min: 10, max: 20 })
        );

        let widen = narrow.replace("min = 10, max = 20", "min = 0, max = 200");
        let err = from_toml(&widen).unwrap_err();
        assert!(
            matches!(&err, SchemaError::RangeNotWithin { owner, .. } if owner.contains("duty")),
            "got {err}"
        );
    }

    #[test]
    fn a_subtype_cannot_escape_its_base_representation() {
        let text = r#"
name = "x"
[[type]]
name = "Big"
base = "u8"
range = { min = 0, max = 300 }
[[field]]
name = "v"
type = "Big"
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::RangeOutOfType { repr, .. } if repr == "u8"),
            "got {err}"
        );
    }

    #[test]
    fn a_backwards_range_is_rejected() {
        let text = r#"
name = "x"
[[field]]
name = "v"
type = "u8"
range = { min = 90, max = 10 }
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::BackwardsRange { .. }
        ));
    }

    #[test]
    fn a_range_only_means_something_on_a_scalar() {
        let text = r#"
name = "x"
[[field]]
name = "label"
type = "text"
len = 4
range = { min = 0, max = 9 }
"#;
        let err = from_toml(text).unwrap_err();
        assert!(
            matches!(&err, SchemaError::RangeOnNonScalar { kind, .. } if kind == "text"),
            "got {err}"
        );
    }

    #[test]
    fn a_subtype_cannot_be_a_record_as_well() {
        let text = r#"
name = "x"
[[type]]
name = "Muddled"
base = "u8"
[[type.field]]
name = "inner"
type = "u8"
[[field]]
name = "v"
type = "Muddled"
"#;
        assert!(matches!(
            from_toml(text).unwrap_err(),
            SchemaError::TypeIsBothRecordAndSubtype { .. }
        ));
    }

    #[test]
    fn a_subtype_reaches_inside_a_record_and_a_repeat() {
        let text = r#"
name = "Bank"

[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 99 }

[[type]]
name = "Led"
[[type.field]]
name = "brightness"
type = "Percent"

[[field]]
name = "led"
type = "Led"
repeat = 2
"#;
        let frame = from_toml(text).unwrap();
        assert_eq!(frame.size(), 2);
        for index in 0..2 {
            assert_eq!(
                frame
                    .field(&format!("led[{index}].brightness"))
                    .unwrap()
                    .range,
                Some(ValueRange::Uint { min: 0, max: 99 }),
                "the constraint must reach every copy"
            );
        }
    }

    #[test]
    fn every_builtin_kind_is_recognised_by_the_parser() {
        // Guards the one duplicated piece of knowledge: BUILTIN_KINDS decides
        // whether a name is a type reference, build_kind decides what it means.
        for kind in BUILTIN_KINDS {
            let text = format!("name = \"x\"\n[[field]]\nname = \"f\"\ntype = \"{kind}\"\n");
            if let Err(SchemaError::UnknownType { .. }) = from_toml(&text) {
                panic!("{kind} is listed as a builtin but the parser rejects it");
            }
        }
    }

    /// Writing a factorised frame back out flattens it: same bytes on the wire,
    /// no types and no repeats left in the file.
    ///
    /// Spelled out rather than glossed over, because it is what stops an editor
    /// from simply re-serialising whatever it is shown. `declared` is the only
    /// thing that can tell the two apart, which is why it exists.
    #[test]
    fn writing_an_expanded_frame_keeps_the_wire_and_loses_the_factorisation() {
        let frame = from_toml(LED_BANK).unwrap();
        let reparsed = from_toml(&to_toml(&frame).unwrap()).expect("rendered toml should parse");

        // Byte for byte the same frame.
        assert_eq!(frame.fields, reparsed.fields);
        assert_eq!(frame.size(), reparsed.size());

        // Three entries in the file became fourteen.
        assert_eq!(frame.declared, ["header", "led", "crc"]);
        assert_eq!(reparsed.declared.len(), reparsed.fields.len());
        assert!(reparsed.declared.len() > frame.declared.len());
        assert_eq!(frame.generated_by("led[0].mode"), Some("led"));
        assert_eq!(
            reparsed.generated_by("led[0].mode"),
            None,
            "flattened, it is nobody's expansion any more"
        );
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

    /// A frame someone took the trouble to explain, which is the whole reason
    /// the writer exists.
    const COMMENTED: &str = r#"# What this frame is for.
name = "Status"
description = "two bytes"

# The sync word never changes.
[[field]]
name = "sync"
type = "u8"
default = 170

[[field]]
name = "mode"
type = "u8"   # picked by the operator
default = 0
"#;

    #[test]
    fn rewriting_a_frame_keeps_every_comment_the_file_had() {
        let mut frame = from_toml(COMMENTED).expect("valid frame");
        frame.fields[1].default = Some(Value::Uint(3));

        let written = update_in(COMMENTED, &frame).expect("rewritten");

        assert!(written.contains("# What this frame is for."));
        assert!(written.contains("# The sync word never changes."));
        assert!(written.contains("# picked by the operator"));
        assert_eq!(
            from_toml(&written).expect("still valid").fields[1].default,
            Some(Value::Uint(3))
        );
    }

    #[test]
    fn rewriting_a_factorised_frame_does_not_flatten_it() {
        let text = r#"
name = "Packed"

[[type]]
name = "Point"
[[type.field]]
name = "x"
type = "u8"
[[type.field]]
name = "y"
type = "u8"

[[field]]
name = "here"
type = "Point"

[[field]]
name = "tag"
type = "u8"
default = 1
"#;
        let frame = from_toml(text).expect("valid frame");
        assert_eq!(frame.declared, ["here", "tag"]);
        assert_eq!(frame.fields.len(), 3);

        let written = update_in(text, &frame).expect("rewritten");

        assert!(written.contains(r#"type = "Point""#));
        assert!(!written.contains("here.x"));
        assert_eq!(from_toml(&written).expect("still valid"), frame);
    }

    #[test]
    fn a_field_added_to_the_model_is_appended_to_the_file() {
        let frame = from_toml(COMMENTED).expect("valid frame");
        let mut grown = frame.clone();
        grown.fields.push(FieldDef {
            name: "extra".to_owned(),
            description: None,
            kind: FieldKind::Scalar(ScalarType::U8),
            endian: Endianness::Big,
            default: None,
            range: None,
        });
        grown.declared.push("extra".to_owned());

        let written = update_in(COMMENTED, &grown).expect("rewritten");

        assert!(written.contains("# The sync word never changes."));
        let reread = from_toml(&written).expect("still valid");
        assert_eq!(
            reread
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["sync", "mode", "extra"]
        );
    }

    #[test]
    fn a_field_dropped_from_the_model_leaves_the_file() {
        let mut frame = from_toml(COMMENTED).expect("valid frame");
        frame.fields.remove(0);
        frame.declared.remove(0);

        let written = update_in(COMMENTED, &frame).expect("rewritten");

        assert!(!written.contains(r#"name = "sync""#));
        assert!(written.contains("# picked by the operator"));
        assert_eq!(from_toml(&written).expect("still valid").fields.len(), 1);
    }
}
