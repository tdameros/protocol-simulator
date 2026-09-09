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

#[test]
fn pausing_freezes_the_view_but_not_the_buffer() {
    let mut app = App::default();
    captured(&mut app, &[0x01], Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('p'));
    assert!(screen(&mut app).contains("paused"));

    captured(&mut app, &[0x02], Duration::from_secs(2));
    let shown = screen(&mut app);
    assert!(shown.contains("1 of 2 shown"), "{shown}");
    assert!(shown.contains(", following, paused)"), "{shown}");

    press(&mut app, KeyCode::Char('p'));
    let shown = screen(&mut app);
    assert!(!shown.contains("paused"), "{shown}");
    assert!(
        shown.contains("2 of 2 shown") && shown.contains(", following)"),
        "the buffered frame catches up: {shown}"
    );
}

#[test]
fn clearing_hides_what_was_captured_before_it() {
    let mut app = App::default();
    captured(&mut app, &[0x01], Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('c'));

    assert!(screen(&mut app).contains("Nothing captured yet"));
    captured(&mut app, &[0x02], Duration::from_secs(2));
    assert!(
        screen(&mut app).contains("1 of 2 shown"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn a_new_monitor_watches_the_same_buffer_on_its_own() {
    let mut app = App::default();
    captured(&mut app, &[0x01], Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));

    let shown = screen(&mut app);
    assert!(shown.contains("[2/2]"), "{shown}");
    assert!(
        shown.contains("Nothing captured yet"),
        "a fresh view starts empty: {shown}"
    );

    press(&mut app, KeyCode::Char('['));
    let shown = screen(&mut app);
    assert!(shown.contains("[1/2]"), "{shown}");
    assert!(
        !shown.contains("Nothing captured yet"),
        "the first view keeps its history: {shown}"
    );
}

#[test]
fn the_last_monitor_cannot_be_closed() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('x'));

    assert_eq!(app.session().monitors.len(), 1);
    assert!(
        screen(&mut app).contains("cannot be closed"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn closing_a_monitor_falls_back_to_another_one() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('x'));

    assert_eq!(app.session().monitors.len(), 1);
    let shown = screen(&mut app);
    assert!(
        !shown.contains("[1/2]") && !shown.contains("[2/2]"),
        "{shown}"
    );
}

#[test]
fn the_filter_narrows_what_is_shown() {
    let mut app = App::default();
    captured(&mut app, b"AT+", Duration::from_secs(1));
    captured(&mut app, b"OK", Duration::from_secs(2));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('/'));
    for _ in 0..5 {
        press(&mut app, KeyCode::Tab); // title, direction, hex, at-offset, source
    }
    for letter in "AT".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Esc);

    let shown = screen(&mut app);
    assert!(shown.contains("AT+"), "{shown}");
    assert!(!shown.contains("4F 4B"), "OK is filtered out: {shown}");
}

#[test]
fn the_tab_name_can_be_changed() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('/'));
    for _ in 0..20 {
        press(&mut app, KeyCode::Backspace);
    }
    for letter in "Uplink".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Esc);

    assert!(screen(&mut app).contains("Uplink"), "{}", screen(&mut app));
}

#[test]
fn a_connection_can_be_ticked_into_the_filter() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Tab); // off Title, onto the "bus" row
    let shown = screen(&mut app);
    assert!(shown.contains("bus"), "{shown}");

    press(&mut app, KeyCode::Char(' '));
    let shown = screen(&mut app);
    assert!(app
        .monitor()
        .expect("a view")
        .filter
        .connections
        .contains("bus"));
    assert!(shown.contains("yes"), "{shown}");
}

#[test]
fn sending_a_row_to_hex_switches_there_with_the_bytes() {
    let mut app = App::default();
    captured(&mut app, &[0xAA, 0x55], Duration::from_secs(1));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('h'));

    assert_eq!(app.tab(), crate::app::Tab::HexInject);
    assert_eq!(app.session().hex_input, "AA 55");
}

#[test]
fn opening_a_row_in_frames_decodes_it_into_the_chosen_definition() {
    let mut app = App::default();
    with_frames(
        &mut app,
        "opening_a_row_in_frames_decodes_it_into_the_chosen_definition",
    );
    captured(
        &mut app,
        &[0xAA, 0x55, 0x02, 0x05, 0xDC],
        Duration::from_secs(1),
    );
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down); // choose Status
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('F'));

    assert_eq!(app.tab(), crate::app::Tab::Frames);
    let shown = screen(&mut app);
    assert!(shown.contains("RUNNING"), "{shown}");
}

fn with_scenario_frames(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-scnfr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("status.toml"), STATUS).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

fn with_scenarios_on_disk(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-scn2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("bring-up.toml"), BRING_UP).expect("a scenario file");
    app.session_mut().scenarios.load_from(dir);
}

