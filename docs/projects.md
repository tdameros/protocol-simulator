# Projects

A project is one TOML file recording the whole bench.

## Contents

| Key | Holds |
| --- | --- |
| `version` | format version, currently 1 |
| `frames_dir` | path to the frame definitions, relative to this file |
| `scenarios_dir` | path to the scenarios |
| `[[connection]]` | name, transport, retry policy, autoconnect |
| `[[monitor]]` | title and filter of each traffic view |
| `values` | what was typed into each frame, by frame name |
| layout, theme | the dock arrangement and the palette |

```toml
version = 1
frames_dir = "frames"
scenarios_dir = "scenarios"

[[connection]]
name = "drive"
autoconnect = true
```

Paths are written relative to the project file where possible and always with
forward slashes, so a project folder copies between machines and between
operating systems.

The frames and scenarios themselves are not in it. The project points at the
folders, so two projects can share one protocol.

## Commands

| Action | Where |
| --- | --- |
| New project | File menu. Clears everything |
| Open project | File menu, File → Recent, or a path on the command line |
| Save | File menu, or Ctrl+S and Cmd+S |
| Save as | File menu |

Connections marked autoconnect are opened as the project loads.

Closing the window with unsaved changes asks first.

## Version

A file whose `version` is higher than the build understands is refused rather
than half read.
