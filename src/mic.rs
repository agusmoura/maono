//! Vendor HID protocol for the Maono PD100W wireless microphone receiver.
//!
//! Reverse-engineered from `libPD100XW.dylib` in Maono Link 3.6.9 and confirmed
//! against the hardware. Every frame is built the way `DataPackage::PackageRandomMessage`
//! builds it:
//!
//! ```text
//! c4 <len> 00 00 <type> [id_lo id_hi val_lo val_hi] <ck_lo ck_hi>   padded to 64
//! ```
//!
//! `len` counts the whole frame including the leading `c4` and the checksum, and
//! `ck` is the two's complement of everything before it. The app computes it as
//! `-(0xd2 + id_hi + id_lo + val_hi + val_lo)`, where `0xd2` is the header sum.
//!
//! The firmware validates nothing: it stores any 16-bit value you send. Ranges
//! below come from the app and from watching the physical controls, never from
//! probing the device.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::proto::{self, Frame};

pub const BATTERY: u16 = 0x0042; // percent
pub const LEVEL: u16 = 0x0044; // input meter, streamed ~10x/sec
pub const MUTE: u16 = 0x207D; // 1 = muted
pub const GAIN: u16 = 0x207E;
pub const NR: u16 = 0x2084; // noise reduction enable
pub const NR_LEVEL: u16 = 0x2085; // 0 low, 1 mid, 2 high
pub const LIGHT: u16 = 0x2089; // RGB light on/off
pub const LIGHT_MODE: u16 = 0x208C; // 0..=8, the order the light button cycles

pub const GAIN_MAX: u16 = 20;
pub const LIGHT_MODE_MAX: u16 = 8;
pub const NR_NAMES: [&str; 3] = ["low", "mid", "high"];

/// The colour each light mode produces, in the order the light button cycles
/// them. Taken from the hardware, not from the app.
pub const LIGHT_MODE_NAMES: [&str; 9] = [
    "white",
    "red",
    "orange",
    "lime",
    "green",
    "cyan",
    "blue",
    "purple",
    "light blue",
];

/// Resolve a colour name to its mode number. Accepts "light blue",
/// "lightblue" and "light-blue" alike.
pub fn light_mode_from_name(name: &str) -> Option<u16> {
    let want: String = name
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .collect();
    LIGHT_MODE_NAMES.iter().position(|n| {
        n.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            == want
    }).map(|i| i as u16)
}

/// Name for a mode number, or "?" when the device reports one we do not know.
pub fn light_mode_name(mode: u16) -> &'static str {
    LIGHT_MODE_NAMES.get(mode as usize).copied().unwrap_or("?")
}

pub struct Mic {
    file: File,
}

impl Mic {
    pub fn open() -> io::Result<Self> {
        let path = crate::device::find()
            .into_iter()
            .next()
            .map(|f| f.hidraw.display().to_string())
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "Maono PD100W not found - is it plugged in?"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc_o_nonblock())
            .open(&path)
            .map_err(|e| match e.kind() {
                ErrorKind::PermissionDenied => io::Error::new(
                    ErrorKind::PermissionDenied,
                    format!("{path} needs the udev rule - see 99-maono.rules"),
                ),
                _ => e,
            })?;
        Ok(Self { file })
    }

    /// Every frame in the next waiting report; empty when nothing is waiting.
    fn try_read(&mut self) -> Vec<Frame> {
        let mut buf = [0u8; 512];
        match self.file.read(&mut buf) {
            Ok(n) if n > 0 => proto::decode(&buf[..n]),
            _ => Vec::new(),
        }
    }

    /// Write a field. Refused (InvalidInput) unless `safety` allows the value.
    /// The device does not acknowledge, so callers that care read it back.
    pub fn set(&mut self, id: u16, val: u16) -> io::Result<()> {
        let val = crate::safety::check(id, val as i64)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e.to_string()))?;
        self.file.write_all(&proto::set(&[(id, val)]))
    }

    /// Query one field, ignoring the level-meter notifications streaming past.
    pub fn get(&mut self, id: u16) -> io::Result<Option<u16>> {
        self.get_within(id, Duration::from_millis(1500))
    }

    fn get_within(&mut self, id: u16, timeout: Duration) -> io::Result<Option<u16>> {
        self.file.write_all(&proto::get(id))?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let frames = self.try_read();
            if frames.is_empty() {
                sleep(Duration::from_millis(2));
                continue;
            }
            let hit = frames
                .iter()
                .filter(|f| f.kind == proto::GET)
                .flat_map(|f| &f.fields)
                .find(|(f, _)| *f == id)
                .map(|&(_, v)| v);
            if hit.is_some() {
                return Ok(hit);
            }
        }
        Ok(None)
    }

    /// Like `get`, but gives up quickly. Used when sweeping a whole id range,
    /// where most ids will not answer at all.
    pub fn get_quick(&mut self, id: u16) -> io::Result<Option<u16>> {
        self.get_within(id, Duration::from_millis(250))
    }

    /// Set a field, then read it back to confirm it landed.
    pub fn set_verify(&mut self, id: u16, val: u16) -> io::Result<Option<u16>> {
        self.set(id, val)?;
        sleep(Duration::from_millis(350));
        self.get(id)
    }

    /// Drain every notification currently buffered. Used by the TUI to follow
    /// the level meter and to notice the physical buttons being pressed.
    pub fn drain(&mut self) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        loop {
            let frames = self.try_read();
            if frames.is_empty() {
                return out;
            }
            out.extend(frames.into_iter().flat_map(|f| f.fields));
        }
    }
}

/// `O_NONBLOCK` without pulling in the `libc` crate for one constant.
pub(crate) const fn libc_o_nonblock() -> i32 {
    0o4000
}

/// The input meter reports the same byte twice; the low byte is the level.
/// Peaks land around 0x3f in practice, so that is the full-scale reference.
pub fn level_fraction(raw: u16) -> f64 {
    ((raw & 0xFF) as f64 / 63.0).clamp(0.0, 1.0)
}