#[test]
fn new_starts_a_scenario_from_scratch() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));

    let shown = screen(&mut app);
    assert!(shown.contains("New scenario"), "{shown}");
    assert!(
        shown.contains("wait 100 ms"),
        "the one step it starts with: {shown}"
    );
}

#[test]
fn the_name_can_be_edited() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    for _ in 0..20 {
        press(&mut app, KeyCode::Backspace);
    }
    for letter in "Bring-up".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        screen(&mut app).contains("Bring-up"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn a_step_can_be_added_and_removed() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a'));
    assert!(screen(&mut app).contains("2."), "{}", screen(&mut app));

    for _ in 0..4 {
        press(&mut app, KeyCode::Down); // Name, Description, Repeat, step 1, step 2
    }
    press(&mut app, KeyCode::Char('x'));
    assert!(!screen(&mut app).contains("2."), "{}", screen(&mut app));
}

#[test]
fn a_step_can_be_reordered() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // onto step 1
    }
    press(&mut app, KeyCode::Char(']')); // moves step 1 down, past step 2

    let shown = screen(&mut app);
    let lines: Vec<&str> = shown.lines().filter(|l| l.contains("wait")).collect();
    assert_eq!(lines.len(), 2, "{shown}");
}

#[test]
fn repeat_reveals_its_own_two_fields_once_switched_on() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down); // Repeat row
    assert!(!screen(&mut app).contains("Every"));

    press(&mut app, KeyCode::Char(' '));
    let shown = screen(&mut app);
    assert!(shown.contains("Every"), "{shown}");
    assert!(shown.contains("Times"), "{shown}");
}

#[test]
fn cycling_the_kind_changes_the_step_body() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app).contains("Delay (ms)"));

    press(&mut app, KeyCode::Right); // Wait -> WaitFor
    assert!(
        screen(&mut app).contains("Wait for"),
        "{}",
        screen(&mut app)
    );

    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left); // WaitFor -> Send -> Raw
    assert!(screen(&mut app).contains("Bytes"), "{}", screen(&mut app));
}

#[test]
fn a_second_target_can_be_ticked_on() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    linked(&mut app, "spare", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait needs no connection; move to Raw first
    press(&mut app, KeyCode::Down); // onto the first target row
    press(&mut app, KeyCode::Down); // onto the second
    press(&mut app, KeyCode::Char(' ')); // tick spare on too

    let step = &app
        .session()
        .scenarios
        .draft
        .as_ref()
        .expect("a draft")
        .scenario
        .steps[0];
    assert_eq!(step.targets.len(), 2, "{:?}", step.targets);
}

/// A step aimed at nothing is a step the loader refuses, so the only
/// target left cannot be unticked.
#[test]
fn the_last_target_cannot_be_unticked() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait -> Raw, which needs a connection
    press(&mut app, KeyCode::Down); // onto the bus target row
    press(&mut app, KeyCode::Char(' ')); // try to untick the only target

    let step = &app
        .session()
        .scenarios
        .draft
        .as_ref()
        .expect("a draft")
        .scenario
        .steps[0];
    assert_eq!(step.targets.len(), 1, "{:?}", step.targets);
}

#[test]
fn sending_a_frame_shows_its_fields_to_override() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_scenario_frames(&mut app, "sending_a_frame_shows_its_fields_to_override");
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left); // Wait -> Raw -> Send

    press(&mut app, KeyCode::Down); // bus target
    press(&mut app, KeyCode::Down); // Frame row
    press(&mut app, KeyCode::Right); // choose Status, the only frame

    let shown = screen(&mut app);
    assert!(shown.contains("sync"), "{shown}");
    assert!(
        shown.contains("frame default"),
        "not overridden yet: {shown}"
    );
}

