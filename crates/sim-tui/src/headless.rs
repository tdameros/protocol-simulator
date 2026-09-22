//! Running one scenario with nothing drawn, for a boot-time service.
//!
//! The same project loading and the same [`EngineHandle`] the interactive
//! front end uses, driven from a plain poll loop instead of a redraw: nothing
//! here needs a terminal, so `--run` costs the rest of the binary nothing to
//! carry.
//!
//! No signal handling: `SIGTERM`/`SIGINT` end the process the way they end
//! any other, which for a scenario with no state of its own to flush is
//! enough. A scenario meant to run until stopped (no `times`) is stopped that
//! way, by whatever manages the service.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sim_core::{Event, Outcome};
use sim_session::engine_handle::EngineHandle;
use sim_session::project::Project;
use sim_session::scenarios;
use sim_session::state::Session;
use sim_session::{hex, links};

use crate::control::{self, LastReceived};

const POLL: Duration = Duration::from_millis(20);

/// Loads `opened_with`, starts the scenario named `scenario_name`, and prints
/// what happens until it ends. The exit code a shell or a service manager
/// reads: 0 once the scenario completed, 1 for anything else.
///
/// `control_port`, when given, opens a local TCP socket an external script
/// can use to override a `send` step's field live and read back the last
/// frame received on a connection, without restarting the scenario.
pub fn run(opened_with: Option<PathBuf>, scenario_name: &str, control_port: Option<u16>) -> i32 {
    let Some(path) = opened_with else {
        eprintln!("--run needs a project file: protocol-simulator-tui project.toml --run \"Name\"");
        return 1;
    };

    let mut session = Session::default();
    let restored = Project::read(&path).and_then(|read| read.apply(&mut session, Some(&path)));
    let restored = match restored {
        Ok(restored) => restored,
        Err(error) => {
            eprintln!("{}: {error:#}", path.display());
            return 1;
        }
    };

    let Some(entry) = session
        .scenarios
        .entries
        .iter()
        .find(|entry| entry.scenario.name == scenario_name)
    else {
        eprintln!(
            "no scenario named \"{scenario_name}\" in {}",
            path.display()
        );
        return 1;
    };
    let scenario = entry.scenario.clone();

    let engine = EngineHandle::new();
    for (id, config, retry) in restored.connect {
        println!("connecting {}...", id.0);
        engine.connect(id, config, retry);
    }

    let last_received: LastReceived = Arc::new(Mutex::new(HashMap::new()));
    if let Some(port) = control_port {
        let frames = session.frames.frames().cloned().collect();
        if let Err(error) = control::spawn(
            port,
            engine.command_sender(),
            scenario_name.to_owned(),
            Arc::clone(&last_received),
            frames,
        ) {
            eprintln!("cannot open the control socket on port {port}: {error}");
            return 1;
        }
    }

    scenarios::start(&mut session, &engine, &scenario);
    if let Some(error) = session.last_error.take() {
        eprintln!("{error}");
        return 1;
    }

    watch(engine, scenario_name, &last_received)
}

