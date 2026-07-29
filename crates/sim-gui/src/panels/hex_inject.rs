use sim_core::{ConnectionId, ConnectionStatus};

use egui::{Color32, ComboBox, RichText, TextStyle, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::state::AppState;

pub fn show(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    ui.heading("Raw hex injection");

    // Only a connection removed from the list clears the selection. A target that
    // merely dropped stays selected, so it does not silently vanish from the combo
    // the moment a send fails — the reason is spelled out below instead.
    if state
        .hex_target
        .as_ref()
        .is_some_and(|id| state.status_of(id).is_none())
    {
        state.hex_target = None;
    }

    let targets: Vec<(ConnectionId, ConnectionStatus)> = state
        .connections
        .iter()
        .map(|(id, entry)| (id.clone(), entry.status))
        .collect();

    let selected_label = state
        .hex_target
        .as_ref()
        .map_or_else(|| "choose...".to_owned(), |id| id.0.clone());
    ui.horizontal(|ui| {
        ui.label("Target connection:");
        ComboBox::from_id_salt("hex_target")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for (id, status) in &targets {
                    let is_selected = state.hex_target.as_ref() == Some(id);
                    let label = match status {
                        ConnectionStatus::Connected => id.0.clone(),
                        ConnectionStatus::Connecting => format!("{} (connecting)", id.0),
                        ConnectionStatus::Listening => format!("{} (no peer yet)", id.0),
                        ConnectionStatus::Disconnected => format!("{} (disconnected)", id.0),
                    };
                    if ui.selectable_label(is_selected, label).clicked() {
                        state.hex_target = Some(id.clone());
                    }
                }
            });
    });

    let target_status = state.hex_target.as_ref().and_then(|id| state.status_of(id));
    let target_ready = target_status == Some(ConnectionStatus::Connected);

    if let (Some(id), false) = (state.hex_target.as_ref(), target_ready) {
        ui.colored_label(
            Color32::from_rgb(200, 120, 40),
            format!("\"{}\" is not connected — reconnect it to send.", id.0),
        );
    }

    ui.add(
        egui::TextEdit::singleline(&mut state.hex_input)
            .font(TextStyle::Monospace)
            .hint_text("DE AD BE EF"),
    );

    let parsed = parse_hex(&state.hex_input);
    match &parsed {
        Ok(bytes) => {
            ui.label(format!("{} byte(s) ready to send.", bytes.len()));
        }
        Err(message) if state.hex_input.is_empty() => {
            ui.weak(message);
        }
        Err(message) => {
            ui.colored_label(Color32::from_rgb(200, 60, 60), message);
        }
    }

    let can_send = target_ready && parsed.is_ok();
    if ui
        .add_enabled(
            can_send,
            egui::Button::new(RichText::new(format!("{} Send", icons::PAPER_PLANE_TILT))),
        )
        .clicked()
    {
        if let (Some(id), Ok(bytes)) = (state.hex_target.clone(), parsed) {
            engine.send_raw(id, bytes);
        }
    }
}

fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("Enter hexadecimal bytes (e.g. DEADBEEF).".to_owned());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err("Odd number of hexadecimal digits.".to_owned());
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Non-hexadecimal character detected.".to_owned());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).map_err(|_| "Invalid byte.".to_owned()))
        .collect()
}
