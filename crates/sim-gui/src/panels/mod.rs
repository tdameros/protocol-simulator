pub mod connections;
pub mod frame_edit;
mod frame_editor;
pub mod hex_inject;
pub mod live_monitor;
pub mod scenario_edit;
pub mod scenario_list;
pub mod type_edit;

use egui::{Color32, Layout, Response, TextStyle, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::TabViewer;

use crate::engine_handle::EngineHandle;
use crate::state::{AppState, MonitorId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Tab {
    Connections,
    /// Several may be open at once, each filtering the shared buffer its own way.
    LiveMonitor(MonitorId),
    HexInject,
    FrameEditor,
    Scenarios,
}

pub struct AppTabViewer<'a> {
    pub state: &'a mut AppState,
    pub engine: &'a EngineHandle,
}

/// How wide the widest of `samples` renders, in the given style.
///
/// Columns are sized from strings chosen up front rather than from whatever is
/// on the row being drawn. A width taken from the content moves as the content
/// does, which is the whole defect this exists to avoid.
pub fn widest(ui: &Ui, style: &TextStyle, samples: &[&str]) -> f32 {
    let font = style.resolve(ui.style());
    samples
        .iter()
        .map(|text| {
            ui.painter()
                .layout_no_wrap((*text).to_owned(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
}

/// Lays out `contents` in a slot of exactly `width`, so whatever comes after it
/// starts at the same place on every row.
///
/// `set_min_width` is what makes it a slot rather than a ceiling: the desired
/// size handed to `allocate_ui_with_layout` only caps the room the contents may
/// use, and the space actually taken shrinks back to whatever they drew. Short
/// contents would then still let the next widget slide left, which is the whole
/// thing this is here to stop.
pub fn column<R>(ui: &mut Ui, width: f32, contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            contents(ui)
        },
    )
    .inner
}

/// A number box that reads the notations a protocol is actually written in.
///
/// egui parses decimal and nothing else, so `0xBA` copied off a datasheet has
/// to be converted by hand before it can be typed anywhere. Every number box in
/// the app goes through here so that the answer is the same wherever you are.
/// `hex` asks for the value to be shown in hexadecimal, padded to that many
/// digits. What the box *accepts* never changes: decimal stays typeable
/// whatever it is showing.
pub fn number<Num: egui::emath::Numeric>(
    value: &mut Num,
    hex: Option<usize>,
) -> egui::DragValue<'_> {
    let widget = egui::DragValue::new(value).custom_parser(read_number);
    match hex {
        Some(digits) => widget.custom_formatter(move |value, _| hex_text(value, digits)),
        None => widget,
    }
}

/// A number as hexadecimal, prefixed so it cannot be mistaken for decimal and
/// padded to the width of whatever holds it.
///
/// The prefix is not decoration: `10` shown bare would read as ten, and the
/// same box takes decimal input, so the two have to be told apart on sight.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value comes from an integer field and is shown, not computed"
)]
fn hex_text(value: f64, digits: usize) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let magnitude = value.abs() as u64;
    format!("{sign}0x{magnitude:0digits$X}")
}

/// Decimal, hexadecimal, binary or octal, signed, with `_` allowed anywhere as
/// a separator.
///
/// `None` for anything else, which leaves the box holding its previous value
/// rather than jumping to zero.
#[allow(
    clippy::cast_precision_loss,
    reason = "a drag value is an f64 whatever is typed into it"
)]
fn read_number(text: &str) -> Option<f64> {
    let text = text.trim();
    let (negative, rest) = match text.strip_prefix(['-', '+']) {
        Some(rest) => (text.starts_with('-'), rest.trim_start()),
        None => (false, text),
    };

    let digits = rest.replace('_', "");
    let radix = ["0x", "0b", "0o"]
        .into_iter()
        .zip([16, 2, 8])
        .find(|(prefix, _)| {
            digits.len() > prefix.len() && digits[..2].eq_ignore_ascii_case(prefix)
        });

    let value = match radix {
        Some((_, radix)) => u64::from_str_radix(&digits[2..], radix).ok()? as f64,
        // Plain decimal, and whatever else Rust reads as a float, so `1e3`
        // still works for anyone who types it.
        None => digits.parse::<f64>().ok()?,
    };
    Some(if negative { -value } else { value })
}

/// The two words that open a library row, so the pickers below them line up.
const LIBRARY_LABELS: [&str; 2] = ["Frame:", "Shared types:"];

