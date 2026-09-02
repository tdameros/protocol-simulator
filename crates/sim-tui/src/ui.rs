//! Putting the application on screen.
//!
//! Sizes come from the layout rather than from numbers chosen by eye, so the
//! same code fits a serial console and a full window.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Tab};

pub fn draw(frame: &mut Frame, app: &App) {
    let [bar, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    tab_bar(frame, bar, app);
    view(frame, body, app.tab());
    hint_line(frame, hints);

    if app.help_is_open() {
        key_map(frame, frame.area());
    }
}

fn tab_bar(frame: &mut Frame, area: Rect, app: &App) {
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

fn view(frame: &mut Frame, area: Rect, tab: Tab) {
    let body = Paragraph::new(vec![
        Line::from(tab.pending()),
        Line::from(""),
        Line::from("Nothing is wired to the engine yet.".dim()),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::bordered().title(format!(" {} ", tab.title())));

    frame.render_widget(body, area);
}

fn hint_line(frame: &mut Frame, area: Rect) {
    let mut spans = Vec::new();
    for (at, (key, does)) in App::KEYS.iter().enumerate() {
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

fn key_map(frame: &mut Frame, area: Rect) {
    let widest = App::KEYS
        .iter()
        .map(|(key, _)| key.len())
        .max()
        .unwrap_or(0);

    let lines: Vec<Line> = App::KEYS
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
