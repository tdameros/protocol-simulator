use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};
use egui::{Color32, DragValue, RichText, ScrollArea, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::state::{
    AppState, Direction, DirectionFilter, HexAnchor, LogEntry, MonitorId, MonitorState,
    TrafficFilter,
};

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const SENT: Color32 = Color32::from_rgb(70, 130, 200);
const RECEIVED: Color32 = Color32::from_rgb(40, 160, 90);
/// Window the frame and byte rates are measured over.
const RATE_WINDOW: Duration = Duration::from_secs(1);

pub fn show(ui: &mut Ui, state: &mut AppState, id: MonitorId) {
    let next_seq = state.next_seq();
    let names: Vec<String> = state
        .connections
        .iter()
        .map(|(id, _)| id.0.clone())
        .collect();

    // Split apart so the monitor can be edited while the buffer is being read.
    let AppState {
        monitors,
        log,
        hex_input,
        pending_frame_hex,
        monitor_requested,
        ..
    } = state;
    let Some(monitor) = monitors.get_mut(&id) else {
        return;
    };

    toolbar(ui, monitor, next_seq, monitor_requested);
    if monitor.show_filter {
        filter_bar(ui, monitor, &names);
    }

    let mut hex_is_valid = true;
    let rows: Vec<&LogEntry> = {
        let compiled = monitor.filter.compile();
        hex_is_valid = hex_is_valid && compiled.hex_is_valid();
        log.iter()
            .filter(|entry| monitor.in_window(entry) && compiled.keeps(entry))
            .collect()
    };

    if !hex_is_valid {
        ui.colored_label(
            ERROR,
            "Hex pattern: pairs of digits, ?? for any byte. Ignored until it parses.",
        );
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new(summary(&rows, log.len())).weak());
    });
    ui.separator();

    if rows.is_empty() {
        ui.label(if log.is_empty() {
            "No frame exchanged yet."
        } else {
            "No frame matches this filter."
        });
        return;
    }

    // show_rows draws only the visible slice. Painting every entry would make
    // the panel crawl once a periodic frame has filled the buffer.
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    ScrollArea::vertical()
        .stick_to_bottom(monitor.follow)
        .show_rows(ui, row_height, rows.len(), |ui, range| {
            for index in range {
                let entry = rows[index];
                let delta = index.checked_sub(1).and_then(|previous| {
                    entry
                        .timestamp
                        .duration_since(rows[previous].timestamp)
                        .ok()
                });
                frame_row(ui, entry, delta, hex_input, pending_frame_hex);
            }
        });
}

fn toolbar(ui: &mut Ui, monitor: &mut MonitorState, next_seq: u64, requested: &mut bool) {
    ui.horizontal(|ui| {
        let paused = monitor.paused_at.is_some();
        let (glyph, hint) = if paused {
            (icons::PLAY, "Resume")
        } else {
            (
                icons::PAUSE,
                "Freeze the view; frames keep arriving in the buffer",
            )
        };
        if ui.button(glyph).on_hover_text(hint).clicked() {
            // Frozen one before the next arrival, so nothing slips in between.
            monitor.paused_at = (!paused).then(|| next_seq.saturating_sub(1));
        }

        if ui
            .button(icons::TRASH)
            .on_hover_text("Hide everything logged so far, in this tab only")
            .clicked()
        {
            monitor.since = next_seq;
            monitor.paused_at = None;
        }

        ui.checkbox(&mut monitor.follow, "Follow")
            .on_hover_text("Keep the newest frame in view");

        let funnel = if monitor.filter.is_active() {
            RichText::new(icons::FUNNEL).color(SENT).strong()
        } else {
            RichText::new(icons::FUNNEL)
        };
        if ui.button(funnel).on_hover_text("Show the filter").clicked() {
            monitor.show_filter = !monitor.show_filter;
        }

        if ui
            .button(icons::PLUS)
            .on_hover_text("Open another Traffic tab on the same buffer")
            .clicked()
        {
            *requested = true;
        }
    });
}

