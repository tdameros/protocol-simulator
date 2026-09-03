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
fn screen(app: &mut App) -> String {
    narrow(app, 80)
}

/// The same screen at a chosen width, since a serial console is not 80 columns.
fn narrow(app: &mut App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 24)).expect("a test terminal");
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
    let mut app = App::default();
    let shown = screen(&mut app);

    for tab in crate::app::Tab::ALL {
        assert!(shown.contains(tab.title()), "{} is missing", tab.title());
    }
}

#[test]
fn a_digit_goes_straight_to_its_view() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));

    assert_eq!(app.tab(), crate::app::Tab::HexInject);
    assert!(screen(&mut app).contains("Hex injection"));
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
    assert!(!screen(&mut app).contains("Keys"));

    press(&mut app, KeyCode::Char('?'));
    let shown = screen(&mut app);
    assert!(shown.contains("Keys"));
    assert!(shown.contains("next view"), "the map lists what a key does");

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay().is_none());
}

/// Reading the key map is not meant to move you somewhere else.
#[test]
fn a_key_pressed_over_the_map_leaves_the_view_alone() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('4'));

    assert_eq!(app.tab(), crate::app::Tab::Connections);
    assert!(app.overlay().is_some());
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
    let shown = screen(&mut App::default());
    let last = shown.lines().last().expect("a hint line").to_owned();

    // The way out and the way to the rest never give way to a view's own
    // keys, whichever view is on show first.
    assert!(last.contains("quit"), "the hint line reads: {last}");
    assert!(last.contains("keys"), "{last}");
}

/// Moving between views is not always in the one-line hint, since a busy
/// view crowds it out, but it is always in the full map.
#[test]
fn moving_between_views_is_always_in_the_full_map() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('?'));
    let shown = screen(&mut app);

    assert!(shown.contains("next view"), "{shown}");
    assert!(shown.contains("previous view"), "{shown}");
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
    let shown = screen(&mut app);

    assert!(shown.contains("bus"), "{shown}");
    assert!(shown.contains("Connected"), "{shown}");
    assert!(shown.contains("UDP 127.0.0.1:9000"), "{shown}");
}

#[test]
fn no_link_says_so_rather_than_showing_an_empty_box() {
    let shown = screen(&mut App::default());
    assert!(shown.contains("No link"), "{shown}");
}

#[test]
fn a_captured_frame_shows_its_bytes_and_its_characters() {
    let mut app = App::default();
    captured(&mut app, b"\xAA\x55ok", Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    let shown = screen(&mut app);

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
    let shown = screen(&mut app);

    assert!(shown.contains("01 F3"), "the last row is drawn: {shown}");
    assert!(!shown.contains("00 00"), "the first is not: {shown}");
}

/// Ragged columns make a list of links unreadable at a glance.
#[test]
fn the_link_columns_keep_one_left_edge() {
    let mut app = App::default();
    linked(&mut app, "drive", ConnectionStatus::Connected);
    linked(&mut app, "sensor-bus", ConnectionStatus::Disconnected);

    let shown = screen(&mut app);
    let starts: Vec<usize> = shown
        .lines()
        .filter(|line| line.contains("UDP"))
        .map(|line| line.find("UDP").expect("the summary"))
        .collect();

    assert_eq!(starts.len(), 2, "{shown}");
    assert_eq!(starts[0], starts[1], "{shown}");
}

/// The frames a bench is watching, so a captured row has something to be read
/// as.
const HEARTBEAT: &str = r#"
name = "Heartbeat"
endian = "big"

[[field]]
name = "sync"
type = "u16"

[[field]]
name = "seq"
type = "u8"

[[field]]
name = "uptime"
type = "u16"
"#;

const STATUS: &str = r#"
name = "Status"
endian = "big"

[[field]]
name = "sync"
type = "u16"

[[field]]
name = "state"
type = "enum"
repr = "u8"
variants = { IDLE = 0, RUNNING = 2 }

[[field]]
name = "rpm"
type = "u16"
"#;

/// A folder of definitions, one per test.
///
/// Named, because the tests in a binary share a process and run at once: a
/// folder named after the process alone would have one test clearing what
/// another is reading.
fn with_frames(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("status.toml"), STATUS).expect("a frame file");
    std::fs::write(dir.join("heartbeat.toml"), HEARTBEAT).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

#[test]
fn a_row_is_read_field_by_field_once_it_is_picked() {
    let mut app = App::default();
    with_frames(&mut app, "a_row_is_read_field_by_field_once_it_is_picked");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    // Two definitions are five bytes, so which one is a question, not a guess.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut app);

    assert!(shown.contains("read as Status"), "{shown}");
    assert!(shown.contains("RUNNING"), "{shown}");
    assert!(shown.contains("1500"), "the rpm is decoded: {shown}");
    assert!(
        shown.contains("2..3"),
        "each field says where it sits: {shown}"
    );
}

#[test]
fn nothing_of_that_length_says_so_rather_than_guessing() {
    let mut app = App::default();
    with_frames(
        &mut app,
        "nothing_of_that_length_says_so_rather_than_guessing",
    );
    captured(&mut app, &[0x01, 0x02], Duration::from_secs(1));

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    assert!(shown.contains("No frame definition is 2 bytes"), "{shown}");
}

/// Reading a row and following the newest frame are opposite things.
#[test]
fn reading_a_row_stops_the_list_following() {
    let mut app = App::default();
    captured(&mut app, &[0x01], Duration::from_secs(1));
    assert!(app.monitor().expect("a view").follow);

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);

    assert!(!app.monitor().expect("a view").follow);
    assert!(app.monitor().expect("a view").selected.is_some());
}

