# Example frames

Frame definitions are plain TOML. Point the simulator at this folder — **Frame
editor → Frames folder** — or pass it on the command line:

```sh
cargo run -p sim-gui -- examples/frames
```

Read the files in order; each one introduces what the previous did not.

| File | Introduces |
| --- | --- |
| `01-minimal.toml` | the smallest frame that works |
| `02-scalars.toml` | every scalar type, and byte order |
| `03-enums-and-bits.toml` | named values and packed flags |
| `04-checksums.toml` | checksums, and the range each protects |
| `05-telemetry.toml` | a realistic frame using all of it |
| `06-templates.toml` | reusable types, for structures that repeat |
| `07-subtypes.toml` | scalars restricted to the values the protocol allows |
| `types/led.toml` | types shared by every frame in the folder |

Every file here is checked by `cargo test -p sim-core --test examples`: it must
load, encode, decode, and match the byte count stated in its description.

## Frame

```toml
name = "Telemetry"                 # required, shown in the picker
description = "Downlink, 26 bytes" # optional
endian = "big"                     # "big" (default) or "little"
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
| `xor8` `sum8` `sum16` `crc8` `crc16` `crc32` | `covers`, `algo` | 1 to 4 bytes |

`repr` must be an unsigned integer. Bit widths are packed most significant bit
first and must add up to exactly the size of `repr`. Variants are listed by
value, not by name, so the order in the file does not matter.

`covers = { from = "first", to = "last" }` names the first and last field
protected, both included. Naming fields rather than byte offsets means inserting
a field later moves the range with it.

CRC presets, given as `algo`:

```
crc8   crc16-ccitt   crc16-x25   crc16-xmodem   crc16-modbus   crc32
```

`crc8` and `crc32` have one dominant variant, so `algo` may be omitted for them.

## Subtypes

A `range` restricts a scalar to part of what its representation can hold, the
way an Ada subtype does. The wire format does not change — a `u8` is still one
byte — but the editor will not let you dial a value outside it, sending one is
refused, and receiving one is flagged.

```toml
[[field]]
name = "gain"
type = "i16"
range = { min = -500, max = 500 }
```

Give the constraint a name and it becomes reusable, as a `[[type]]` with a
`base` instead of fields:

```toml
[[type]]
name = "Percent"
base = "u8"
range = { min = 0, max = 100 }

[[field]]
name = "duty"
type = "Percent"
```

A subtype may narrow another subtype, and a field may narrow the subtype it
uses, as long as each stays inside the one above it. Widening is refused, and so
is a range that does not fit the base representation — `{ min = 0, max = 300 }`
on a `u8` fails at load time rather than truncating quietly.

Ranges apply to scalars only, integer or float. On a `text`, `bytes`, `enum`,
`bits` or checksum field they are rejected rather than ignored.

The asymmetry between the two directions is deliberate: **sending** a value your
own specification forbids is a mistake, so the encoder refuses it; **receiving**
one is a finding, so the decoder reports it and still shows you the frame.

## Reusable types

A `[[type]]` block is a named group of fields. Use it wherever a structure
repeats, instead of copying it:

```toml
[[type]]
name = "LedConfig"
[[type.field]]
name = "mode"
type = "u8"
[[type.field]]
name = "period_ms"
type = "u16"

[[field]]
name = "led"
type = "LedConfig"
repeat = 8
```

Types may be declared inline in a frame or, as in `types/led.toml`, in any file
under `types/` — those are shared by every frame in the folder. An inline
definition wins over a shared one of the same name. Types may contain other
types.

Instantiating expands the group under the instance name:

| Attribute | Result |
| --- | --- |
| `repeat = 8` | `led[0].mode`, `led[1].mode`, … |
| `instances = ["left", "right"]` | `zone.left.mode`, `zone.right.mode` |
| neither | `led.mode` |

`repeat` and `instances` work on ordinary fields too: a `u16` with `repeat = 4`
gives `sample[0]` through `sample[3]`.

Naming an instance in `covers` selects the whole block it expanded into, so
`covers = { from = "header", to = "led" }` protects all eight LEDs and keeps
doing so when the count changes. A checksum declared *inside* a type covers its
own instance, not the first copy.

The editor groups the expanded fields back together, one collapsible block per
instance.

## Known limitations

- **Range bounds go through TOML integers**, so a `u64` bound above
  9 223 372 036 854 775 807 cannot be written. Every other bound is exact.
- **Fixed size only.** A `bytes` field sized by an upstream `length` field is
  not supported yet; `len` must be a constant.
- **Saving from the GUI writes the expanded layout.** Types are resolved when
  the file is read, so a frame saved from the editor lists every field
  individually. The bytes on the wire are identical, but the factorisation is
  lost — keep the hand-written file if you care about it.
