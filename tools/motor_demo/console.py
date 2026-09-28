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
import socket
import sys

STEP = 1
CONNECTION = "motor"
FRAME = "MotorCommand"

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


def set_field(sock: socket.socket, reader, field: str, value) -> None:
    response = request(
        sock, reader, {"cmd": "set", "step": STEP, "field": field, "value": value}
    )
    if response.get("ok"):
        print("  -> envoye.")
    else:
        print(f"  -> erreur : {response.get('error')}")


def set_rpm(sock: socket.socket, reader) -> None:
    raw = input("  Nouveau regime moteur (RPM) : ").strip()
    try:
        rpm = int(raw)
    except ValueError:
        print("  -> un nombre entier est attendu")
        return
    set_field(sock, reader, "target_rpm", rpm)


def set_mode(sock: socket.socket, reader) -> None:
    print("  0 = arret, 1 = marche, 2 = reset defaut")
    raw = input("  Mode : ").strip()
    if raw not in MODES:
        print("  -> 0, 1 ou 2 attendu")
        return
    set_field(sock, reader, "mode", int(raw))


def show_last_command(sock: socket.socket, reader) -> None:
    response = request(
        sock, reader, {"cmd": "last_received", "on": CONNECTION, "as": FRAME}
    )
    if not response.get("ok"):
        print(f"  -> erreur : {response.get('error')}")
        return
    fields = response["fields"]
    mode_label = MODES.get(str(fields["mode"]), "inconnu")
    print(f"  regime : {fields['target_rpm']} rpm")
    print(f"  mode   : {fields['mode']} ({mode_label})")


MENU = """
1) Regler le regime moteur (RPM)
2) Changer le mode moteur
3) Voir la derniere commande envoyee au moteur
4) Quitter
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port", type=int, help="the --control-port the simulator was started with")
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

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
                show_last_command(sock, reader)
            elif choice == "4":
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