#[test]
fn an_overridden_field_can_be_given_a_value() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_scenario_frames(&mut app, "an_overridden_field_can_be_given_a_value");
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left); // Wait -> Raw -> Send
    press(&mut app, KeyCode::Down); // bus
    press(&mut app, KeyCode::Down); // Frame
    press(&mut app, KeyCode::Right); // Status
    press(&mut app, KeyCode::Down); // sync
    press(&mut app, KeyCode::Char(' ')); // override it
    for letter in "1500".chars() {
        press(&mut app, KeyCode::Char(letter));
    }

    let shown = screen(&mut app);
    assert!(shown.contains("1500"), "{shown}");
    press(&mut app, KeyCode::Esc);
    assert!(
        screen(&mut app).contains("send Status with sync"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn waiting_for_a_frame_only_offers_to_tick_its_fields() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_scenario_frames(
        &mut app,
        "waiting_for_a_frame_only_offers_to_tick_its_fields",
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait -> WaitFor, starts by frame

    // Rows so far: Kind, bus target, wait-by-frame toggle, Frame.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Right); // choose Status
    press(&mut app, KeyCode::Down); // sync
    press(&mut app, KeyCode::Char(' ')); // match it

    let shown = screen(&mut app);
    let sync_line = shown.lines().find(|l| l.contains("sync")).unwrap_or("");
    assert!(
        !sync_line.contains("any value"),
        "sync is now matched: {sync_line}"
    );
    let state_line = shown.lines().find(|l| l.contains("state")).unwrap_or("");
    assert!(
        state_line.contains("any value"),
        "state is untouched: {state_line}"
    );
}

#[test]
fn switching_wait_mode_replaces_the_body() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // WaitFor, by frame

    press(&mut app, KeyCode::Down); // bus target
    press(&mut app, KeyCode::Down); // Wait for (mode) row
    press(&mut app, KeyCode::Char(' ')); // switch to bytes

    let shown = screen(&mut app);
    assert!(shown.contains("Pattern"), "{shown}");
    // The tab bar always names the Frames view; only the step popup's own
    // "Frame" row is what switching away from frame mode should drop.
    assert!(!shown.contains("  Frame "), "{shown}");
}

#[test]
fn a_timeout_can_be_set_and_cleared() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // WaitFor, which starts with a timeout

    let shown = screen(&mut app);
    assert!(shown.contains("Timeout (ms)"), "{shown}");

    // Rows: Kind, bus target, wait-by-frame, Frame, Give up after.
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' ')); // clears it
    assert!(
        !screen(&mut app).contains("Timeout"),
        "{}",
        screen(&mut app)
    );

    press(&mut app, KeyCode::Char(' ')); // sets it again
    assert!(
        screen(&mut app).contains("Timeout (ms)"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn escape_closes_the_step_editor() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay().is_some());

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay().is_none());
    assert_eq!(app.tab(), crate::app::Tab::Scenarios);
}

#[test]
fn cancel_discards_the_whole_draft() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Esc);

    assert!(
        screen(&mut app).contains("No scenario"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn edit_opens_an_existing_scenario() {
    let mut app = App::default();
    with_scenarios_on_disk(&mut app, "edit_opens_an_existing_scenario");
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('e'));

    assert!(
        screen(&mut app).contains("Bring-up"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn a_running_scenario_cannot_be_edited_or_deleted() {
    let mut app = App::default();
    with_scenarios_on_disk(&mut app, "a_running_scenario_cannot_be_edited_or_deleted");
    app.session_mut().running.insert(
        "Bring-up".to_owned(),
        sim_session::state::ScenarioRun { step: 1, pass: 0 },
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('e'));

    assert!(app.session().scenarios.draft.is_none());
    assert!(
        screen(&mut app).contains("Stop it before editing it"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn delete_removes_a_scenario_from_disk() {
    let mut app = App::default();
    with_scenarios_on_disk(&mut app, "delete_removes_a_scenario_from_disk");
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('d'));

    assert!(app.session().scenarios.entries.is_empty());
}

fn scratch_project_dir(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sim-tui-save-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch folder");
    root
}

#[test]
fn save_as_writes_a_readable_project() {
    let root = scratch_project_dir("save_as_writes_a_readable_project");
    let mut app = App::opening(Some(root.join("nonexistent.toml")));
    press(&mut app, KeyCode::Char('W'));
    press(&mut app, KeyCode::Char('s'));
    for _ in 0..30 {
        press(&mut app, KeyCode::Backspace);
    }
    for letter in "bench.toml".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    press(&mut app, KeyCode::Enter);

    let path = root.join("bench.toml");
    assert!(path.exists(), "{}", screen(&mut app));
    let read = sim_session::project::Project::read(&path).expect("should read back");
    assert_eq!(read.version, 1);
    assert_eq!(app.opened(), Some(path.as_path()));
}

#[test]
fn saving_confirms_and_clears_the_dirty_marker() {
    let root = scratch_project_dir("saving_confirms_and_clears_the_dirty_marker");
    let mut app = App::opening(Some(root.join("nonexistent.toml")));
    press(&mut app, KeyCode::Char('W'));
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("Saved to"), "{shown}");
    assert!(!shown.contains("simulator.toml *"), "{shown}");
}

#[test]
fn w_saves_straight_back_once_a_path_is_known() {
    let root = scratch_project_dir("w_saves_straight_back_once_a_path_is_known");
    let mut app = App::opening(Some(root.join("nonexistent.toml")));
    press(&mut app, KeyCode::Char('W'));
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay().is_none());

    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('w'));

    assert!(!app.is_dirty());
    let read =
        sim_session::project::Project::read(&root.join("simulator.toml")).expect("should read");
    assert_eq!(read.connections.len(), 1);
}

#[test]
fn a_pane_layout_from_the_window_survives_a_save_from_here() {
    let root = scratch_project_dir("a_pane_layout_from_the_window_survives_a_save_from_here");
    let path = root.join("bench.toml");
    std::fs::write(
        &path,
        "version = 1\n\n[ui]\ntheme = \"dark\"\n\n[ui.layout]\nwhatever = \"a window wrote this\"\n",
    )
    .expect("a project file");

    let mut app = App::opening(Some(path.clone()));
    press(&mut app, KeyCode::Char('w'));

    let written = std::fs::read_to_string(&path).expect("should still be there");
    assert!(written.contains("a window wrote this"), "{written}");
    assert!(written.contains("theme = \"dark\""), "{written}");
}

#[test]
fn nothing_to_save_leaves_no_mark() {
    let root = scratch_project_dir("nothing_to_save_leaves_no_mark");
    let path = root.join("bench.toml");
    std::fs::write(&path, "version = 1\n").expect("a project file");
    let app = App::opening(Some(path));

    assert!(!app.is_dirty());
}

#[test]
fn a_change_after_opening_shows_the_dirty_mark() {
    let root = scratch_project_dir("a_change_after_opening_shows_the_dirty_mark");
    let path = root.join("bench.toml");
    std::fs::write(&path, "version = 1\n").expect("a project file");
    let mut app = App::opening(Some(path));

    linked(&mut app, "bus", ConnectionStatus::Connected);
    assert!(app.is_dirty());
    let shown = screen(&mut app);
    assert!(shown.contains("bench.toml *"), "{shown}");
}

/// A session that has never been touched is not "unsaved work".
#[test]
fn a_fresh_app_with_nothing_opened_is_not_dirty() {
    let app = App::default();
    assert!(!app.is_dirty());
}

fn type_text(app: &mut App, text: &str) {
    for letter in text.chars() {
        press(app, KeyCode::Char(letter));
    }
}

fn clear_field(app: &mut App) {
    for _ in 0..40 {
        press(app, KeyCode::Backspace);
    }
}

#[test]
fn new_starts_a_frame_from_scratch() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));

    let shown = screen(&mut app);
    assert!(shown.contains("New frame"), "{shown}");
    assert!(shown.contains("id"), "the field it starts with: {shown}");
}

