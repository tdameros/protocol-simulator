//! Putting the application on screen.
//!
//! Sizes come from the layout rather than from numbers chosen by eye, so the
//! same code fits a serial console and a full window.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Color;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use sim_core::frame::codec;
use sim_core::frame::value::seed_values;
use sim_core::frame::value::Value;
use sim_core::frame::{FieldDef, FieldKind, FrameDef};
use sim_core::ConnectionStatus;
use sim_session::kinds;
use sim_session::reading;
use sim_session::scenarios;
use sim_session::state::{Direction, LogEntry};
use sim_session::{hex, links, traffic};

use crate::app::{App, Browser, Overlay, Picker, Tab};
use crate::connection_form::ConnectionForm;

/// The two directions, told apart at a glance rather than read.
const SENT: Color = Color::Rgb(90, 140, 220);
const RECEIVED: Color = Color::Rgb(40, 160, 90);
/// What a frame that will not decode is written in.
const ERROR: Color = Color::Rgb(200, 60, 60);
/// What a note about a decode that partly worked is written in.
const WARNING: Color = Color::Rgb(200, 120, 40);

pub fn draw(frame: &mut Frame, app: &mut App) {
    // The line only exists while there is something to say, so a working
    // bench gives the whole screen to what it is watching.
    let said = app
        .trouble()
        .map(|text| (text.to_owned(), ERROR))
        .or_else(|| app.status().map(|text| (text.to_owned(), RECEIVED)));
    let [bar, body, line, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(said.is_some())),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    tab_bar(frame, bar, app);
    view(frame, body, app);
    if let Some((text, tint)) = said {
        frame.render_widget(Paragraph::new(Line::from(Span::raw(text).fg(tint))), line);
    }
    hint_line(frame, hints, app);

    match app.overlay() {
        Some(Overlay::Keys) => key_map(frame, frame.area(), app),
        Some(Overlay::Pick(picker, _)) => list_over(frame, frame.area(), picker),
        Some(Overlay::Browse(browser)) => walk_over(frame, frame.area(), browser),
        Some(Overlay::EditText(edit)) => edit_over(frame, frame.area(), edit),
        Some(Overlay::NewConnection(form)) => connection_form_over(frame, frame.area(), form),
        Some(Overlay::Filter(edit)) => filter_editor_over(frame, frame.area(), edit, app),
        Some(Overlay::Step(edit)) => step_editor_over(frame, frame.area(), edit, app),
        Some(Overlay::FrameField(edit)) => frame_field_editor_over(frame, frame.area(), edit, app),
        None => {}
    }
}

fn tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    // The project keeps the right edge, so a bench with several boards open
    // says which one this terminal is looking at.
    let named = app.opened().map(|path| {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        if app.is_dirty() {
            format!("{name} *")
        } else {
            name.into_owned()
        }
    });
    let (area, tail) = match &named {
        Some(name) => {
            let width = u16::try_from(name.len() + 1).unwrap_or(u16::MAX);
            let [tabs, tail] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);
            (tabs, Some((tail, name)))
        }
        None => (area, None),
    };
    if let Some((tail, name)) = tail {
        frame.render_widget(Paragraph::new(name.as_str().dim()), tail);
    }

    let titles = Tab::ALL
        .iter()
        .enumerate()
        .map(|(at, tab)| format!(" {} {} ", at + 1, tab.title()));

    let tabs = Tabs::new(titles)
        .select(Tab::ALL.iter().position(|tab| *tab == app.tab()))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .divider("");

    frame.render_widget(tabs, area);
}

fn view(frame: &mut Frame, area: Rect, app: &mut App) {
    match app.tab() {
        Tab::Connections => connections(frame, area, app),
        Tab::Traffic => watch(frame, area, app),
        Tab::Scenarios => scenarios_view(frame, area, app),
        Tab::HexInject => inject_view(frame, area, app),
        Tab::Frames => frames_view(frame, area, app),
    }
}

