# Watching traffic

Every frame sent and received, on every connection, in one buffer.

![The traffic monitor](images/traffic.png)

A row carries the timestamp, the gap since the previous frame, the direction,
the connection, the bytes, and their printable characters.

The header shows how many rows pass the filter out of how many are held, with
the current rate in frames and bytes per second.

The buffer holds the last 10 000 frames. Older ones are dropped.

## Controls

| Control | Does |
| --- | --- |
| Pause | freezes the view. Capture continues |
| Clear | hides everything logged so far. Capture continues |
| Follow | keeps the newest row in view |
| Filter | connections, direction, and a hex pattern the frame must contain |
| + | opens another monitor over the same buffer |

Several monitors can be open at once, each with its own filter and its own
paused state. A monitor hides everything logged before it was opened.

`??` in a filter pattern matches any byte, so `AA 55 ?? 01` leaves the third
byte free.

## Fields

Click a row to read its bytes as fields. Click it again to put them away.

The pane offers every definition of exactly the row's length and nothing else.
A single candidate is taken without asking. Several wait to be told which, and
the answer holds for the next row of the same length.

| Column | Holds |
| --- | --- |
| Name | the field, as the definition names it |
| Span | the bytes it covers, `from..to` |
| Bytes | those bytes |
| Value | what they decode to |

An enum shows the name of its variant, or `unknown` for a value no variant
claims. A bitfield adds a row per sub-field carrying its position in the word,
written as a datasheet writes it. A checksum reads `ok`, or the value the
bytes should have carried. A field outside the range its type allows says so.

Whole numbers follow the `0x` switch in the Frames tab.

Reading a row switches Follow off. A list that keeps scrolling to the newest
frame moves the row being read out from under the pointer.

A row is one read from the transport. A UDP datagram is one frame. TCP and
serial hand over whatever had arrived, so a row may hold part of a frame or
more than one, and only a row holding exactly one can be read as fields.

## Row actions

| Action | Does |
| --- | --- |
| Open in Frames | loads the bytes into the frame picked there, ready to send back |
| Send to Hex Inject | copies the bytes into the injection box |

## Hex injection

![Raw hex injection](images/hex-inject.png)

Whitespace is ignored. Every other character must be a hex digit, and the total
must be an even number of them. The count under the box is what the input parsed
to.