#[test]
fn the_read_row_stays_on_screen_when_it_is_far_from_the_end() {
    let mut app = App::default();
    for n in 0..400u16 {
        captured(
            &mut app,
            &n.to_be_bytes(),
            Duration::from_millis(u64::from(n)),
        );
    }
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    for _ in 0..300 {
        press(&mut app, KeyCode::Up);
    }

    let shown = screen(&mut app);
    assert!(shown.contains("00 63"), "row 99 is in view: {shown}");
}

#[test]
fn escape_puts_the_fields_away() {
    let mut app = App::default();
    with_frames(&mut app, "escape_puts_the_fields_away");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app).contains("read as Status"));

    press(&mut app, KeyCode::Esc);
    assert!(!screen(&mut app).contains("read as Status"));
}

/// A key that only one view answers to is only offered there.
#[test]
fn the_hint_line_follows_the_view() {
    let mut app = App::default();
    let last = |shown: String| shown.lines().last().expect("a hint line").to_owned();

    assert!(!last(screen(&mut app)).contains("read a row"));

    press(&mut app, KeyCode::Char('2'));
    assert!(last(screen(&mut app)).contains("read a row"));
}

/// Not knowing how to leave a terminal program is how a session gets killed
/// from another window.
#[test]
fn the_way_out_is_offered_however_narrow_the_terminal() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('2'));

    for width in [80u16, 60, 40, 30] {
        let shown = narrow(&mut app, width);
        let last = shown.lines().last().expect("a hint line").to_owned();
        assert!(last.contains("q quit"), "at {width} columns: {last}");
    }
}

#[test]
fn a_row_several_definitions_could_be_asks_which() {
    let mut app = App::default();
    with_frames(&mut app, "a_row_several_definitions_could_be_asks_which");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);
    assert!(shown.contains("Pick the frame"), "{shown}");

    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut app);
    assert!(shown.contains("Read as"), "{shown}");
    assert!(shown.contains("Status"), "{shown}");
    assert!(shown.contains("Heartbeat"), "{shown}");
}

/// A folder with many frames in it is what typing is for.
#[test]
fn typing_narrows_the_list() {
    let mut app = App::default();
    with_frames(&mut app, "typing_narrows_the_list");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('h'));

    let shown = screen(&mut app);
    assert!(shown.contains("Heartbeat"), "{shown}");
    assert!(!shown.contains("Status"), "the other is gone: {shown}");
}

#[test]
fn a_list_backed_out_of_changes_nothing() {
    let mut app = App::default();
    with_frames(&mut app, "a_list_backed_out_of_changes_nothing");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);

    let shown = screen(&mut app);
    assert!(shown.contains("Pick the frame"), "{shown}");
    assert!(app.overlay().is_none());
}

