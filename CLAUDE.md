# Working on this repository

A protocol simulator in Rust. [README.md](README.md) says what it does for the
person using it. [docs/architecture.md](docs/architecture.md) says how it is put
together. [docs/testing.md](docs/testing.md) says how it is checked, and lists
the egui behaviour that has already cost a debugging session.

## Commands

| Command | Runs |
| --- | --- |
| `make ci` | fmt, clippy, release type-check, tests. The gate, word for word what CI runs |
| `make run` | the GUI |
| `make test-core` | the engine tests only, including the UDP and TCP loopbacks |
| `cargo test -p sim-gui --features shots shots` | redraws the documentation screenshots |
| `make third-party` | regenerates THIRD-PARTY.md after a dependency change |

Run `make ci` before every commit. `make check-release` is part of it because
the release profile compiles different code, which is explained in
[docs/testing.md](docs/testing.md).

## Invariants

`sim-core` has no GUI dependency and `sim-gui` holds no protocol logic. Both
hold for a headless front end later, and both are worth refusing a change over.

The GUI reaches the engine through `EngineHandle` and nothing else. There is no
second path to a connection.

Every file format is mirrored by plain `Raw*` structs and converted into the
model. Validation then reports what is wrong in the file rather than failing
with a deserialiser message.

Files a person keeps in Git are written back through `sim_core::document`,
which leaves comments, blank lines and key order where their author put them.

## Conventions

Both crates open with `#![deny(clippy::all)]` and `#![warn(clippy::pedantic)]`.
Clippy is run with `-D warnings`, so a warning is a build failure.

The code speaks for itself. A comment says why, never what. A module header
says what the module is for and what was decided against.

No pixel value is written down. Sizes come from `ui.spacing()` and from the
text style, so the layout follows the theme and the platform.

Documentation is reference material. No semicolons, no dashes as punctuation,
no filler. A table beats a paragraph.

Commits are small and their subject follows the conventional prefixes `feat:`,
`fix:`, `chore:`, `test:`, `docs:` and `refactor:`. Subject only.
