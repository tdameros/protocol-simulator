use std::fmt::Write as _;

use sim_core::scenario::{Action, Scenario, Step};

use egui::{Color32, Grid, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::panels::widest;
use crate::state::AppState;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const RUNNING: Color32 = Color32::from_rgb(40, 160, 90);

pub fn show(ui: &mut Ui, state: &mut AppState, engine: &EngineHandle) {
    library_bar(ui, state);

    for (file, reason) in &state.scenarios.failures {
        ui.colored_label(ERROR, format!("{file}: {reason}"));
    }

    if state.scenarios.scenarios.is_empty() {
        if state.scenarios.directory.is_some() && state.scenarios.failures.is_empty() {
            ui.label("No .toml scenario in that folder.");
        }
        return;
    }

    ui.separator();
    scenario_list(ui, state, engine);

    let Some(scenario) = state.scenarios.selected_scenario().cloned() else {
        return;
    };
    ui.separator();
    steps(ui, state, &scenario);
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
        .scenarios
        .iter()
        .cloned()
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

    engine.start_scenario(scenario.clone(), frames);
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
        Action::WaitFor {
            timeout, anchor, ..
        } => {
            let where_ = match anchor.offset() {
                Some(offset) => format!(" at offset {offset}"),
                None => String::new(),
            };
            match timeout {
                Some(limit) => format!(
                    "wait for a frame{where_}, giving up after {} ms",
                    limit.as_millis()
                ),
                None => format!("wait for a frame{where_}"),
            }
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
"#,
        );

        let lines: Vec<String> = scenario.steps.iter().map(describe).collect();
        assert_eq!(lines[0], "send Telemetry with mode counting seq");
        assert_eq!(lines[1], "send raw AA 55");
        assert_eq!(lines[2], "wait 40 ms");
        assert_eq!(
            lines[3],
            "wait for a frame at offset 2, giving up after 500 ms"
        );
    }
}
