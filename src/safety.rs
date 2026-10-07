//! The only gate in front of the device. Every write path calls `check`
//! before a frame is built. Nothing (descriptor, profile, CLI flag) can widen
//! this table; it is compiled in on purpose.

#[derive(Debug, Clone, Copy)]
pub enum Allowed {
    Range(u16, u16),
    OneOf(&'static [u16]),
}

/// Writable fields of the PD100W (TX1 / wired slot) and what they accept, in
/// raw device units. Sources: docs/protocol/pd100w.md §4 and §8.
pub fn rule(id: u16) -> Option<Allowed> {
    use Allowed::*;
    Some(match id {
        0x0045 => Range(0, 1),                   // level-meter stream: 1 = off
        0x207D | 0x2084 | 0x2089 => Range(0, 1), // mute, NR on, light on
        0x207E | 0x207F => Range(0, 20),         // gain, headphone volume
        0x2085 => Range(0, 2),                   // NR level
        0x20AF => OneOf(&[0, 1, 2, 4, 7]),       // monitor sources (enum, not bits)
        0x208A => Range(1, 100),                 // brightness
        0x208B => Range(0, 2),                   // light effect
        0x208C => Range(0, 12),                  // colour: 0-7 presets, 8+k custom slot k
        0x208E => Range(0, 5),                   // custom colour count
        0x208F..=0x209D => {
            if (id - 0x208F) % 3 == 0 {
                Range(0, 35_900) // hue x100
            } else {
                Range(0, 100) // saturation / value x100
            }
        }
        // Experimental DSP: no audible effect observed on the USB stream (§8).
        0x2069 | 0x206E | 0x2082 => Range(0, 1),
        0x206A | 0x206F => Range(1520, 2000), // -48..0 dB as dB*10+2000
        0x206B => Range(0, 400),
        0x206C => Range(0, 2000),
        0x206D => Range(1, 12),
        0x2070 => Range(5, 400),
        0x2071 => Range(5, 2000),
        0x2083 => Range(0, 9),
        0x2023..=0x2045 => match (id - 0x2023) % 5 {
            0 => Range(0, 1),               // enable
            1 => OneOf(&[0, 1, 2, 3, 5]),   // type
            2 => Range(20, 20_000),         // Hz
            3 => Range(1880, 2120),         // ±12 dB as dB*10+2000
            _ => Range(10, 2000),           // Q x100
        },
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    UnknownId(u16),
    OutOfRange { id: u16, value: i64 },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Refused::UnknownId(id) => write!(f, "0x{id:04x} is not a writable field"),
            Refused::OutOfRange { id, value } => write!(f, "0x{id:04x} does not accept {value}"),
        }
    }
}

impl std::error::Error for Refused {}

pub fn check(id: u16, value: i64) -> Result<u16, Refused> {
    let ok = match rule(id).ok_or(Refused::UnknownId(id))? {
        Allowed::Range(lo, hi) => (lo as i64..=hi as i64).contains(&value),
        Allowed::OneOf(set) => set.iter().any(|&v| v as i64 == value),
    };
    if ok { Ok(value as u16) } else { Err(Refused::OutOfRange { id, value }) }
}

pub fn check_all(pairs: &[(u16, i64)]) -> Result<Vec<(u16, u16)>, Refused> {
    pairs.iter().map(|&(id, v)| check(id, v).map(|v| (id, v))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_and_unknown_ids_are_refused() {
        for id in [0x100A, 0x001E, 0x002D, 0x2046, 0x2068, 0x20B4, 0x2086, 0x0000] {
            assert_eq!(check(id, 0), Err(Refused::UnknownId(id)), "{id:#06x}");
        }
    }

    #[test]
    fn ranges_and_enums_are_enforced() {
        assert_eq!(check(0x207E, 20), Ok(20));
        assert!(matches!(check(0x207E, 21), Err(Refused::OutOfRange { .. })));
        assert!(matches!(check(0x207E, -1), Err(Refused::OutOfRange { .. })));
        assert_eq!(check(0x20AF, 4), Ok(4));
        assert!(check(0x20AF, 3).is_err());
        assert_eq!(check(0x208F, 35_900), Ok(35_900)); // hue x100
        assert!(check(0x2090, 101).is_err()); // saturation x100
        assert!(check(0x2024, 4).is_err()); // EQ type 4 does not exist on the device
        assert_eq!(check(0x2026, 1880), Ok(1880)); // EQ gain -12 dB
        assert!(check(0x208A, 0).is_err()); // brightness starts at 1
    }

    #[test]
    fn check_all_is_all_or_nothing() {
        assert!(check_all(&[(0x207E, 10), (0x100A, 3)]).is_err());
        assert_eq!(check_all(&[(0x207E, 10), (0x2089, 1)]), Ok(vec![(0x207E, 10), (0x2089, 1)]));
    }
}
