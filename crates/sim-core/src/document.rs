//! Editing a TOML document without flattening it.
//!
//! Both the scenario writer and the frame writer have the same job: change what
//! someone typed into the editor, and leave everything else exactly where the
//! person who wrote the file put it. Comments, blank lines, key order, the
//! sections nobody touched. Re-serialising a model would lose all of that, so
//! the changes are copied into the existing document key by key.
//!
//! The subtle parts live here rather than twice: replacing a value under a key
//! keeps the key, and with it the comment written above it, and a new section
//! needs a position or it lands wherever the writer happens to reach it.

use toml_edit::{Item, Table};

/// Copies `from` over `into`, key by key rather than wholesale.
///
/// Keys named in `keep` are left alone whatever `from` says: they hold what the
/// file is the authority on, not the model. Keys `from` no longer has are
/// removed, so unticking something takes it out of the file.
pub fn merge(into: &mut Table, from: &Table, keep: &[&str]) {
    let stale: Vec<String> = into
        .iter()
        .map(|(key, _)| key.to_owned())
        .filter(|key| !keep.contains(&key.as_str()) && from.get(key).is_none())
        .collect();
    for key in stale {
        into.remove(&key);
    }

    for (key, item) in from {
        if keep.contains(&key) {
            continue;
        }
        // Assigning through the existing slot rather than inserting: the
        // comment above a setting belongs to its key, and replacing the key
        // would take the comment with it.
        match into.get_mut(key) {
            Some(slot) => overwrite(slot, item),
            None => {
                into.insert(key, item.clone());
            }
        }
    }
}

/// Replaces what is under a key, disturbing the text as little as it can.
///
/// A setting the editor did not change is left untouched down to its spacing,
/// which is what keeps a hand-laid-out array of bit definitions readable after
/// a save. One that did change keeps whatever surrounded it, in particular the
/// comment written to the right of it.
fn overwrite(slot: &mut Item, item: &Item) {
    if equivalent(slot, item) {
        return;
    }
    let surroundings = slot.as_value().map(|value| value.decor().clone());
    *slot = item.clone();
    if let (Some(decor), Some(value)) = (surroundings, slot.as_value_mut()) {
        *value.decor_mut() = decor;
    }
}

/// Whether two items say the same thing, whatever they look like.
fn equivalent(left: &Item, right: &Item) -> bool {
    fn meaning(item: &Item) -> Option<toml::Value> {
        let mut document = toml_edit::DocumentMut::new();
        document.insert("x", item.clone());
        toml::from_str::<toml::Table>(&document.to_string())
            .ok()?
            .remove("x")
    }
    match (meaning(left), meaning(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The position of the last section in the document.
#[must_use]
pub fn last_position(document: &toml_edit::DocumentMut) -> isize {
    fn scan(table: &Table, best: &mut isize) {
        if let Some(position) = table.position() {
            *best = (*best).max(position);
        }
        for (_, item) in table {
            match item {
                Item::Table(inner) => scan(inner, best),
                Item::ArrayOfTables(entries) => {
                    for entry in entries {
                        scan(entry, best);
                    }
                }
                _ => {}
            }
        }
    }
    let mut best = 0;
    scan(document.as_table(), &mut best);
    best
}

/// Sends a table and everything nested inside it to the end of the document, in
/// the order they are written.
///
/// A section with no position of its own is written wherever `toml_edit`
/// reaches it, which for an array of tables means landing in the middle of an
/// earlier entry's own sections and quietly stealing them.
pub fn place_after(table: &mut Table, next: &mut isize) {
    *next += 1;
    table.set_position(Some(*next));
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(inner) => place_after(inner, next),
            Item::ArrayOfTables(entries) => {
                for entry in entries.iter_mut() {
                    place_after(entry, next);
                }
            }
            _ => {}
        }
    }
}

/// Turns `table[key]`, if it is a section of its own, into a value on one line.
pub fn fold(table: &mut Table, key: &str) {
    let Some(section) = table.remove(key) else {
        return;
    };
    let folded = match section {
        Item::Table(inner) => Item::Value(toml_edit::Value::InlineTable(inner.into_inline_table())),
        other => other,
    };
    table.insert(key, folded);
}

/// Puts an array back on one line.
pub fn compact(table: &mut Table, key: &str) {
    if let Some(array) = table.get_mut(key).and_then(Item::as_array_mut) {
        array.fmt();
    }
}

/// Renders a document with the line endings the text it came from used.
///
/// `toml_edit` keeps comments, spacing and key order, but normalises every line
/// ending to a bare newline. On a file written on Windows that turns a one-line
/// edit into a diff touching every line, which is the opposite of what all the
/// care above is for.
#[must_use]
pub fn render(document: &toml_edit::DocumentMut, original: &str) -> String {
    let written = document.to_string();
    if original.contains("\r\n") && !written.contains("\r\n") {
        return written.replace('\n', "\r\n");
    }
    written
}
