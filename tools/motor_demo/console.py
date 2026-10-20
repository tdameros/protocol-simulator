#!/usr/bin/env python3
"""Technician console for the motor demo.

Talks to a running:

    protocol-simulator-tui tools/motor_demo/project.toml --run "Motor demo" \\
        --control-port 7878

over the JSON-lines protocol documented in docs/scenarios.md. Hardcoded to
this one scenario's shape (its single `send` step, its two fields, its one
connection) rather than asking the operator to spell any of that out: a
technician on the floor picks a speed and a mode, nothing else.
"""

import argparse
import json
import select
import socket
import sys

STEP = 1
CONNECTION = "motor"
DEFAULT_FRAME = "MotorCommand"

MODES = {
    "0": "arret",
    "1": "marche",
    "2": "reset defaut",
}


def request(sock: socket.socket, reader, payload: dict) -> dict:
    sock.sendall((json.dumps(payload) + "\n").encode("utf-8"))
    line = reader.readline()
    if not line:
        raise ConnectionError("the simulator closed the connection")
    return json.loads(line)


def set_field(sock: socket.socket, reader, step: int, field: str, value) -> None:
    response = request(
        sock, reader, {"cmd": "set", "step": step, "field": field, "value": value}
    )
    if response.get("ok"):
        print("  -> envoye.")
    else:
        print(f"  -> erreur : {response.get('error')}")


def set_fields(sock: socket.socket, reader, step: int, fields: dict[str, object]) -> None:
    """Apply several live field values through one control-socket request."""
    response = request(sock, reader, {"cmd": "set_many", "step": step, "fields": fields})
    if response.get("ok"):
        print("  -> valeurs envoyees.")
    else:
        print(f"  -> erreur : {response.get('error')}")


def set_rpm(sock: socket.socket, reader) -> None:
    raw = input("  Nouveau regime moteur (RPM) : ").strip()
    try:
        rpm = int(raw)
    except ValueError:
        print("  -> un nombre entier est attendu")
        return
    set_field(sock, reader, STEP, "target_rpm", rpm)


def set_mode(sock: socket.socket, reader) -> None:
    print("  0 = arret, 1 = marche, 2 = reset defaut")
    raw = input("  Mode : ").strip()
    if raw not in MODES:
        print("  -> 0, 1 ou 2 attendu")
        return
    set_field(sock, reader, STEP, "mode", int(raw))


def set_field_from_input(sock: socket.socket, reader) -> None:
    """Set any live-editable field using a JSON value entered by the operator."""
    field = input("  Champ : ").strip()
    if not field:
        print("  -> le nom du champ est obligatoire")
        return
    raw_value = input("  Valeur JSON (ex. 1500, \"texte\", [1, 2]) : ").strip()
    try:
        value = json.loads(raw_value)
    except json.JSONDecodeError:
        print("  -> une valeur JSON valide est attendue")
        return
    set_field(sock, reader, STEP, field, value)


IHM_MODES = {
    "1": "OFF",
    "2": "Fixe",
    "3": "Clignotement 1",
    "4": "Clignotement 2",
}

IHM_COLORS = {
    "1": "Rouge",
    "2": "Vert",
    "3": "Orange",
}

IHM_RAW_VALUES = {
    (mode, color): 19
    for mode in IHM_MODES
    if mode != "1"
    for color in IHM_COLORS
}
IHM_OFF_RAW_VALUE = 19


def ihm_menu(sock: socket.socket, reader) -> None:
    """Choose an IHM state, then send its raw value through ihm_all_leds."""
    print("\nMode IHM")
    for choice, label in IHM_MODES.items():
        print(f"{choice}) {label}")
    mode = input("Mode : ").strip()
    if mode not in IHM_MODES:
        print("  -> choix invalide")
        return
    if mode == "1":
        print(f"  -> OFF (raw={IHM_OFF_RAW_VALUE})")
        ihm_all_leds(sock, reader, IHM_OFF_RAW_VALUE)
        return
    print("\nCouleur IHM")
    for choice, label in IHM_COLORS.items():
        print(f"{choice}) {label}")
    color = input("Couleur : ").strip()
    if color not in IHM_COLORS:
        print("  -> choix invalide")
        return
    value = IHM_RAW_VALUES[(mode, color)]
    print(f"  -> {IHM_MODES[mode]} {IHM_COLORS[color]} (raw={value})")
    ihm_all_leds(sock, reader, value)


def dump_fields(
    fields: dict, field_order: list[str], bit_layouts: dict, compact_bits: bool = True, indent: int = 2
) -> None:
    """Print decoded values, optionally compacting bitfields into one decimal value."""
    padding = " " * indent
    ordered_names = [name for name in field_order if name in fields]
    ordered_names.extend(name for name in fields if name not in ordered_names)
    for name in ordered_names:
        value = fields[name]
        if compact_bits and name in bit_layouts and isinstance(value, dict):
            packed = 0
            for bit in bit_layouts[name]:
                packed = (packed << bit["width"]) | value.get(bit["name"], 0)
            print(f"{padding}{name}: {packed}")
        elif isinstance(value, dict):
            print(f"{padding}{name}:")
            dump_fields(value, [], {}, compact_bits, indent + 2)
        elif isinstance(value, list):
            rendered = json.dumps(value, ensure_ascii=False)
            print(f"{padding}{name}: {rendered}")
        else:
            print(f"{padding}{name}: {value}")


