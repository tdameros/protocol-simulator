Protocol Simulator __VERSION__. One executable per platform, nothing to install.

## Install

**Linux x86_64.** `tar xzf protocol-simulator-linux-x86_64.tar.gz`, then run
`./protocol-simulator`. Needs glibc 2.35 or newer, so Ubuntu 22.04+, Debian 12+,
Fedora 36+. Not RHEL 9, which ships 2.34.

**Windows x86_64.** Run `protocol-simulator-windows-x86_64.exe`. Not code
signed, so SmartScreen asks once: **More info → Run anyway**.

**macOS, Intel and Apple Silicon.** Unzip and drag `ProtocolSimulator.app` to
Applications. Not notarised, so the first launch needs **right-click → Open →
Open**, or `xattr -dr com.apple.quarantine /Applications/ProtocolSimulator.app`.

The app takes one optional argument, a frames folder or a project file.

## Documentation

[README](https://github.com/tdameros/protocol-simulator#readme) for the
overview, `docs/` for a reference page per part of the app, and
`examples/frames/README.md` for the frame file format.

`THIRD-PARTY.md` beside the archive lists every crate compiled in with the full
text of its licence. The Linux and macOS downloads carry a copy inside.
