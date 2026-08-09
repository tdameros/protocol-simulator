use std::fmt::Write as _;
use std::time::Duration;

use sim_core::scenario::{Action, Expect, Scenario, Step};

use egui::{Color32, Grid, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::panels::{scenario_edit, widest};
use crate::state::AppState;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const RUNNING: Color32 = Color32::from_rgb(40, 160, 90);

pub fn show(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    library_bar(ui, state);

    for (file, reason) in &state.scenarios.failures {
        ui.colored_label(ERROR, format!("{file}: {reason}"));
    }

    if state.scenarios.entries.is_empty() {
        if state.scenarios.directory.is_some() && state.scenarios.failures.is_empty() {
            ui.label("No .toml scenario in that folder.");
        }
        return;
    }

    ui.separator();
    scenario_list(ui, state, engine);
    ui.separator();

    // Editing a copy, so what the list and the disk hold is untouched until
    // Save says otherwise.
    if state.scenarios.draft.is_some() {
        let dirty = state.scenarios.draft_is_dirty();
        scenario_edit::header(ui, state);
        ui.separator();
        ScrollArea::vertical()
            .id_salt("scenario_editor")
            .show(ui, |ui| scenario_edit::steps(ui, state));
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    dirty,
                    egui::Button::new(format!("{} Save", icons::FLOPPY_DISK)),
                )
                .clicked()
            {
                save(state);
            }
            if ui.button("Cancel").clicked() {
                state.scenarios.cancel_edit();
            }
        });
        return;
    }

    let Some(scenario) = state.scenarios.selected_scenario().cloned() else {
        return;
    };
    steps(ui, state, &scenario);
}

/// Writes the draft out, choosing a file for one that has never had a home.
fn save(state: &mut AppState) {
    let Some(directory) = state.scenarios.directory.clone() else {
        state.last_error = Some("No scenarios folder to save into.".to_owned());
        return;
    };
    let name = state
        .scenarios
        .draft
        .as_ref()
        .map(|draft| draft.scenario.name.clone())
        .unwrap_or_default();

    let into = crate::scenarios::suggested_file(&directory, &name);
    if let Err(error) = state.scenarios.save_draft(&into) {
        state.last_error = Some(format!("{error:#}"));
    }
}

/// What New starts from: one delay, which is the only step that needs nothing
/// else to exist yet, since there may be no connection configured at all.
fn blank() -> Scenario {
    Scenario {
        name: "New scenario".to_owned(),
        description: None,
        steps: vec![Step {
            targets: Vec::new(),
            action: Action::Wait {
                delay: Duration::from_millis(100),
            },
        }],
        repeat: None,
    }
}

fn library_bar(ui: &mut Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        if ui
            .button(RichText::new(format!(
                "{} Scenarios folder",
                icons::FOLDER_OPEN
            )))
            .clicked()
        {
            if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                state.scenarios.load_from(directory);
            }
        }
        if state.scenarios.directory.is_some()
            && ui
                .button(RichText::new(format!("{} Reload", icons::ARROWS_CLOCKWISE)))
                .on_hover_text("Re-read the .toml files from disk")
                .clicked()
        {
            state.scenarios.reload();
        }

        ui.separator();

        let editing = state.scenarios.draft.is_some();
        if ui
            .add_enabled(
                state.scenarios.directory.is_some() && !editing,
                egui::Button::new(format!("{} New", icons::FILE_PLUS)),
            )
            .on_hover_text("Start a scenario from scratch")
            .clicked()
        {
            state.scenarios.begin_new(blank());
        }
        if ui
            .add_enabled(
                state.scenarios.selected_entry().is_some() && !editing,
                egui::Button::new(format!("{} Edit", icons::PENCIL_SIMPLE)),
            )
            .clicked()
        {
            state.scenarios.begin_edit();
        }
        if ui
            .add_enabled(
                state.scenarios.selected_entry().is_some() && !editing,
                egui::Button::new(icons::TRASH),
            )
            .on_hover_text("Delete this scenario from its file")
            .clicked()
        {
            if let Err(error) = state.scenarios.delete_selected() {
                state.last_error = Some(format!("{error:#}"));
            }
        }
    });

    match &state.scenarios.directory {
        Some(directory) => {
            ui.label(RichText::new(directory.display().to_string()).weak());
        }
        None => {
            ui.label("Pick the folder holding your scenario .toml files.");
        }
    }
}