fn connections(frame: &mut Frame, area: Rect, app: &App) {
    let links = &app.session().connections;
    let block = Block::bordered().title(format!(" Connections ({}) ", links.len()));

    if links.is_empty() {
        let empty = Paragraph::new("No link. Press o to open a project, or n to add one.".dim())
            .wrap(Wrap { trim: true })
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    // Measured on what is shown rather than on the longest either column could
    // ever hold: "Port open, waiting for a peer" would push the summary off a
    // narrow terminal for the sake of a state nothing is in.
    let named = links.iter().map(|(id, _)| id.0.len()).max().unwrap_or(0);
    let stated = links
        .iter()
        .map(|(_, entry)| links::status(entry.status).len())
        .max()
        .unwrap_or(0);

    let rows: Vec<Line> = links
        .iter()
        .enumerate()
        .map(|(at, (id, entry))| {
            let auto = if entry.autoconnect { "*" } else { " " };
            let retry = if entry.retry.is_some() { "~" } else { " " };
            let line = Line::from(vec![
                Span::raw(format!("{auto} {retry} ")).dim(),
                Span::styled(
                    format!("{:named$}", id.0),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(format!("{:stated$}", links::status(entry.status))),
                Span::raw("  "),
                Span::raw(links::summary(entry)).dim(),
            ]);
            if app.connection_at() == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(rows).block(block), area);
}

/// Adding a connection, or fixing the one field it complained about.
fn connection_form_over(frame: &mut Frame, area: Rect, form: &ConnectionForm) {
    let lines = form.lines();
    let widest = lines
        .iter()
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);

    let mut rendered: Vec<Line> = lines
        .into_iter()
        .map(|(label, value, focused)| {
            let line = Line::from(vec![
                Span::raw(format!("{label:widest$}  ")).dim(),
                Span::raw(value),
            ]);
            if focused {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    if let Some(trouble) = form.trouble() {
        rendered.push(Line::from(""));
        rendered.push(Line::from(Span::raw(trouble.to_owned()).fg(ERROR)));
    }

    let wanted = rendered.iter().map(Line::width).max().unwrap_or(0) + 4;
    let popup = centred(area, wanted, rendered.len() + 4);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(rendered).block(Block::bordered().title(" New connection ")),
        popup,
    );
}

/// The current view's name and filter, changed live.
fn filter_editor_over(frame: &mut Frame, area: Rect, edit: &crate::app::FilterEdit, app: &App) {
    let names: Vec<String> = app
        .session()
        .connections
        .iter()
        .map(|(id, _)| id.0.clone())
        .collect();
    let Some(monitor) = app.monitor() else {
        return;
    };

    let lines = edit.lines(&monitor.title, &monitor.filter, &names);
    let widest = lines
        .iter()
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);

    let rendered: Vec<Line> = lines
        .into_iter()
        .map(|(label, value, focused)| {
            let line = Line::from(vec![
                Span::raw(format!("{label:widest$}  ")).dim(),
                Span::raw(value),
            ]);
            if focused {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    let wanted = rendered.iter().map(Line::width).max().unwrap_or(0) + 4;
    let popup = centred(area, wanted, rendered.len() + 4);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(rendered).block(Block::bordered().title(" Filter ")),
        popup,
    );
}

/// The step under the cursor, its own fields laid out the same way every
/// other form in this front end is.
fn step_editor_over(frame: &mut Frame, area: Rect, edit: &crate::app::StepEdit, app: &App) {
    let Some(draft) = &app.session().scenarios.draft else {
        return;
    };
    let Some(step) = draft.scenario.steps.get(edit.step_index()) else {
        return;
    };
    let names: Vec<String> = app
        .session()
        .connections
        .iter()
        .map(|(id, _)| id.0.clone())
        .collect();
    let frames: Vec<sim_core::frame::FrameDef> = app.session().frames.frames().cloned().collect();

    let lines = edit.lines(step, &names, &frames);
    scrolled_popup(frame, area, lines, " Step ");
}

/// A popup listing one field per row, windowed to the focused one: a frame
/// with more rows to show than the terminal is tall used to grow the popup to
/// fit every one of them, leaving Tab move the focus straight off screen.
fn scrolled_popup(frame: &mut Frame, area: Rect, lines: Vec<(String, String, bool)>, title: &str) {
    let widest = lines
        .iter()
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);
    let focus_at = lines.iter().position(|(_, _, focused)| *focused);

    let all: Vec<Line> = lines
        .into_iter()
        .map(|(label, value, focused)| {
            let line = Line::from(vec![
                Span::raw(format!("{label:widest$}  ")).dim(),
                Span::raw(value),
            ]);
            if focused {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    let room = area.height.saturating_sub(4) as usize;
    let first = match focus_at {
        Some(at) => at
            .saturating_sub(room / 2)
            .min(all.len().saturating_sub(room)),
        None => 0,
    };
    let last = (first + room).min(all.len());
    let rendered = all[first..last].to_vec();

    let wanted = all.iter().map(Line::width).max().unwrap_or(0) + 4;
    let popup = centred(area, wanted, rendered.len() + 4);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(rendered).block(Block::bordered().title(title.to_owned())),
        popup,
    );
}

fn frames_view(frame: &mut Frame, area: Rect, app: &App) {
    if app.session().frames.draft.is_some() {
        frame_editor_view(frame, area, app);
        return;
    }
    let library = &app.session().frames;
    let block = Block::bordered().title(format!(" Frames ({}) ", library.entries.len()));

    if library.entries.is_empty() {
        let empty = Paragraph::new("No frame definition. Open a project, or pass a folder.".dim())
            .wrap(Wrap { trim: true })
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let [list, detail] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Min(3)]).areas(area);

    let widest = library
        .entries
        .iter()
        .map(|entry| entry.frame.name.len())
        .max()
        .unwrap_or(0);

    let lines: Vec<Line> = library
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            let line = Line::from(vec![
                Span::styled(
                    format!("{:widest$}", entry.frame.name),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(format!("{} bytes", entry.frame.size())).dim(),
            ]);
            if library.selected == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), list);
    frame_detail(frame, detail, app);
}

/// The header and the fields of the frame being built, as one navigable
/// list.
fn frame_editor_view(frame: &mut Frame, area: Rect, app: &App) {
    let Some(draft) = &app.session().frames.draft else {
        return;
    };
    let definition = &draft.frame;
    let rows = crate::app::frame_rows(definition);
    let cursor = app.frame_row();

    let widest = 8; // "Endian" plus room, the widest label in the header.
    let invalid = if draft.problem(app.session().frames.types()).is_some() {
        " · invalid"
    } else {
        ""
    };
    let block = Block::bordered().title(format!(" {}{invalid} ", definition.name));

    // The row a frame's field list can grow past a screen's height under, kept
    // on screen the same way the traffic list keeps the row being read.
    let room = block.inner(area).height as usize;
    let first = match cursor {
        Some(at) => at
            .saturating_sub(room / 2)
            .min(rows.len().saturating_sub(room)),
        None => 0,
    };
    let last = (first + room).min(rows.len());

    let lines: Vec<Line> = rows[first..last]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let at = offset + first;
            let (label, value) = match row {
                crate::app::FrameRow::Name => ("Name".to_owned(), definition.name.clone()),
                crate::app::FrameRow::Endian => (
                    "Endian".to_owned(),
                    format!("{:?}", definition.endian).to_lowercase(),
                ),
                crate::app::FrameRow::Field(index) => {
                    let field = &definition.fields[*index];
                    (
                        format!("{}.", index + 1),
                        format!("{}  {}", field.name, kinds::label_of(&field.kind)),
                    )
                }
            };
            let line = Line::from(vec![
                Span::raw(format!("{label:widest$}  ")).dim(),
                Span::raw(value),
            ]);
            if cursor == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// The field under the cursor, opened for its own editor.
fn frame_field_editor_over(
    frame: &mut Frame,
    area: Rect,
    edit: &crate::app::FrameFieldEdit,
    app: &App,
) {
    let Some(draft) = &app.session().frames.draft else {
        return;
    };
    let Some(field) = draft.frame.fields.get(edit.field_index()) else {
        return;
    };

    let lines = edit.lines(field, &draft.frame);
    scrolled_popup(frame, area, lines, &format!(" {} ", field.name));
}

/// The chosen definition, its values, and the bytes they encode to.
fn frame_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(chosen) = app.session().frames.selected_frame().cloned() else {
        let hint = Paragraph::new("Choose a frame to see its fields.".dim())
            .block(Block::bordered().title(" Fields "));
        frame.render_widget(hint, area);
        return;
    };

    let values = app
        .session()
        .frames
        .saved_values()
        .get(&chosen.name)
        .cloned()
        .unwrap_or_else(|| seed_values(&chosen));

    let widest = chosen
        .fields
        .iter()
        .map(|field| sim_session::tree::leaf_name(&field.name).len())
        .max()
        .unwrap_or(0);

    let hex_values = app.session().hex_values;
    let focused = app.frame_focus_is_fields();
    let cursor = app.field_at(&chosen.name);
    let rows = crate::app::field_rows(&chosen);

    let title = if focused {
        format!(" {} (fields) ", chosen.name)
    } else {
        format!(" {} ", chosen.name)
    };
    let block = Block::bordered().title(title);

    // The preview, the note and the target keep their room; a long field list
    // scrolls under them rather than pushing them off the bottom, since they
    // are the answer the fields above are working towards.
    let note = app.session().frame_hex_note.is_some();
    let reserved = 3 + usize::from(note);
    let room = (block.inner(area).height as usize).saturating_sub(reserved);
    let at = cursor.filter(|_| focused);
    let first = match at {
        Some(at) => at
            .saturating_sub(room / 2)
            .min(rows.len().saturating_sub(room)),
        None => 0,
    };
    let last = (first + room).min(rows.len());

    let mut lines: Vec<Line> = rows[first..last]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let at = offset + first;
            let line = field_row_line(&chosen, row, &values, widest, hex_values);
            if focused && cursor == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    lines.push(Line::from(""));
    // What would go out, which is the answer the fields above are working
    // towards.
    lines.push(match codec::encode(&chosen, &values) {
        Ok(bytes) => Line::from(Span::raw(hex::spaced(&bytes))),
        Err(error) => Line::from(Span::raw(error.to_string()).fg(ERROR)),
    });
    if let Some(note) = &app.session().frame_hex_note {
        lines.push(Line::from(Span::raw(note.clone()).fg(WARNING)));
    }
    lines.push(target_line(app));

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// Which connection a frame would go out on, and whether it actually could.
fn target_line(app: &App) -> Line<'static> {
    let Some(id) = &app.session().frame_target else {
        return Line::from(Span::raw("Target: none (t to pick one)").dim());
    };
    match app.session().status_of(id) {
        Some(ConnectionStatus::Connected) => {
            Line::from(Span::raw(format!("Target: {} (connected)", id.0)).dim())
        }
        _ => Line::from(Span::raw(format!("Target: {} (not connected)", id.0)).fg(ERROR)),
    }
}

/// One row of a frame's detail, field or single bit alike.
fn field_row_line(
    frame: &FrameDef,
    row: &crate::app::FieldRow,
    values: &sim_core::frame::value::FieldValues,
    widest: usize,
    hex: bool,
) -> Line<'static> {
    match *row {
        crate::app::FieldRow::Field(index) => {
            let field = &frame.fields[index];
            let held = values.get(&field.name);
            let said = held.map_or_else(String::new, |value| reading::describe(field, value, hex));
            Line::from(vec![
                Span::styled(
                    format!("{:widest$}", sim_session::tree::leaf_name(&field.name)),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(sim_session::frames::type_label(field)).dim(),
                Span::raw("  "),
                Span::raw(said),
            ])
        }
        crate::app::FieldRow::Bit { field, bit } => {
            let FieldKind::Bits { repr, bits } = &frame.fields[field].kind else {
                return Line::from("");
            };
            let bit_def = &bits[bit];
            let position = kinds::bit_positions(*repr, bits)
                .get(bit)
                .cloned()
                .flatten()
                .unwrap_or_default();
            let held = values
                .get(&frame.fields[field].name)
                .and_then(Value::as_bits)
                .and_then(|set| set.get(&bit_def.name))
                .copied()
                .unwrap_or(0);
            let said = reading::unsigned(held, bit_def.width.div_ceil(4) as usize, hex);
            let tint = if held == 0 {
                Style::new().add_modifier(Modifier::DIM)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            Line::from(vec![
                Span::raw(format!("  {}", bit_def.name)).dim(),
                Span::raw("  "),
                Span::raw(position).dim(),
                Span::raw("  "),
                Span::styled(said, tint),
            ])
        }
    }
}

fn inject_view(frame: &mut Frame, area: Rect, app: &App) {
    let typed = &app.session().hex_input;
    let target = app
        .session()
        .hex_target
        .clone()
        .or_else(|| app.session().connections.first().map(|(id, _)| id.clone()));

    let said = match hex::parse(typed) {
        Ok(bytes) => Span::raw(format!("{} byte(s) ready to send.", bytes.len())),
        // Nothing typed is not a mistake to point at, only a box not filled in.
        Err(hex::Problem::Empty) => Span::raw("Type hexadecimal bytes.").dim(),
        Err(problem) => Span::raw(problem.to_string()).fg(ERROR),
    };

    let on = match &target {
        Some(id) => Span::raw(format!("on {}", id.0)),
        None => Span::raw("No link to send on.").fg(ERROR),
    };

    // The cursor is drawn rather than left to the terminal: nothing else on
    // screen says which box has the keyboard.
    let box_text = if app.is_editing() {
        format!("{typed}_")
    } else {
        typed.clone()
    };

    let body = Paragraph::new(vec![
        Line::from(Span::raw(box_text)),
        Line::from(""),
        Line::from(said),
        Line::from(on),
    ])
    .wrap(Wrap { trim: false })
    .block(Block::bordered().title(" Hex injection "));

    frame.render_widget(body, area);
}

fn scenarios_view(frame: &mut Frame, area: Rect, app: &App) {
    if app.session().scenarios.draft.is_some() {
        scenario_editor_view(frame, area, app);
        return;
    }
    let library = &app.session().scenarios;
    let block = Block::bordered().title(format!(" Scenarios ({}) ", library.entries.len()));

    if library.entries.is_empty() {
        let empty =
            Paragraph::new("No scenario. Open a project, or pass a folder that holds some.".dim())
                .wrap(Wrap { trim: true })
                .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let [list, steps] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Min(3)]).areas(area);

    let widest = library
        .entries
        .iter()
        .map(|entry| entry.scenario.name.len())
        .max()
        .unwrap_or(0);

    let lines: Vec<Line> = library
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            let scenario = &entry.scenario;
            let run = app.session().running.get(&scenario.name);
            let state = match run {
                // Counted as the file numbers them, which is what a person
                // reading the file alongside is looking at.
                Some(run) => format!("step {} pass {}", run.step, run.pass + 1),
                None => scenarios::shape(scenario),
            };
            let tint = if run.is_some() {
                Style::new().fg(RECEIVED)
            } else {
                Style::new().add_modifier(Modifier::DIM)
            };

            let line = Line::from(vec![
                Span::styled(
                    format!("{:widest$}", scenario.name),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(state, tint),
            ]);
            if library.selected == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), list);
    steps_view(frame, steps, app);
}

/// What the chosen scenario does, step by step, as its file spells it.
/// The header and the steps of the scenario being built, as one navigable
/// list.
fn scenario_editor_view(frame: &mut Frame, area: Rect, app: &App) {
    let Some(draft) = &app.session().scenarios.draft else {
        return;
    };
    let scenario = &draft.scenario;
    let rows = crate::app::scenario_rows(scenario);
    let cursor = app.scenario_row();

    let widest = 14; // "Repeat times" plus room, the widest label in the header.
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            let (label, value) = match row {
                crate::app::ScenarioRow::Name => ("Name".to_owned(), scenario.name.clone()),
                crate::app::ScenarioRow::Description => (
                    "Description".to_owned(),
                    scenario.description.clone().unwrap_or_default(),
                ),
                crate::app::ScenarioRow::Repeat => (
                    "Repeat".to_owned(),
                    if scenario.repeat.is_some() {
                        "yes"
                    } else {
                        "no"
                    }
                    .to_owned(),
                ),
                crate::app::ScenarioRow::RepeatEvery => (
                    "  Every (ms)".to_owned(),
                    scenario
                        .repeat
                        .map_or_else(String::new, |repeat| repeat.every.as_millis().to_string()),
                ),
                crate::app::ScenarioRow::RepeatTimes => (
                    "  Times".to_owned(),
                    scenario
                        .repeat
                        .and_then(|repeat| repeat.times)
                        .map_or_else(|| "forever".to_owned(), |times| times.to_string()),
                ),
                crate::app::ScenarioRow::Step(index) => {
                    let step = &scenario.steps[*index];
                    let targets = step
                        .targets
                        .iter()
                        .map(|id| id.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        format!("{}.", index + 1),
                        format!("{}  {}", scenarios::describe(step), targets),
                    )
                }
            };
            let line = Line::from(vec![
                Span::raw(format!("{label:widest$}  ")).dim(),
                Span::raw(value),
            ]);
            if cursor == Some(at) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    let dirty = if draft.problem().is_some() {
        " · invalid"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" {}{dirty} ", scenario.name))),
        area,
    );
}