#[test]
fn the_frame_name_can_be_edited() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    clear_field(&mut app);
    type_text(&mut app, "Heartbeat");
    press(&mut app, KeyCode::Enter);

    assert!(
        screen(&mut app).contains("Heartbeat"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn a_field_can_be_added_and_removed() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a'));
    assert!(screen(&mut app).contains("2."), "{}", screen(&mut app));

    press(&mut app, KeyCode::Down); // Endian
    press(&mut app, KeyCode::Down); // field 1
    press(&mut app, KeyCode::Down); // field 2
    press(&mut app, KeyCode::Char('x'));
    assert!(!screen(&mut app).contains("2."), "{}", screen(&mut app));
}

#[test]
fn a_field_can_be_reordered() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a')); // adds "field", cursor lands on it
    press(&mut app, KeyCode::Char('[')); // moves it up, before "id"

    let shown = screen(&mut app);
    let id_at = shown.find("id").expect("the id field: {shown}");
    let field_at = shown.find("field").expect("the added field: {shown}");
    assert!(field_at < id_at, "{shown}");
}

#[test]
fn endian_can_be_toggled() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Down); // Endian row

    assert!(screen(&mut app).contains("little"), "{}", screen(&mut app));
    press(&mut app, KeyCode::Right);
    assert!(screen(&mut app).contains("big"), "{}", screen(&mut app));
    press(&mut app, KeyCode::Left);
    assert!(screen(&mut app).contains("little"), "{}", screen(&mut app));
}

#[test]
fn cancel_discards_the_frame_draft() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Esc);

    assert!(app.session().frames.draft.is_none());
}

#[test]
fn edit_opens_an_existing_frame() {
    let mut app = App::default();
    with_frames(&mut app, "edit_opens_an_existing_frame");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('e'));

    assert!(
        screen(&mut app).contains("Heartbeat"),
        "the first frame alphabetically, selected by default: {}",
        screen(&mut app)
    );
}

#[test]
fn delete_removes_a_frame_from_disk() {
    let mut app = App::default();
    with_frames(&mut app, "delete_removes_a_frame_from_disk");
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('d'));

    assert!(!app
        .session()
        .frames
        .entries
        .iter()
        .any(|entry| entry.frame.name == "Heartbeat"));
}

/// Opens a fresh frame draft and moves the cursor onto its one field.
fn on_the_one_field(app: &mut App) {
    press(app, KeyCode::Char('4'));
    press(app, KeyCode::Char('n'));
    press(app, KeyCode::Down); // Endian
    press(app, KeyCode::Down); // the field
}

#[test]
fn a_field_opens_its_own_editor() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);

    let shown = screen(&mut app);
    assert!(shown.contains("Kind"), "{shown}");
    assert!(shown.contains("u8"), "{shown}");
}

