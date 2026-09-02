//! Traffic as a person reads it: a clock, a gap, and a rate.
//!
//! Widths are fixed on purpose. A column that changes size as the numbers do
//! makes a scrolling list unreadable, whichever front end draws it.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};

use crate::state::LogEntry;

/// How far back a rate is measured.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// How much is on screen, and how fast it is arriving.
#[must_use]
pub fn summary(rows: &[&LogEntry], total: usize) -> String {
    let now = SystemTime::now();
    let recent: Vec<&&LogEntry> = rows
        .iter()
        .rev()
        .take_while(|entry| {
            now.duration_since(entry.timestamp)
                .is_ok_and(|age| age < RATE_WINDOW)
        })
        .collect();
    let bytes: usize = recent.iter().map(|entry| entry.bytes.len()).sum();

    format!(
        "{} of {total} shown  ·  {} frame/s  ·  {bytes} B/s",
        rows.len(),
        recent.len()
    )
}

/// Wall-clock time in the machine's timezone, so frames line up with scope
/// captures and equipment logs rather than with UTC.
#[must_use]
pub fn timestamp(timestamp: SystemTime) -> String {
    DateTime::<Local>::from(timestamp)
        .format("%H:%M:%S%.3f")
        .to_string()
}

/// Time since the previous frame *on screen*, which is what makes a filtered
/// view of one periodic message readable.
#[must_use]
pub fn delta(delta: Option<Duration>) -> String {
    let Some(delta) = delta else {
        return "        ".to_owned();
    };
    let millis = delta.as_secs_f64() * 1000.0;
    if millis < 1000.0 {
        format!("+{millis:6.1}m")
    } else {
        format!("+{:6.2}s", delta.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::delta;
    use std::time::Duration;

    #[test]
    fn the_delta_column_keeps_a_fixed_width() {
        // Ragged columns make a scrolling list unreadable, so every rendering
        // has to occupy the same room, including the empty first row.
        let widths = [
            delta(None).len(),
            delta(Some(Duration::from_micros(500))).len(),
            delta(Some(Duration::from_millis(20))).len(),
            delta(Some(Duration::from_millis(999))).len(),
            delta(Some(Duration::from_secs(12))).len(),
        ];
        assert!(
            widths.iter().all(|width| *width == widths[0]),
            "got {widths:?}"
        );
    }

    #[test]
    fn a_delta_switches_unit_at_a_second() {
        assert!(delta(Some(Duration::from_millis(999))).ends_with('m'));
        assert!(delta(Some(Duration::from_secs(1))).ends_with('s'));
    }
}
