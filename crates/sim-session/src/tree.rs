//! The field tree a dotted name implies.
//!
//! An instantiated type produces `zone.left.x`, and both front ends want to
//! fold `zone.left` away with everything under it. The nesting is worked out
//! here so that neither has to agree with the other about it by accident.

use sim_core::frame::FieldDef;

/// One level of the field tree rebuilt from the dotted names an instantiated
/// type produces, so `zone.left` folds away with everything under it.
pub enum Entry<'a> {
    Field(&'a FieldDef),
    Group(Group<'a>),
}

pub struct Group<'a> {
    /// The last path segment, which is what the header shows.
    pub label: &'a str,
    /// The whole path, used to decide what belongs to this group.
    path: &'a str,
    /// Name of the first field inside, which unlike the path is always unique
    /// even when hand-written names interleave two blocks.
    pub salt: &'a str,
    pub entries: Vec<Entry<'a>>,
}

impl Entry<'_> {
    #[must_use]
    pub fn size(&self) -> usize {
        match self {
            Self::Field(field) => field.kind.size(),
            Self::Group(group) => group.entries.iter().map(Self::size).sum(),
        }
    }
}

#[must_use]
pub fn build_tree(fields: &[FieldDef]) -> Vec<Entry<'_>> {
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

#[must_use]
pub fn leaf_name(name: &str) -> &str {
    name.rfind('.').map_or(name, |at| &name[at + 1..])
}

#[cfg(test)]
mod tests {
    use super::{build_tree, Entry};
    use sim_core::frame::{Endianness, FieldDef, FieldKind, ScalarType};

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