/// The answer holds, so stepping down a run of the same message does not ask
/// again at every row.
#[test]
fn a_choice_survives_the_next_row_of_the_same_shape() {
    let mut app = App::default();
    with_frames(&mut app, "a_choice_survives_the_next_row_of_the_same_shape");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );
    captured(
        &mut app,
        &[0xAA, 0x55, 0x00, 0x00, 0x01],
        Duration::from_secs(2),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Up);

    // The row above is the same shape, so it is read without being asked about.
    let shown = screen(&mut app);
    assert!(shown.contains("read as Status"), "{shown}");
    assert!(!shown.contains("Pick the frame"), "{shown}");
    assert!(shown.contains("RUNNING"), "{shown}");
}

/// Whatever has the keyboard says what it answers to.
#[test]
fn the_hints_follow_the_list_that_is_open() {
    let mut app = App::default();
    with_frames(&mut app, "the_hints_follow_the_list_that_is_open");
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    let last = shown.lines().last().expect("a hint line").to_owned();
    assert!(last.contains("narrow"), "{last}");
    assert!(!last.contains("read a row"), "{last}");
}

const BRING_UP: &str = r#"
[[scenario]]
name = "Bring-up"
on = "drive"

[[scenario.step]]
send = "Status"

[[scenario.step]]
wait_ms = 100
"#;

fn with_scenarios(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-scn-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("bring-up.toml"), BRING_UP).expect("a scenario file");
    app.session_mut().scenarios.load_from(dir);
}

#[test]
fn a_scenario_says_what_it_does_before_it_is_run() {
    let mut app = App::default();
    with_scenarios(&mut app, "a_scenario_says_what_it_does_before_it_is_run");

    press(&mut app, KeyCode::Char('5'));
    let shown = screen(&mut app);

    assert!(shown.contains("Bring-up"), "{shown}");
    assert!(shown.contains("2 steps, once"), "{shown}");
}

#[test]
fn choosing_a_scenario_shows_its_steps() {
    let mut app = App::default();
    with_scenarios(&mut app, "choosing_a_scenario_shows_its_steps");

    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    assert!(shown.contains("send Status"), "{shown}");
    assert!(shown.contains("wait 100 ms"), "{shown}");
    assert!(
        shown.contains("drive"),
        "the link each step runs on: {shown}"
    );
}

#[test]
fn no_scenario_says_so_rather_than_showing_an_empty_box() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    assert!(
        screen(&mut app).contains("No scenario"),
        "{}",
        screen(&mut app)
    );
}

/// What a bench looks at while something is running.
#[test]
fn a_running_scenario_says_how_far_it_has_got() {
    let mut app = App::default();
    with_scenarios(&mut app, "a_running_scenario_says_how_far_it_has_got");
    app.session_mut().running.insert(
        "Bring-up".to_owned(),
        sim_session::state::ScenarioRun { step: 2, pass: 0 },
    );

    press(&mut app, KeyCode::Char('5'));
    let shown = screen(&mut app);

    assert!(shown.contains("step 2 pass 1"), "{shown}");
    assert!(
        !shown.contains("2 steps, once"),
        "the shape gives way: {shown}"
    );
}

#[test]
fn bytes_typed_by_hand_are_counted_before_they_are_sent() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);

    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    for letter in "AA55".chars() {
        press(&mut app, KeyCode::Char(letter));
    }

    let shown = screen(&mut app);
    assert!(shown.contains("2 byte(s) ready"), "{shown}");
    assert!(shown.contains("on bus"), "{shown}");
}

#[test]
fn a_half_typed_byte_says_what_is_wrong_with_it() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('A'));

    assert!(
        screen(&mut app).contains("Odd number"),
        "{}",
        screen(&mut app)
    );
}

/// A box being typed into swallows every letter, so the keys that move between
/// views are not the view's to take while one does.
#[test]
fn a_digit_typed_into_the_box_does_not_change_the_view() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('2'));

    assert_eq!(app.tab(), crate::app::Tab::HexInject);
    assert!(screen(&mut app).contains("1 byte") || screen(&mut app).contains("Odd"));
}

/// And promising a way out that types a letter instead would be a lie.
#[test]
fn the_offered_way_out_is_the_one_that_works() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));

    let last = |app: &mut App| screen(app).lines().last().expect("a hint line").to_owned();
    assert!(last(&mut app).contains("q quit"));

    press(&mut app, KeyCode::Enter);
    let hints = last(&mut app);
    assert!(!hints.contains("q quit"), "{hints}");
    assert!(hints.contains("Esc done"), "{hints}");
}

