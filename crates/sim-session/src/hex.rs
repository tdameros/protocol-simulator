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

/// A number as hexadecimal, prefixed so it cannot be mistaken for decimal and
/// padded to the width of whatever holds it.
///
/// The prefix is not decoration: `10` shown bare would read as ten, and the
/// same box takes decimal input, so the two have to be told apart on sight.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value comes from an integer field and is shown, not computed"
)]
pub fn number(value: f64, digits: usize) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let magnitude = value.abs() as u64;
    format!("{sign}0x{magnitude:0digits$X}")
}

/// Decimal, hexadecimal, binary or octal, signed, with `_` allowed anywhere as
/// a separator.
///
/// `None` for anything else, which leaves the box holding its previous value
/// rather than jumping to zero.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a drag value is an f64 whatever is typed into it"
)]
pub fn read_number(text: &str) -> Option<f64> {
    let text = text.trim();
    let (negative, rest) = match text.strip_prefix(['-', '+']) {
        Some(rest) => (text.starts_with('-'), rest.trim_start()),
        None => (false, text),
    };

    let digits = rest.replace('_', "");
    // Byte-indexed, not `.chars()`: a prefix is two ASCII bytes, and slicing by
    // byte index panics the moment the string holds anything wider, which a
    // pasted or mistyped character can put in front of a real prefix.
    let radix = digits
        .is_char_boundary(2)
        .then(|| &digits[..2])
        .and_then(|head| {
            ["0x", "0b", "0o"]
                .into_iter()
                .zip([16, 2, 8])
                .find(|(prefix, _)| head.eq_ignore_ascii_case(prefix))
        });

    let value = match radix {
        Some((_, radix)) => u64::from_str_radix(&digits[2..], radix).ok()? as f64,
        // Plain decimal, and whatever else Rust reads as a float, so `1e3`
        // still works for anyone who types it.
        None => digits.parse::<f64>().ok()?,
    };
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::{packed, parse, printable, read_number, spaced, Problem};

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

    /// A prefix check done by byte index used to panic the moment a
    /// multi-byte character sat across the boundary it sliced at.
    #[test]
    fn a_multi_byte_character_does_not_panic_the_prefix_check() {
        assert_eq!(read_number("\u{20ac}1"), None);
        assert_eq!(read_number("\u{20ac}"), None);
        assert_eq!(read_number("0x\u{20ac}"), None);
    }

    #[test]
    fn a_hexadecimal_prefix_with_nothing_after_it_reads_as_nothing() {
        assert_eq!(read_number("0x"), None);
    }

    #[test]
    fn prefixed_numbers_are_read_in_their_own_base() {
        assert_eq!(read_number("0x1F"), Some(31.0));
        assert_eq!(read_number("0b101"), Some(5.0));
        assert_eq!(read_number("-0x0A"), Some(-10.0));
    }
}
