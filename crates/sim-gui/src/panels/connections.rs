use std::net::{IpAddr, Ipv4Addr};

use sim_core::{ConnectionStatus, RetryPolicy};

use egui::{Color32, ComboBox, Grid, RichText, Ui};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::state::{ConnectionEntry, NewConnectionForm, TransportKindChoice};

pub fn show(ui: &mut Ui, state: &mut crate::state::AppState, engine: &EngineHandle) {
    ui.heading("New connection");
    new_connection_form(ui, &mut state.new_connection);

    if ui
        .button(RichText::new(format!("{} Connect", icons::PLUG)))
        .clicked()
    {
        if let Some((id, config)) = state.new_connection.build(&state.connections) {
            let retry = state
                .new_connection
                .auto_reconnect
                .then(RetryPolicy::standard);
            engine.connect(id.clone(), config.clone(), retry);
            state.connections.push((
                id,
                ConnectionEntry {
                    config,
                    status: ConnectionStatus::Connecting,
                    retry,
                    autoconnect: state.new_connection.autoconnect,
                },
            ));
            state.new_connection.name.clear();
        }
    }
    if let Some(error) = &state.new_connection.error {
        ui.colored_label(Color32::from_rgb(200, 60, 60), error);
    }

    ui.separator();
    ui.heading("Connections");
    if state.connections.is_empty() {
        ui.label("No connection configured yet.");
    }

    let mut to_disconnect = Vec::new();
    let mut to_reconnect = Vec::new();
    let mut to_remove = Vec::new();

    // A grid rather than a row of horizontals: names and summaries are all
    // different lengths, and laid out one row at a time the controls end up at
    // a different place on every line. Cells are aligned left and centred
    // vertically, which is the other half of what is wanted here.
    Grid::new("connection_list")
        .num_columns(6)
        // Otherwise every column is at least as wide as a button, which the
        // status dot is not.
        .min_col_width(0.0)
        .show(ui, |ui| {
            for (id, entry) in &mut state.connections {
                ui.label(status_dot(entry.status))
                    .on_hover_text(status_label(entry.status));
                ui.label(RichText::new(&id.0).strong());
                ui.label(kind_summary(entry));

                // Always a cell, even when there is nothing to show: a skipped
                // one would pull the whole row left.
                if entry.retry.is_some() {
                    ui.label(RichText::new(icons::ARROWS_CLOCKWISE).weak())
                        .on_hover_text("Reopens itself when the link drops");
                } else {
                    ui.label("");
                }

                ui.checkbox(&mut entry.autoconnect, icons::POWER)
                    .on_hover_text("Open this connection when the project is loaded");

                ui.horizontal(|ui| match entry.status {
                    ConnectionStatus::Disconnected => {
                        if ui
                            .button(icons::PLUG)
                            .on_hover_text("Reconnect with the same settings")
                            .clicked()
                        {
                            to_reconnect.push(id.clone());
                        }
                        if ui.button(icons::TRASH).on_hover_text("Remove").clicked() {
                            to_remove.push(id.clone());
                        }
                    }
                    ConnectionStatus::Connecting
                    | ConnectionStatus::Listening
                    | ConnectionStatus::Connected => {
                        if ui
                            .button(icons::PLUGS)
                            .on_hover_text("Disconnect")
                            .clicked()
                        {
                            to_disconnect.push(id.clone());
                        }
                    }
                });

                ui.end_row();
            }
        });

    for id in to_disconnect {
        engine.disconnect(id);
    }
    for id in to_reconnect {
        if let Some((config, retry)) = state.begin_reconnect(&id) {
            engine.connect(id, config, retry);
        }
    }
    for id in to_remove {
        state.remove_connection(&id);
    }
}

fn new_connection_form(ui: &mut Ui, form: &mut NewConnectionForm) {
    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.text_edit_singleline(&mut form.name);
    });

    labeled_combo(
        ui,
        "Type:",
        "connection_kind",
        &TransportKindChoice::ALL,
        &mut form.kind,
        |kind| kind.label().to_owned(),
    );

    match form.kind {
        TransportKindChoice::Udp => udp_fields(ui, form),
        TransportKindChoice::TcpClient => {
            ui.horizontal(|ui| {
                ui.label("Remote address:");
                ui.text_edit_singleline(&mut form.tcp_addr);
            });
        }
        TransportKindChoice::TcpServer => {
            ui.horizontal(|ui| {
                ui.label("Listen address:");
                ui.text_edit_singleline(&mut form.tcp_addr);
            });
        }
        TransportKindChoice::Serial => serial_fields(ui, form),
    }

    ui.checkbox(&mut form.auto_reconnect, "Auto-reconnect")
        .on_hover_text("Keep retrying when the link drops, backing off from 0.5 s up to 10 s");
    ui.checkbox(&mut form.autoconnect, "Open with the project")
        .on_hover_text("Open this connection as soon as the project is loaded");
}

fn udp_fields(ui: &mut Ui, form: &mut NewConnectionForm) {
    let group = form.udp_multicast_group();

    ui.horizontal(|ui| {
        ui.label(if group.is_some() { "Group:" } else { "Remote:" });
        ui.text_edit_singleline(&mut form.udp_remote);
        if group.is_some() {
            ui.label(
                RichText::new(format!("{} multicast", icons::BROADCAST))
                    .color(Color32::from_rgb(45, 110, 200)),
            );
        }
    });

    match group {
        // The bind port is dictated by the group, so there is nothing to ask for.
        Some(group) => {
            interface_picker(ui, &mut form.multicast_interface);
            ui.weak(format!("bind: 0.0.0.0:{}", group.port()));
        }
        None => {
            ui.horizontal(|ui| {
                ui.label("Bind (local):");
                ui.text_edit_singleline(&mut form.udp_bind);
            });
        }
    }
}