def show_last_command(sock: socket.socket, reader) -> None:
    frame = input(f"  Trame de decodage [{DEFAULT_FRAME}] : ").strip() or DEFAULT_FRAME
    show_last_received(sock, reader, CONNECTION, frame)


def show_last_sent(sock: socket.socket, reader) -> None:
    frame = input(f"  Trame de decodage [{DEFAULT_FRAME}] : ").strip() or DEFAULT_FRAME
    show_last_frame(sock, reader, "last_sent", CONNECTION, firame)


def show_last_received(sock: socket.socket, reader, connection: str, frame: str) -> dict:
    return show_last_frame(sock, reader, "last_received", connection, frame)


def show_last_frame(sock: socket.socket, reader, command: str, connection: str, frame: str) -> dict:
    response = request(sock, reader, {"cmd": command, "on": connection, "as": frame})
    if not response.get("ok"):
        print(f"  -> erreur : {response.get('error')}")
        return response
    print("==============================")
    print(f"  trame : {frame}")
    print(f"  bytes : {response['bytes']}")
    print("++++++++++++++++++++++++++++++")
    dump_fields(response["fields"], response.get("field_order", []), response.get("bit_layouts", {}))
    print("==============================")
    show_status(sock, reader)
    return response


def show_status(sock: socket.socket, reader) -> None:
    response = request(sock, reader, {"cmd": "status"})
    if not response.get("ok"):
        print(f"Statut du simulateur : indisponible ({response.get('error', 'erreur inconnue')})")
        return
    parts = [f"etat={response.get('state', 'inconnu')}"]
    if "step" in response:
        parts.append(f"etape={response['step']}")
    if "pass" in response:
        parts.append(f"passage={response['pass']}")
    if response.get("connection"):
        parts.append(f"connexion={response['connection']}")
    if response.get("reason"):
        parts.append(f"raison={response['reason']}")
    print("Statut du simulateur : " + " | ".join(parts))


def follow_last_frame(
    sock: socket.socket, reader, command: str, connection: str, frame: str, refresh_ms: int
) -> None:
    """Refresh one displayed frame at the configured rate until interrupted."""
    interval = refresh_ms / 1000
    print("  Surveillance active. Appuyez sur Entree ou Ctrl+C pour revenir au menu.")
    try:
        while True:
            print("\033[H\033[J", end="")
            print(f"Derniere trame sur {connection} (rafraichissement toutes les {refresh_ms} ms)")
            show_last_frame(sock, reader, command, connection, frame)
            print("Entree ou Ctrl+C : retour au menu")
            ready, _, _ = select.select([sys.stdin], [], [], interval)
            if ready:
                sys.stdin.readline()
                return
    except KeyboardInterrupt:
        print()


MENU = """
1) Regler le regime moteur (RPM)
2) Changer le mode moteur
3) Changer un champ (valeur JSON)
4) Voir la derniere trame envoyee au moteur
5) Voir la derniere trame recue du moteur
6) Suivre la derniere trame recue en direct
7) Voir le statut du scenario
8) Piloter l'IHM
9) Quitter
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port", type=int, help="the --control-port the simulator was started with")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument(
        "--refresh-ms",
        type=int,
        default=1000,
        help="refresh interval while following a frame in milliseconds (default: 1000)",
    )
    args = parser.parse_args()
    if args.refresh_ms <= 0:
        parser.error("--refresh-ms must be greater than zero")

    try:
        sock = socket.create_connection((args.host, args.port))
    except OSError as error:
        print(f"impossible de se connecter a {args.host}:{args.port} : {error}", file=sys.stderr)
        return 1

    reader = sock.makefile("r", encoding="utf-8")
    print(f"connecte au moteur simule sur {args.host}:{args.port}")

    try:
        while True:
            print(MENU)
            choice = input("Choix : ").strip()
            if choice == "1":
                set_rpm(sock, reader)
            elif choice == "2":
                set_mode(sock, reader)
            elif choice == "3":
                set_field_from_input(sock, reader)
            elif choice == "4":
                show_last_sent(sock, reader)
            elif choice == "5":
                show_last_command(sock, reader)
            elif choice == "6":
                connection = input(f"  Connexion [{CONNECTION}] : ").strip() or CONNECTION
                frame = input(f"  Trame de decodage [{DEFAULT_FRAME}] : ").strip() or DEFAULT_FRAME
                follow_last_frame(sock, reader, "last_received", connection, frame, args.refresh_ms)
            elif choice == "7":
                show_status(sock, reader)
            elif choice == "8":
                ihm_menu(sock, reader)
            elif choice == "9":
                return 0
            else:
                print("choix invalide")
    except (KeyboardInterrupt, EOFError):
        print()
        return 0
    finally:
        sock.close()


if __name__ == "__main__":
    raise SystemExit(main())
