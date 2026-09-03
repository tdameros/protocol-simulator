//! What the terminal actually shows, driven without a terminal.
//!
//! `TestBackend` renders into a buffer, so a test can read the screen the way a
//! person reads it and press keys the way a person presses them.

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use std::time::{Duration, SystemTime};

use sim_core::{ConnectionId, ConnectionStatus, TransportConfig};
use sim_session::state::{ConnectionEntry, Direction, LogEntry};

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

fn linked(app: &mut App, name: &str, status: ConnectionStatus) {
    app.session_mut().connections.push((
        ConnectionId::from(name),
        ConnectionEntry {
            config: TransportConfig::Udp {
                bind: "127.0.0.1:9000".parse().expect("address"),
                remote: "127.0.0.1:9001".parse().expect("address"),
            },
            status,
            retry: None,
            autoconnect: false,
        },
    ));
}

fn captured(app: &mut App, bytes: &[u8], at: Duration) {
    app.session_mut().push_log(LogEntry {
        seq: 0,
        id: ConnectionId::from("bus"),
        direction: Direction::Received,
        bytes: bytes.to_vec(),
        source: None,
        timestamp: SystemTime::UNIX_EPOCH + at,
    });
}

#[test]
fn a_link_says_what_it_is_and_how_it_is_doing() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    let shown = screen(&app);

    assert!(shown.contains("bus"), "{shown}");
    assert!(shown.contains("Connected"), "{shown}");
    assert!(shown.contains("UDP 127.0.0.1:9000"), "{shown}");
}

#[test]
fn no_link_says_so_rather_than_showing_an_empty_box() {
    let shown = screen(&App::default());
    assert!(shown.contains("No link"), "{shown}");
}

#[test]
fn a_captured_frame_shows_its_bytes_and_its_characters() {
    let mut app = App::default();
    captured(&mut app, b"\xAA\x55ok", Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    let shown = screen(&app);

    assert!(shown.contains("AA 55 6F 6B"), "{shown}");
    assert!(shown.contains(".Uok"), "{shown}");
    assert!(shown.contains("RX"), "{shown}");
}

/// A board left running fills the buffer. What matters is the end of it.
#[test]
fn a_full_buffer_shows_its_newest_rows() {
    let mut app = App::default();
    for n in 0..500u16 {
        captured(
            &mut app,
            &n.to_be_bytes(),
            Duration::from_millis(u64::from(n)),
        );
    }
    press(&mut app, KeyCode::Char('2'));
    let shown = screen(&app);

    assert!(shown.contains("01 F3"), "the last row is drawn: {shown}");
    assert!(!shown.contains("00 00"), "the first is not: {shown}");
}