#[test]
fn escape_gives_the_keyboard_back_to_the_view() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    assert!(app.is_editing());

    press(&mut app, KeyCode::Esc);
    assert!(!app.is_editing());

    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.tab(), crate::app::Tab::Connections);
}

#[test]
fn a_frame_shows_its_fields_and_the_bytes_they_make() {
    let mut app = App::default();
    with_frames(&mut app, "a_frame_shows_its_fields_and_the_bytes_they_make");

    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    assert!(shown.contains("Heartbeat"), "{shown}");
    assert!(shown.contains("Status"), "{shown}");
    assert!(shown.contains("5 bytes"), "{shown}");
    assert!(
        shown.contains("sync"),
        "the fields of the chosen one: {shown}"
    );
    assert!(shown.contains("enum"), "and what kind each is: {shown}");
}

#[test]
fn no_frame_definition_says_so_rather_than_showing_an_empty_box() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    assert!(screen(&mut app).contains("No frame definition"));
}

/// What would go out, which is the answer the fields are working towards.
#[test]
fn a_frame_shows_the_bytes_it_would_send() {
    let mut app = App::default();
    with_frames(&mut app, "a_frame_shows_the_bytes_it_would_send");

    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    // Five bytes of zeroes until a value is typed, which is what a seeded
    // frame encodes to.
    assert!(shown.contains("00 00 00 00 00"), "{shown}");
}

const FLAGS: &str = r#"
name = "Flags"
endian = "big"

[[field]]
name = "state"
type = "bits"
repr = "u8"
bits = [
  { name = "armed",   width = 1 },
  { name = "link_up", width = 1 },
  { name = "fault",   width = 1 },
  { name = "spare",   width = 5 },
]
"#;

fn with_bits(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-bits-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("flags.toml"), FLAGS).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

/// A bench that cannot say what went wrong is a bench you debug from the logs.
#[test]
fn what_went_wrong_is_on_screen() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    for letter in "AA".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        screen(&mut app).contains("No link to send on"),
        "{}",
        screen(&mut app)
    );
}

/// And it gives way once it has been read, rather than outliving the thing it
/// complained about.
#[test]
fn a_complaint_lasts_until_the_next_key() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('A'));
    press(&mut app, KeyCode::Char('A'));
    press(&mut app, KeyCode::Enter);
    assert!(app.trouble().is_some());

    press(&mut app, KeyCode::Char('1'));
    assert!(app.trouble().is_none());
}

/// The packed word says nothing on its own. What a bitfield says is in its
/// flags.
#[test]
fn a_bitfield_shows_each_of_its_flags() {
    let mut app = App::default();
    with_bits(&mut app, "a_bitfield_shows_each_of_its_flags");
    captured(&mut app, &[0b1010_0000], Duration::from_secs(1));

    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    assert!(shown.contains("armed"), "{shown}");
    assert!(shown.contains("link_up"), "{shown}");
    assert!(shown.contains("fault"), "{shown}");
    assert!(
        shown.contains("4:0"),
        "each flag says where it sits: {shown}"
    );
    assert!(shown.contains("armed"), "and a set one stands out: {shown}");
}

#[test]
fn a_bitfield_shows_its_flags_in_the_frames_view_too() {
    let mut app = App::default();
    with_bits(
        &mut app,
        "a_bitfield_shows_its_flags_in_the_frames_view_too",
    );

    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down);
    let shown = screen(&mut app);

    assert!(shown.contains("armed"), "{shown}");
    assert!(shown.contains("fault"), "{shown}");
}

/// The list is what tells you which row you are on, so it keeps its half.
#[test]
fn a_long_definition_does_not_squeeze_the_list_away() {
    let mut app = App::default();
    let dir = std::env::temp_dir().join(format!("sim-tui-wide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    let fields = (0..20).fold(String::new(), |mut out, n| {
        use std::fmt::Write as _;
        let _ = write!(out, "\n[[field]]\nname = \"f{n}\"\ntype = \"u8\"\n");
        out
    });
    std::fs::write(
        dir.join("wide.toml"),
        format!("name = \"Wide\"\nendian = \"big\"\n{fields}"),
    )
    .expect("a frame file");
    app.session_mut().frames.load_from(dir);

    for n in 0..30u8 {
        captured(&mut app, &[n; 20], Duration::from_millis(u64::from(n)));
    }
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);

    let shown = screen(&mut app);
    let rows = shown.lines().filter(|line| line.contains(" RX ")).count();
    assert!(
        rows >= 8,
        "the list keeps its half, got {rows} rows:\n{shown}"
    );
}