fn filter_bar(ui: &mut Ui, monitor: &mut MonitorState, names: &[String]) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label("Tab name:");
            ui.text_edit_singleline(&mut monitor.title);
            if ui.button("Reset filter").clicked() {
                monitor.filter = TrafficFilter::default();
            }
        });

        let filter = &mut monitor.filter;

        ui.horizontal_wrapped(|ui| {
            ui.label("Connections:");
            if names.is_empty() {
                ui.label(RichText::new("none configured").weak());
            }
            for name in names {
                // An empty set means all of them, so a fresh tab shows
                // everything without anyone having to tick every box.
                let mut on = filter.connections.contains(name);
                if ui.checkbox(&mut on, name).changed() {
                    if on {
                        filter.connections.insert(name.clone());
                    } else {
                        filter.connections.remove(name);
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("Direction:");
            for choice in DirectionFilter::ALL {
                ui.selectable_value(&mut filter.direction, choice, choice.label());
            }
        });

        ui.horizontal(|ui| {
            ui.label("Hex:");
            ui.add(
                egui::TextEdit::singleline(&mut filter.hex)
                    .font(TextStyle::Monospace)
                    .hint_text("AA 55 ?? 01"),
            );
            let mut anchored = matches!(filter.anchor, HexAnchor::At(_));
            if ui
                .checkbox(&mut anchored, "at offset")
                .on_hover_text("Otherwise the pattern may sit anywhere in the frame")
                .changed()
            {
                filter.anchor = if anchored {
                    HexAnchor::At(0)
                } else {
                    HexAnchor::Anywhere
                };
            }
            if let HexAnchor::At(offset) = &mut filter.anchor {
                ui.add(DragValue::new(offset).range(0..=u16::MAX));
            }
        });

        ui.horizontal(|ui| {
            ui.label("Source:");
            ui.add(egui::TextEdit::singleline(&mut filter.source).hint_text("192.168.1."));
            ui.label("Text:");
            ui.add(egui::TextEdit::singleline(&mut filter.text).hint_text("AT+"));
        });

        ui.horizontal(|ui| {
            ui.label("Length:");
            length_bound(ui, &mut filter.min_len, "min");
            ui.label("to");
            length_bound(ui, &mut filter.max_len, "max");
            ui.checkbox(&mut filter.invert, "Hide matches")
                .on_hover_text(
                    "Show everything except what matches, to get heartbeats out of the way",
                );
        });
    });
}

/// Zero reads as "no limit": a bound of zero bytes would exclude every frame,
/// so it can carry the meaning without costing a second widget.
fn length_bound(ui: &mut Ui, bound: &mut Option<usize>, hint: &str) {
    let mut value = bound.unwrap_or(0);
    if ui
        .add(DragValue::new(&mut value).range(0..=u16::MAX).prefix(""))
        .on_hover_text(format!("{hint} bytes, 0 for no limit"))
        .changed()
    {
        *bound = (value > 0).then_some(value);
    }
}

fn frame_row(
    ui: &mut Ui,
    entry: &LogEntry,
    delta: Option<Duration>,
    hex_input: &mut String,
    pending_frame_hex: &mut Option<Vec<u8>>,
) {
    ui.horizontal(|ui| {
        ui.menu_button(icons::DOTS_THREE, |ui| {
            if ui.button("Copy hex").clicked() {
                ui.ctx().copy_text(format_hex(&entry.bytes));
                ui.close();
            }
            if ui.button("Send to Hex Inject").clicked() {
                *hex_input = format_hex(&entry.bytes);
                ui.close();
            }
            if ui
                .button("Open in Frames")
                .on_hover_text("Decode these bytes into the selected frame's fields")
                .clicked()
            {
                *pending_frame_hex = Some(entry.bytes.clone());
                ui.close();
            }
        });

        ui.label(RichText::new(format_timestamp(entry.timestamp)).weak());
        ui.label(RichText::new(format_delta(delta)).weak().monospace());

        // Phosphor glyphs rather than "→"/"←": the arrows are missing from
        // egui's default font and render as tofu.
        let (arrow, color) = match entry.direction {
            Direction::Sent => (icons::ARROW_RIGHT, SENT),
            Direction::Received => (icons::ARROW_LEFT, RECEIVED),
        };
        ui.label(RichText::new(arrow).color(color).strong());
        ui.label(RichText::new(&entry.id.0).strong());
        if let Some(source) = entry.source {
            ui.label(RichText::new(source.to_string()).weak());
        }
        ui.label(RichText::new(format_hex(&entry.bytes)).text_style(TextStyle::Monospace));
        ui.label(
            RichText::new(format_ascii(&entry.bytes))
                .text_style(TextStyle::Monospace)
                .weak(),
        );
    });
}

/// How much is on screen, and how fast it is arriving.
fn summary(rows: &[&LogEntry], total: usize) -> String {
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
fn format_timestamp(timestamp: SystemTime) -> String {
    DateTime::<Local>::from(timestamp)
        .format("%H:%M:%S%.3f")
        .to_string()
}

/// Time since the previous frame *on screen*, which is what makes a filtered
/// view of one periodic message readable.
fn format_delta(delta: Option<Duration>) -> String {
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

fn format_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_ascii(bytes: &[u8]) -> String {
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
    use super::*;

    #[test]
    fn the_delta_column_keeps_a_fixed_width() {
        // Ragged columns make a scrolling list unreadable, so every rendering
        // has to occupy the same room, including the empty first row.
        let widths = [
            format_delta(None).len(),
            format_delta(Some(Duration::from_micros(500))).len(),
            format_delta(Some(Duration::from_millis(20))).len(),
            format_delta(Some(Duration::from_millis(999))).len(),
            format_delta(Some(Duration::from_secs(12))).len(),
        ];
        assert!(
            widths.iter().all(|width| *width == widths[0]),
            "got {widths:?}"
        );
    }

    #[test]
    fn a_delta_switches_unit_at_a_second() {
        assert!(format_delta(Some(Duration::from_millis(999))).ends_with('m'));
        assert!(format_delta(Some(Duration::from_secs(1))).ends_with('s'));
    }
}
