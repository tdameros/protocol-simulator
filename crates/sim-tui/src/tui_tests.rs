//! What the terminal actually shows, driven without a terminal.
//!
//! `TestBackend` renders into a buffer, so a test can read the screen the way a
//! person reads it and press keys the way a person presses them.

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use crate::app::App;
use crate::ui;

/// The screen as one string, one line per row.
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal.draw(|frame| ui::draw(frame, app)).expect("a draw");

    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, code: KeyCode) {
    app.handle(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn every_view_is_offered_from_the_first_screen() {
    let app = App::default();
    let shown = screen(&app);

    for tab in crate::app::Tab::ALL {
        assert!(shown.contains(tab.title()), "{} is missing", tab.title());
    }
}

#[test]
fn a_digit_goes_straight_to_its_view() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));

    assert_eq!(app.tab(), crate::app::Tab::HexInject);
    assert!(screen(&app).contains("Bytes typed by hand"));
}

#[test]
fn tab_walks_the_views_and_comes_back_round() {
    let mut app = App::default();
    for _ in 0..crate::app::Tab::ALL.len() {
        press(&mut app, KeyCode::Tab);
    }

    assert_eq!(app.tab(), crate::app::Tab::Connections);
}

#[test]
fn shift_tab_walks_the_other_way() {
    let mut app = App::default();
    press(&mut app, KeyCode::BackTab);

    assert_eq!(app.tab(), crate::app::Tab::Scenarios);
}

#[test]
fn the_key_map_opens_and_closes() {
    let mut app = App::default();
    assert!(!screen(&app).contains("Keys"));

    press(&mut app, KeyCode::Char('?'));
    let shown = screen(&app);
    assert!(shown.contains("Keys"));
    assert!(shown.contains("next view"), "the map lists what a key does");

    press(&mut app, KeyCode::Esc);
    assert!(!app.help_is_open());
}

/// Reading the key map is not meant to move you somewhere else.
#[test]
fn a_key_pressed_over_the_map_leaves_the_view_alone() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('4'));

    assert_eq!(app.tab(), crate::app::Tab::Connections);
    assert!(app.help_is_open());
}

#[test]
fn q_ends_it() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('q'));

    assert!(!app.running());
}

/// The one key a terminal program may not keep for itself.
#[test]
fn ctrl_c_ends_it_even_with_the_map_open() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('?'));
    app.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert!(!app.running());
}

#[test]
fn the_keys_are_offered_without_being_asked_for() {
    let shown = screen(&App::default());
    let last = shown.lines().last().expect("a hint line").to_owned();

    assert!(last.contains("quit"), "the hint line reads: {last}");
    assert!(last.contains("Tab"));
}