/// The label opening a library row, in a column wide enough for either of them.
///
/// Frames and shared types are picked and managed the same way, so the two rows
/// are read as one thing. Two labels of different widths would put their
/// pickers a few pixels apart, which is enough to make them look unrelated.
pub fn library_label(ui: &mut Ui, text: &str) {
    let width = widest(ui, &TextStyle::Body, &LIBRARY_LABELS);
    field_label(ui, text, width);
}

/// A label filling a fixed column, for the left edge of a form.
pub fn field_label(ui: &mut Ui, text: &str, width: f32) -> Response {
    column(ui, width, |ui| ui.label(text))
}

impl TabViewer for AppTabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        match tab {
            Tab::Connections => "Connections".into(),
            // Renamed from inside the panel, so the tab reads back what you typed.
            Tab::LiveMonitor(id) => self
                .state
                .monitors
                .get(id)
                .map_or("Traffic", |monitor| monitor.title.as_str())
                .to_owned()
                .into(),
            Tab::HexInject => "Hex Inject".into(),
            Tab::FrameEditor => "Frames".into(),
            Tab::Scenarios => "Scenarios".into(),
        }
    }

    /// The tab itself, never its title.
    ///
    /// `egui_dock` defaults to hashing the title, and hangs the whole tab body
    /// off the result. A Traffic tab can be renamed from inside itself, so that
    /// default would give every keystroke a different tab, and the field being
    /// typed into would lose focus on each character.
    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        match tab {
            Tab::Connections => connections::show(ui, self.state, self.engine),
            Tab::LiveMonitor(id) => live_monitor::show(ui, self.state, *id),
            Tab::HexInject => hex_inject::show(ui, self.state, self.engine),
            Tab::FrameEditor => frame_editor::show(ui, self.state, self.engine),
            Tab::Scenarios => scenario_list::show(ui, self.state, self.engine),
        }
    }

    /// Closing the last view of the buffer is allowed; the buffer itself is not
    /// the tab's to keep.
    fn on_close(&mut self, tab: &mut Self::Tab) -> OnCloseResponse {
        if let Tab::LiveMonitor(id) = tab {
            self.state.close_monitor(*id);
        }
        OnCloseResponse::Close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_can_be_written_the_way_the_protocol_writes_it() {
        for (typed, expected) in [
            ("0xBA", 186.0),
            ("0XbA", 186.0),
            (" 0x10 ", 16.0),
            ("0xFF_FF", 65535.0),
            ("0b1011", 11.0),
            ("0o17", 15.0),
            ("255", 255.0),
            ("-0x10", -16.0),
            ("+42", 42.0),
            // Still whatever it always read, so nobody loses what they had.
            ("1.5", 1.5),
            ("1e3", 1000.0),
            ("-7", -7.0),
        ] {
            assert_eq!(read_number(typed), Some(expected), "reading {typed}");
        }
    }

    #[test]
    fn a_number_shown_as_hexadecimal_can_be_read_back() {
        for (value, digits, shown) in [
            (186.0, 2, "0xBA"),
            (43605.0, 4, "0xAA55"),
            (5.0, 4, "0x0005"),
            (-16.0, 2, "-0x10"),
            (0.0, 2, "0x00"),
        ] {
            assert_eq!(hex_text(value, digits), shown);
            // What it shows has to be something it would take back, or a box
            // could not be edited from the value it is displaying.
            assert_eq!(read_number(shown), Some(value), "reading back {shown}");
        }
    }

    #[test]
    fn something_that_is_not_a_number_leaves_the_box_alone() {
        // `None` keeps the previous value, where a zero would silently replace
        // whatever was in the field.
        for typed in ["", "   ", "0x", "0b", "nope", "0xZZ", "12ab", "0x1.5"] {
            assert_eq!(read_number(typed), None, "reading {typed}");
        }
    }

    #[test]
    fn renaming_a_traffic_tab_leaves_its_identity_alone() {
        let mut state = AppState::default();
        let monitor = state.open_monitor();
        let engine = EngineHandle::new();
        let mut viewer = AppTabViewer {
            state: &mut state,
            engine: &engine,
        };

        let mut tab = Tab::LiveMonitor(monitor);
        let before = viewer.id(&mut tab);
        assert_eq!(viewer.title(&mut tab).text(), "Traffic");

        if let Some(monitor) = viewer.state.monitors.get_mut(&monitor) {
            monitor.title = "Heartbeats".to_owned();
        }

        assert_eq!(viewer.title(&mut tab).text(), "Heartbeats");
        // Every widget in the tab hangs off this. If it moved with the title,
        // typing a name would knock the focus out of the field on each letter.
        assert_eq!(viewer.id(&mut tab), before);
    }
}