fn steps_view(frame: &mut Frame, area: Rect, app: &App) {
    let Some(scenario) = app.session().scenarios.selected_scenario() else {
        let hint = Paragraph::new("Choose a scenario to see its steps.".dim())
            .block(Block::bordered().title(" Steps "));
        frame.render_widget(hint, area);
        return;
    };

    let lines: Vec<Line> = scenario
        .steps
        .iter()
        .enumerate()
        .map(|(at, step)| {
            let links = step
                .targets
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Line::from(vec![
                Span::raw(format!("{:>3}  ", at + 1)).dim(),
                Span::raw(scenarios::describe(step)),
                Span::raw("  "),
                Span::raw(links).dim(),
            ])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(" {} ", scenario.name))),
        area,
    );
}

fn watch(frame: &mut Frame, area: Rect, app: &mut App) {
    // Built first, while the reading may still settle which definition it uses.
    // What comes back is owned, so the list below can borrow freely.
    let fields = field_lines(app);

    let (list, pane) = match &fields {
        Some(lines) => {
            // Never more than half the screen: the list is what tells you which
            // row you are on, and a definition with twenty fields would
            // otherwise leave one row of it.
            let wanted = u16::try_from(lines.len() + 2)
                .unwrap_or(u16::MAX)
                .min(area.height / 2);
            let [list, pane] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(wanted)]).areas(area);
            (list, Some(pane))
        }
        None => (area, None),
    };

    rows_view(frame, list, app);

    if let (Some(pane), Some(lines)) = (pane, fields) {
        // A frame with more fields than the pane's own share of the screen
        // used to have the rest simply cut off, with no way to reach them.
        let focused = app.traffic_focus_is_fields();
        let room = pane.height.saturating_sub(2) as usize;
        let start = app
            .traffic_field_scroll()
            .min(lines.len().saturating_sub(room));
        let end = (start + room).min(lines.len());
        let title = if focused {
            " Fields (focused) "
        } else {
            " Fields "
        };
        frame.render_widget(
            Paragraph::new(lines[start..end].to_vec()).block(Block::bordered().title(title)),
            pane,
        );
    }
}

