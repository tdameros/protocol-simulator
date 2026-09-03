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
use sim_session::reading;
use sim_session::scenarios;
use sim_session::state::{Direction, LogEntry};
use sim_session::{hex, links, traffic};

use crate::app::{App, Overlay, Picker, Tab};

/// The two directions, told apart at a glance rather than read.
const SENT: Color = Color::Rgb(90, 140, 220);
const RECEIVED: Color = Color::Rgb(40, 160, 90);
/// What a frame that will not decode is written in.
const ERROR: Color = Color::Rgb(200, 60, 60);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [bar, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    tab_bar(frame, bar, app);
    view(frame, body, app);
    hint_line(frame, hints, app);

    match app.overlay() {
        Some(Overlay::Keys) => key_map(frame, frame.area(), app),
        Some(Overlay::Pick(picker)) => list_over(frame, frame.area(), picker),
        None => {}
    }
}

fn tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    // The project keeps the right edge, so a bench with several boards open
    // says which one this terminal is looking at.
    let named = app.opened().map(|path| {
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned()
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
        tab @ Tab::Frames => pending(frame, area, tab),
    }
}

fn pending(frame: &mut Frame, area: Rect, tab: Tab) {
    let body = Paragraph::new(vec![
        Line::from(tab.pending()),
        Line::from(""),
        Line::from("Nothing is wired to the engine yet.".dim()),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::bordered().title(format!(" {} ", tab.title())));

    frame.render_widget(body, area);
}

fn connections(frame: &mut Frame, area: Rect, app: &App) {
    let links = &app.session().connections;
    let block = Block::bordered().title(format!(" Connections ({}) ", links.len()));

    if links.is_empty() {
        let empty = Paragraph::new("No link. Open a project that describes one.".dim())
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
        .map(|(id, entry)| {
            Line::from(vec![
                Span::styled(
                    format!("{:named$}", id.0),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(format!("{:stated$}", links::status(entry.status))),
                Span::raw("  "),
                Span::raw(links::summary(entry)).dim(),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(rows).block(block), area);
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
            // row you are on.
            let wanted = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
            let [list, pane] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(wanted)]).areas(area);
            (list, Some(pane))
        }
        None => (area, None),
    };

    rows_view(frame, list, app);

    if let (Some(pane), Some(lines)) = (pane, fields) {
        frame.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" Fields ")),
            pane,
        );
    }
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
    }
    Some(lines)
}

fn rows_view(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows();
    let title = app.monitor().map_or_else(
        || " Traffic ".to_owned(),
        |monitor| {
            let following = if monitor.follow { ", following" } else { "" };
            format!(" {} ({}{}) ", monitor.title, rows.len(), following)
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

    let lines: Vec<Line> = rows[first..]
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
        format!(" {} ", picker.title)
    } else {
        format!(" {} · {} ", picker.title, picker.typed())
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