/// The map is what you open to learn the keys the view answers to.
#[test]
fn the_key_map_lists_the_keys_of_the_view_behind_it() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('?'));

    let shown = screen(&mut app);
    assert!(shown.contains("read a row"), "{shown}");
    assert!(
        shown.contains("follow"),
        "the key nothing else documents: {shown}"
    );
}

/// A board reached over ssh has no desktop to put a file dialog on.
fn a_project_on_disk(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sim-tui-open-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("frames")).expect("a scratch folder");
    std::fs::write(root.join("frames").join("status.toml"), STATUS).expect("a frame file");
    std::fs::write(
        root.join("bench.toml"),
        "version = 1\nframes_dir = \"frames\"\n",
    )
    .expect("a project file");
    root
}

#[test]
fn a_project_can_be_opened_without_leaving_the_terminal() {
    let root = a_project_on_disk("a_project_can_be_opened_without_leaving_the_terminal");
    let mut app = App::opening(Some(root.join("elsewhere.toml")));

    press(&mut app, KeyCode::Char('o'));
    let shown = screen(&mut app);
    assert!(
        shown.contains("bench.toml"),
        "the folder is listed: {shown}"
    );
    assert!(shown.contains("frames/"), "folders are marked: {shown}");

    press(&mut app, KeyCode::Char('b'));
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.opened(), Some(root.join("bench.toml").as_path()));
    press(&mut app, KeyCode::Char('4'));
    assert!(
        screen(&mut app).contains("Status"),
        "its frames came with it"
    );
}

#[test]
fn a_folder_is_walked_into_rather_than_opened() {
    let root = a_project_on_disk("a_folder_is_walked_into_rather_than_opened");
    let mut app = App::opening(Some(root.join("elsewhere.toml")));

    press(&mut app, KeyCode::Char('o'));
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("status.toml"), "now inside it: {shown}");
    assert!(app.opened().is_none(), "and nothing was opened");
}

#[test]
fn the_walk_can_go_back_up() {
    let root = a_project_on_disk("the_walk_can_go_back_up");
    let mut app = App::opening(Some(root.join("frames").join("elsewhere.toml")));

    press(&mut app, KeyCode::Char('o'));
    assert!(screen(&mut app).contains("status.toml"));

    press(&mut app, KeyCode::Enter);
    assert!(
        screen(&mut app).contains("bench.toml"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn backing_out_of_the_walk_opens_nothing() {
    let root = a_project_on_disk("backing_out_of_the_walk_opens_nothing");
    let mut app = App::opening(Some(root.join("elsewhere.toml")));

    press(&mut app, KeyCode::Char('o'));
    press(&mut app, KeyCode::Esc);

    assert!(app.overlay().is_none());
    assert!(app.opened().is_none());
}

#[test]
fn a_udp_connection_can_be_added_without_leaving_the_terminal() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    for letter in "bus".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("bus"), "{shown}");
    assert!(shown.contains("UDP"), "{shown}");
    assert!(app.overlay().is_none(), "the form closed on success");
}

#[test]
fn a_name_is_required() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("required"), "{shown}");
    assert!(app.overlay().is_some(), "the form stays open to fix it");
}

#[test]
fn cycling_the_kind_changes_which_fields_are_asked_for() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Right);

    let shown = screen(&mut app);
    assert!(shown.contains("Address"), "{shown}");
    assert!(!shown.contains("Bind (local)"), "{shown}");
}

#[test]
fn a_serial_connection_asks_for_serial_settings() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab);
    for _ in 0..3 {
        press(&mut app, KeyCode::Right);
    }
    let shown = screen(&mut app);

    assert!(shown.contains("Baud rate"), "{shown}");
    assert!(shown.contains("Parity"), "{shown}");
}

#[test]
fn a_typed_multicast_address_asks_for_an_interface_instead_of_a_bind() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a')); // name
    press(&mut app, KeyCode::Tab); // kind, still udp
    press(&mut app, KeyCode::Tab); // remote, prefilled
    for _ in 0..20 {
        press(&mut app, KeyCode::Backspace);
    }
    for letter in "239.1.1.1:9000".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    let shown = screen(&mut app);

    assert!(shown.contains("Interface"), "{shown}");
    assert!(!shown.contains("Bind (local)"), "{shown}");
}

