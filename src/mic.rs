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

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// USB ids as they appear in a hidraw uevent's `HID_ID`: the wireless
/// receiver (0414) and the mic itself plugged in by cable (0417).
const HID_MATCH: [&str; 2] = ["352F:0414", "352F:0417"];

const SET: u8 = 0x03; // host -> device, and what the device uses to notify
const GET: u8 = 0x04; // host -> device, device answers with the same type
const REPORT_LEN: usize = 64;

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

/// Locate the receiver's hidraw node. The number moves between replugs, so we
/// match on the USB ids rather than assuming `hidraw0`.
pub fn find_device() -> Option<String> {
    let mut nodes: Vec<_> = fs::read_dir("/sys/class/hidraw")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    nodes.sort();
    for node in nodes {
        let uevent = node.join("device/uevent");
        if let Ok(text) = fs::read_to_string(&uevent) {
            // HID_ID looks like 0003:0000352F:00000414
            let id = text.to_uppercase().replace("0000", "");
            if HID_MATCH.iter().any(|m| id.contains(m)) {
                let name = node.file_name()?.to_str()?.to_string();
                return Some(format!("/dev/{name}"));
            }
        }
    }
    None
}

fn checksum(bytes: &[u8]) -> u16 {
    let sum: u32 = bytes.iter().map(|&b| b as u32).sum();
    (0x1_0000u32.wrapping_sub(sum) & 0xFFFF) as u16
}

fn build(msg_type: u8, id: u16, val: u16) -> [u8; REPORT_LEN] {
    let mut f = [0u8; REPORT_LEN];
    f[..9].copy_from_slice(&[
        0xC4,
        0x0B,
        0x00,
        0x00,
        msg_type,
        id as u8,
        (id >> 8) as u8,
        val as u8,
        (val >> 8) as u8,
    ]);
    let ck = checksum(&f[..9]);
    f[9] = ck as u8;
    f[10] = (ck >> 8) as u8;
    f
}

/// Decode one report into its message type and `(id, value)` pairs.
/// Returns `None` when the frame is malformed or fails its checksum.
fn parse(buf: &[u8]) -> Option<(u8, Vec<(u16, u16)>)> {
    if buf.len() < 7 || buf[0] != 0xC4 {
        return None;
    }
    let len = buf[1] as usize;
    if !(7..=buf.len()).contains(&len) {
        return None;
    }
    let frame = &buf[..len];
    let got = u16::from_le_bytes([frame[len - 2], frame[len - 1]]);
    if got != checksum(&frame[..len - 2]) {
        return None;
    }
    let body = &frame[5..len - 2];
    if body.len() % 4 != 0 {
        return None;
    }
    let fields = body
        .chunks_exact(4)
        .map(|c| {
            (
                u16::from_le_bytes([c[0], c[1]]),
                u16::from_le_bytes([c[2], c[3]]),
            )
        })
        .collect();
    Some((frame[4], fields))
}

pub struct Mic {
    file: File,
}

impl Mic {
    pub fn open() -> io::Result<Self> {
        let path = find_device().ok_or_else(|| {
            io::Error::new(
                ErrorKind::NotFound,
                "Maono PD100W receiver not found - is it plugged in?",
            )
        })?;
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

    fn send(&mut self, msg_type: u8, id: u16, val: u16) -> io::Result<()> {
        self.file.write_all(&build(msg_type, id, val))
    }

    /// Read one report, or `None` if nothing is waiting.
    fn try_read(&mut self) -> Option<(u8, Vec<(u16, u16)>)> {
        let mut buf = [0u8; 512];
        match self.file.read(&mut buf) {
            Ok(n) if n > 0 => parse(&buf[..n]),
            _ => None,
        }
    }

    /// Write a field. The device does not acknowledge, so callers that care
    /// should read the value back.
    pub fn set(&mut self, id: u16, val: u16) -> io::Result<()> {
        self.send(SET, id, val)
    }

    /// Query one field, ignoring the level-meter notifications streaming past.
    pub fn get(&mut self, id: u16) -> io::Result<Option<u16>> {
        self.get_within(id, Duration::from_millis(1500))
    }

    fn get_within(&mut self, id: u16, timeout: Duration) -> io::Result<Option<u16>> {
        self.send(GET, id, 0)?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.try_read() {
                Some((msg_type, fields)) if msg_type == GET => {
                    if let Some((_, v)) = fields.into_iter().find(|(f, _)| *f == id) {
                        return Ok(Some(v));
                    }
                }
                Some(_) => continue,
                None => sleep(Duration::from_millis(2)),
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
        while let Some((_, fields)) = self.try_read() {
            out.extend(fields);
        }
        out
    }
}

/// `O_NONBLOCK` without pulling in the `libc` crate for one constant.
const fn libc_o_nonblock() -> i32 {
    0o4000
}

/// The input meter reports the same byte twice; the low byte is the level.
/// Peaks land around 0x3f in practice, so that is the full-scale reference.
pub fn level_fraction(raw: u16) -> f64 {
    ((raw & 0xFF) as f64 / 63.0).clamp(0.0, 1.0)
}
