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

## Scenarios

A scenario is an ordered list of steps run against your connections: send a
frame with the values you choose, send raw bytes, wait a delay, or wait for a
frame matching a pattern before going on. Several can run at once, which is
where concurrency comes from, and a periodic emitter is simply a scenario that
repeats:

```toml
[[scenario]]
name = "Telemetry 10 Hz"
on = "bus"
repeat = { every_ms = 100 }

[[scenario.step]]
send = "Telemetry"
with = { mode = 1 }
counters = { seq = { wrap = 255 } }
```

`on` names the connection a step acts on, and takes a list as readily as a
single name, so one step can drive a serial port and a UDP socket together. A
`wait_for` aimed at several waits until every one of them has answered.

They live in their own folder beside the frames, one or more per file, and the
project remembers where. `examples/scenarios` holds documented examples.

The **Scenarios** tab builds them without opening a file: pick the action, tick
the connections, pick the frame, tick the fields you want to set and leave the
rest to the frame's own defaults. A wait can name a frame and the fields that
have to match rather than a run of hex. Saving writes back into the file it came
from and leaves the comments a developer wrote where they were, so the same
scenario can be edited from either side.

Delays are desktop timers: expect millisecond resolution at best, and jitter
under load. The cadence is pinned to a fixed grid rather than chained delay to
delay, so a stream does not drift, and a pass that overruns loses its slot
instead of firing a catch-up burst.

## Numbers

Every number box takes hexadecimal, binary and octal as readily as decimal, so
`0xBA` copied off a datasheet can be typed in as it stands. `_` is allowed as a
separator, so `0xFF_FF` works too.

The **0x** button beside the frame picker shows whole-number fields in
hexadecimal, padded to the width of what holds them, so a `u16` reads `0x00FF`
and lines up with the byte preview. Floats stay as they are, having no
hexadecimal to show. The choice is remembered with the project, and the boxes go
on taking decimal whichever way they are showing.

## Frame definitions

The `examples/frames` folder in the source tree holds documented examples, from
a three-byte frame up to reusable types and constrained scalars. Point the
**Frames folder** button at a copy of it, or pass the folder on the command
line. A project remembers the folder, relative to itself.