#[test]
fn cancelling_the_form_creates_nothing() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    for letter in "bus".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Esc);

    assert!(app.overlay().is_none());
    assert!(app.session().connections.is_empty());
}

#[test]
fn a_link_can_be_removed_once_it_is_down() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Disconnected);
    press(&mut app, KeyCode::Char('x'));

    assert!(app.session().connections.is_empty());
}

/// The window refuses this too: a link has to be told to stop before it can
/// be forgotten.
#[test]
fn a_running_link_cannot_be_removed_out_from_under_itself() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('x'));

    assert_eq!(app.session().connections.len(), 1);
}

#[test]
fn autoconnect_can_be_flipped_from_the_list() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Disconnected);
    assert!(!app.session().connections[0].1.autoconnect);

    press(&mut app, KeyCode::Char('a'));
    assert!(app.session().connections[0].1.autoconnect);
}

/// The whole point of the Frames view: a value typed by hand, sent as
/// something other than zero.
#[test]
fn a_scalar_field_can_be_edited_before_sending() {
    let mut app = App::default();
    with_frames(&mut app, "a_scalar_field_can_be_edited_before_sending");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down); // Status
    press(&mut app, KeyCode::Right); // into its fields
    press(&mut app, KeyCode::Enter); // edit sync
    for letter in "1500".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("1500"), "{shown}");
    assert!(
        shown.contains("05 DC 00 00 00"),
        "the bytes follow: {shown}"
    );
}

#[test]
fn an_enum_field_is_chosen_from_its_variants() {
    let mut app = App::default();
    with_frames(&mut app, "an_enum_field_is_chosen_from_its_variants");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down); // Status
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down); // state
    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut app);
    assert!(shown.contains("IDLE = 0"), "{shown}");
    assert!(shown.contains("RUNNING = 2"), "{shown}");

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("RUNNING (2)"), "{shown}");
    assert!(shown.contains("00 00 02 00 00"), "{shown}");
}

/// A flag that is only ever 0 or 1 flips on the spot: there is nothing to
/// type.
#[test]
fn a_single_bit_flag_toggles_without_an_editor() {
    let mut app = App::default();
    with_bits(&mut app, "a_single_bit_flag_toggles_without_an_editor");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down); // armed
    press(&mut app, KeyCode::Char(' '));

    let shown = screen(&mut app);
    assert!(app.overlay().is_none(), "no editor was needed");
    assert!(shown.contains("80"), "{shown}");
}

/// A flag wider than one bit is a number, and needs a box.
#[test]
fn a_wide_bit_field_is_typed_into_a_box() {
    let mut app = App::default();
    with_bits(&mut app, "a_wide_bit_field_is_typed_into_a_box");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    // Rows: the field, then armed, link_up, fault, spare.
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay().is_some(), "spare is wider than one bit");
    for letter in "9".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("09"), "{shown}");
}

#[test]
fn escape_leaves_a_field_edit_unapplied() {
    let mut app = App::default();
    with_frames(&mut app, "escape_leaves_a_field_edit_unapplied");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Enter);
    for letter in "1500".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Esc);

    let shown = screen(&mut app);
    assert!(!shown.contains("1500"), "{shown}");
    assert!(shown.contains("00 00 00 00 00"), "{shown}");
}

/// Left steps back out to the frame list, and up/down there chooses a
/// different frame rather than a different field.
#[test]
fn left_returns_focus_to_the_frame_list() {
    let mut app = App::default();
    with_frames(&mut app, "left_returns_focus_to_the_frame_list");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    assert!(app.frame_focus_is_fields());

    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Down);

    assert!(
        !app.frame_focus_is_fields(),
        "left handed the keyboard back"
    );
    let shown = screen(&mut app);
    assert!(
        shown.contains("Status"),
        "the second frame is now chosen: {shown}"
    );
}

