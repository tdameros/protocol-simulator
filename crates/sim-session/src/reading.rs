//! Reading a captured row through a frame definition.
//!
//! Which definition is the operator's call, not a guess: only definitions of
//! exactly the row's length are offered, since anything else cannot be what
//! was captured. What the fields then say is the same in a window and in a
//! terminal, so it is settled here and drawn there.

use sim_core::frame::value::Value;
use sim_core::frame::{FieldDef, FieldKind, FrameDef};

use crate::frames::FrameLibrary;
use crate::hex;
use crate::state::LogEntry;

/// What a row can be read as, settled before anything is drawn.
///
/// Held apart from the drawing so the pane can be given a height that suits
/// what it is about to show, which is decided by the caller owning the room.
pub struct Reading<'a> {
    /// Every definition of exactly the row's length.
    candidates: Vec<&'a FrameDef>,
    /// The one being read through, if that is settled.
    chosen: Option<&'a FrameDef>,
    /// Nothing to draw, and why.
    empty: Option<String>,
}

impl<'a> Reading<'a> {
    /// Every definition the row could be read through.
    #[must_use]
    pub fn candidates(&self) -> &[&'a FrameDef] {
        &self.candidates
    }

    /// The one it is being read through, if that is settled.
    #[must_use]
    pub fn chosen(&self) -> Option<&'a FrameDef> {
        self.chosen
    }

    /// Why there is nothing to show, when there is nothing to show.
    #[must_use]
    pub fn nothing(&self) -> Option<&str> {
        self.empty.as_deref()
    }

    /// How many rows it is about to fill: one per field, plus one per flag of
    /// a bitfield. The room those rows need is the caller's business.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.chosen.map_or(1, |frame| {
            frame
                .fields
                .iter()
                .map(|field| match &field.kind {
                    FieldKind::Bits { bits, .. } => 1 + bits.len(),
                    _ => 1,
                })
                .sum()
        })
    }
}

/// Works out what `entry` can be read as, and settles `decode_as`.
pub fn read<'a>(
    frames: &'a FrameLibrary,
    entry: &LogEntry,
    decode_as: &mut Option<String>,
) -> Reading<'a> {
    let candidates: Vec<&FrameDef> = frames
        .frames()
        .filter(|frame| frame.size() == entry.bytes.len())
        .collect();
    pick(&candidates, decode_as);
    let chosen = decode_as
        .as_deref()
        .and_then(|name| candidates.iter().find(|frame| frame.name == name))
        .copied();
    let empty = chosen
        .is_none()
        .then(|| nothing_to_read(frames, &candidates, entry));
    Reading {
        candidates,
        chosen,
        empty,
    }
}

/// Settles which definition the row is read through.
///
/// A choice made against one row means nothing against a row of another
/// length, so it lapses rather than decoding the wrong thing. And one candidate
/// is not a choice: asking for it would cost a click on every row of a protocol
/// whose messages all have distinct lengths.
pub fn pick(candidates: &[&FrameDef], decode_as: &mut Option<String>) {
    if decode_as
        .as_deref()
        .is_some_and(|name| !candidates.iter().any(|frame| frame.name == name))
    {
        *decode_as = None;
    }
    if decode_as.is_none() {
        if let [only] = candidates {
            *decode_as = Some(only.name.clone());
        }
    }
}

/// Why there are no fields on screen, in the operator's terms.
fn nothing_to_read(frames: &FrameLibrary, candidates: &[&FrameDef], entry: &LogEntry) -> String {
    if frames.is_empty() {
        return "Open a frames folder to read these bytes as fields.".to_owned();
    }
    if candidates.is_empty() {
        return format!("No frame definition is {} bytes.", entry.bytes.len());
    }
    "Pick the frame these bytes are.".to_owned()
}

#[must_use]
pub fn describe(field: &FieldDef, value: &Value, hex: bool) -> String {
    let digits = field.kind.size() * 2;
    match (&field.kind, value) {
        (FieldKind::Enum { variants, .. }, Value::Uint(raw)) => {
            let name = variants
                .iter()
                .find(|variant| variant.value == *raw)
                .map_or("unknown", |variant| variant.name.as_str());
            format!("{name} ({})", unsigned(*raw, digits, hex))
        }
        // The packed word, since the rows underneath carry the sub-fields.
        (_, Value::Bits(_)) => String::new(),
        (_, Value::Uint(raw)) => unsigned(*raw, digits, hex),
        (_, Value::Int(raw)) => raw.to_string(),
        (_, Value::Float(raw)) => raw.to_string(),
        (_, Value::Text(text)) => format!("{text:?}"),
        // Whose hex is already in the column before this one.
        (_, Value::Bytes(raw)) => hex::printable(raw),
    }
}

/// Padded to the width of what holds it, so a u16 reads 0x00FF and a column of
/// them lines up.
#[must_use]
pub fn unsigned(value: u64, digits: usize, hex: bool) -> String {
    if hex {
        format!("0x{value:0digits$X}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::pick;
    use sim_core::frame::{Endianness, FieldDef};
    use sim_core::frame::{FieldKind, FrameDef, ScalarType};

    fn frame(name: &str, bytes: usize) -> FrameDef {
        let fields = (0..bytes)
            .map(|index| FieldDef {
                name: format!("byte{index}"),
                description: None,
                kind: FieldKind::Scalar(ScalarType::U8),
                endian: Endianness::default(),
                default: None,
                range: None,
            })
            .collect();
        FrameDef::flat(name, fields)
    }

    #[test]
    fn the_only_candidate_is_taken_without_asking() {
        let only = frame("Status", 3);
        let mut chosen = None;
        pick(&[&only], &mut chosen);
        assert_eq!(chosen.as_deref(), Some("Status"));
    }

    #[test]
    fn several_candidates_wait_for_an_answer() {
        let (status, limits) = (frame("Status", 3), frame("Limits", 3));
        let mut chosen = None;
        pick(&[&status, &limits], &mut chosen);
        assert_eq!(chosen, None, "picking one of them is the operator's to do");
    }

    #[test]
    fn a_choice_survives_the_next_row_of_the_same_shape() {
        let (status, limits) = (frame("Status", 3), frame("Limits", 3));
        let mut chosen = Some("Limits".to_owned());
        pick(&[&status, &limits], &mut chosen);
        assert_eq!(
            chosen.as_deref(),
            Some("Limits"),
            "stepping down a list of one message type costs no clicks"
        );
    }

    #[test]
    fn a_choice_that_no_longer_fits_lapses() {
        let heartbeat = frame("Heartbeat", 5);
        let mut chosen = Some("Status".to_owned());
        pick(&[&heartbeat], &mut chosen);
        assert_eq!(
            chosen.as_deref(),
            Some("Heartbeat"),
            "the row is read as what it can be, never as what it cannot"
        );
    }

    #[test]
    fn nothing_of_that_length_leaves_nothing_chosen() {
        let mut chosen = Some("Status".to_owned());
        pick(&[], &mut chosen);
        assert_eq!(chosen, None);
    }
}
