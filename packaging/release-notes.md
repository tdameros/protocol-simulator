Protocol Simulator __VERSION__: one executable per platform, nothing to install.

## Linux (x86_64)

```sh
tar xzf protocol-simulator-linux-x86_64.tar.gz
./protocol-simulator            # optionally: ./protocol-simulator my-project.toml
```

Built against glibc 2.35, so it runs on Ubuntu 22.04+, Debian 12+, Fedora 36+
and RHEL 9+. It uses the X11 or Wayland libraries your desktop already provides;
there is nothing to install.

## Windows (x86_64)

Download `protocol-simulator-windows-x86_64.exe` and run it. The C runtime is
linked in, so no Visual C++ redistributable is needed.

The executable is not code-signed, so SmartScreen shows a warning the first
time: **More info → Run anyway**.

## macOS (Intel and Apple Silicon)

Unzip and drag `ProtocolSimulator.app` to Applications. One download covers both
architectures.

The app is not notarised, so Gatekeeper refuses a plain double-click the first
time. Either **right-click → Open → Open**, or clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/ProtocolSimulator.app
```

Only needed once.

## Projects

**File → Save** writes everything you have set up (connections, frames folder,
Traffic tabs and their filters, the values you typed) into a single `.toml` file
that is meant to be read, kept in Git, and handed to a colleague. Opening it
brings the session back, and the connections marked to open with the project
reopen themselves. Serial port names are the one thing that will not travel:
what is `/dev/ttyUSB0` on one machine is `COM3` on another.

Launching with no argument reopens the last project used on this machine.

## Frame definitions

The `examples/frames` folder in the source tree holds documented examples, from
a three-byte frame up to reusable types and constrained scalars. Point the
**Frames folder** button at a copy of it, or pass the folder on the command
line. A project remembers the folder, relative to itself.