/// One row per sub-field, under the word holding them.
///
/// The packed word reads as nothing on its own, which is why `describe` leaves
/// it blank: what a bitfield says is in its flags. A set flag stands out of a
/// column of clear ones, so a fault is what the eye lands on.
fn bit_rows(field: &FieldDef, value: Option<&Value>, hex: bool) -> Vec<Line<'static>> {
    let FieldKind::Bits { repr, bits } = &field.kind else {
        return Vec::new();
    };
    let Some(Value::Bits(set)) = value else {
        return Vec::new();
    };

    let widest = bits.iter().map(|bit| bit.name.len()).max().unwrap_or(0);

    bits.iter()
        .zip(kinds::bit_positions(*repr, bits))
        .map(|(bit, position)| {
            let held = set.get(&bit.name).copied().unwrap_or_default();
            let said = reading::unsigned(held, (bit.width.div_ceil(4)) as usize, hex);
            let tint = if held == 0 {
                Style::new().add_modifier(Modifier::DIM)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            Line::from(vec![
                Span::raw(format!("  {:widest$}", bit.name)).dim(),
                Span::raw("  "),
                Span::raw(position.unwrap_or_default()).dim(),
                Span::raw("  "),
                Span::styled(said, tint),
            ])
        })
        .collect()
}

