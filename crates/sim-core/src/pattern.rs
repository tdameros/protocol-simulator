//! Matching raw bytes against a written-down pattern.
//!
//! Shared ground: the traffic filter narrows a log with it, and a scenario step
//! waits for a frame with it. Both mean the same thing by `AA 55 ?? 01`, and
//! there is no reason for them to disagree about what matches.

use serde::{Deserialize, Serialize};

/// Where in the frame a pattern has to sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Anywhere,
    At(usize),
}

impl Anchor {
    /// The written form: an offset, or nothing for anywhere.
    #[must_use]
    pub fn offset(self) -> Option<usize> {
        match self {
            Self::Anywhere => None,
            Self::At(offset) => Some(offset),
        }
    }
}

impl From<Option<usize>> for Anchor {
    fn from(offset: Option<usize>) -> Self {
        offset.map_or(Self::Anywhere, Self::At)
    }
}

/// A byte pattern in which `??` stands for any byte.
///
/// Byte granularity rather than nibble: `A?` reads like a typo far more often
/// than it reads like an intent, so it is refused rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexPattern(Vec<Option<u8>>);

impl HexPattern {
    /// `None` when the text is not a usable pattern.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let cleaned: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
            return None;
        }
        cleaned
            .chunks(2)
            .map(|pair| {
                if pair == ['?', '?'] {
                    return Some(None);
                }
                let byte: String = pair.iter().collect();
                u8::from_str_radix(&byte, 16).ok().map(Some)
            })
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }

    /// How many bytes the pattern covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn matches_at(&self, bytes: &[u8], offset: usize) -> bool {
        // Checked, because the offset now comes from a scenario file as well as
        // from a text box, and `at = 18446744073709551615` parses perfectly
        // well. Overflowing here would panic a debug build.
        let Some(end) = offset.checked_add(self.0.len()) else {
            return false;
        };
        let Some(window) = bytes.get(offset..end) else {
            return false;
        };
        self.0
            .iter()
            .zip(window)
            .all(|(expected, actual)| expected.is_none_or(|byte| byte == *actual))
    }

    /// A pattern over `bytes`, keeping only the ones `keep` marks and letting
    /// every other byte be anything.
    ///
    /// How a frame becomes something to wait for: encode it with its defaults,
    /// mark the bytes belonging to the fields that have to match, and the rest
    /// falls away as wildcards.
    #[must_use]
    pub fn masked(bytes: &[u8], keep: &[bool]) -> Self {
        Self(
            bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| keep.get(index).copied().unwrap_or(false).then_some(*byte))
                .collect(),
        )
    }

    /// The pattern as it is written down, `??` for a byte that may be anything.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0
            .iter()
            .map(|byte| match byte {
                Some(value) => format!("{value:02X}"),
                None => "??".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[must_use]
    pub fn found_in(&self, bytes: &[u8], anchor: Anchor) -> bool {
        match anchor {
            Anchor::At(offset) => self.matches_at(bytes, offset),
            Anchor::Anywhere => (0..bytes.len()).any(|offset| self.matches_at(bytes, offset)),
        }
    }
}

/// A pattern as it appears in a file, before anyone has checked it parses.
///
/// Kept as text rather than compiled on the way in, so a typo is reported
/// against the step it belongs to instead of failing the whole file with a
/// deserialiser message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternSpec {
    pub hex: String,
    /// Offset the pattern has to sit at. Absent means anywhere in the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<usize>,
}

impl PatternSpec {
    /// `None` when `hex` is not a usable pattern.
    #[must_use]
    pub fn compile(&self) -> Option<(HexPattern, Anchor)> {
        HexPattern::parse(&self.hex).map(|pattern| (pattern, Anchor::from(self.at)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_matches_with_wildcards_and_can_be_anchored() {
        let pattern = HexPattern::parse("AA 55 ?? 02").expect("should parse");
        let frame = [0xAA, 0x55, 0x01, 0x02];

        assert!(pattern.found_in(&frame, Anchor::Anywhere));
        assert!(pattern.found_in(&frame, Anchor::At(0)));
        // The same pattern one byte along matches nothing.
        assert!(!pattern.found_in(&frame, Anchor::At(1)));
    }

    #[test]
    fn a_pattern_can_sit_anywhere_in_the_frame() {
        let pattern = HexPattern::parse("0102").expect("should parse");
        assert!(pattern.found_in(&[0xAA, 0x55, 0x01, 0x02], Anchor::Anywhere));
        assert!(!pattern.found_in(&[0xAA, 0x55, 0x01, 0x02], Anchor::At(0)));
    }

    #[test]
    fn a_pattern_longer_than_the_frame_never_matches() {
        let pattern = HexPattern::parse("AA 55 01 02 03").expect("should parse");
        assert!(!pattern.found_in(&[0xAA, 0x55, 0x01, 0x02], Anchor::Anywhere));
        assert!(!pattern.found_in(&[], Anchor::Anywhere));
    }

    #[test]
    fn a_masked_pattern_keeps_only_what_was_marked() {
        let pattern = HexPattern::masked(&[0xAA, 0x55, 0x07, 0x01], &[true, true, false, true]);
        assert_eq!(pattern.to_hex(), "AA 55 ?? 01");

        // What it keeps still has to be there, and what it dropped is free.
        assert!(pattern.found_in(&[0xAA, 0x55, 0xFF, 0x01], Anchor::At(0)));
        assert!(!pattern.found_in(&[0xAA, 0x55, 0xFF, 0x02], Anchor::At(0)));

        // Marking nothing matches any frame of that length, which is why the
        // loader refuses an empty list of fields rather than writing this.
        assert_eq!(
            HexPattern::masked(&[1, 2], &[false, false]).to_hex(),
            "?? ??"
        );
    }

    #[test]
    fn an_absurd_offset_matches_nothing_instead_of_overflowing() {
        let pattern = HexPattern::parse("AA55").expect("should parse");
        assert!(!pattern.found_in(&[0xAA, 0x55], Anchor::At(usize::MAX)));
        assert!(!pattern.found_in(&[0xAA, 0x55], Anchor::At(1)));
    }

    #[test]
    fn half_a_byte_is_refused_rather_than_guessed_at() {
        assert!(HexPattern::parse("AA 5").is_none());
        assert!(HexPattern::parse("A?").is_none());
        assert!(HexPattern::parse("").is_none());
        assert!(HexPattern::parse("??").is_some());
    }

    #[test]
    fn the_written_form_survives_a_round_trip() {
        let spec = PatternSpec {
            hex: "AA 55 ?? 01".to_owned(),
            at: Some(0),
        };
        let text = toml::to_string(&spec).expect("should serialise");
        assert_eq!(
            toml::from_str::<PatternSpec>(&text).expect("should parse back"),
            spec
        );

        let (pattern, anchor) = spec.compile().expect("should compile");
        assert_eq!(pattern.len(), 4);
        assert_eq!(anchor, Anchor::At(0));

        // An offset left out means anywhere, which is the useful default.
        let loose: PatternSpec = toml::from_str(r#"hex = "AA55""#).expect("should parse");
        assert_eq!(loose.compile().expect("should compile").1, Anchor::Anywhere);
    }

    #[test]
    fn a_broken_pattern_reports_itself_instead_of_matching_everything() {
        let spec = PatternSpec {
            hex: "AA 5".to_owned(),
            at: None,
        };
        assert!(spec.compile().is_none());
    }
}