fn scenario_list(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    // Cloned out: the rows both read the library and start scenarios from it,
    // and the borrow checker is right that those cannot overlap.
    let listed: Vec<(usize, Scenario)> = state
        .scenarios
        .entries
        .iter()
        .map(|entry| entry.scenario.clone())
        .enumerate()
        .collect();

    Grid::new("scenario_list")
        .num_columns(4)
        .min_col_width(0.0)
        .show(ui, |ui| {
            for (index, scenario) in listed {
                let run = state.running.get(&scenario.name).copied();

                ui.label(if run.is_some() {
                    RichText::new(icons::CIRCLE_HALF).color(RUNNING)
                } else {
                    RichText::new(icons::CIRCLE).weak()
                });

                if ui
                    .selectable_label(
                        state.scenarios.selected == Some(index),
                        RichText::new(&scenario.name).strong(),
                    )
                    .clicked()
                {
                    state.scenarios.selected = Some(index);
                }

                ui.label(RichText::new(shape(&scenario)).weak());

                if let Some(run) = run {
                    if ui
                        .button(icons::STOP)
                        .on_hover_text("Stop this scenario")
                        .clicked()
                    {
                        engine.stop_scenario(scenario.name.clone());
                    }
                    // Passes counted from one here: the file says "10 times",
                    // and seeing "pass 0" against that reads as a bug.
                    ui.label(
                        RichText::new(format!("step {}  ·  pass {}", run.step, run.pass + 1))
                            .weak(),
                    );
                } else {
                    if ui.button(icons::PLAY).on_hover_text("Run it").clicked() {
                        start(state, engine, &scenario);
                    }
                    ui.label("");
                }
                ui.end_row();
            }
        });
}

/// Hands the scenario to the engine along with the definitions it will encode
/// against, so the run is unaffected by anything edited afterwards.
fn start(state: &mut AppState, engine: &EngineHandle, scenario: &Scenario) {
    let wanted = scenario.frames_used();
    let frames: Vec<_> = state
        .frames
        .frames
        .iter()
        .filter(|frame| wanted.contains(&frame.name.as_str()))
        .cloned()
        .collect();

    // Said here rather than left to fail on the step that needs it, since the
    // cause is a frames folder, not the scenario.
    let missing: Vec<&str> = wanted
        .into_iter()
        .filter(|name| !frames.iter().any(|frame| frame.name == *name))
        .collect();
    if !missing.is_empty() {
        state.last_error = Some(format!(
            "[{}] no frame loaded named {}",
            scenario.name,
            missing.join(", ")
        ));
        return;
    }

    // Connections get the same treatment. A scenario file is loaded on its own,
    // knowing nothing of the project's links, so a misspelt `on` used to sail
    // through and only show up as an error per send, once a second or once
    // every 10 ms, without stopping anything.
    let unknown = unknown_connections(state, scenario);
    if !unknown.is_empty() {
        state.last_error = Some(format!(
            "[{}] no connection named {}",
            scenario.name,
            unknown.join(", ")
        ));
        return;
    }

    engine.start_scenario(scenario.clone(), frames);
}

/// Names the scenario aims at that the project does not define, in the order
/// they first appear so the message points at the first line to fix.
fn unknown_connections(state: &AppState, scenario: &Scenario) -> Vec<String> {
    let mut unknown: Vec<String> = Vec::new();
    for target in scenario.steps.iter().flat_map(|step| &step.targets) {
        let known = state.connections.iter().any(|(id, _)| id == target);
        if !known && !unknown.contains(&target.0) {
            unknown.push(target.0.clone());
        }
    }
    unknown
}

/// What the scenario does, in one line, so the list is readable without opening
/// anything.
fn shape(scenario: &Scenario) -> String {
    let steps = scenario.steps.len();
    let plural = if steps == 1 { "step" } else { "steps" };
    match scenario.repeat {
        None => format!("{steps} {plural}, once"),
        Some(repeat) => {
            let period = repeat.every.as_millis();
            match repeat.times {
                Some(times) => format!("{steps} {plural}, {times} times every {period} ms"),
                None => format!("{steps} {plural}, every {period} ms"),
            }
        }
    }
}

