//! The screenshots the documentation uses, drawn by the application itself.
//!
//! Photographing a window would go stale the day a button moved, and nothing
//! would say so. These are rendered from the real panels, with the real theme,
//! by `cargo test -p sim-gui --features shots shots`, so a picture in the
//! documentation is a picture of the code as it stands.
//!
//! Kept behind a feature because rendering pulls in a GPU backend that the
//! ordinary test run has no use for.

use egui::Theme;
use egui_kittest::Harness;

use std::time::SystemTime;

use sim_core::{ConnectionId, ConnectionStatus, TransportConfig};

use crate::engine_handle::EngineHandle;
use crate::state::{AppState, ConnectionEntry, Direction, LogEntry};
use crate::theme;

/// Where the documentation looks for them, from the workspace root rather than
/// from wherever the test happens to be run.
fn into() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/images")
}

/// The folder of example frames, the same one the documentation tells a reader
/// to open.
///
/// Named relative to the workspace root, which the panel then shows as
/// `examples/frames` rather than as whatever absolute path this machine
/// happens to use.
fn examples() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::set_current_dir(root).expect("the workspace root should be reachable");
    "examples/frames".to_owned()
}

struct World {
    state: AppState,
    engine: EngineHandle,
}

/// Renders one panel to `docs/images/<name>.png`.
///
/// `height` is given rather than measured: a panel asked for its natural size
/// would report the whole scroll area, and a screenshot of twenty empty rows
/// teaches nobody anything.
fn shoot(name: &str, size: (f32, f32), world: World, panel: impl Fn(&mut egui::Ui, &mut World)) {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size.0, size.1))
        .build_ui_state(
            |ui, world| {
                theme::apply(ui.ctx(), Theme::Light);
                theme::sync_row_height(ui);
                panel(ui, world);
            },
            world,
        );
    // Twice: the first pass installs the fonts, the second draws with them.
    harness.run();
    harness.run();

    let image = harness.render().expect("the panel should render");
    let into = into();
    std::fs::create_dir_all(&into).expect("the images folder should be writable");
    image
        .save(into.join(format!("{name}.png")))
        .expect("the image should be written");
}

/// Unfolds one field row, which is closed by default.
fn open_row(ctx: &egui::Context, index: usize) {
    let id = egui::Id::new(("frame_field", index));
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, false);
    state.set_open(true);
    state.store(ctx);
}

fn world_on(folder: &str) -> World {
    let mut state = AppState::default();
    state.frames.load_from(std::path::PathBuf::from(folder));
    World {
        state,
        engine: EngineHandle::default(),
    }
}

/// A world showing the named frame, which is what a reader is looking at.
fn showing(folder: &str, frame: &str) -> World {
    let mut world = world_on(folder);
    let at = world
        .state
        .frames
        .frames()
        .position(|held| held.name == frame);
    world.state.frames.selected = at;
    world
}

#[test]
fn frames_panel() {
    let world = showing(&examples(), "Telemetry");
    shoot("frames-panel", (760.0, 470.0), world, |ui, world| {
        super::frame_editor::show(ui, &mut world.state, &world.engine);
    });
}

#[test]
fn frame_editor() {
    let mut world = showing(&examples(), "Status");
    world.state.frames.begin_edit();
    shoot("frame-editor", (760.0, 430.0), world, |ui, world| {
        // The second row opened, since the interesting half of a field is what
        // its body holds: the bits, their widths and where they sit.
        open_row(ui.ctx(), 1);
        super::frame_editor::show(ui, &mut world.state, &world.engine);
    });
}

#[test]
fn shared_types() {
    let mut world = world_on(&examples());
    world.state.frames.type_selected = world
        .state
        .frames
        .type_entries
        .iter()
        .position(|entry| entry.definition.name() == "Header");
    world.state.frames.begin_type_edit();
    shoot("shared-type", (760.0, 310.0), world, |ui, world| {
        super::frame_editor::show(ui, &mut world.state, &world.engine);
    });
}

/// Two connections, one open and one still trying, which is what the panel is
/// for: seeing at a glance what is up.
#[test]
fn connections_panel() {
    let mut world = World {
        state: AppState::default(),
        engine: EngineHandle::default(),
    };
    world.state.connections = vec![
        (
            ConnectionId("uart".to_owned()),
            ConnectionEntry {
                config: TransportConfig::Serial {
                    port_name: "/dev/tty.usbserial-1420".to_owned(),
                    baud_rate: 115_200,
                    data_bits: tokio_serial::DataBits::Eight,
                    parity: tokio_serial::Parity::None,
                    stop_bits: tokio_serial::StopBits::One,
                    flow_control: tokio_serial::FlowControl::None,
                },
                status: ConnectionStatus::Connected,
                retry: None,
                autoconnect: true,
            },
        ),
        (
            ConnectionId("telemetry".to_owned()),
            ConnectionEntry {
                config: TransportConfig::Udp {
                    bind: "0.0.0.0:9000".parse().expect("a literal address"),
                    remote: "192.168.1.42:9001".parse().expect("a literal address"),
                },
                status: ConnectionStatus::Disconnected,
                retry: None,
                autoconnect: false,
            },
        ),
    ];
    shoot("connections", (760.0, 400.0), world, |ui, world| {
        super::connections::show(ui, &mut world.state, &world.engine);
    });
}