/// The selected row read field by field, or the reason there is nothing to
/// read.
fn field_lines(app: &mut App) -> Option<Vec<Line<'static>>> {
    let hex = app.session().hex_values;
    let (entry, reading) = app.selected_reading()?;

    let Some(frame) = reading.chosen() else {
        let said = reading.nothing().unwrap_or("Nothing to read.").to_owned();
        return Some(vec![Line::from(said.dim())]);
    };

    let decoded = match codec::decode(frame, &entry.bytes) {
        Ok(decoded) => decoded,
        Err(error) => return Some(vec![Line::from(error.to_string().fg(ERROR))]),
    };

    let widest = frame
        .fields
        .iter()
        .map(|field| field.name.len())
        .max()
        .unwrap_or(0);

    let mut lines = vec![Line::from(format!("read as {}", frame.name).fg(RECEIVED))];
    for (index, field) in frame.fields.iter().enumerate() {
        let offset = frame.offset_of(index);
        let end = offset + field.kind.size();
        let said = decoded
            .values
            .get(&field.name)
            .map_or_else(String::new, |value| reading::describe(field, value, hex));

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:widest$}", field.name),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(format!("{offset}..{end}")).dim(),
            Span::raw("  "),
            Span::raw(hex::spaced(&entry.bytes[offset..end])),
            Span::raw("  "),
            Span::raw(said),
        ]));
        lines.extend(bit_rows(field, decoded.values.get(&field.name), hex));
    }
    Some(lines)
}