fn interface_picker(ui: &mut Ui, selected: &mut Ipv4Addr) {
    let selected_text = if selected.is_unspecified() {
        "auto (0.0.0.0)".to_owned()
    } else {
        selected.to_string()
    };

    ui.horizontal(|ui| {
        ui.label("Interface:")
            .on_hover_text("Interface used to join the group and emit. 'auto' lets the OS pick, which often guesses wrong on multi-NIC machines.");
        ComboBox::from_id_salt("multicast_interface")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected, Ipv4Addr::UNSPECIFIED, "auto (0.0.0.0)");
                for (name, addr) in local_ipv4_interfaces() {
                    ui.selectable_value(selected, addr, format!("{name} ({addr})"));
                }
            });
    });
}

fn local_ipv4_interfaces() -> Vec<(String, Ipv4Addr)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut found: Vec<_> = interfaces
        .into_iter()
        .filter_map(|iface| match iface.addr.ip() {
            IpAddr::V4(addr) => Some((iface.name, addr)),
            IpAddr::V6(_) => None,
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

fn serial_fields(ui: &mut Ui, form: &mut NewConnectionForm) {
    ui.horizontal(|ui| {
        ui.label("Port:");
        ui.text_edit_singleline(&mut form.serial_port);
        if let Ok(ports) = tokio_serial::available_ports() {
            ComboBox::from_id_salt("serial_port_picker")
                .selected_text("detected")
                .show_ui(ui, |ui| {
                    for port in ports {
                        if ui.selectable_label(false, &port.port_name).clicked() {
                            form.serial_port = port.port_name;
                        }
                    }
                });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Baud rate:");
        ui.text_edit_singleline(&mut form.serial_baud);
    });
    labeled_combo(
        ui,
        "Data bits:",
        "serial_data_bits",
        &[
            tokio_serial::DataBits::Five,
            tokio_serial::DataBits::Six,
            tokio_serial::DataBits::Seven,
            tokio_serial::DataBits::Eight,
        ],
        &mut form.serial_data_bits,
        |v| v.to_string(),
    );
    labeled_combo(
        ui,
        "Parity:",
        "serial_parity",
        &[
            tokio_serial::Parity::None,
            tokio_serial::Parity::Odd,
            tokio_serial::Parity::Even,
        ],
        &mut form.serial_parity,
        |v| v.to_string(),
    );
    labeled_combo(
        ui,
        "Stop bits:",
        "serial_stop_bits",
        &[tokio_serial::StopBits::One, tokio_serial::StopBits::Two],
        &mut form.serial_stop_bits,
        |v| v.to_string(),
    );
    labeled_combo(
        ui,
        "Flow control:",
        "serial_flow_control",
        &[
            tokio_serial::FlowControl::None,
            tokio_serial::FlowControl::Software,
            tokio_serial::FlowControl::Hardware,
        ],
        &mut form.serial_flow_control,
        |v| v.to_string(),
    );
}

fn labeled_combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    label: &str,
    id: &str,
    values: &[T],
    selected: &mut T,
    show: impl Fn(T) -> String,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ComboBox::from_id_salt(id)
            .selected_text(show(*selected))
            .show_ui(ui, |ui| {
                for &value in values {
                    let text = show(value);
                    ui.selectable_value(selected, value, text);
                }
            });
    });
}

fn status_dot(status: ConnectionStatus) -> RichText {
    // Phosphor only ships three circles, so colour carries the difference
    // between reaching out and waiting to be reached; the tooltip spells it out.
    let (glyph, color) = match status {
        ConnectionStatus::Connecting => (icons::CIRCLE_DASHED, Color32::from_rgb(200, 160, 40)),
        ConnectionStatus::Listening => (icons::CIRCLE_DASHED, Color32::from_rgb(45, 110, 200)),
        ConnectionStatus::Connected => (icons::CIRCLE_HALF, Color32::from_rgb(40, 160, 80)),
        ConnectionStatus::Disconnected => (icons::CIRCLE, Color32::from_rgb(150, 150, 150)),
    };
    RichText::new(glyph).color(color)
}

fn status_label(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Listening => "Port open, waiting for a peer",
        ConnectionStatus::Connected => "Connected",
        ConnectionStatus::Disconnected => "Disconnected",
    }
}

fn kind_summary(entry: &ConnectionEntry) -> String {
    use sim_core::{TcpMode, TransportConfig};

    match &entry.config {
        TransportConfig::Udp { bind, remote } => format!("UDP {bind} -> {remote}"),
        TransportConfig::UdpMulticast { group, interface } => {
            let via = if interface.is_unspecified() {
                "auto".to_owned()
            } else {
                interface.to_string()
            };
            format!("UDP multicast {group} via {via}")
        }
        TransportConfig::Tcp {
            mode: TcpMode::Client { addr },
        } => format!("TCP client -> {addr}"),
        TransportConfig::Tcp {
            mode: TcpMode::Server { listen },
        } => format!("TCP server on {listen}"),
        TransportConfig::Serial {
            port_name,
            baud_rate,
            ..
        } => format!("Serial {port_name} @ {baud_rate}"),
    }
}