#[test]
fn the_field_name_can_be_edited() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter); // Name row's own editor
    clear_field(&mut app);
    type_text(&mut app, "sequence");
    press(&mut app, KeyCode::Enter);

    assert!(
        screen(&mut app).contains("sequence"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn cycling_the_kind_changes_what_the_editor_asks_for() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row

    // u8 -> u16 -> ... eventually reaches bytes.
    for _ in 0..12 {
        press(&mut app, KeyCode::Right);
        if screen(&mut app).contains("Length") {
            break;
        }
    }
    assert!(screen(&mut app).contains("Length"), "{}", screen(&mut app));
}

fn cycle_kind_to(app: &mut App, label: &str) {
    for _ in 0..40 {
        if screen(app).contains(label) {
            return;
        }
        press(app, KeyCode::Right);
    }
    panic!("never reached {label}: {}", screen(app));
}

#[test]
fn a_bytes_field_length_can_be_edited() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "bytes");

    press(&mut app, KeyCode::Down); // Length row
    press(&mut app, KeyCode::Enter);
    clear_field(&mut app);
    type_text(&mut app, "6");
    press(&mut app, KeyCode::Enter);

    assert!(screen(&mut app).contains('6'), "{}", screen(&mut app));
}

#[test]
fn an_enum_variant_can_be_added_edited_and_removed() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "enum");

    press(&mut app, KeyCode::Down); // Repr
    press(&mut app, KeyCode::Down); // Variant 0
    press(&mut app, KeyCode::Char('a'));
    assert!(
        screen(&mut app).contains("Variant 1"),
        "{}",
        screen(&mut app)
    );

    press(&mut app, KeyCode::Enter);
    clear_field(&mut app);
    type_text(&mut app, "ON = 1");
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app).contains("ON = 1"), "{}", screen(&mut app));

    press(&mut app, KeyCode::Char('x'));
    assert!(
        !screen(&mut app).contains("Variant 1"),
        "{}",
        screen(&mut app)
    );
}

#[test]
fn the_last_enum_variant_cannot_be_removed() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "enum");
    press(&mut app, KeyCode::Down); // Repr
    press(&mut app, KeyCode::Down); // Variant 0

    press(&mut app, KeyCode::Char('x'));
    assert!(
        screen(&mut app).contains("Variant 0"),
        "the only variant stays: {}",
        screen(&mut app)
    );
}

#[test]
fn a_bit_can_be_added_edited_and_removed() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "bits");

    press(&mut app, KeyCode::Down); // Repr
    press(&mut app, KeyCode::Down); // Bit 0
    press(&mut app, KeyCode::Char('a'));
    assert!(screen(&mut app).contains("Bit 1"), "{}", screen(&mut app));

    press(&mut app, KeyCode::Enter);
    clear_field(&mut app);
    type_text(&mut app, "ready 2");
    press(&mut app, KeyCode::Enter);
    assert!(
        screen(&mut app).contains("ready (2)"),
        "{}",
        screen(&mut app)
    );

    press(&mut app, KeyCode::Char('x'));
    assert!(!screen(&mut app).contains("Bit 1"), "{}", screen(&mut app));
}

/// A repr used to cycle through every scalar, including signed and floating
/// point ones a bitfield or an enum can never actually hold.
#[test]
fn a_bitfields_repr_only_ever_cycles_through_unsigned_widths() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "bits");
    press(&mut app, KeyCode::Down); // Repr row

    for _ in 0..8 {
        press(&mut app, KeyCode::Right);
        let shown = screen(&mut app);
        assert!(
            ["u8", "u16", "u32", "u64"]
                .iter()
                .any(|repr| shown.contains(repr)),
            "{shown}"
        );
    }
}

#[test]
fn the_last_bit_cannot_be_removed() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "bits");
    press(&mut app, KeyCode::Down); // Repr
    press(&mut app, KeyCode::Down); // Bit 0

    press(&mut app, KeyCode::Char('x'));
    assert!(
        screen(&mut app).contains("Bit 0"),
        "the only bit stays: {}",
        screen(&mut app)
    );
}

#[test]
fn a_checksum_covers_from_and_to_can_be_cycled() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a')); // a second field
    press(&mut app, KeyCode::Down); // Endian
    press(&mut app, KeyCode::Down); // id
    press(&mut app, KeyCode::Down); // field
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "xor8");

    press(&mut app, KeyCode::Down); // Covers from
    let before = screen(&mut app);
    press(&mut app, KeyCode::Right);
    let after = screen(&mut app);
    assert_ne!(before, after, "cycling covers-from changes what is shown");
}

#[test]
fn a_checksum_field_starts_with_no_length_row() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Down); // Endian
    press(&mut app, KeyCode::Down); // id
    press(&mut app, KeyCode::Down); // field
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Kind row
    cycle_kind_to(&mut app, "xor8");

    assert!(!screen(&mut app).contains("Length"), "{}", screen(&mut app));
}

#[test]
fn escape_closes_the_frame_field_editor() {
    let mut app = App::default();
    on_the_one_field(&mut app);
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay().is_some());
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay().is_none());
}