fn rows_view(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows();
    let title = app.monitor().map_or_else(
        || " Traffic ".to_owned(),
        |monitor| {
            let following = if monitor.follow { ", following" } else { "" };
            let paused = if monitor.paused_at.is_some() {
                ", paused"
            } else {
                ""
            };
            let tab = app
                .monitor_position()
                .filter(|(_, of)| *of > 1)
                .map_or_else(String::new, |(at, of)| format!(" [{at}/{of}]"));
            let counted = traffic::summary(&rows, app.session().log.len());
            format!(" {}{} ({counted}{following}{paused}) ", monitor.title, tab)
        },
    );
    let block = Block::bordered().title(title);

    if rows.is_empty() {
        let empty = Paragraph::new("Nothing captured yet.".dim()).block(block);
        frame.render_widget(empty, area);
        return;
    }

    let reading = app.monitor().and_then(|monitor| monitor.selected);
    let at = reading.and_then(|seq| rows.iter().position(|entry| entry.seq == seq));

    // Only the visible slice is drawn. Painting ten thousand rows to show
    // twenty would cost a board its idle time.
    let room = block.inner(area).height as usize;
    let first = match at {
        // Keep the read row on screen, and the rows around it for context.
        Some(at) => at
            .saturating_sub(room / 2)
            .min(rows.len().saturating_sub(room)),
        None => rows.len().saturating_sub(room),
    };

    let last = (first + room).min(rows.len());
    let lines: Vec<Line> = rows[first..last]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let index = offset + first;
            let previous = index.checked_sub(1).and_then(|before| rows.get(before));
            let line = row(entry, previous.copied());
            if at == Some(index) {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One captured frame: when, how long since the last one on screen, which way,
/// which link, and the bytes.
fn row(entry: &LogEntry, previous: Option<&LogEntry>) -> Line<'static> {
    let gap = previous.and_then(|before| entry.timestamp.duration_since(before.timestamp).ok());
    let (arrow, tint) = match entry.direction {
        Direction::Sent => ("TX", Style::new().fg(SENT)),
        Direction::Received => ("RX", Style::new().fg(RECEIVED)),
    };

    Line::from(vec![
        Span::raw(traffic::timestamp(entry.timestamp)).dim(),
        Span::raw(" "),
        Span::raw(traffic::delta(gap)).dim(),
        Span::raw(" "),
        Span::styled(arrow, tint),
        Span::raw(" "),
        Span::raw(entry.id.0.clone()),
        Span::raw("  "),
        Span::raw(hex::spaced(&entry.bytes)),
        Span::raw("  "),
        Span::raw(hex::printable(&entry.bytes)).dim(),
    ])
}

fn hint_line(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;

    // The way out keeps its room whatever else has to go. What the view adds
    // comes next, being the keys the screen in front of you answers to, and
    // moving between views is what gets dropped first on a narrow terminal.
    let escapes = hints(app.escapes(), width);
    let room = width.saturating_sub(escapes.width() + 3);
    let offered: Vec<(&str, &str)> = app
        .view_keys()
        .iter()
        .chain(App::KEYS.iter())
        .copied()
        .collect();

    let mut line = hints(&offered, room);
    if !line.spans.is_empty() {
        line.push_span(Span::raw("   "));
    }
    line.spans.extend(escapes.spans);

    frame.render_widget(Paragraph::new(line), area);
}

/// As many of `keys` as fit in `room`, dropped whole rather than cut: half a
/// word reads as a mistake.
fn hints(keys: &[(&str, &str)], room: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (key, does) in keys {
        let gap = usize::from(!spans.is_empty()) * 3;
        let wanted = gap + key.len() + 1 + does.len();
        if used + wanted > room {
            break;
        }
        used += wanted;
        if gap > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw((*does).to_owned()).dim());
    }
    Line::from(spans)
}