/// Switching to a differently shaped frame does not leave the cursor on
/// whatever row happened to share its number.
#[test]
fn switching_frames_resets_the_field_cursor() {
    let mut app = App::default();
    with_frames(&mut app, "switching_frames_resets_the_field_cursor");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down); // seq, row 1 of Heartbeat
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Down); // Status
    press(&mut app, KeyCode::Right);
    // Row 1 of Status is "state", an enum. Landing there would open a
    // picker; the cursor resetting to row 0 opens a text box for "sync"
    // instead.
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(
        shown.contains("┌ sync"),
        "the box on sync, not state: {shown}"
    );
}

/// The one field a person does not write: it is worked out on send.
const GUARDED: &str = r#"
name = "Guarded"
endian = "big"

[[field]]
name = "id"
type = "u8"

[[field]]
name = "crc"
type = "xor8"
covers = { from = "id", to = "id" }
"#;

fn with_checksum(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-crc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("guarded.toml"), GUARDED).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

/// The one field a person does not write: it is worked out on send.
#[test]
fn a_checksum_field_cannot_be_edited() {
    let mut app = App::default();
    with_checksum(&mut app, "a_checksum_field_cannot_be_edited");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down); // crc
    press(&mut app, KeyCode::Enter);

    assert!(app.overlay().is_none(), "no box was opened");
    let shown = screen(&mut app);
    assert!(shown.contains("Computed automatically"), "{shown}");
}

/// Pressing x while a link is up used to do nothing, silently. It now says
/// why, instead of leaving the operator to wonder if the key even works.
#[test]
fn removing_a_link_that_is_up_says_why_it_did_not_go() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('x'));

    assert_eq!(app.session().connections.len(), 1, "nothing was removed");
    let shown = screen(&mut app);
    assert!(shown.contains("disconnect it first"), "{shown}");
}

#[test]
fn a_target_is_chosen_from_the_connected_links() {
    let mut app = App::default();
    with_frames(&mut app, "a_target_is_chosen_from_the_connected_links");
    linked(&mut app, "bus", ConnectionStatus::Connected);
    linked(&mut app, "spare", ConnectionStatus::Disconnected);
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('t'));

    let shown = screen(&mut app);
    assert!(shown.contains("bus"), "{shown}");
    assert!(
        !shown.contains("spare"),
        "only what is up is offered: {shown}"
    );

    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut app);
    assert!(shown.contains("Target: bus (connected)"), "{shown}");
}

#[test]
fn no_connected_link_says_so_instead_of_an_empty_list() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Disconnected);
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('t'));

    assert!(app.overlay().is_none(), "nothing to choose from");
    let shown = screen(&mut app);
    assert!(shown.contains("No connected link"), "{shown}");
}

/// Sending is what the whole view is for; it deserves to say so.
#[test]
fn sending_a_frame_says_so() {
    let mut app = App::default();
    with_frames(&mut app, "sending_a_frame_says_so");
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('s'));

    let shown = screen(&mut app);
    assert!(shown.contains("Sent 5 byte(s) to bus"), "{shown}");
}

/// The confirmation is not left standing once something else has happened.
#[test]
fn the_send_confirmation_gives_way_to_the_next_key() {
    let mut app = App::default();
    with_frames(&mut app, "the_send_confirmation_gives_way_to_the_next_key");
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('s'));
    assert!(app.status().is_some());

    press(&mut app, KeyCode::Down);
    assert!(app.status().is_none());
}

#[test]
fn sending_without_a_target_says_to_pick_one() {
    let mut app = App::default();
    with_frames(&mut app, "sending_without_a_target_says_to_pick_one");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('s'));

    let shown = screen(&mut app);
    assert!(shown.contains("Press t to pick one"), "{shown}");
}

#[test]
fn sending_to_a_target_that_dropped_is_refused() {
    let mut app = App::default();
    with_frames(&mut app, "sending_to_a_target_that_dropped_is_refused");
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Enter);

    // The link drops after being picked, without being un-picked.
    app.session_mut().connections[0].1.status = ConnectionStatus::Disconnected;
    press(&mut app, KeyCode::Char('s'));

    let shown = screen(&mut app);
    assert!(shown.contains("is not connected"), "{shown}");
}

/// The label comes from `sim_session`, the single place the GUI reads it
/// from too, so the two front ends cannot drift into calling the same
/// transport by two different names.
#[test]
fn the_kind_label_matches_the_one_shared_definition() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Right);

    let shown = screen(&mut app);
    assert!(shown.contains("TCP (client)"), "{shown}");
}