fn steps(ui: &mut Ui, state: &AppState, scenario: &Scenario) {
    if let Some(description) = &scenario.description {
        ui.label(RichText::new(description).weak());
    }
    let current = state.running.get(&scenario.name).map(|run| run.step);

    let numbers: Vec<String> = (1..=scenario.steps.len())
        .map(|n| format!("{n}."))
        .collect();
    let column = widest(
        ui,
        &egui::TextStyle::Body,
        &numbers.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    ScrollArea::vertical()
        .id_salt("scenario_steps")
        .show(ui, |ui| {
            Grid::new("scenario_steps_grid")
                .num_columns(3)
                .min_col_width(0.0)
                .striped(true)
                .show(ui, |ui| {
                    for (index, step) in scenario.steps.iter().enumerate() {
                        let number = index + 1;
                        let live = current == Some(number);

                        ui.add_sized(
                            [column, ui.spacing().interact_size.y],
                            egui::Label::new(if live {
                                RichText::new(icons::CARET_RIGHT).color(RUNNING)
                            } else {
                                RichText::new(format!("{number}.")).weak()
                            }),
                        );

                        let text = RichText::new(describe(step));
                        ui.label(if live { text.strong() } else { text });
                        let targets: Vec<&str> =
                            step.targets.iter().map(|id| id.0.as_str()).collect();
                        ui.label(RichText::new(targets.join(", ")).weak());
                        ui.end_row();
                    }
                });
        });
}

fn describe(step: &Step) -> String {
    match &step.action {
        Action::Send {
            frame,
            with,
            counters,
        } => {
            let mut text = format!("send {frame}");
            if !with.is_empty() {
                let fields: Vec<&str> = with.keys().map(String::as_str).collect();
                let _ = write!(text, " with {}", fields.join(", "));
            }
            if !counters.is_empty() {
                let fields: Vec<&str> = counters.keys().map(String::as_str).collect();
                let _ = write!(text, " counting {}", fields.join(", "));
            }
            text
        }
        Action::Raw { bytes } => {
            let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("send raw {}", hex.join(" "))
        }
        Action::Wait { delay } => format!("wait {} ms", delay.as_millis()),
        Action::WaitFor { expect, timeout } => {
            let mut text = match expect {
                Expect::Frame { frame, fields } => {
                    format!("wait for {frame} matching {}", fields.join(", "))
                }
                Expect::Pattern { pattern, anchor } => {
                    let mut text = format!("wait for {}", pattern.to_hex());
                    if let Some(offset) = anchor.offset() {
                        let _ = write!(text, " at offset {offset}");
                    }
                    text
                }
            };
            if let Some(limit) = timeout {
                let _ = write!(text, ", giving up after {} ms", limit.as_millis());
            }
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Scenario {
        sim_core::scenario::from_toml(text)
            .expect("should parse")
            .remove(0)
    }

    #[test]
    fn a_misspelt_connection_is_caught_before_anything_is_sent() {
        let mut state = AppState::default();
        state.connections = vec![(
            sim_core::ConnectionId::from("bus"),
            crate::state::ConnectionEntry {
                config: sim_core::TransportConfig::Udp {
                    bind: "127.0.0.1:9000".parse().expect("address"),
                    remote: "127.0.0.1:9001".parse().expect("address"),
                },
                status: sim_core::ConnectionStatus::Connected,
                retry: None,
                autoconnect: false,
            },
        )];

        let good = parse(
            r#"
[[scenario]]
name = "Fine"
on = "bus"
[[scenario.step]]
raw = "00"
"#,
        );
        assert!(unknown_connections(&state, &good).is_empty());

        let typo = parse(
            r#"
[[scenario]]
name = "Typo"
on = ["bus", "buss"]
[[scenario.step]]
raw = "00"
[[scenario.step]]
raw = "01"
on = "uart"
"#,
        );
        // Reported once each, in the order they appear, so the message points
        // at the first line to go and fix.
        assert_eq!(unknown_connections(&state, &typo), ["buss", "uart"]);

        // A delay names no link, so it can never be the reason for a refusal.
        let waiting = parse(
            r#"
[[scenario]]
name = "Waiting"
[[scenario.step]]
wait_ms = 10
"#,
        );
        assert!(unknown_connections(&state, &waiting).is_empty());
    }

    #[test]
    fn the_one_line_shape_says_how_often_it_runs() {
        let once = parse(
            r#"
[[scenario]]
name = "Boot"
on = "bus"
[[scenario.step]]
wait_ms = 5
"#,
        );
        assert_eq!(shape(&once), "1 step, once");

        let forever = parse(
            r#"
[[scenario]]
name = "Beat"
on = "bus"
repeat = { every_ms = 100 }
[[scenario.step]]
raw = "00"
[[scenario.step]]
wait_ms = 5
"#,
        );
        assert_eq!(shape(&forever), "2 steps, every 100 ms");

        let counted = parse(
            r#"
[[scenario]]
name = "Burst"
on = "bus"
repeat = { every_ms = 250, times = 10 }
[[scenario.step]]
raw = "00"
"#,
        );
        assert_eq!(shape(&counted), "1 step, 10 times every 250 ms");
    }

    #[test]
    fn every_kind_of_step_says_what_it_does() {
        let scenario = parse(
            r#"
[[scenario]]
name = "All of them"
on = "bus"
[[scenario.step]]
send = "Telemetry"
with = { mode = 1 }
counters = { seq = { wrap = 255 } }
[[scenario.step]]
raw = "AA 55"
[[scenario.step]]
wait_ms = 40
[[scenario.step]]
wait_for = { hex = "C0 FE", at = 2, timeout_ms = 500 }
[[scenario.step]]
wait_for = { frame = "Telemetry", match = ["sync", "mode"] }
"#,
        );

        let lines: Vec<String> = scenario.steps.iter().map(describe).collect();
        assert_eq!(lines[0], "send Telemetry with mode counting seq");
        assert_eq!(lines[1], "send raw AA 55");
        assert_eq!(lines[2], "wait 40 ms");
        assert_eq!(
            lines[3],
            "wait for C0 FE at offset 2, giving up after 500 ms"
        );
        assert_eq!(lines[4], "wait for Telemetry matching sync, mode");
    }
}
