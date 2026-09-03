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
use sim_session::state::{Direction, LogEntry};
use sim_session::{hex, links, traffic};

use crate::app::{App, Overlay, Tab};

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

    if app.overlay() == Some(Overlay::Keys) {
        key_map(frame, frame.area(), app);
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
        tab => pending(frame, area, tab),
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
    let mut spans = Vec::new();
    let keys = app.view_keys().iter().chain(App::KEYS.iter());
    for (at, (key, does)) in keys.enumerate() {
        if at > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            *key,
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(*does).dim());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn key_map(frame: &mut Frame, area: Rect, app: &App) {
    let keys: Vec<(&str, &str)> = app
        .view_keys()
        .iter()
        .chain(App::KEYS.iter())
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
