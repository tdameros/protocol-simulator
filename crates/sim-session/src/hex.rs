//! Bytes as text, and text back into bytes.
//!
//! One reading of what a hexadecimal string means, because the front ends and
//! the panels within them had grown three: two parsers with the same rules and
//! different answers, and the byte spacer written out twice.

/// Why a string is not a run of bytes.
///
/// Returned rather than a message, because the callers disagree about what to
/// do with it. Typing into the frame editor's preview goes quiet on a half
/// typed byte, where the injection box says what is wrong with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Nothing but whitespace.
    Empty,
    /// A character that is not a hexadecimal digit.
    NotHex,
    /// Hexadecimal, but half a byte short.
    OddDigits,
}

impl std::fmt::Display for Problem {
    /// One wording per problem, so the panels cannot disagree about what is
    /// wrong with the same string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let said = match self {
            Self::Empty => "Enter hexadecimal bytes (e.g. DEADBEEF).",
            Self::NotHex => "Not hexadecimal.",
            Self::OddDigits => "Odd number of hexadecimal digits.",
        };
        f.write_str(said)
    }
}

/// Bytes from a string of hexadecimal digits, whitespace anywhere.
///
/// `NotHex` is answered before `OddDigits`: given `Z`, naming the character is
/// more use than counting the digits.
///
/// # Errors
///
/// Returns why the string is not a run of bytes.
pub fn parse(input: &str) -> Result<Vec<u8>, Problem> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();

    if cleaned.is_empty() {
        return Err(Problem::Empty);
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Problem::NotHex);
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(Problem::OddDigits);
    }

    Ok((0..cleaned.len())
        .step_by(2)
        .filter_map(|at| u8::from_str_radix(&cleaned[at..at + 2], 16).ok())
        .collect())
}

/// `AA 55 01`, which is how a person reads bytes.
#[must_use]
pub fn spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `AA5501`, which is how a person types them.
#[must_use]
pub fn packed(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02X}");
        out
    })
}

/// The bytes as characters, with a dot for everything that has no shape.
#[must_use]
pub fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{packed, parse, printable, spaced, Problem};

    #[test]
    fn whitespace_anywhere_is_ignored() {
        assert_eq!(parse(" AA 55\n01 "), Ok(vec![0xAA, 0x55, 0x01]));
    }

    #[test]
    fn nothing_typed_is_not_an_empty_frame() {
        assert_eq!(parse("   "), Err(Problem::Empty));
    }

    #[test]
    fn half_a_byte_says_so() {
        assert_eq!(parse("AA5"), Err(Problem::OddDigits));
    }

    /// Both are wrong with `Z`, and the character is the more useful answer.
    #[test]
    fn a_bad_character_is_named_before_the_digits_are_counted() {
        assert_eq!(parse("Z"), Err(Problem::NotHex));
        assert_eq!(parse("ZZ"), Err(Problem::NotHex));
    }

    #[test]
    fn bytes_read_the_way_they_are_written() {
        assert_eq!(spaced(&[0xAA, 0x55]), "AA 55");
        assert_eq!(packed(&[0xAA, 0x55]), "AA55");
        assert_eq!(printable(b"ok\x00!"), "ok.!");
    }
}
