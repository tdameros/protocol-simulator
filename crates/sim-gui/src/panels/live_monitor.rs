use std::time::SystemTime;

use chrono::{DateTime, Local};
use egui::{Color32, RichText, ScrollArea, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::state::{AppState, Direction};

pub fn show(ui: &mut Ui, state: &AppState) {
    ui.heading("Traffic");

    if state.log.is_empty() {
        ui.label("No frame exchanged yet.");
        return;
    }

    // show_rows draws only the visible slice. Painting every entry would make the
    // panel crawl once a periodic frame has filled the buffer.
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    ScrollArea::vertical().stick_to_bottom(true).show_rows(
        ui,
        row_height,
        state.log.len(),
        |ui, rows| {
            for entry in state.log.range(rows) {
                ui.horizontal(|ui| {
                    // Phosphor glyphs rather than "→"/"←": the arrows are missing
                    // from egui's default font and render as tofu.
                    let (arrow, color) = match entry.direction {
                        Direction::Sent => (icons::ARROW_RIGHT, Color32::from_rgb(70, 130, 200)),
                        Direction::Received => (icons::ARROW_LEFT, Color32::from_rgb(40, 160, 90)),
                    };
                    ui.label(RichText::new(format_timestamp(entry.timestamp)).weak());
                    ui.label(RichText::new(arrow).color(color).strong());
                    ui.label(RichText::new(&entry.id.0).strong());
                    if let Some(source) = entry.source {
                        ui.label(RichText::new(source.to_string()).weak());
                    }
                    ui.label(
                        RichText::new(format_hex(&entry.bytes)).text_style(TextStyle::Monospace),
                    );
                    ui.label(
                        RichText::new(format_ascii(&entry.bytes))
                            .text_style(TextStyle::Monospace)
                            .weak(),
                    );
                });
            }
        },
    );
}

/// Wall-clock time in the machine's timezone, so frames line up with scope
/// captures and equipment logs rather than with UTC.
fn format_timestamp(timestamp: SystemTime) -> String {
    DateTime::<Local>::from(timestamp)
        .format("%H:%M:%S%.3f")
        .to_string()
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