#[test]
fn saving_the_frame_writes_it_to_disk_and_closes_the_draft() {
    let mut app = App::default();
    let dir = std::env::temp_dir().join(format!("sim-tui-frame-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    app.session_mut().frames.load_from(dir.clone());

    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('s'));

    assert!(app.session().frames.draft.is_none());
    let written = std::fs::read_dir(&dir).expect("the scratch folder").count();
    assert_eq!(written, 1, "the new frame's own file");
}

#[test]
fn a_mismatched_row_leaves_its_own_note_rather_than_the_error_line() {
    let mut app = App::default();
    with_frames(
        &mut app,
        "a_mismatched_row_leaves_its_own_note_rather_than_the_error_line",
    );
    captured(&mut app, &[0xAA, 0x55, 0x02], Duration::from_secs(1));
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Down); // choose Status
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('F'));

    assert_eq!(
        app.trouble(),
        None,
        "a decode note is not a standing error, so it must not evict one already on screen"
    );
    assert!(
        app.session()
            .frame_hex_note
            .as_deref()
            .is_some_and(|note| note.contains("bytes typed")),
        "{:?}",
        app.session().frame_hex_note
    );
}

const NESTED: &str = r#"
name = "Nested"
endian = "little"

[[field]]
name = "zone.left"
type = "u16"

[[field]]
name = "zone.right"
type = "u16"
"#;

fn with_nested_frame(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-nested-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("nested.toml"), NESTED).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

#[test]
fn a_dotted_field_name_shows_only_its_own_leaf() {
    let mut app = App::default();
    with_nested_frame(&mut app, "a_dotted_field_name_shows_only_its_own_leaf");
    press(&mut app, KeyCode::Char('4'));

    let shown = screen(&mut app);
    assert!(shown.contains("left"), "{shown}");
    assert!(!shown.contains("zone.left"), "{shown}");
}

#[test]
fn a_little_endian_multi_byte_field_says_so_next_to_its_type() {
    let mut app = App::default();
    with_nested_frame(
        &mut app,
        "a_little_endian_multi_byte_field_says_so_next_to_its_type",
    );
    press(&mut app, KeyCode::Char('4'));

    assert!(screen(&mut app).contains("u16 le"), "{}", screen(&mut app));
}

