/// How a checksum field is computed.
///
/// CRCs are described by the usual parameter model (width, polynomial, initial
/// value, input/output reflection, final xor) rather than hardcoded variants:
/// every protocol seems to pick a different CRC-16, so the parameters have to be
/// expressible in the frame file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumSpec {
    Crc(CrcSpec),
    /// All covered bytes xored together.
    Xor8,
    /// Sum of the covered bytes, truncated to the given width.
    Sum {
        width_bytes: usize,
    },
}

impl ChecksumSpec {
    #[must_use]
    pub fn width_bytes(self) -> usize {
        match self {
            Self::Crc(crc) => crc.width_bits as usize / 8,
            Self::Xor8 => 1,
            Self::Sum { width_bytes } => width_bytes,
        }
    }

    #[must_use]
    pub fn compute(self, data: &[u8]) -> u64 {
        match self {
            Self::Crc(crc) => crc.compute(data),
            Self::Xor8 => u64::from(data.iter().fold(0u8, |acc, byte| acc ^ byte)),
            Self::Sum { width_bytes } => {
                let sum = data
                    .iter()
                    .fold(0u64, |acc, byte| acc.wrapping_add(u64::from(*byte)));
                let bits = width_bytes * 8;
                if bits >= 64 {
                    sum
                } else {
                    sum & ((1u64 << bits) - 1)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrcSpec {
    pub width_bits: u8,
    pub poly: u64,
    pub init: u64,
    pub reflect_in: bool,
    pub reflect_out: bool,
    pub xor_out: u64,
}

impl CrcSpec {
    /// Presets for the variants that keep coming up in the field.
    ///
    /// Names follow the catalogue at reveng.sourceforge.io so they can be matched
    /// against a protocol specification without guesswork.
    #[must_use]
    pub fn preset(name: &str) -> Option<Self> {
        Some(match name {
            // CRC-8/SMBUS
            "crc8" => Self {
                width_bits: 8,
                poly: 0x07,
                init: 0x00,
                reflect_in: false,
                reflect_out: false,
                xor_out: 0x00,
            },
            // CRC-16/IBM-SDLC, a.k.a. X-25
            "crc16-x25" => Self {
                width_bits: 16,
                poly: 0x1021,
                init: 0xFFFF,
                reflect_in: true,
                reflect_out: true,
                xor_out: 0xFFFF,
            },
            // CRC-16/IBM-3740, often labelled "CRC-16-CCITT (false)"
            "crc16-ccitt" => Self {
                width_bits: 16,
                poly: 0x1021,
                init: 0xFFFF,
                reflect_in: false,
                reflect_out: false,
                xor_out: 0x0000,
            },
            // CRC-16/XMODEM
            "crc16-xmodem" => Self {
                width_bits: 16,
                poly: 0x1021,
                init: 0x0000,
                reflect_in: false,
                reflect_out: false,
                xor_out: 0x0000,
            },
            // CRC-16/MODBUS
            "crc16-modbus" => Self {
                width_bits: 16,
                poly: 0x8005,
                init: 0xFFFF,
                reflect_in: true,
                reflect_out: true,
                xor_out: 0x0000,
            },
            // CRC-32/ISO-HDLC, the zip/ethernet one
            "crc32" => Self {
                width_bits: 32,
                poly: 0x04C1_1DB7,
                init: 0xFFFF_FFFF,
                reflect_in: true,
                reflect_out: true,
                xor_out: 0xFFFF_FFFF,
            },
            _ => return None,
        })
    }

    #[must_use]
    pub fn preset_names() -> &'static [&'static str] {
        &[
            "crc8",
            "crc16-ccitt",
            "crc16-x25",
            "crc16-xmodem",
            "crc16-modbus",
            "crc32",
        ]
    }

    #[must_use]
    pub fn compute(self, data: &[u8]) -> u64 {
        let width = u32::from(self.width_bits);
        let mask = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let top_bit = 1u64 << (width - 1);

        let mut crc = self.init & mask;
        for byte in data {
            let byte = if self.reflect_in {
                byte.reverse_bits()
            } else {
                *byte
            };
            crc ^= u64::from(byte) << (width - 8);
            for _ in 0..8 {
                crc = if crc & top_bit != 0 {
                    ((crc << 1) ^ self.poly) & mask
                } else {
                    (crc << 1) & mask
                };
            }
        }

        if self.reflect_out {
            crc = reflect(crc, width);
        }
        (crc ^ self.xor_out) & mask
    }
}

fn reflect(value: u64, width: u32) -> u64 {
    let mut out = 0u64;
    for bit in 0..width {
        if value & (1 << bit) != 0 {
            out |= 1 << (width - 1 - bit);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue check value: CRC of the ASCII string "123456789".
    const CHECK: &[u8] = b"123456789";

    #[test]
    fn presets_match_their_catalogue_check_values() {
        let expected = [
            ("crc8", 0xF4_u64),
            ("crc16-ccitt", 0x29B1),
            ("crc16-x25", 0x906E),
            ("crc16-xmodem", 0x31C3),
            ("crc16-modbus", 0x4B37),
            ("crc32", 0xCBF4_3926),
        ];
        for (name, want) in expected {
            let spec = CrcSpec::preset(name).expect("preset should exist");
            assert_eq!(spec.compute(CHECK), want, "{name} check value");
        }
    }

    #[test]
    fn every_listed_preset_resolves() {
        for name in CrcSpec::preset_names() {
            assert!(CrcSpec::preset(name).is_some(), "{name} should resolve");
        }
    }

    #[test]
    fn xor_and_sum_truncate_to_their_width() {
        assert_eq!(ChecksumSpec::Xor8.compute(&[0x0F, 0xF0]), 0xFF);
        assert_eq!(
            ChecksumSpec::Sum { width_bytes: 1 }.compute(&[0xFF, 0x02]),
            0x01
        );
        assert_eq!(
            ChecksumSpec::Sum { width_bytes: 2 }.compute(&[0xFF, 0x02]),
            0x0101
        );
    }
}