fn watch(mut engine: EngineHandle, scenario_name: &str, last_received: &LastReceived) -> i32 {
    loop {
        for event in engine.drain_events() {
            match event {
                Event::ConnectionStatus { id, status } => {
                    println!("{}: {}", id.0, links::status(status));
                }
                Event::FrameSent { id, bytes, .. } => {
                    println!("TX {} {}", id.0, hex::spaced(&bytes));
                }
                Event::FrameReceived { id, bytes, .. } => {
                    println!("RX {} {}", id.0, hex::spaced(&bytes));
                    last_received.lock().unwrap().insert(id, bytes);
                }
                Event::Error { id, error } => {
                    let who = id.map(|id| id.0).unwrap_or_default();
                    eprintln!("{who}: {error}");
                }
                Event::ScenarioStep { name, step, pass } if name == scenario_name => {
                    println!("[{name}] pass {pass}, step {step}");
                }
                Event::ScenarioFinished { name, outcome } if name == scenario_name => {
                    println!("[{name}] {outcome:?}");
                    return i32::from(outcome != Outcome::Completed);
                }
                Event::ScenarioStep { .. } | Event::ScenarioFinished { .. } => {}
            }
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;

    /// A project with one loopback connection and a scenario that sends once,
    /// on disk, ready for `run` to load exactly as a real invocation would.
    fn a_project_on_disk(name: &str, scenario_toml: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("sim-tui-headless-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("frames")).expect("a scratch folder");
        std::fs::create_dir_all(root.join("scenarios")).expect("a scratch folder");
        std::fs::write(
            root.join("frames").join("ping.toml"),
            "name = \"Ping\"\n[[field]]\nname = \"seq\"\ntype = \"u8\"\n\
             [[field]]\nname = \"mode\"\ntype = \"u8\"\ndefault = 1\n",
        )
        .expect("a frame file");
        std::fs::write(root.join("scenarios").join("once.toml"), scenario_toml)
            .expect("a scenario file");
        // Bind and remote are the same port, on purpose: the connection sends
        // to itself, so the scenario has something to answer its own send
        // without a second connection to stand in for a peer.
        let port = free_port();
        let project = format!(
            "version = 1\nframes_dir = \"frames\"\nscenarios_dir = \"scenarios\"\n\n\
             [[connection]]\nname = \"loop\"\ntransport = \"udp\"\n\
             bind = \"127.0.0.1:{port}\"\nremote = \"127.0.0.1:{port}\"\nautoconnect = true\n",
        );
        let path = root.join("project.toml");
        std::fs::write(&path, project).expect("a project file");
        path
    }

    /// A loopback address bound to port 0 and released, for a connection spec
    /// that needs a concrete port rather than the one the OS would have
    /// chosen for it.
    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("a bound address")
            .port()
    }

    #[test]
    fn a_scenario_run_from_the_command_line_exits_zero_once_it_completes() {
        let path = a_project_on_disk(
            "completes",
            "[[scenario]]\nname = \"Once\"\non = \"loop\"\n\n[[scenario.step]]\nsend = \"Ping\"\n",
        );
        assert_eq!(run(Some(path), "Once", None), 0);
    }

    #[test]
    fn an_unknown_scenario_name_exits_nonzero() {
        let path = a_project_on_disk(
            "unknown",
            "[[scenario]]\nname = \"Once\"\non = \"loop\"\n\n[[scenario.step]]\nsend = \"Ping\"\n",
        );
        assert_eq!(run(Some(path), "Never heard of it", None), 1);
    }

    #[test]
    fn a_missing_project_path_exits_nonzero() {
        assert_eq!(run(None, "Once", None), 1);
    }

    #[test]
    fn a_project_file_that_does_not_exist_exits_nonzero() {
        assert_eq!(
            run(Some(PathBuf::from("/does/not/exist.toml")), "Once", None),
            1
        );
    }

    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("a bound address")
            .port()
    }

    #[test]
    fn a_control_port_lets_an_external_client_steer_the_run() {
        let path = a_project_on_disk(
            "control",
            "[[scenario]]\nname = \"Steerable\"\non = \"loop\"\nrepeat = { every_ms = 30, times = 3 }\n\n\
             [[scenario.step]]\nsend = \"Ping\"\nwith = { mode = 1 }\n",
        );
        let control_port = free_tcp_port();

        let path_for_thread = path.clone();
        let runner =
            std::thread::spawn(move || run(Some(path_for_thread), "Steerable", Some(control_port)));

        // Give the run a moment to open its connection and the control
        // socket before dialling in.
        let mut client = loop {
            match std::net::TcpStream::connect(("127.0.0.1", control_port)) {
                Ok(stream) => break stream,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };

        writeln!(
            client,
            r#"{{"cmd":"set","step":1,"field":"mode","value":9}}"#
        )
        .expect("write should succeed");
        let mut reader = BufReader::new(client.try_clone().expect("clone should succeed"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read should succeed");
        assert!(line.contains("\"ok\":true"), "{line}");

        assert_eq!(runner.join().expect("the run thread should not panic"), 0);

        writeln!(
            client,
            r#"{{"cmd":"last_received","on":"loop","as":"Ping"}}"#
        )
        .expect("write should succeed");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read should succeed");
        let response: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(response["ok"], true, "{line}");
        assert_eq!(response["fields"]["mode"], 9, "{line}");
    }
}
