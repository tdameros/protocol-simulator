use sim_core::scenario::Scenario;

use egui::{Color32, Grid, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icons;

use crate::panels::{scenario_edit, widest};
use sim_session::engine_handle::EngineHandle;
use sim_session::scenarios;
use sim_session::state::Session;

const ERROR: Color32 = Color32::from_rgb(200, 60, 60);
const RUNNING: Color32 = Color32::from_rgb(40, 160, 90);

pub fn show(ui: &mut Ui, state: &mut Session, engine: &EngineHandle) {
    library_bar(ui, state);

    for (file, reason) in &state.scenarios.failures {
        ui.colored_label(ERROR, format!("{file}: {reason}"));
    }

    // Before the emptiness check below, and not after it: the first scenario a
    // folder gets is written into a draft while the list is still empty, and
    // returning early would leave New setting a draft nothing ever draws.
    if state.scenarios.draft.is_none() {
        if state.scenarios.entries.is_empty() {
            if state.scenarios.directory.is_some() && state.scenarios.failures.is_empty() {
                ui.label("No .toml scenario in that folder.");
            }
            return;
        }

        ui.separator();
        scenario_list(ui, state, engine);
    }
    ui.separator();

    // Editing a copy, so what the list and the disk hold is untouched until
    // Save says otherwise.
    if state.scenarios.draft.is_some() {
        let dirty = state.scenarios.draft_is_dirty();
        let problem = state
            .scenarios
            .draft
            .as_ref()
            .and_then(sim_session::scenarios::Draft::problem);
        scenario_edit::header(ui, state);
        ui.separator();
        ScrollArea::vertical()
            .id_salt("scenario_editor")
            .show(ui, |ui| scenario_edit::steps(ui, state));
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    dirty && problem.is_none(),
                    egui::Button::new(format!("{} Save", icons::FLOPPY_DISK)),
                )
                .clicked()
            {
                scenarios::save(state);
            }
            if ui.button("Cancel").clicked() {
                state.scenarios.cancel_edit();
            }
            // Said here rather than after the click: a half-made scenario is a
            // normal state to be in while building one.
            if let Some(reason) = &problem {
                ui.colored_label(ERROR, reason);
            }
        });
        return;
    }

    let Some(scenario) = state.scenarios.selected_scenario().cloned() else {
        return;
    };
    steps(ui, state, &scenario);
}

fn library_bar(ui: &mut Ui, state: &mut Session) {
    ui.horizontal(|ui| {
        // Both throw the draft away, so neither is offered while one is open:
        // losing unsaved work to a stray click is not a trade worth making.
        let idle = state.scenarios.draft.is_none();
        if ui
            .add_enabled(
                idle,
                egui::Button::new(RichText::new(format!(
                    "{} Scenarios folder",
                    icons::FOLDER_OPEN
                ))),
            )
            .clicked()
        {
            if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                state.scenarios.load_from(directory);
            }
        }
        if state.scenarios.directory.is_some()
            && ui
                .add_enabled(
                    idle,
                    egui::Button::new(RichText::new(format!("{} Reload", icons::ARROWS_CLOCKWISE))),
                )
                .on_hover_text("Re-read the .toml files from disk")
                .clicked()
        {
            state.scenarios.reload();
        }

        ui.separator();

        let editing = state.scenarios.draft.is_some();
        // A running scenario is keyed by name in the engine, so renaming or
        // deleting it would leave something running with no row to stop it
        // from, and a rename would even let a second copy start.
        let running = state
            .scenarios
            .selected_scenario()
            .is_some_and(|scenario| state.running.contains_key(&scenario.name));
        if ui
            .add_enabled(
                state.scenarios.directory.is_some() && !editing,
                egui::Button::new(format!("{} New", icons::FILE_PLUS)),
            )
            .on_hover_text("Start a scenario from scratch")
            .clicked()
        {
            state.scenarios.begin_new(scenarios::blank());
        }
        let editable = state.scenarios.selected_entry().is_some() && !editing && !running;
        if ui
            .add_enabled(
                editable,
                egui::Button::new(format!("{} Edit", icons::PENCIL_SIMPLE)),
            )
            .on_hover_text(if running {
                "Stop it before editing it"
            } else {
                "Edit this scenario"
            })
            .clicked()
        {
            state.scenarios.begin_edit();
        }
        if ui
            .add_enabled(editable, egui::Button::new(icons::TRASH))
            .on_hover_text(if running {
                "Stop it before deleting it"
            } else {
                "Delete this scenario from its file"
            })
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

fn scenario_list(ui: &mut Ui, state: &mut Session, engine: &EngineHandle) {
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

                ui.label(RichText::new(scenarios::shape(&scenario)).weak());

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
                        scenarios::start(state, engine, &scenario);
                    }
                    ui.label("");
                }
                ui.end_row();
            }
        });
}

fn steps(ui: &mut Ui, state: &Session, scenario: &Scenario) {
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

                        let text = RichText::new(scenarios::describe(step));
                        ui.label(if live { text.strong() } else { text });
                        let targets: Vec<&str> =
                            step.targets.iter().map(|id| id.0.as_str()).collect();
                        ui.label(RichText::new(targets.join(", ")).weak());
                        ui.end_row();
                    }
                });
        });
}