fn with_many_fields(app: &mut App, name: &str, count: u32) {
    use std::fmt::Write as _;
    let dir = std::env::temp_dir().join(format!("sim-tui-many-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    let mut text = "name = \"Wide\"\nendian = \"big\"\n".to_owned();
    for at in 0..count {
        let _ = write!(text, "\n[[field]]\nname = \"f{at}\"\ntype = \"u8\"\n");
    }
    std::fs::write(dir.join("wide.toml"), text).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

/// A frame with more fields than the pane's height used to leave the ones
/// past the fold unreachable: the list never scrolled to follow the cursor.
#[test]
fn a_long_field_list_scrolls_to_keep_the_cursor_on_screen() {
    let mut app = App::default();
    with_many_fields(
        &mut app,
        "a_long_field_list_scrolls_to_keep_the_cursor_on_screen",
        20,
    );
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Right);
    assert!(screen(&mut app).contains("f0"), "{}", screen(&mut app));

    for _ in 0..19 {
        press(&mut app, KeyCode::Down);
    }

    let shown = screen(&mut app);
    assert!(
        shown.contains("f19"),
        "the last field came into view: {shown}"
    );
    assert!(
        !shown.contains("f0 "),
        "and the first scrolled out to make room: {shown}"
    );
    assert!(
        shown.contains("Target:"),
        "the summary below the list is not pushed off screen: {shown}"
    );
}

/// The frame structure editor's own field list has the same shape, and the
/// same bug to not have.
#[test]
fn a_long_frame_structure_scrolls_to_keep_the_cursor_on_screen() {
    let mut app = App::default();
    with_many_fields(
        &mut app,
        "a_long_frame_structure_scrolls_to_keep_the_cursor_on_screen",
        40,
    );
    press(&mut app, KeyCode::Char('4'));
    press(&mut app, KeyCode::Char('e'));
    assert!(screen(&mut app).contains("f0"), "{}", screen(&mut app));

    for _ in 0..39 {
        press(&mut app, KeyCode::Down);
    }

    let shown = screen(&mut app);
    assert!(
        shown.contains("f39"),
        "the last field came into view: {shown}"
    );
    assert!(
        !shown.contains("f0 "),
        "and the first scrolled out to make room: {shown}"
    );
}

/// The same shape of bug as the frame views: a step overriding a frame with
/// more fields than the terminal is tall left the ones past the fold both
/// unreadable and unreachable, since the popup grew to fit every row instead
/// of scrolling.
#[test]
fn a_step_overriding_a_wide_frame_scrolls_to_keep_the_focus_on_screen() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_many_fields(
        &mut app,
        "a_step_overriding_a_wide_frame_scrolls_to_keep_the_focus_on_screen",
        40,
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left); // Wait -> Raw -> Send

    press(&mut app, KeyCode::Down); // bus target
    press(&mut app, KeyCode::Down); // Frame row
    press(&mut app, KeyCode::Right); // choose Wide, the only frame
    assert!(screen(&mut app).contains("f0"), "{}", screen(&mut app));

    for _ in 0..39 {
        press(&mut app, KeyCode::Down);
    }

    let shown = screen(&mut app);
    assert!(
        shown.contains("f39"),
        "the last field came into focus: {shown}"
    );
    assert!(
        !shown.contains("f0 "),
        "and the first scrolled out to make room: {shown}"
    );
}

const RESPONSE: &str = r#"
name = "Response"
[[field]]
name = "code"
type = "u8"
"#;

const FORWARD: &str = r#"
name = "Forward"
[[field]]
name = "payload"
type = "u8"
"#;

fn with_relay_frames(app: &mut App, name: &str) {
    let dir = std::env::temp_dir().join(format!("sim-tui-relay-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    std::fs::write(dir.join("response.toml"), RESPONSE).expect("a frame file");
    std::fs::write(dir.join("forward.toml"), FORWARD).expect("a frame file");
    app.session_mut().frames.load_from(dir);
}

/// Nothing offered to fill from until something has captured a variable, and
/// once something has, the picker starts on the one variable in scope.
#[test]
fn capturing_a_replys_field_lets_a_later_send_fill_from_it() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_relay_frames(
        &mut app,
        "capturing_a_replys_field_lets_a_later_send_fill_from_it",
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('a'));
    // cursor already sits on the new (second) step

    // Step 2: a Send of Forward. Nothing captures yet, so "c" does nothing.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Left); // Raw -> Send
    press(&mut app, KeyCode::Down); // bus target
    press(&mut app, KeyCode::Down); // Frame row
    press(&mut app, KeyCode::Right); // Response
    press(&mut app, KeyCode::Right); // Forward
    press(&mut app, KeyCode::Down); // payload row
    press(&mut app, KeyCode::Char('c'));
    assert!(
        screen(&mut app).contains("frame default"),
        "nothing to capture from yet: {}",
        screen(&mut app)
    );
    press(&mut app, KeyCode::Esc);

    // Step 1: a WaitFor of Response, capturing "code".
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait -> WaitFor
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, wait-by-frame, Frame
    }
    press(&mut app, KeyCode::Right); // Response
    press(&mut app, KeyCode::Down); // code row
    press(&mut app, KeyCode::Right); // capture it
    let shown = screen(&mut app);
    assert!(
        shown.contains("capture as code"),
        "captured under its own name to start with: {shown}"
    );

    for _ in 0.."code".len() {
        press(&mut app, KeyCode::Backspace);
    }
    for letter in "server1_code".chars() {
        press(&mut app, KeyCode::Char(letter));
    }
    assert!(
        screen(&mut app).contains("capture as server1_code"),
        "{}",
        screen(&mut app)
    );
    press(&mut app, KeyCode::Esc);

    // Step 2 again: "c" on payload now has a variable to offer.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, Frame, payload
    }
    press(&mut app, KeyCode::Char('c'));
    assert!(
        screen(&mut app).contains("from capture: server1_code"),
        "the one variable in scope is picked automatically: {}",
        screen(&mut app)
    );
}

/// With more than one variable in scope, left and right cycle between them
/// rather than only ever offering the first.
#[test]
fn cycling_a_captured_field_picks_a_different_variable() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_relay_frames(
        &mut app,
        "cycling_a_captured_field_picks_a_different_variable",
    );
    app.session_mut()
        .scenarios
        .begin_new(sim_session::scenarios::blank());
    {
        let draft = app.session_mut().scenarios.draft.as_mut().unwrap();
        let target = sim_core::ConnectionId::from("bus");
        draft.scenario.steps = vec![
            sim_core::scenario::Step {
                targets: vec![target.clone()],
                action: sim_core::scenario::Action::WaitFor {
                    expect: sim_core::scenario::Expect::Frame {
                        frame: "Response".to_owned(),
                        values: std::collections::BTreeMap::default(),
                        capture: std::collections::BTreeMap::from([(
                            "code".to_owned(),
                            "first".to_owned(),
                        )]),
                    },
                    timeout: None,
                },
            },
            sim_core::scenario::Step {
                targets: vec![target.clone()],
                action: sim_core::scenario::Action::WaitFor {
                    expect: sim_core::scenario::Expect::Frame {
                        frame: "Response".to_owned(),
                        values: std::collections::BTreeMap::default(),
                        capture: std::collections::BTreeMap::from([(
                            "code".to_owned(),
                            "second".to_owned(),
                        )]),
                    },
                    timeout: None,
                },
            },
            sim_core::scenario::Step {
                targets: vec![target],
                action: sim_core::scenario::Action::Send {
                    frame: "Forward".to_owned(),
                    with: std::collections::BTreeMap::default(),
                    counters: std::collections::BTreeMap::default(),
                    from_capture: std::collections::BTreeMap::default(),
                },
            },
        ];
    }
    press(&mut app, KeyCode::Char('5'));
    for _ in 0..5 {
        press(&mut app, KeyCode::Down); // Name, Description, Repeat, step1, step2, step3
    }
    press(&mut app, KeyCode::Enter);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, Frame, payload
    }
    press(&mut app, KeyCode::Char('c'));
    let first = screen(&mut app);
    assert!(
        first.contains("from capture: first"),
        "the first variable in scope: {first}"
    );

    press(&mut app, KeyCode::Right);
    let second = screen(&mut app);
    assert!(
        second.contains("from capture: second"),
        "cycled to the other one: {second}"
    );
}