fn key_map(frame: &mut Frame, area: Rect, app: &App) {
    let keys: Vec<(&str, &str)> = app
        .view_keys()
        .iter()
        .chain(App::KEYS.iter())
        .chain(app.escapes().iter())
        .copied()
        .collect();

    let widest = keys.iter().map(|(key, _)| key.len()).max().unwrap_or(0);

    let lines: Vec<Line> = keys
        .iter()
        .map(|(key, does)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:>widest$}"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(*does),
            ])
        })
        .collect();

    // Wide enough for the longest line, tall enough for every key, plus the
    // border and one row of air on each side.
    let wanted = lines.iter().map(Line::width).max().unwrap_or(0) + 4;
    let popup = centred(area, wanted, lines.len() + 4);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" Keys ")
                .padding(ratatui::widgets::Padding::symmetric(1, 1)),
        ),
        popup,
    );
}

/// One answer chosen from a list, over whatever the view was showing.
fn list_over(frame: &mut Frame, area: Rect, picker: &Picker) {
    let named = picker.title.clone();
    list_titled(frame, area, picker, &named);
}

fn list_titled(frame: &mut Frame, area: Rect, picker: &Picker, named: &str) {
    let (shown, at) = picker.shown();

    let lines: Vec<Line> = shown
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let line = Line::from(format!("  {option}  "));
            if index == at {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    // What is typed goes in the title, where it explains a list that has just
    // become shorter without anything else changing.
    let title = if picker.typed().is_empty() {
        format!(" {named} ")
    } else {
        format!(" {named} · {} ", picker.typed())
    };

    let widest = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(title.len());
    let popup = centred(area, widest + 2, lines.len() + 2);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        popup,
    );
}

