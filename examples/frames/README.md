# Example frames

One protocol, written out the way you would write your own. It belongs to a
brushless motor drive on RS-485 or UDP, and the files cover everything the
format can do.

Point the simulator at this folder with **Frames folder**, or pass it on the
command line:

```sh
cargo run -p sim-gui -- examples/frames
```

Read them in order. Each one introduces what the previous did not.

| File | Message | Introduces |
| --- | --- | --- |
| `01-heartbeat.toml` | host is alive | the smallest frame that works |
| `02-command.toml` | speed setpoint | every scalar width, and byte order |
| `03-status.toml` | drive state | named values and packed flags |
| `04-identity.toml` | serial and firmware | text and raw bytes |
| `05-telemetry.toml` | running values | the shared header and units |
| `06-motor-bank.toml` | four drives at once | a structure that repeats |
| `07-limits.toml` | protection thresholds | scalars restricted to what is allowed |
| `types/header.toml` | | the four bytes every message starts with |
| `types/units.toml` | | rpm, milliamps, degrees, per mille |

Every file here is checked by `cargo test -p sim-core --test examples`. It must
load, encode, decode, match the byte count in its description, and come back
byte for byte unchanged when the editor rewrites it.

## Frame

```toml
name = "Telemetry"                 # required, shown in the picker
description = "Running values, 16 bytes"   # optional
endian = "little"                  # "little" (default) or "big"
```

## Fields

Fields are written in wire order, one `[[field]]` per entry. Every field takes
`name`, `type`, an optional `description` shown on hover, and an optional
`endian` overriding the frame's.

| `type` | Extra attributes | Size |
| --- | --- | --- |
| `u8` `i8` `u16` `i16` `u32` `i32` `u64` `i64` `f32` `f64` | `default`, `range` | 1 to 8 bytes |
| `bytes` | `len` | `len` |
| `text` | `len`, `default` | `len`, NUL padded |
| `enum` | `repr`, `variants`, `default` | size of `repr` |
| `bits` | `repr`, `bits` | size of `repr` |
| `xor8` `sum8` `sum16` `sum32` `crc8` `crc16` `crc32` | `covers`, `algo` | 1 to 4 bytes |

`repr` must be an unsigned integer. Bit widths are packed most significant bit
first and must add up to exactly the size of `repr`. Variants are listed by
value rather than by name, so the order in the file does not matter.

`covers = { from = "first", to = "last" }` names the first and last field
protected, both included. Naming fields rather than byte offsets means inserting
a field later moves the range with it.

CRC presets, given as `algo`:

```
crc8   crc16-ccitt   crc16-x25   crc16-xmodem   crc16-modbus   crc32
```

`crc8` and `crc32` have one dominant variant, so `algo` may be left out for
them.

## Subtypes

A `range` restricts a scalar to part of what its representation can hold, the
way an Ada subtype does. The wire format does not change, a `u8` is still one
byte, but the editor will not let you dial a value outside it, sending one is
refused, and receiving one is flagged.

```toml
[[field]]
name = "target_rpm"
type = "i16"
range = { min = -6000, max = 6000 }
```

Give the constraint a name and it becomes reusable, as a `[[type]]` with a
`base` instead of fields:

```toml
[[type]]
name = "Celsius"
base = "i8"
range = { min = -40, max = 125 }

[[field]]
name = "temperature"
type = "Celsius"
```

A subtype may narrow another subtype, and a field may narrow the subtype it
uses, as long as each stays inside the one above it. Widening is refused, and so
is a range that does not fit the base representation. `{ min = 0, max = 300 }`
on a `u8` fails at load time rather than truncating quietly.

Ranges apply to scalars only, integer or float. On a `text`, `bytes`, `enum`,
`bits` or checksum field they are rejected rather than ignored.

The asymmetry between the two directions is deliberate. **Sending** a value your
own specification forbids is a mistake, so the encoder refuses it. **Receiving**
one is a finding, so the decoder reports it and still shows you the frame.

## Reusable types

A `[[type]]` block is a named group of fields. Use it wherever a structure
repeats, instead of copying it:

```toml
[[type]]
name = "MotorReading"
[[type.field]]
name = "rpm"
type = "Rpm"
[[type.field]]
name = "current"
type = "Milliamps"

[[field]]
name = "motor"
type = "MotorReading"
repeat = 4
```

Types may be declared inline in a frame, as `MotorReading` is in
`06-motor-bank.toml`, or in any file under `types/`, which are shared by every
frame in the folder. An inline definition wins over a shared one of the same
name. Types may contain other types.

Instantiating expands the group under the instance name:

| Attribute | Result |
| --- | --- |
| `repeat = 4` | `motor[0].rpm`, `motor[1].rpm`, … |
| `instances = ["left", "right"]` | `axle.left.rpm`, `axle.right.rpm` |
| neither | `head.sync` |

`repeat` and `instances` work on ordinary fields too. A `u16` with `repeat = 4`
gives `sample[0]` through `sample[3]`.

Naming an instance in `covers` selects the whole block it expanded into, so
`covers = { from = "head", to = "motor" }` protects all four drives and keeps
doing so when the count changes. A checksum declared *inside* a type covers its
own instance, not the first copy.

The editor groups the expanded fields back together, one collapsible block per
instance.

## What the editor does to these files

Saving from the app writes back into the file the frame came from, key by key.
Comments, blank lines, key order and line endings survive it, and a
factorisation is never flattened. `06-motor-bank.toml` is three declarations
that expand to sixteen fields, and it is written back as three.

## Known limitations

* **Range bounds go through TOML integers**, so a `u64` bound above
  9 223 372 036 854 775 807 cannot be written. Every other bound is exact.
* **Fixed size only.** A `bytes` field sized by an upstream `length` field is
  not supported yet, `len` has to be a constant.