/// A step moved or removed can leave a `from_capture` pointing at a variable
/// nothing captures any more. "c" still has to turn that off, even though
/// there is nothing left to turn it on to.
#[test]
fn a_dangling_capture_can_still_be_turned_off_with_c() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_relay_frames(
        &mut app,
        "a_dangling_capture_can_still_be_turned_off_with_c",
    );
    app.session_mut()
        .scenarios
        .begin_new(sim_session::scenarios::blank());
    {
        let draft = app.session_mut().scenarios.draft.as_mut().unwrap();
        draft.scenario.steps = vec![sim_core::scenario::Step {
            targets: vec![sim_core::ConnectionId::from("bus")],
            action: sim_core::scenario::Action::Send {
                frame: "Forward".to_owned(),
                with: std::collections::BTreeMap::new(),
                counters: std::collections::BTreeMap::new(),
                from_capture: std::collections::BTreeMap::from([(
                    "payload".to_owned(),
                    "gone".to_owned(),
                )]),
            },
        }];
    }
    press(&mut app, KeyCode::Char('5'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // Name, Description, Repeat, step1
    }
    press(&mut app, KeyCode::Enter);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, Frame, payload
    }
    let before = screen(&mut app);
    assert!(
        before.contains("from capture: gone"),
        "the dangling reference is still shown: {before}"
    );

    press(&mut app, KeyCode::Char('c'));
    let after = screen(&mut app);
    assert!(
        after.contains("frame default"),
        "c still turns it off with nothing left to turn it on to: {after}"
    );
}

/// Turning a capture on seeds its variable name from the field, and typing
/// straight away has to continue from that name rather than from nothing:
/// the screen already says "capture as code" before the first keystroke.
#[test]
fn typing_right_after_turning_a_capture_on_continues_its_seeded_name() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_relay_frames(
        &mut app,
        "typing_right_after_turning_a_capture_on_continues_its_seeded_name",
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // Name, Description, Repeat, step1
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait -> WaitFor
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, wait-by-frame, Frame
    }
    press(&mut app, KeyCode::Right); // Response
    press(&mut app, KeyCode::Down); // code row
    press(&mut app, KeyCode::Right); // capture it, seeded "code"

    press(&mut app, KeyCode::Char('!'));
    let shown = screen(&mut app);
    assert!(
        shown.contains("capture as code!"),
        "the seeded name, not an empty buffer, is what the key extends: {shown}"
    );
}

/// The hint line used to advertise "c" for capturing a field, but the wait
/// side of a step popup does that with left/right instead, "c" doing nothing
/// there and something different on the send side.
#[test]
fn the_step_hint_does_not_promise_a_key_the_wait_side_does_not_answer_to() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);

    let hints = screen(&mut app)
        .lines()
        .last()
        .expect("a hint line")
        .to_owned();
    assert!(!hints.contains("c capture"), "{hints}");
    assert!(hints.contains("left/right"), "{hints}");
}

/// Ticking a matched field seeds the frame's own default, but a wait for an
/// exact number needs more than that default: typing has to reach it too,
/// the same way it already reaches a send step's overridden value.
#[test]
fn a_matched_field_can_be_given_an_exact_value_to_wait_for() {
    let mut app = App::default();
    linked(&mut app, "bus", ConnectionStatus::Connected);
    with_scenario_frames(
        &mut app,
        "a_matched_field_can_be_given_an_exact_value_to_wait_for",
    );
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Right); // Wait -> WaitFor, starts by frame
    for _ in 0..3 {
        press(&mut app, KeyCode::Down); // bus, wait-by-frame, Frame
    }
    press(&mut app, KeyCode::Right); // choose Status
    press(&mut app, KeyCode::Down); // sync
    press(&mut app, KeyCode::Char(' ')); // match it, seeded from the default

    for letter in "56".chars() {
        press(&mut app, KeyCode::Char(letter));
    }

    let shown = screen(&mut app);
    let sync_line = shown.lines().rfind(|l| l.contains("sync")).unwrap_or("");
    assert!(
        sync_line.contains("56"),
        "the typed value, not the seeded default: {shown}"
    );
}