/// The disk, a folder at a time.
///
/// Where the walk is stands in the title rather than beside the list, since it
/// is the one thing that changes when a folder is entered and everything else
/// on screen looks the same.
fn walk_over(frame: &mut Frame, area: Rect, browser: &Browser) {
    if let Some(trouble) = browser.trouble() {
        let popup = centred(area, trouble.len() + 4, 3);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(trouble.to_owned()).fg(ERROR)))
                .block(Block::bordered().title(" Open ")),
            popup,
        );
        return;
    }
    // The whole path, not just the folder name: two projects both under a
    // `frames` folder look the same otherwise.
    list_titled(
        frame,
        area,
        browser.picker(),
        &browser.at().display().to_string(),
    );
}

/// One value, typed as text, over whatever the view was showing.
fn edit_over(frame: &mut Frame, area: Rect, edit: &crate::app::EditBox) {
    let box_text = format!("{}_", edit.text);
    let lines = vec![Line::from(Span::raw(box_text))];
    let wanted = (lines[0].width() + 4).max(edit.title.len() + 4);
    let popup = centred(area, wanted, 3);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(" {} ", edit.title))),
        popup,
    );
}

/// A box of the size asked for, in the middle, never wider than what there is.
fn centred(area: Rect, width: usize, height: usize) -> Rect {
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let height = u16::try_from(height).unwrap_or(u16::MAX);

    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [boxed] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    boxed
}