/// The scenarios the examples ship, with one of them opened.
#[test]
fn scenarios_panel() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::set_current_dir(root).expect("the workspace root should be reachable");
    let mut world = world_on("examples/frames");
    world
        .state
        .scenarios
        .load_from(std::path::PathBuf::from("examples/scenarios"));
    shoot("scenarios", (760.0, 415.0), world, |ui, world| {
        super::scenario_list::show(ui, &mut world.state, &world.engine);
    });
}

/// Traffic as it arrives, which is the panel a bench is watched through.
#[test]
fn traffic_monitor() {
    let mut world = World {
        state: AppState::default(),
        engine: EngineHandle::default(),
    };
    let id = ConnectionId("uart".to_owned());
    let sent = [
        (Direction::Sent, vec![0xAA, 0x55, 0x01, 0x00, 0x2E, 0x1C]),
        (
            Direction::Received,
            vec![0xAA, 0x55, 0x81, 0x03, 0x7B, 0x44],
        ),
        (Direction::Sent, vec![0xAA, 0x55, 0x02, 0x64, 0x11, 0x9F]),
        (
            Direction::Received,
            vec![0xAA, 0x55, 0x82, 0x00, 0xC3, 0x08],
        ),
    ];
    // Opened first: a monitor hides everything logged before it existed.
    let monitor = world.state.open_monitor();
    for (direction, bytes) in sent {
        world.state.push_log(LogEntry {
            seq: 0,
            id: id.clone(),
            direction,
            bytes,
            source: None,
            timestamp: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000),
        });
    }
    shoot("traffic", (760.0, 230.0), world, move |ui, world| {
        super::live_monitor::show(ui, &mut world.state, monitor);
    });
}

/// Bytes typed by hand, for the reply no definition describes yet.
#[test]
fn hex_inject() {
    let mut world = World {
        state: AppState::default(),
        engine: EngineHandle::default(),
    };
    let id = ConnectionId("uart".to_owned());
    world.state.connections = vec![(
        id.clone(),
        ConnectionEntry {
            config: TransportConfig::Udp {
                bind: "0.0.0.0:9000".parse().expect("a literal address"),
                remote: "192.168.1.42:9001".parse().expect("a literal address"),
            },
            status: ConnectionStatus::Connected,
            retry: None,
            autoconnect: false,
        },
    )];
    world.state.hex_target = Some(id);
    world.state.hex_input = "AA 55 01 00 2E 1C".to_owned();
    shoot("hex-inject", (400.0, 160.0), world, |ui, world| {
        super::hex_inject::show(ui, &mut world.state, &world.engine);
    });
}

/// The whole application, which is what the front page shows.
///
/// Driven through `eframe` rather than by drawing the dock alone, so the
/// toolbar, the status bar and the arrangement are the ones a first run gets
/// and not a reassembly of them.
#[test]
fn the_whole_thing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::set_current_dir(root).expect("the workspace root should be reachable");

    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_eframe(|cc| {
            // Set before the app is built, because it reads the context back to
            // decide which palette to install.
            cc.egui_ctx.set_theme(Theme::Light);
            let mut app = crate::app::SimApp::new(cc, None);
            app.state
                .frames
                .load_from(std::path::PathBuf::from("examples/frames"));
            let at = app
                .state
                .frames
                .frames()
                .position(|frame| frame.name == "Telemetry");
            app.state.frames.selected = at;
            app.state
                .scenarios
                .load_from(std::path::PathBuf::from("examples/scenarios"));

            let id = ConnectionId("drive".to_owned());
            app.state.connections = vec![(
                id.clone(),
                ConnectionEntry {
                    config: TransportConfig::Serial {
                        port_name: "/dev/tty.usbserial-1420".to_owned(),
                        baud_rate: 115_200,
                        data_bits: tokio_serial::DataBits::Eight,
                        parity: tokio_serial::Parity::None,
                        stop_bits: tokio_serial::StopBits::One,
                        flow_control: tokio_serial::FlowControl::None,
                    },
                    status: ConnectionStatus::Connected,
                    retry: None,
                    autoconnect: true,
                },
            )];
            for (direction, bytes) in [
                (Direction::Sent, vec![0xAA, 0x55, 0x01, 0x07, 0x6B]),
                (
                    Direction::Received,
                    vec![0xAA, 0x55, 0x11, 0x02, 0x00, 0x00, 0x3D],
                ),
                (Direction::Sent, vec![0xAA, 0x55, 0x01, 0x08, 0xE2]),
            ] {
                app.state.push_log(LogEntry {
                    seq: 0,
                    id: id.clone(),
                    direction,
                    bytes,
                    source: None,
                    timestamp: SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(1_760_000_000),
                });
            }
            app
        });
    // Set on the built context: eframe hands the app a theme of its own during
    // construction, so asking earlier is overruled.
    harness.ctx.set_theme(Theme::Light);
    // Fixed passes rather than running to a standstill: the app asks for a
    // repaint every hundred milliseconds, so it never reaches one.
    harness.run_steps(3);

    let image = harness.render().expect("the window should render");
    let into = into();
    std::fs::create_dir_all(&into).expect("the images folder should be writable");
    image
        .save(into.join("overview.png"))
        .expect("the image should be written");
}
