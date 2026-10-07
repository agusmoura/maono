# maono backend (serve, profiles, PipeWire filter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the `maono` Rust CLI into the single owner of the PD100W: a safety-gated device layer, a data-driven descriptor, profiles, a generated PipeWire filter chain, and a `maono serve` JSONL process that the Omarchy panel (plan 2) and the CLI both talk to.

**Architecture:** Pure modules first (frame codec, allowlist, descriptor, filter model), then device I/O behind a `Link` trait with an in-memory fake, PipeWire behind an `Audio` trait with a fake, then `Core` (all command logic, unit-tested with both fakes), then the thin `serve` loop (stdin + Unix socket + device polling) and the CLI client. Every write to the mic goes through `safety::check`; every file write is atomic.

**Tech Stack:** Rust 2024 (toolchain via mise, 1.99), `serde` + `serde_json` (new), `ratatui`/`crossterm` (existing TUI), PipeWire 1.6 `filter-chain` (builtin biquads, LADSPA RNNoise, LADSPA LSP Compressor Mono), `pw-dump`, `pw-cli`, `pactl`, `systemctl --user`.

**Spec:** `docs/superpowers/specs/2026-10-07-omarchy-panel-design.md` (rev 2). Protocol facts: `docs/protocol/pd100w.md` (§8 = calibration).

**Repo:** `~/dev/maono`, branch `omarchy-panel`. Cargo is not on PATH; every cargo command in this plan is `mise exec rust@stable -- cargo …`.

## Global Constraints

- Rust edition 2024, `rust-version = "1.89"` (std `File::try_lock`). New dependencies: `serde` (derive) and `serde_json` only.
- Nothing writes to the hidraw node except through `safety::check` / `safety::check_all`. There is no raw `set` anymore. `get` and `scan` stay read-only.
- Never written: `0x100A` (factory reset), serial `0x001E–0x003D`, factory EQ `0x2046–0x2068`, `0x20B4`, any id not in `safety::rule`.
- Profiles never contain `mic.mute`. Reapplying a profile never unmutes.
- The filter chain is mono, 48 kHz, captures with `node.dont-fallback = true` and `node.linger = true`, and finds the mic's PipeWire source by USB properties (`device.vendor.id = 0x352f`, `device.product.id` `0x0417` preferred over `0x0414`), never by building a name.
- JSONL protocol version 1: one JSON object per line, stdout carries only JSONL, logs go to stderr. Every request gets exactly one `ack`.
- Files: `~/.config/maono/{config.json, filter.json, profiles/<id>.json, eq/<id>.json}`, `~/.config/pipewire/filter-chain.conf.d/maono-clean.conf`; socket and lock in `$XDG_RUNTIME_DIR` (`maono.sock`, `maono.lock`). All file writes are atomic (temp file + rename). Corrupt files are reported, never deleted.
- Ranges, options, presets, plugin labels/ports and factory profiles live in embedded JSON data (`devices/`, `presets/`, `profiles/`), not in Rust literals. Only the safety allowlist is deliberately compiled in.
- Real-mic write tests happen only in Task 13, after Agus explicitly authorizes them.

## Deliberate simplifications vs the spec

- No 0x05/0x06 range frames: multi-field SET covers every write we make (EQ slots, custom colours), and reads are single GETs.
- The filter conf is rewritten on every accepted change (atomic, no restart) instead of after a 1 s debounce; the panel already debounces sliders at 120 ms.
- `filter.bypass` is not a separate command: `filter.set {"enabled": false}` / `{"eq.on": false}` / `{"rnnoise.on": false}` / `{"comp.on": false}`.
- The HID level meter is switched off on connect (0x0045 = 1); the panel meters PipeWire instead (spec rev 2, item 14).

## Review Focus

1. Mic unplugged in the middle of applying a profile → ack `ok:false` with `applied`/`failed`, `partial` set, `activeProfile` unchanged, link `disconnected`, no hang. Test: Task 10 `apply_profile_unplugged_midway_reports_partial`.
2. A hand-edited profile with a forbidden or out-of-range key (`mic.mute`, gain 99) → rejected before anything is written. Tests: Task 9 `validate_rejects_bad_profiles`, Task 10 `invalid_profile_writes_nothing`.
3. filter-chain does not bring the node up after a restart → previous conf restored, filter reported failed, mic part still applied. Tests: Task 8 `broken_restart_restores_previous_conf`, Task 10 `profile_with_broken_filter_is_partial`.
4. hidraw permission denied (udev rule missing) → `state.device == "permission"`, serve keeps running, keeps probing. Test: Task 11 `permission_denied_is_reported_and_loop_survives`.
5. Two writers at once (panel slider + a Hyprland keybind going through the CLI socket) → requests run one at a time and each client gets its own ack. Test: Task 11 `queued_requests_run_in_order_with_own_acks`.

---

### Task 1: Frame codec (`proto.rs`) and dependencies

**Files:**
- Modify: `Cargo.toml`
- Create: `src/proto.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `proto::{REPORT_LEN, SET, GET, MAX_PAIRS, Frame { kind: u8, fields: Vec<(u16,u16)> }, encode(kind, &[(u16,u16)]) -> [u8;64], set(&[(u16,u16)]) -> [u8;64], get(u16) -> [u8;64], decode(&[u8]) -> Vec<Frame>}`

- [ ] **Step 1: Add dependencies and rust-version**

In `Cargo.toml`, under `[package]` add `rust-version = "1.89"`, and make `[dependencies]`:

```toml
[dependencies]
crossterm = "0.29.0"
ratatui = "0.30.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Write the failing tests**

Create `src/proto.rs` with only the tests for now, and add `pub mod proto;` to `src/lib.rs`:

```rust
//! Frame codec for the vendor HID channel: report id 0xC4, 64-byte reports.
//!
//! ```text
//! c4 <len> 00 00 <kind> [id_lo id_hi val_lo val_hi]... <ck_lo ck_hi>   padded to 64
//! ```
//! `len` counts the whole frame including `c4` and the checksum; `ck` is the
//! two's complement of the sum of every byte before it.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_set_matches_known_bytes() {
        let f = set(&[(0x207D, 1)]);
        assert_eq!(&f[..11], &[0xC4, 0x0B, 0, 0, 0x03, 0x7D, 0x20, 0x01, 0x00, 0x90, 0xFE]);
        assert!(f[11..].iter().all(|&b| b == 0));
    }

    #[test]
    fn multi_set_roundtrips() {
        let pairs = [(0x2041, 1), (0x2042, 3), (0x2043, 20000)];
        let f = set(&pairs);
        assert_eq!(f[1], 19);
        assert_eq!(decode(&f), vec![Frame { kind: SET, fields: pairs.to_vec() }]);
    }

    #[test]
    fn decode_splits_concatenated_frames() {
        let a = get(0x0042);
        let b = set(&[(0x207D, 0)]);
        let mut buf = a[..11].to_vec();
        buf.extend_from_slice(&b[..11]);
        buf.resize(REPORT_LEN, 0);
        let frames = decode(&buf);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], Frame { kind: GET, fields: vec![(0x0042, 0)] });
        assert_eq!(frames[1].kind, SET);
    }

    #[test]
    fn decode_rejects_bad_checksum() {
        let mut f = set(&[(0x207D, 1)]);
        f[9] ^= 0xFF;
        assert!(decode(&f).is_empty());
    }

    #[test]
    #[should_panic]
    fn encode_refuses_too_many_pairs() {
        let pairs = vec![(0x208F_u16, 0_u16); MAX_PAIRS + 1];
        set(&pairs);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib proto`
Expected: compile errors `cannot find function 'set' in this scope` (and `get`, `decode`, `Frame`).

- [ ] **Step 4: Implement**

Put this above the `#[cfg(test)]` block in `src/proto.rs`:

```rust
pub const REPORT_LEN: usize = 64;
pub const SET: u8 = 0x03; // host -> device; the device also uses it to announce button presses
pub const GET: u8 = 0x04; // host -> device; the device answers with the same kind
/// Pairs that fit in one frame: (64 - 7) / 4.
pub const MAX_PAIRS: usize = 14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub fields: Vec<(u16, u16)>,
}

fn checksum(bytes: &[u8]) -> u16 {
    let sum: u32 = bytes.iter().map(|&b| b as u32).sum();
    (0x1_0000u32.wrapping_sub(sum) & 0xFFFF) as u16
}

pub fn encode(kind: u8, pairs: &[(u16, u16)]) -> [u8; REPORT_LEN] {
    assert!(!pairs.is_empty() && pairs.len() <= MAX_PAIRS, "1..=14 pairs per frame");
    let len = 7 + 4 * pairs.len();
    let mut f = [0u8; REPORT_LEN];
    f[0] = 0xC4;
    f[1] = len as u8;
    f[4] = kind;
    for (i, &(id, val)) in pairs.iter().enumerate() {
        let o = 5 + 4 * i;
        f[o..o + 4].copy_from_slice(&[id as u8, (id >> 8) as u8, val as u8, (val >> 8) as u8]);
    }
    let ck = checksum(&f[..len - 2]);
    f[len - 2] = ck as u8;
    f[len - 1] = (ck >> 8) as u8;
    f
}

pub fn set(pairs: &[(u16, u16)]) -> [u8; REPORT_LEN] {
    encode(SET, pairs)
}

/// Single-field read in the 11-byte form maono has always sent.
pub fn get(id: u16) -> [u8; REPORT_LEN] {
    encode(GET, &[(id, 0)])
}

/// Split one input report into frames. A report can carry several frames back
/// to back; a bad checksum or impossible length ends the scan (padding).
pub fn decode(buf: &[u8]) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= buf.len() && buf[i] == 0xC4 {
        let len = buf[i + 1] as usize;
        if len < 7 || i + len > buf.len() {
            break;
        }
        let f = &buf[i..i + len];
        if u16::from_le_bytes([f[len - 2], f[len - 1]]) != checksum(&f[..len - 2]) {
            break;
        }
        let body = &f[5..len - 2];
        if body.len() % 4 == 0 {
            out.push(Frame {
                kind: f[4],
                fields: body
                    .chunks_exact(4)
                    .map(|c| (u16::from_le_bytes([c[0], c[1]]), u16::from_le_bytes([c[2], c[3]])))
                    .collect(),
            });
        }
        i += len;
    }
    out
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib proto`
Expected: `5 passed`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/proto.rs src/lib.rs
git commit -m "feat: frame codec with multi-field SET and concatenated-frame decode

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Safety allowlist (`safety.rs`) and route `Mic` through it

**Files:**
- Create: `src/safety.rs`
- Modify: `src/lib.rs` (add `pub mod safety;`)
- Modify: `src/mic.rs` (drop its own codec, use `proto` + `safety`)

**Interfaces:**
- Consumes: `proto::{set, get, decode, Frame, GET}`
- Produces: `safety::{Allowed, rule(u16) -> Option<Allowed>, Refused { UnknownId(u16), OutOfRange { id: u16, value: i64 } }, check(u16, i64) -> Result<u16, Refused>, check_all(&[(u16, i64)]) -> Result<Vec<(u16,u16)>, Refused>}`

- [ ] **Step 1: Write the failing tests**

Create `src/safety.rs`:

```rust
//! The only gate in front of the device. Every write path calls `check`
//! before a frame is built. Nothing (descriptor, profile, CLI flag) can widen
//! this table; it is compiled in on purpose.

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
```

Add `pub mod safety;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib safety`
Expected: compile errors `cannot find function 'check'`.

- [ ] **Step 3: Implement**

Above the tests in `src/safety.rs`:

```rust
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
```

- [ ] **Step 4: Route `Mic` through `proto` and `safety`**

In `src/mic.rs`:
1. Delete `const SET`, `const GET`, `const REPORT_LEN`, `fn checksum`, `fn build`, `fn parse` and `fn send`.
2. Add `use crate::proto::{self, Frame};` to the imports.
3. Replace `try_read`, `set`, `get_within` and `drain` with:

```rust
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
```

- [ ] **Step 5: Run tests and build**

Run: `mise exec rust@stable -- cargo test --lib && mise exec rust@stable -- cargo build`
Expected: all tests pass (`8 passed`); the binary builds with no errors.

- [ ] **Step 6: Commit**

```bash
git add src/safety.rs src/lib.rs src/mic.rs
git commit -m "feat: compiled-in write allowlist; Mic writes go through it

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Device descriptor (`devices/pd100w.json`, `descriptor.rs`)

**Files:**
- Create: `devices/pd100w.json`
- Create: `src/descriptor.rs`
- Modify: `src/lib.rs` (add `pub mod descriptor;`)

**Interfaces:**
- Consumes: `safety::check`
- Produces:
  - `descriptor::Kind { Bool, Int, Num, Enum, Version }`
  - `descriptor::Encoding { Raw, Db10_2000, X100 }`
  - `descriptor::Field { key: String, id: u16, kind: Kind, min/max/step: Option<f64>, unit: Option<String>, encoding: Encoding, options: Vec<Opt>, group: String, status: String, profile: bool, mask: Option<String> }` with `to_raw(&Value) -> Result<u16, String>` and `from_raw(u16) -> Value`
  - `descriptor::{Opt { value: i64, label: String }, LightMeta { color: u16, count: u16, base: u16, custom_max: u16, presets: Vec<ColorPreset> }, ColorPreset { value: u16, name: String, hex: String }, Span { start: u16, count: u16 }, Meter { id: u16, off: u16 }}`
  - `descriptor::Descriptor { fields: Vec<Field>, readonly: Vec<Field>, light: LightMeta, serial: Span, meter: Meter }` with `load()`, `parse(&str)`, `field(&str) -> Option<&Field>`, `field_by_id(u16) -> Option<&Field>`, `schema() -> Value`

- [ ] **Step 1: Create the data file**

`devices/pd100w.json`:

```json
{
  "device": "Maono PD100W",
  "fields": [
    {"key": "mic.mute", "id": "0x207D", "type": "bool", "group": "voice", "status": "verified"},
    {"key": "mic.gain", "id": "0x207E", "type": "int", "min": 0, "max": 20, "step": 1, "group": "voice", "status": "verified", "profile": true},
    {"key": "mic.nr.on", "id": "0x2084", "type": "bool", "group": "voice", "status": "verified", "profile": true},
    {"key": "mic.nr.level", "id": "0x2085", "type": "enum", "options": [{"value": 0, "label": "nr.low"}, {"value": 1, "label": "nr.mid"}, {"value": 2, "label": "nr.high"}], "group": "voice", "status": "verified", "profile": true},
    {"key": "headphones.volume", "id": "0x207F", "type": "int", "min": 0, "max": 20, "step": 1, "group": "headphones", "status": "experimental", "profile": true},
    {"key": "headphones.monitor", "id": "0x20AF", "type": "enum", "options": [{"value": 0, "label": "monitor.none"}, {"value": 1, "label": "monitor.voice"}, {"value": 2, "label": "monitor.pc"}, {"value": 4, "label": "monitor.both"}, {"value": 7, "label": "monitor.all"}], "group": "headphones", "status": "experimental", "profile": true},
    {"key": "light.on", "id": "0x2089", "type": "bool", "group": "light", "status": "verified", "profile": true},
    {"key": "light.brightness", "id": "0x208A", "type": "int", "min": 1, "max": 100, "step": 1, "unit": "%", "group": "light", "status": "verified", "profile": true},
    {"key": "light.effect", "id": "0x208B", "type": "enum", "options": [{"value": 0, "label": "effect.fixed"}, {"value": 1, "label": "effect.breathe"}, {"value": 2, "label": "effect.cycle"}], "group": "light", "status": "verified", "profile": true},
    {"key": "dsp.comp.on", "id": "0x2069", "type": "bool", "group": "dsp", "status": "experimental"},
    {"key": "dsp.comp.threshold", "id": "0x206A", "type": "int", "min": -48, "max": 0, "step": 1, "unit": "dB", "encoding": "db10_2000", "group": "dsp", "status": "experimental"},
    {"key": "dsp.comp.attack", "id": "0x206B", "type": "int", "min": 0, "max": 400, "step": 1, "unit": "ms", "group": "dsp", "status": "experimental"},
    {"key": "dsp.comp.release", "id": "0x206C", "type": "int", "min": 0, "max": 2000, "step": 1, "unit": "ms", "group": "dsp", "status": "experimental"},
    {"key": "dsp.comp.ratio", "id": "0x206D", "type": "int", "min": 1, "max": 12, "step": 1, "group": "dsp", "status": "experimental"},
    {"key": "dsp.limiter.on", "id": "0x206E", "type": "bool", "group": "dsp", "status": "experimental"},
    {"key": "dsp.limiter.threshold", "id": "0x206F", "type": "int", "min": -48, "max": 0, "step": 1, "unit": "dB", "encoding": "db10_2000", "group": "dsp", "status": "experimental"},
    {"key": "dsp.limiter.attack", "id": "0x2070", "type": "int", "min": 5, "max": 400, "step": 1, "unit": "ms", "group": "dsp", "status": "experimental"},
    {"key": "dsp.limiter.release", "id": "0x2071", "type": "int", "min": 5, "max": 2000, "step": 1, "unit": "ms", "group": "dsp", "status": "experimental"},
    {"key": "dsp.reverb.on", "id": "0x2082", "type": "bool", "group": "dsp", "status": "experimental"},
    {"key": "dsp.reverb.level", "id": "0x2083", "type": "int", "min": 0, "max": 9, "step": 1, "group": "dsp", "status": "experimental"}
  ],
  "eqSlots": {
    "keyPrefix": "dsp.eq", "base": "0x2023", "count": 7, "status": "experimental",
    "fields": [
      {"name": "enable", "type": "bool"},
      {"name": "type", "type": "enum", "options": [{"value": 1, "label": "eq.peak"}, {"value": 0, "label": "eq.lowshelf"}, {"value": 2, "label": "eq.highshelf"}, {"value": 3, "label": "eq.lowpass"}, {"value": 5, "label": "eq.highpass"}]},
      {"name": "freq", "type": "int", "min": 20, "max": 20000, "step": 1, "unit": "Hz"},
      {"name": "gain", "type": "num", "min": -12, "max": 12, "step": 0.1, "unit": "dB", "encoding": "db10_2000"},
      {"name": "q", "type": "num", "min": 0.1, "max": 20, "step": 0.01, "encoding": "x100"}
    ]
  },
  "readonly": [
    {"key": "info.battery", "id": "0x0042", "type": "int", "mask": "low", "unit": "%", "group": "info", "status": "verified"},
    {"key": "info.charging", "id": "0x0049", "type": "bool", "mask": "low", "group": "info", "status": "verified"},
    {"key": "info.firmware", "id": "0x000B", "type": "version", "group": "info", "status": "verified"}
  ],
  "serial": {"start": "0x001E", "count": 16},
  "meter": {"id": "0x0045", "off": 1},
  "light": {
    "color": "0x208C", "count": "0x208E", "base": "0x208F", "customMax": 5,
    "presets": [
      {"value": 0, "name": "white", "hex": "#FFFFFF"},
      {"value": 1, "name": "red", "hex": "#FF0000"},
      {"value": 2, "name": "orange", "hex": "#FFA500"},
      {"value": 3, "name": "yellow", "hex": "#FFFF00"},
      {"value": 4, "name": "green", "hex": "#00FF00"},
      {"value": 5, "name": "cyan", "hex": "#00FFFF"},
      {"value": 6, "name": "blue", "hex": "#0000FF"},
      {"value": 7, "name": "magenta", "hex": "#FF00FF"}
    ]
  }
}
```

- [ ] **Step 2: Write the failing tests**

Create `src/descriptor.rs` with the tests and add `pub mod descriptor;` to `src/lib.rs`:

```rust
//! What the mic exposes, as data: keys, ids, ranges, encodings, options.
//! The UI renders from `schema()`; writes are validated here and then again by
//! `safety` (which this file can never widen).

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_writable_value_the_descriptor_allows_passes_safety() {
        let d = Descriptor::load();
        for f in &d.fields {
            let candidates: Vec<serde_json::Value> = match f.kind {
                Kind::Bool => vec![json!(false), json!(true)],
                Kind::Enum => f.options.iter().map(|o| json!(o.value)).collect(),
                _ => vec![json!(f.min.unwrap()), json!(f.max.unwrap())],
            };
            for v in candidates {
                assert!(f.to_raw(&v).is_ok(), "{} = {v}: {:?}", f.key, f.to_raw(&v));
            }
        }
    }

    #[test]
    fn eq_slots_expand_to_35_fields() {
        let d = Descriptor::load();
        assert_eq!(d.fields.iter().filter(|f| f.key.starts_with("dsp.eq.")).count(), 35);
        assert_eq!(d.field("dsp.eq.6.freq").unwrap().id, 0x2023 + 30 + 2);
    }

    #[test]
    fn to_raw_rejects_bad_values() {
        let d = Descriptor::load();
        assert!(d.field("mic.gain").unwrap().to_raw(&json!(21)).is_err());
        assert!(d.field("mic.gain").unwrap().to_raw(&json!(1.5)).is_err());
        assert!(d.field("mic.nr.level").unwrap().to_raw(&json!(3)).is_err());
        assert!(d.field("light.brightness").unwrap().to_raw(&json!(0)).is_err());
        assert!(d.field("mic.mute").unwrap().to_raw(&json!("yes")).is_err());
        assert!(d.field("headphones.monitor").unwrap().to_raw(&json!(3)).is_err());
    }

    #[test]
    fn encodings_roundtrip() {
        let d = Descriptor::load();
        let thr = d.field("dsp.comp.threshold").unwrap();
        assert_eq!(thr.to_raw(&json!(-20)), Ok(1800));
        assert_eq!(thr.from_raw(1800), json!(-20));
        let gain = d.field("dsp.eq.0.gain").unwrap();
        assert_eq!(gain.to_raw(&json!(3.5)), Ok(2035));
        assert_eq!(gain.from_raw(2035), json!(3.5));
        assert_eq!(d.field("dsp.eq.0.q").unwrap().from_raw(71), json!(0.71));
        assert_eq!(d.field("info.firmware").unwrap().from_raw(108), json!("1.0.8"));
        assert_eq!(d.field("info.battery").unwrap().from_raw(0x3C46), json!(70));
        assert_eq!(d.field("mic.mute").unwrap().from_raw(1), json!(true));
    }

    #[test]
    fn light_and_profile_metadata() {
        let d = Descriptor::load();
        assert_eq!(d.light.presets.len(), 8);
        assert_eq!(d.light.custom_max, 5);
        assert!(!d.field("mic.mute").unwrap().profile);
        assert!(d.field("mic.gain").unwrap().profile);
        assert_eq!(d.field_by_id(0x207E).unwrap().key, "mic.gain");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib descriptor`
Expected: compile errors `cannot find type 'Descriptor'`.

- [ ] **Step 4: Implement**

Above the tests in `src/descriptor.rs`:

```rust
use crate::safety;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use std::collections::HashMap;

fn hex_u16<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    let s = String::deserialize(d)?;
    u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
}

fn ser_hex<S: Serializer>(id: &u16, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("0x{id:04X}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Num,
    Enum,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Encoding {
    #[default]
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "db10_2000")]
    Db10_2000,
    #[serde(rename = "x100")]
    X100,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opt {
    pub value: i64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub id: u16,
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub encoding: Encoding,
    #[serde(default)]
    pub options: Vec<Opt>,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub profile: bool,
    #[serde(default)]
    pub mask: Option<String>,
}

impl Field {
    /// UI value -> raw device value. Checks the field's own domain, then `safety`.
    pub fn to_raw(&self, v: &Value) -> Result<u16, String> {
        let n = match (self.kind, v) {
            (Kind::Bool, Value::Bool(b)) => *b as u8 as f64,
            (Kind::Bool, Value::Number(n)) if matches!(n.as_i64(), Some(0 | 1)) => n.as_f64().unwrap(),
            (Kind::Int | Kind::Num | Kind::Enum, Value::Number(n)) => n.as_f64().unwrap(),
            _ => return Err(format!("{}: {v} is not a valid {:?}", self.key, self.kind)),
        };
        if let (Some(lo), Some(hi)) = (self.min, self.max) {
            if !(lo..=hi).contains(&n) {
                return Err(format!("{}: {n} is outside {lo}..{hi}", self.key));
            }
        }
        if matches!(self.kind, Kind::Int | Kind::Enum) && n.fract() != 0.0 {
            return Err(format!("{}: {n} is not a whole number", self.key));
        }
        if let (Kind::Int, Some(step), Some(lo)) = (self.kind, self.step, self.min) {
            if step >= 1.0 && ((n - lo) % step) != 0.0 {
                return Err(format!("{}: {n} is not a multiple of {step}", self.key));
            }
        }
        if self.kind == Kind::Enum && !self.options.iter().any(|o| o.value as f64 == n) {
            return Err(format!("{}: {n} is not one of the options", self.key));
        }
        let raw = match self.encoding {
            Encoding::Raw => n.round(),
            Encoding::Db10_2000 => (n * 10.0).round() + 2000.0,
            Encoding::X100 => (n * 100.0).round(),
        };
        safety::check(self.id, raw as i64).map_err(|e| format!("{}: {e}", self.key))
    }

    pub fn from_raw(&self, raw: u16) -> Value {
        let raw = if self.mask.as_deref() == Some("low") { raw & 0xFF } else { raw };
        match self.kind {
            Kind::Bool => Value::Bool(raw != 0),
            Kind::Version => json!(raw.to_string().chars().map(String::from).collect::<Vec<_>>().join(".")),
            _ => {
                let v = match self.encoding {
                    Encoding::Raw => raw as f64,
                    Encoding::Db10_2000 => (raw as f64 - 2000.0) / 10.0,
                    Encoding::X100 => raw as f64 / 100.0,
                };
                if v.fract() == 0.0 { json!(v as i64) } else { json!((v * 100.0).round() / 100.0) }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorPreset {
    pub value: u16,
    pub name: String,
    pub hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightMeta {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub color: u16,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub count: u16,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub base: u16,
    pub custom_max: u16,
    pub presets: Vec<ColorPreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub start: u16,
    pub count: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meter {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub id: u16,
    pub off: u16,
}

#[derive(Debug, Clone, Deserialize)]
struct SlotField {
    name: String,
    #[serde(rename = "type")]
    kind: Kind,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    encoding: Encoding,
    #[serde(default)]
    options: Vec<Opt>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EqSlots {
    key_prefix: String,
    #[serde(deserialize_with = "hex_u16")]
    base: u16,
    count: u16,
    status: String,
    fields: Vec<SlotField>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    fields: Vec<Field>,
    eq_slots: EqSlots,
    readonly: Vec<Field>,
    serial: Span,
    meter: Meter,
    light: LightMeta,
}

#[derive(Debug, Clone)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    pub readonly: Vec<Field>,
    pub light: LightMeta,
    pub serial: Span,
    pub meter: Meter,
    by_key: HashMap<String, usize>,
    by_id: HashMap<u16, usize>,
}

impl Descriptor {
    pub fn load() -> Self {
        Self::parse(include_str!("../devices/pd100w.json")).expect("bundled descriptor is valid")
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawDescriptor = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut fields = raw.fields;
        let s = &raw.eq_slots;
        for slot in 0..s.count {
            for (i, sf) in s.fields.iter().enumerate() {
                fields.push(Field {
                    key: format!("{}.{slot}.{}", s.key_prefix, sf.name),
                    id: s.base + 5 * slot + i as u16,
                    kind: sf.kind,
                    min: sf.min,
                    max: sf.max,
                    step: sf.step,
                    unit: sf.unit.clone(),
                    encoding: sf.encoding,
                    options: sf.options.clone(),
                    group: "dsp".into(),
                    status: s.status.clone(),
                    profile: false,
                    mask: None,
                });
            }
        }
        // `fields` first, then `readonly`: indexes past fields.len() point into readonly.
        let mut by_key = HashMap::new();
        let mut by_id = HashMap::new();
        for (i, f) in fields.iter().chain(&raw.readonly).enumerate() {
            by_key.insert(f.key.clone(), i);
            by_id.insert(f.id, i);
        }
        Ok(Descriptor { fields, readonly: raw.readonly, light: raw.light, serial: raw.serial, meter: raw.meter, by_key, by_id })
    }

    fn at(&self, i: usize) -> &Field {
        if i < self.fields.len() { &self.fields[i] } else { &self.readonly[i - self.fields.len()] }
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.by_key.get(key).map(|&i| self.at(i))
    }

    pub fn field_by_id(&self, id: u16) -> Option<&Field> {
        self.by_id.get(&id).map(|&i| self.at(i))
    }

    /// What the panel needs to render controls without hardcoding them.
    pub fn schema(&self) -> Value {
        json!({ "fields": self.fields, "readonly": self.readonly, "light": self.light })
    }
}
```

`field()` returns readonly fields too (needed by `from_raw` in tests and by `on_frame`). `to_raw` on a readonly field fails at `safety` because those ids are not writable.

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib descriptor`
Expected: `5 passed`.

- [ ] **Step 6: Commit**

```bash
git add devices/pd100w.json src/descriptor.rs src/lib.rs
git commit -m "feat: PD100W descriptor as data, validated against the allowlist

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Device discovery and I/O (`device.rs`) with an in-memory fake

**Files:**
- Create: `src/device.rs`
- Create: `src/fake.rs` (test-only)
- Create: `src/testutil.rs` (test-only)
- Modify: `src/lib.rs`
- Modify: `src/mic.rs` (`Mic::open` uses `device::find`)

**Interfaces:**
- Consumes: `proto::{self, Frame, GET, MAX_PAIRS, REPORT_LEN}`, `safety::{check_all, Refused}`
- Produces:
  - `device::{Model { Wired, Receiver }, Found { hidraw: PathBuf, model: Model }, find_in(&Path) -> Vec<Found>, find() -> Vec<Found>}`
  - `device::{Incoming { Frames(Vec<Frame>), Gone }, Link (trait: send(&mut self, &[u8;64]) -> io::Result<()>)}`
  - `device::DevError { Gone, Timeout(u16), Refused(Refused), Io(io::Error) }`
  - `device::Device { pub model: Model }` with `new(Box<dyn Link>, Receiver<Incoming>, Model)`, `open(&Found) -> io::Result<Device>`, `set(&mut self, &[(u16, i64)]) -> Result<(), DevError>`, `get(&mut self, u16, &mut dyn FnMut(&Frame)) -> Result<u16, DevError>`, `poll(&mut self, Duration, &mut dyn FnMut(&Frame)) -> Result<(), DevError>`
  - test-only `fake::{FakeMic, pd100w_regs()}`, `testutil::tempdir(&str) -> PathBuf`

- [ ] **Step 1: Test helpers**

`src/testutil.rs`:

```rust
//! Test-only helpers.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

/// A fresh empty directory under the system temp dir.
pub fn tempdir(name: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("maono-test-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
```

`src/fake.rs`:

```rust
//! Test-only stand-in for the mic: a register map that answers GETs, records
//! SETs, and can simulate button presses and unplugging.
use crate::device::{Device, Incoming, Link, Model};
use crate::proto::{self, Frame, GET, SET};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct FakeMic {
    pub regs: Arc<Mutex<HashMap<u16, u16>>>,
    pub writes: Arc<Mutex<Vec<(u16, u16)>>>,
    pub silent: Arc<AtomicBool>,
    /// Unplug as soon as this many SET pairs have been written.
    pub gone_after_writes: Arc<Mutex<Option<usize>>>,
    tx: Sender<Incoming>,
}

struct FakeLink(FakeMic);

impl Link for FakeLink {
    fn send(&mut self, report: &[u8; proto::REPORT_LEN]) -> io::Result<()> {
        let m = &self.0;
        for f in proto::decode(report) {
            match f.kind {
                SET => {
                    for (id, v) in f.fields {
                        let mut w = m.writes.lock().unwrap();
                        if let Some(n) = *m.gone_after_writes.lock().unwrap() {
                            if w.len() >= n {
                                let _ = m.tx.send(Incoming::Gone);
                                return Err(io::Error::from_raw_os_error(19)); // ENODEV
                            }
                        }
                        w.push((id, v));
                        m.regs.lock().unwrap().insert(id, v);
                    }
                }
                GET if !m.silent.load(Ordering::SeqCst) => {
                    for (id, _) in f.fields {
                        if let Some(&v) = m.regs.lock().unwrap().get(&id) {
                            let _ = m.tx.send(Incoming::Frames(vec![Frame { kind: GET, fields: vec![(id, v)] }]));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl FakeMic {
    pub fn new(regs: &[(u16, u16)]) -> (Device, FakeMic) {
        let (tx, rx) = mpsc::channel();
        let m = FakeMic {
            regs: Arc::new(Mutex::new(regs.iter().copied().collect())),
            writes: Arc::default(),
            silent: Arc::default(),
            gone_after_writes: Arc::default(),
            tx,
        };
        (Device::new(Box::new(FakeLink(m.clone())), rx, Model::Wired), m)
    }

    /// The physical button: the device stores the value and announces it with SET.
    pub fn press(&self, id: u16, v: u16) {
        self.regs.lock().unwrap().insert(id, v);
        let _ = self.tx.send(Incoming::Frames(vec![Frame { kind: SET, fields: vec![(id, v)] }]));
    }

    pub fn unplug(&self) {
        let _ = self.tx.send(Incoming::Gone);
    }

    pub fn reg(&self, id: u16) -> Option<u16> {
        self.regs.lock().unwrap().get(&id).copied()
    }

    /// SET pairs written so far, excluding the level-meter switch serve sends on connect.
    pub fn writes(&self) -> Vec<(u16, u16)> {
        self.writes.lock().unwrap().iter().copied().filter(|&(id, _)| id != 0x0045).collect()
    }
}

/// The real mic's registers as read on 2026-10-07 (docs/protocol/pd100w.md).
pub fn pd100w_regs() -> Vec<(u16, u16)> {
    let mut r = vec![
        (0x000B, 108), (0x0042, 70), (0x0049, 0), (0x0045, 0),
        (0x207D, 0), (0x207E, 20), (0x207F, 10), (0x2084, 1), (0x2085, 0), (0x20AF, 7),
        (0x2089, 0), (0x208A, 15), (0x208B, 0), (0x208C, 1), (0x208E, 0),
        (0x2069, 0), (0x206A, 1800), (0x206B, 20), (0x206C, 800), (0x206D, 2),
        (0x206E, 0), (0x206F, 1600), (0x2070, 5), (0x2071, 5), (0x2082, 0), (0x2083, 1),
        // serial "PD100W", then 0xFEFE padding
        (0x001E, 0x4450), (0x001F, 0x3031), (0x0020, 0x5730), (0x0021, 0xFEFE),
    ];
    let flat = [(100, 1), (500, 1), (1000, 1), (2000, 1), (10000, 1), (20, 5), (20000, 3)];
    for (slot, (freq, kind)) in flat.into_iter().enumerate() {
        let b = 0x2023 + 5 * slot as u16;
        r.extend([(b, 1), (b + 1, kind), (b + 2, freq), (b + 3, 2000), (b + 4, 100)]);
    }
    for i in 0..15 {
        r.push((0x208F + i, 0));
    }
    r
}
```

In `src/lib.rs` add:

```rust
pub mod device;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod testutil;
```

- [ ] **Step 2: Write the failing tests**

Create `src/device.rs` with only the tests:

```rust
//! Finding the mic and talking to it. Every write goes through `safety`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeMic, pd100w_regs};
    use crate::testutil::tempdir;
    use std::sync::atomic::Ordering;

    fn uevent(root: &Path, node: &str, hid: &str) {
        let d = root.join(node).join("device");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("uevent"), format!("DRIVER=hid-generic\nHID_ID={hid}\nHID_NAME=x\n")).unwrap();
    }

    #[test]
    fn find_prefers_the_wired_mic() {
        let root = tempdir("hidraw");
        uevent(&root, "hidraw3", "0003:0000352F:00000414");
        uevent(&root, "hidraw5", "0003:0000352F:00000417");
        uevent(&root, "hidraw1", "0003:0000046D:0000C548");
        let found = find_in(&root);
        assert_eq!(found, vec![
            Found { hidraw: "/dev/hidraw5".into(), model: Model::Wired },
            Found { hidraw: "/dev/hidraw3".into(), model: Model::Receiver },
        ]);
    }

    #[test]
    fn get_answers_and_forwards_other_frames() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        fake.press(0x207D, 1);
        let mut seen = Vec::new();
        assert_eq!(dev.get(0x207E, &mut |f| seen.push(f.clone())).unwrap(), 20);
        assert_eq!(seen, vec![Frame { kind: proto::SET, fields: vec![(0x207D, 1)] }]);
    }

    #[test]
    fn get_times_out_when_the_mic_is_silent() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        fake.silent.store(true, Ordering::SeqCst);
        assert!(matches!(dev.get(0x207E, &mut |_| {}), Err(DevError::Timeout(0x207E))));
    }

    #[test]
    fn set_refuses_before_writing_anything() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        assert!(matches!(dev.set(&[(0x207E, 5), (0x100A, 3)]), Err(DevError::Refused(_))));
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn set_splits_long_writes_into_frames() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let pairs: Vec<(u16, i64)> = (0..15).map(|i| (0x208F + i, if i % 3 == 0 { 100 } else { 50 })).collect();
        dev.set(&pairs).unwrap();
        assert_eq!(fake.writes().len(), 15);
    }

    #[test]
    fn unplug_is_reported_as_gone() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        fake.unplug();
        assert!(matches!(dev.poll(Duration::ZERO, &mut |_| {}), Err(DevError::Gone)));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib device`
Expected: compile errors `cannot find function 'find_in'`, `cannot find type 'Device'`.

- [ ] **Step 4: Implement**

Above the tests in `src/device.rs`:

```rust
use crate::proto::{self, Frame, GET, MAX_PAIRS, REPORT_LEN};
use crate::safety::{self, Refused};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    Wired,
    Receiver,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub hidraw: PathBuf,
    pub model: Model,
}

/// PD100W hidraw nodes under `root` (normally /sys/class/hidraw), wired first.
pub fn find_in(root: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else { return out };
    for e in entries.flatten() {
        let Ok(text) = fs::read_to_string(e.path().join("device/uevent")) else { continue };
        let Some(hid) = text.lines().find_map(|l| l.strip_prefix("HID_ID=")) else { continue };
        let model = match hid.to_uppercase().as_str() {
            "0003:0000352F:00000417" => Model::Wired,
            "0003:0000352F:00000414" => Model::Receiver,
            _ => continue,
        };
        out.push(Found { hidraw: Path::new("/dev").join(e.file_name()), model });
    }
    out.sort_by_key(|f| (f.model != Model::Wired, f.hidraw.clone()));
    out
}

pub fn find() -> Vec<Found> {
    find_in(Path::new("/sys/class/hidraw"))
}

pub enum Incoming {
    Frames(Vec<Frame>),
    Gone,
}

pub trait Link: Send {
    fn send(&mut self, report: &[u8; REPORT_LEN]) -> io::Result<()>;
}

impl Link for File {
    fn send(&mut self, report: &[u8; REPORT_LEN]) -> io::Result<()> {
        self.write_all(report)
    }
}

#[derive(Debug)]
pub enum DevError {
    Gone,
    Timeout(u16),
    Refused(Refused),
    Io(io::Error),
}

impl std::fmt::Display for DevError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            DevError::Gone => write!(f, "the mic was disconnected"),
            DevError::Timeout(id) => write!(f, "the mic did not answer for 0x{id:04x}"),
            DevError::Refused(r) => write!(f, "refused: {r}"),
            DevError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn io_or_gone(e: io::Error) -> DevError {
    match e.raw_os_error() {
        Some(19) | Some(5) => DevError::Gone, // ENODEV, EIO: the node went away
        _ => DevError::Io(e),
    }
}

pub struct Device {
    link: Box<dyn Link>,
    rx: Receiver<Incoming>,
    pub model: Model,
}

impl Device {
    pub fn new(link: Box<dyn Link>, rx: Receiver<Incoming>, model: Model) -> Self {
        Device { link, rx, model }
    }

    /// Open the hidraw node. A blocking reader thread feeds every report into
    /// the channel and sends `Gone` when the node disappears.
    pub fn open(found: &Found) -> io::Result<Device> {
        let file = OpenOptions::new().read(true).write(true).open(&found.hidraw)?;
        let mut reader = file.try_clone()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            loop {
                match reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if tx.send(Incoming::Frames(proto::decode(&buf[..n]))).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    _ => {
                        let _ = tx.send(Incoming::Gone);
                        return;
                    }
                }
            }
        });
        Ok(Device::new(Box::new(file), rx, found.model))
    }

    /// Write fields. Nothing is sent unless every pair passes `safety`.
    pub fn set(&mut self, pairs: &[(u16, i64)]) -> Result<(), DevError> {
        let checked = safety::check_all(pairs).map_err(DevError::Refused)?;
        for chunk in checked.chunks(MAX_PAIRS) {
            self.link.send(&proto::set(chunk)).map_err(io_or_gone)?;
        }
        Ok(())
    }

    /// Read one field. Frames that are not the answer (button presses, answers
    /// to other readers) go to `other`, so nothing is lost.
    pub fn get(&mut self, id: u16, other: &mut dyn FnMut(&Frame)) -> Result<u16, DevError> {
        for _ in 0..3 {
            self.link.send(&proto::get(id)).map_err(io_or_gone)?;
            let deadline = Instant::now() + Duration::from_millis(400);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match self.rx.recv_timeout(left) {
                    Ok(Incoming::Frames(frames)) => {
                        let mut answer = None;
                        for f in frames {
                            let hit = if f.kind == GET && answer.is_none() {
                                f.fields.iter().find(|(i, _)| *i == id).map(|&(_, v)| v)
                            } else {
                                None
                            };
                            match hit {
                                Some(v) => answer = Some(v),
                                None => other(&f),
                            }
                        }
                        if let Some(v) = answer {
                            return Ok(v);
                        }
                    }
                    Ok(Incoming::Gone) | Err(RecvTimeoutError::Disconnected) => return Err(DevError::Gone),
                    Err(RecvTimeoutError::Timeout) => break,
                }
            }
        }
        Err(DevError::Timeout(id))
    }

    /// Hand every waiting frame to `other`, waiting up to `wait` for the first.
    pub fn poll(&mut self, wait: Duration, other: &mut dyn FnMut(&Frame)) -> Result<(), DevError> {
        let mut timeout = wait;
        loop {
            match self.rx.recv_timeout(timeout) {
                Ok(Incoming::Frames(frames)) => frames.iter().for_each(|f| other(f)),
                Ok(Incoming::Gone) | Err(RecvTimeoutError::Disconnected) => return Err(DevError::Gone),
                Err(RecvTimeoutError::Timeout) => return Ok(()),
            }
            timeout = Duration::ZERO;
        }
    }
}
```

In `src/mic.rs`:
1. Delete `const HID_MATCH` and `pub fn find_device`.
2. In `Mic::open`, replace the `let path = find_device()…?;` statement with:

```rust
        let path = crate::device::find()
            .into_iter()
            .next()
            .map(|f| f.hidraw.display().to_string())
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "Maono PD100W not found - is it plugged in?"))?;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib && mise exec rust@stable -- cargo build`
Expected: all tests pass, including `6` in `device::tests`; the build succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/device.rs src/fake.rs src/testutil.rs src/lib.rs src/mic.rs
git commit -m "feat: device discovery and safety-gated I/O with an in-memory fake

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Mic state read and apply (`state.rs`)

**Files:**
- Create: `src/state.rs`
- Modify: `src/lib.rs` (add `pub mod state;`)

**Interfaces:**
- Consumes: `Descriptor`, `Device::{get,set}`, `DevError`, `Frame`
- Produces:
  - `state::Values` (= `serde_json::Map<String, Value>`)
  - `state::read_all(&mut Device, &Descriptor, &mut dyn FnMut(&Frame)) -> Result<Values, DevError>`, which fills every descriptor key plus `info.serial`, `light.color` (`{"preset":name}` | `{"hsv":[h,s,v]}` | `null`) and `light.custom` (an array of `[h,s,v]`)
  - `state::ApplyError { Invalid(Vec<String>), Device(DevError) }`
  - `state::apply(&mut Device, &Descriptor, changes: &Values, current: &Values, other) -> Result<Values /*effective*/, ApplyError>`
  - `state::custom_update(dev, desc, current, index: usize, hsv: &Value, other) -> Result<Values, ApplyError>`
  - `state::custom_delete(dev, desc, current, index: usize, other) -> Result<Values, ApplyError>`
  - `state::check_color(&Descriptor, &Value) -> Result<(), String>`, `state::same_color(&Value, &Value) -> bool`, `state::hsv_raw(Option<&Value>) -> Option<[i64; 3]>`

- [ ] **Step 1: Write the failing tests**

Create `src/state.rs`:

```rust
//! The mic's settings as a JSON map keyed by descriptor keys, read and written
//! through the descriptor (and therefore through `safety`).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeMic, pd100w_regs};
    use serde_json::json;

    fn values(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn read_all_decodes_the_real_registers() {
        let d = Descriptor::load();
        let (mut dev, _fake) = FakeMic::new(&pd100w_regs());
        let v = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        assert_eq!(v["mic.gain"], json!(20));
        assert_eq!(v["mic.nr.on"], json!(true));
        assert_eq!(v["info.battery"], json!(70));
        assert_eq!(v["info.firmware"], json!("1.0.8"));
        assert_eq!(v["dsp.comp.threshold"], json!(-20));
        assert_eq!(v["light.color"], json!({"preset": "red"}));
        assert_eq!(v["light.custom"], json!([]));
        assert_eq!(v["info.serial"], json!("PD100W"));
    }

    #[test]
    fn invalid_changes_write_nothing() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let r = apply(&mut dev, &d, &values(json!({"mic.gain": 21, "light.brightness": 50, "nope": 1})), &cur, &mut |_| {});
        match r {
            Err(ApplyError::Invalid(e)) => assert_eq!(e.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn apply_writes_and_reads_back() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = apply(&mut dev, &d, &values(json!({"mic.gain": 14, "mic.nr.level": 2})), &cur, &mut |_| {}).unwrap();
        assert_eq!(eff, values(json!({"mic.gain": 14, "mic.nr.level": 2})));
        assert_eq!(fake.reg(0x207E), Some(14));
    }

    #[test]
    fn new_hsv_colour_takes_the_next_custom_slot() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = apply(&mut dev, &d, &values(json!({"light.color": {"hsv": [330, 1, 1]}})), &cur, &mut |_| {}).unwrap();
        assert_eq!(fake.writes(), vec![(0x208F, 33000), (0x2090, 100), (0x2091, 100), (0x208E, 1), (0x208C, 8)]);
        assert_eq!(eff["light.color"], json!({"hsv": [330.0, 1.0, 1.0]}));
    }

    #[test]
    fn existing_hsv_colour_reuses_its_slot() {
        let d = Descriptor::load();
        let mut regs = pd100w_regs();
        regs.extend([(0x208E, 1), (0x208F, 33000), (0x2090, 100), (0x2091, 100)]);
        let (mut dev, fake) = FakeMic::new(&regs);
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        apply(&mut dev, &d, &values(json!({"light.color": {"hsv": [330, 1, 1]}})), &cur, &mut |_| {}).unwrap();
        assert_eq!(fake.writes(), vec![(0x208C, 8)]);
    }

    #[test]
    fn deleting_a_custom_colour_shifts_the_rest() {
        let d = Descriptor::load();
        let mut regs = pd100w_regs();
        regs.extend([(0x208E, 2), (0x208F, 1000), (0x2090, 100), (0x2091, 100), (0x2092, 2000), (0x2093, 50), (0x2094, 50), (0x208C, 9)]);
        let (mut dev, fake) = FakeMic::new(&regs);
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = custom_delete(&mut dev, &d, &cur, 0, &mut |_| {}).unwrap();
        assert_eq!(fake.reg(0x208F), Some(2000));
        assert_eq!(fake.reg(0x208E), Some(1));
        assert_eq!(fake.reg(0x208C), Some(8));
        assert_eq!(eff["light.custom"], json!([[20.0, 0.5, 0.5]]));
    }

    #[test]
    fn colour_helpers() {
        let d = Descriptor::load();
        assert!(check_color(&d, &json!({"preset": "blue"})).is_ok());
        assert!(check_color(&d, &json!({"preset": "teal"})).is_err());
        assert!(check_color(&d, &json!({"hsv": [400, 1, 1]})).is_err());
        assert!(same_color(&json!({"hsv": [330.0, 1.0, 1.0]}), &json!({"hsv": [330, 1, 1]})));
        assert!(!same_color(&json!({"preset": "red"}), &json!({"preset": "blue"})));
    }
}
```

Add `pub mod state;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib state`
Expected: compile errors `cannot find function 'read_all'`.

- [ ] **Step 3: Implement**

Above the tests in `src/state.rs`:

```rust
use crate::descriptor::Descriptor;
use crate::device::{DevError, Device};
use crate::proto::Frame;
use serde_json::{Map, Value, json};

pub type Values = Map<String, Value>;

#[derive(Debug)]
pub enum ApplyError {
    Invalid(Vec<String>),
    Device(DevError),
}

/// `Ok(None)` when the mic did not answer; `Gone`/IO errors propagate.
fn get_opt(dev: &mut Device, id: u16, other: &mut dyn FnMut(&Frame)) -> Result<Option<u16>, DevError> {
    match dev.get(id, other) {
        Ok(v) => Ok(Some(v)),
        Err(DevError::Timeout(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn read_all(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<Values, DevError> {
    let mut out = Values::new();
    for f in desc.fields.iter().chain(&desc.readonly) {
        let v = get_opt(dev, f.id, other)?.map_or(Value::Null, |raw| f.from_raw(raw));
        out.insert(f.key.clone(), v);
    }
    out.insert("info.serial".into(), read_serial(dev, desc, other)?);
    let (color, custom) = read_color(dev, desc, other)?;
    out.insert("light.color".into(), color);
    out.insert("light.custom".into(), custom);
    Ok(out)
}

fn read_serial(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<Value, DevError> {
    let mut s = String::new();
    for i in 0..desc.serial.count {
        let Some(w) = get_opt(dev, desc.serial.start + i, other)? else { break };
        for b in [w as u8, (w >> 8) as u8] {
            if b == 0 || b == 0xFE {
                return Ok(json!(s));
            }
            s.push(b as char);
        }
    }
    Ok(json!(s))
}

fn read_color(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<(Value, Value), DevError> {
    let l = &desc.light;
    let index = get_opt(dev, l.color, other)?;
    let count = get_opt(dev, l.count, other)?.unwrap_or(0).min(l.custom_max);
    let mut custom = Vec::new();
    for k in 0..count {
        let b = l.base + 3 * k;
        let hsv = (get_opt(dev, b, other)?, get_opt(dev, b + 1, other)?, get_opt(dev, b + 2, other)?);
        custom.push(match hsv {
            (Some(h), Some(s), Some(v)) => json!([h as f64 / 100.0, s as f64 / 100.0, v as f64 / 100.0]),
            _ => Value::Null,
        });
    }
    let color = match index {
        None => Value::Null,
        Some(i) if i < 8 => l.presets.iter().find(|p| p.value == i).map_or(Value::Null, |p| json!({"preset": p.name})),
        Some(i) => custom
            .get((i - 8) as usize)
            .filter(|c| !c.is_null())
            .map_or(json!({"slot": i - 8}), |c| json!({"hsv": c})),
    };
    Ok((color, Value::Array(custom)))
}

/// `[h°, s, v]` with h in 0..360 and s, v in 0..1 -> raw `[h*100, s*100, v*100]`.
pub fn hsv_raw(v: Option<&Value>) -> Option<[i64; 3]> {
    let a = v?.as_array()?;
    let [h, s, x] = [a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?];
    if a.len() != 3 || !(0.0..360.0).contains(&h) || !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&x) {
        return None;
    }
    Some([((h * 100.0).round() as i64).min(35_900), (s * 100.0).round() as i64, (x * 100.0).round() as i64])
}

pub fn check_color(desc: &Descriptor, v: &Value) -> Result<(), String> {
    if let Some(name) = v.get("preset").and_then(Value::as_str) {
        return match desc.light.presets.iter().any(|p| p.name == name) {
            true => Ok(()),
            false => Err(format!("light.color: unknown preset {name}")),
        };
    }
    hsv_raw(v.get("hsv")).map(drop).ok_or_else(|| "light.color: expected {\"preset\": name} or {\"hsv\": [h, s, v]}".into())
}

pub fn same_color(a: &Value, b: &Value) -> bool {
    match (a.get("preset"), b.get("preset")) {
        (Some(x), Some(y)) => x == y,
        (None, None) => hsv_raw(a.get("hsv")).is_some() && hsv_raw(a.get("hsv")) == hsv_raw(b.get("hsv")),
        _ => false,
    }
}

fn custom_list(current: &Values) -> Vec<Value> {
    current.get("light.custom").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn plan_color(desc: &Descriptor, v: &Value, current: &Values) -> Result<Vec<(u16, i64)>, String> {
    check_color(desc, v)?;
    let l = &desc.light;
    if let Some(name) = v.get("preset").and_then(Value::as_str) {
        let p = l.presets.iter().find(|p| p.name == name).unwrap();
        return Ok(vec![(l.color, p.value as i64)]);
    }
    let raw = hsv_raw(v.get("hsv")).unwrap();
    let custom = custom_list(current);
    if let Some(k) = custom.iter().position(|c| hsv_raw(Some(c)) == Some(raw)) {
        return Ok(vec![(l.color, 8 + k as i64)]);
    }
    let k = custom.len() as u16;
    if k >= l.custom_max {
        return Err("light.color: all custom colour slots are taken".into());
    }
    let b = l.base + 3 * k;
    Ok(vec![(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2]), (l.count, k as i64 + 1), (l.color, 8 + k as i64)])
}

/// Validate everything first (nothing is written if any value is bad), write,
/// then read back every touched key. Returns the effective values.
pub fn apply(dev: &mut Device, desc: &Descriptor, changes: &Values, current: &Values, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    let mut pairs = Vec::new();
    let mut errors = Vec::new();
    let mut colour = None;
    for (k, v) in changes {
        if k == "light.color" {
            match plan_color(desc, v, current) {
                Ok(p) => colour = Some(p),
                Err(e) => errors.push(e),
            }
            continue;
        }
        match desc.field(k) {
            Some(f) => match f.to_raw(v) {
                Ok(raw) => pairs.push((f.id, raw as i64)),
                Err(e) => errors.push(e),
            },
            None => errors.push(format!("{k}: unknown setting")),
        }
    }
    if !errors.is_empty() {
        return Err(ApplyError::Invalid(errors));
    }
    if let Some(c) = &colour {
        pairs.extend(c.iter().copied());
    }
    dev.set(&pairs).map_err(ApplyError::Device)?;
    let mut effective = Values::new();
    for k in changes.keys().filter(|k| *k != "light.color") {
        let f = desc.field(k).unwrap();
        let v = get_opt(dev, f.id, other).map_err(ApplyError::Device)?.map_or(Value::Null, |raw| f.from_raw(raw));
        effective.insert(k.clone(), v);
    }
    if colour.is_some() {
        read_light_into(dev, desc, &mut effective, other)?;
    }
    Ok(effective)
}

fn read_light_into(dev: &mut Device, desc: &Descriptor, out: &mut Values, other: &mut dyn FnMut(&Frame)) -> Result<(), ApplyError> {
    let (c, custom) = read_color(dev, desc, other).map_err(ApplyError::Device)?;
    out.insert("light.color".into(), c);
    out.insert("light.custom".into(), custom);
    Ok(())
}

pub fn custom_update(dev: &mut Device, desc: &Descriptor, current: &Values, index: usize, hsv: &Value, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    if index >= custom_list(current).len() {
        return Err(ApplyError::Invalid(vec![format!("light.custom: no colour at {index}")]));
    }
    let raw = hsv_raw(Some(hsv)).ok_or_else(|| ApplyError::Invalid(vec!["light.custom: hsv must be [h, s, v]".into()]))?;
    let b = desc.light.base + 3 * index as u16;
    dev.set(&[(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2])]).map_err(ApplyError::Device)?;
    let mut out = Values::new();
    read_light_into(dev, desc, &mut out, other)?;
    Ok(out)
}

/// Remove custom colour `index`; later slots move down one, and the selected
/// colour index follows its colour (a deleted selection falls back to white).
pub fn custom_delete(dev: &mut Device, desc: &Descriptor, current: &Values, index: usize, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    let l = &desc.light;
    let custom = custom_list(current);
    if index >= custom.len() {
        return Err(ApplyError::Invalid(vec![format!("light.custom: no colour at {index}")]));
    }
    let mut pairs = Vec::new();
    for (k, c) in custom.iter().enumerate().skip(index + 1) {
        let raw = hsv_raw(Some(c)).unwrap_or([0, 0, 0]);
        let b = l.base + 3 * (k as u16 - 1);
        pairs.extend([(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2])]);
    }
    pairs.push((l.count, custom.len() as i64 - 1));
    let selected = get_opt(dev, l.color, other).map_err(ApplyError::Device)?.unwrap_or(0) as usize;
    let deleted = 8 + index;
    if selected == deleted {
        pairs.push((l.color, 0));
    } else if selected > deleted {
        pairs.push((l.color, selected as i64 - 1));
    }
    dev.set(&pairs).map_err(ApplyError::Device)?;
    let mut out = Values::new();
    read_light_into(dev, desc, &mut out, other)?;
    Ok(out)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib state`
Expected: `7 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/lib.rs
git commit -m "feat: read and apply mic state with validation, readback and custom colours

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Files and config (`store.rs`, `config.rs`)

**Files:**
- Create: `src/store.rs`
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod store; pub mod config;`)

**Interfaces:**
- Produces:
  - `store::{config_dir(), pipewire_conf(), runtime_dir(), socket_path(), lock_path()} -> PathBuf`
  - `store::write_atomic(&Path, &[u8]) -> io::Result<()>`
  - `store::write_json<T: Serialize>(&Path, &T) -> io::Result<()>`
  - `store::read_json<T: DeserializeOwned>(&Path) -> io::Result<Option<T>>`
  - `store::slugify(&str) -> String`
  - `config::Config { version: u32, active_profile: Option<String>, apply_on_reconnect: bool }` with `load(&Path) -> (Config, Vec<String>)` and `save(&self, &Path) -> io::Result<()>`

- [ ] **Step 1: Write the failing tests**

`src/store.rs`:

```rust
//! Paths and atomic file I/O shared by every module that persists something.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[test]
    fn write_atomic_creates_parents_and_leaves_no_temp_file() {
        let dir = tempdir("store");
        let path = dir.join("a/b/c.json");
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn read_json_missing_is_none_and_corrupt_is_an_error_that_keeps_the_file() {
        let dir = tempdir("store");
        assert!(read_json::<serde_json::Value>(&dir.join("nope.json")).unwrap().is_none());
        let bad = dir.join("bad.json");
        fs::write(&bad, "{not json").unwrap();
        assert_eq!(read_json::<serde_json::Value>(&bad).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(bad.exists());
    }

    #[test]
    fn slugify_makes_file_safe_ids() {
        assert_eq!(slugify("Voz de Noche!"), "voz-de-noche");
        assert_eq!(slugify("Ñandú  Grabación"), "nandu-grabacion");
        assert_eq!(slugify("  "), "");
        assert_eq!(slugify("Streaming / Discord"), "streaming-discord");
    }
}
```

`src/config.rs`:

```rust
//! Backend settings in ~/.config/maono/config.json.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[test]
    fn roundtrip_and_corrupt_fallback() {
        let dir = tempdir("config");
        let (c, w) = Config::load(&dir);
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
        let c = Config { active_profile: Some("llamada".into()), ..Config::default() };
        c.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).0, c);
        std::fs::write(dir.join("config.json"), "garbage").unwrap();
        let (c, w) = Config::load(&dir);
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
    }
}
```

Add `pub mod store; pub mod config;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib store config`
Expected: compile errors `cannot find function 'write_atomic'`, `cannot find type 'Config'`.

- [ ] **Step 3: Implement**

`src/store.rs`, above the tests:

```rust
use serde::{Serialize, de::DeserializeOwned};
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg_config() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

/// ~/.config/maono (override with MAONO_CONFIG_DIR).
pub fn config_dir() -> PathBuf {
    env::var_os("MAONO_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| xdg_config().join("maono"))
}

/// The filter-chain fragment we own (override with MAONO_PW_CONF).
pub fn pipewire_conf() -> PathBuf {
    env::var_os("MAONO_PW_CONF")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg_config().join("pipewire/filter-chain.conf.d/maono-clean.conf"))
}

pub fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(env::temp_dir)
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("maono.sock")
}

pub fn lock_path() -> PathBuf {
    runtime_dir().join("maono.lock")
}

/// Write to a temp file in the same directory, fsync, then rename over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// `Ok(None)` when missing; `InvalidData` when the file exists but does not parse.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// "Voz de Noche!" -> "voz-de-noche": lowercase ASCII, digits and single dashes, at most 40 chars.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}
```

`src/config.rs`, above the tests:

```rust
use crate::store;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,
    pub active_profile: Option<String>,
    pub apply_on_reconnect: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: 1, active_profile: None, apply_on_reconnect: true }
    }
}

impl Config {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("config.json")
    }

    /// A corrupt file is left on disk and reported; defaults are used for this run.
    pub fn load(dir: &Path) -> (Config, Vec<String>) {
        match store::read_json(&Self::path(dir)) {
            Ok(Some(c)) => (c, Vec::new()),
            Ok(None) => (Config::default(), Vec::new()),
            Err(e) => (Config::default(), vec![e.to_string()]),
        }
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        store::write_json(&Self::path(dir), self)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib store config`
Expected: `4 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/config.rs src/lib.rs
git commit -m "feat: atomic file store, slugs and backend config

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Filter model, schema and EQ presets (`filter.rs`)

**Files:**
- Create: `devices/filter.json`
- Create: `presets/eq/maono.json`
- Create: `src/filter.rs`
- Modify: `src/lib.rs` (add `pub mod filter;`)

**Interfaces:**
- Consumes: `store::{read_json, write_json, slugify}`, `state::Values`
- Produces:
  - `filter::{FilterState { version, enabled, hpf: Pass, eq: Eq, lpf: Pass, rnnoise: RnNoise, comp: Comp }, Pass { on, freq }, Eq { on, preset: Option<String>, bands: Vec<Band> }, Band { kind (json "type"): String, freq, gain, q }, RnNoise { on, vad, grace, retro }, Comp { on, threshold, ratio, attack, release, makeup }}`
  - `filter::FilterSchema { source: SourceMeta { node, capture, description }, fields, defaults: FilterState, band_labels, bypass: BypassMeta { hpf_freq, lpf_freq, pass_q }, plugins: Plugins { rnnoise: Plugin, comp: Plugin } }` with `load()`, `validate(&FilterState) -> Result<(), Vec<String>>` and `with_changes(&FilterState, &Values, &Presets) -> Result<FilterState, Vec<String>>`
  - `filter::Plugin { package, file, plugin, label, input, output, controls: BTreeMap<String,String>, switch: Switch { port, on, off } }`
  - `filter::{Deps { rnnoise: bool, comp: bool }, params(&FilterState, &Deps, &FilterSchema) -> Vec<(String, f64)>, render_conf(&FilterState, &Deps, &FilterSchema, target: &str) -> String, structure_key(&FilterState, &Deps) -> String, flatten(&FilterState) -> Values, db_to_gain(f64) -> f64}`
  - `filter::{Preset { id, name, source, hpf: f64, lpf: Option<f64>, bands }, Presets { list: Vec<Preset> }}`, with `Presets::{load(&Path) -> (Presets, Vec<String>), get(&str), save_user(&mut self, &Path, &str, &FilterState) -> Result<Preset,String>, delete_user(&mut self, &Path, &str) -> Result<(),String>}`

- [ ] **Step 1: Create the data files**

`presets/eq/maono.json` (Maono Link 3.8.52 curves, docs/protocol/pd100w.md §5; Pop's LPF is bypassed):

```json
[
  {"id": "original", "name": "Original", "source": "maono", "hpf": 20, "lpf": 20000, "bands": [
    {"type": "peak", "freq": 125, "gain": 0, "q": 1}, {"type": "peak", "freq": 250, "gain": 0, "q": 1},
    {"type": "peak", "freq": 500, "gain": 0, "q": 1}, {"type": "peak", "freq": 1000, "gain": 0, "q": 1},
    {"type": "peak", "freq": 2000, "gain": 0, "q": 1}]},
  {"id": "game1", "name": "Game 1", "source": "maono", "hpf": 39.6, "lpf": 10015.6, "bands": [
    {"type": "peak", "freq": 180, "gain": 1, "q": 0.7}, {"type": "peak", "freq": 50, "gain": -10, "q": 1},
    {"type": "peak", "freq": 1500, "gain": 3, "q": 0.7}, {"type": "peak", "freq": 5009, "gain": 0, "q": 3},
    {"type": "highshelf", "freq": 12000, "gain": -10, "q": 0.8}]},
  {"id": "game2", "name": "Game 2", "source": "maono", "hpf": 48.65, "lpf": 10015.6, "bands": [
    {"type": "lowshelf", "freq": 136, "gain": 8.571, "q": 0.7}, {"type": "peak", "freq": 40, "gain": -10, "q": 1},
    {"type": "peak", "freq": 1200, "gain": 2, "q": 0.7}, {"type": "peak", "freq": 5009, "gain": 0, "q": 3},
    {"type": "highshelf", "freq": 12000, "gain": -10, "q": 0.8}]},
  {"id": "stream1", "name": "Stream 1", "source": "maono", "hpf": 59.23, "lpf": 16822.2, "bands": [
    {"type": "lowshelf", "freq": 52.4, "gain": -12, "q": 1}, {"type": "peak", "freq": 1350, "gain": 3, "q": 0.5},
    {"type": "peak", "freq": 5000, "gain": 2, "q": 0.8}, {"type": "peak", "freq": 8810, "gain": -1, "q": 1},
    {"type": "highshelf", "freq": 10000, "gain": -1, "q": 0.7}]},
  {"id": "stream2", "name": "Stream 2", "source": "maono", "hpf": 59.23, "lpf": 10919.7, "bands": [
    {"type": "lowshelf", "freq": 50, "gain": -12, "q": 0.8}, {"type": "peak", "freq": 100, "gain": -2, "q": 1.5},
    {"type": "peak", "freq": 800, "gain": 0, "q": 0.5}, {"type": "peak", "freq": 1500, "gain": 3, "q": 0.8},
    {"type": "highshelf", "freq": 4500, "gain": 3, "q": 0.8}]},
  {"id": "pop", "name": "Pop", "source": "maono", "hpf": 63.27, "lpf": null, "bands": [
    {"type": "peak", "freq": 199, "gain": -2, "q": 1}, {"type": "peak", "freq": 500, "gain": 0, "q": 1},
    {"type": "peak", "freq": 1000, "gain": 0, "q": 1}, {"type": "peak", "freq": 7010, "gain": -3, "q": 8},
    {"type": "highshelf", "freq": 11000, "gain": 1, "q": 0.7}]},
  {"id": "folk", "name": "Folk", "source": "maono", "hpf": 40.52, "lpf": 11303.8, "bands": [
    {"type": "peak", "freq": 40, "gain": -10, "q": 1.5}, {"type": "peak", "freq": 3150, "gain": -2, "q": 5},
    {"type": "peak", "freq": 500, "gain": 0, "q": 1}, {"type": "peak", "freq": 5700, "gain": -4.32, "q": 5},
    {"type": "highshelf", "freq": 8810, "gain": -3, "q": 0.5}]}
]
```

`devices/filter.json`:

```json
{
  "source": {"node": "maono_clean", "capture": "maono_clean_in", "description": "Maono PD100W (limpio)"},
  "fields": [
    {"key": "enabled", "type": "bool"},
    {"key": "hpf.on", "type": "bool"},
    {"key": "hpf.freq", "type": "num", "min": 10, "max": 400, "step": 1, "unit": "Hz"},
    {"key": "eq.on", "type": "bool"},
    {"key": "eq.bands.*.type", "type": "enum", "options": ["peak", "lowshelf", "highshelf"]},
    {"key": "eq.bands.*.freq", "type": "num", "min": 20, "max": 20000, "step": 1, "unit": "Hz"},
    {"key": "eq.bands.*.gain", "type": "num", "min": -24, "max": 24, "step": 0.1, "unit": "dB"},
    {"key": "eq.bands.*.q", "type": "num", "min": 0.1, "max": 20, "step": 0.01},
    {"key": "lpf.on", "type": "bool"},
    {"key": "lpf.freq", "type": "num", "min": 1000, "max": 22000, "step": 10, "unit": "Hz"},
    {"key": "rnnoise.on", "type": "bool"},
    {"key": "rnnoise.vad", "type": "num", "min": 0, "max": 99, "step": 1, "unit": "%"},
    {"key": "rnnoise.grace", "type": "num", "min": 0, "max": 1000, "step": 10, "unit": "ms"},
    {"key": "rnnoise.retro", "type": "num", "min": 0, "max": 200, "step": 5, "unit": "ms"},
    {"key": "comp.on", "type": "bool"},
    {"key": "comp.threshold", "type": "num", "min": -60, "max": 0, "step": 1, "unit": "dB"},
    {"key": "comp.ratio", "type": "num", "min": 1, "max": 20, "step": 0.1},
    {"key": "comp.attack", "type": "num", "min": 0, "max": 500, "step": 1, "unit": "ms"},
    {"key": "comp.release", "type": "num", "min": 10, "max": 2000, "step": 10, "unit": "ms"},
    {"key": "comp.makeup", "type": "num", "min": 0, "max": 24, "step": 0.5, "unit": "dB"}
  ],
  "defaults": {
    "version": 1, "enabled": true,
    "hpf": {"on": true, "freq": 90},
    "eq": {"on": true, "preset": "original", "bands": [
      {"type": "peak", "freq": 125, "gain": 0, "q": 1}, {"type": "peak", "freq": 250, "gain": 0, "q": 1},
      {"type": "peak", "freq": 500, "gain": 0, "q": 1}, {"type": "peak", "freq": 1000, "gain": 0, "q": 1},
      {"type": "peak", "freq": 2000, "gain": 0, "q": 1}]},
    "lpf": {"on": true, "freq": 20000},
    "rnnoise": {"on": true, "vad": 85, "grace": 250, "retro": 30},
    "comp": {"on": false, "threshold": -24, "ratio": 3, "attack": 10, "release": 150, "makeup": 0}
  },
  "bandLabels": {"peak": "bq_peaking", "lowshelf": "bq_lowshelf", "highshelf": "bq_highshelf"},
  "bypass": {"hpfFreq": 5, "lpfFreq": 22000, "passQ": 0.707},
  "plugins": {
    "rnnoise": {
      "package": "noise-suppression-for-voice", "file": "librnnoise_ladspa.so", "plugin": "librnnoise_ladspa", "label": "noise_suppressor_mono",
      "in": "Input", "out": "Output",
      "controls": {"vad": "VAD Threshold (%)", "grace": "VAD Grace Period (ms)", "retro": "Retroactive VAD Grace (ms)"},
      "switch": {"port": "Dry Mix", "on": 0, "off": 1}
    },
    "comp": {
      "package": "lsp-plugins-ladspa", "file": "lsp-plugins-ladspa.so", "plugin": "lsp-plugins-ladspa", "label": "http://lsp-plug.in/plugins/ladspa/compressor_mono",
      "in": "Input", "out": "Output",
      "controls": {"threshold": "Attack threshold (G)", "ratio": "Ratio", "attack": "Attack time (ms)", "release": "Release time (ms)", "makeup": "Makeup gain (G)"},
      "switch": {"port": "Bypass", "on": 1, "off": 0}
    }
  }
}
```

Leave the `switch` polarities as written: LSP's "Bypass" port defaults to 1 (processing), and RNNoise's "Dry Mix" 1 is assumed to be fully dry. Task 13 confirms both on hardware, and a wrong polarity is fixed in this JSON alone.

- [ ] **Step 2: Write the failing tests**

Create `src/filter.rs` with the tests and add `pub mod filter;` to `src/lib.rs`:

```rust
//! The PipeWire cleanup chain: its state, schema (data), EQ presets, the
//! generated filter-chain fragment and the live control values.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;
    use serde_json::json;

    fn ch(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    fn setup() -> (FilterSchema, Presets, FilterState) {
        let s = FilterSchema::load();
        let (p, w) = Presets::load(&tempdir("eq"));
        assert!(w.is_empty());
        let d = s.defaults.clone();
        (s, p, d)
    }

    #[test]
    fn defaults_are_valid_and_every_control_the_code_uses_exists() {
        let (s, p, d) = setup();
        s.validate(&d).unwrap();
        assert_eq!(p.list.len(), 7);
        for k in ["vad", "grace", "retro"] {
            assert!(s.plugins.rnnoise.controls.contains_key(k));
        }
        for k in ["threshold", "ratio", "attack", "release", "makeup"] {
            assert!(s.plugins.comp.controls.contains_key(k));
        }
        for b in &d.eq.bands {
            assert!(s.band_labels.contains_key(&b.kind));
        }
    }

    #[test]
    fn with_changes_validates_and_tracks_presets() {
        let (s, p, d) = setup();
        assert!(s.with_changes(&d, &ch(json!({"eq.bands.0.gain": 30})), &p).is_err());
        assert!(s.with_changes(&d, &ch(json!({"foo.bar": 1})), &p).is_err());
        assert!(s.with_changes(&d, &ch(json!({"eq.preset": "nope"})), &p).is_err());
        assert!(s.with_changes(&d, &ch(json!({"hpf.freq": "loud"})), &p).is_err());
        assert_eq!(s.with_changes(&d, &ch(json!({"rnnoise.vad": 80})), &p).unwrap().rnnoise.vad, 80.0);

        let st = s.with_changes(&d, &ch(json!({"eq.preset": "stream1"})), &p).unwrap();
        assert_eq!(st.eq.bands[0].kind, "lowshelf");
        assert_eq!((st.hpf.freq, st.lpf.freq), (59.23, 16822.2));
        assert_eq!(st.eq.preset.as_deref(), Some("stream1"));

        let st = s.with_changes(&d, &ch(json!({"eq.preset": "stream1", "hpf.freq": 90})), &p).unwrap();
        assert_eq!((st.hpf.freq, st.eq.preset.as_deref()), (90.0, Some("stream1")));

        let st = s.with_changes(&d, &ch(json!({"eq.bands.1.freq": 300})), &p).unwrap();
        assert_eq!(st.eq.preset, None);

        assert!(!s.with_changes(&d, &ch(json!({"eq.preset": "pop"})), &p).unwrap().lpf.on);
    }

    #[test]
    fn params_encode_bypass() {
        let (s, _, mut d) = setup();
        let deps = Deps { rnnoise: true, comp: true };
        let get = |p: &[(String, f64)], k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| *v).unwrap();
        d.enabled = false;
        let p = params(&d, &deps, &s);
        assert_eq!(get(&p, "hp1:Freq"), 5.0);
        assert_eq!(get(&p, "b0:Gain"), 0.0);
        assert_eq!(get(&p, "lpf:Freq"), 22000.0);
        assert_eq!(get(&p, "rnnoise:VAD Threshold (%)"), 0.0);
        assert_eq!(get(&p, "rnnoise:Dry Mix"), 1.0);
        assert_eq!(get(&p, "comp:Bypass"), 0.0);
        d.enabled = true;
        d.comp.on = true;
        d.comp.threshold = -20.0;
        let p = params(&d, &deps, &s);
        assert_eq!(get(&p, "comp:Bypass"), 1.0);
        assert!((get(&p, "comp:Attack threshold (G)") - 0.1).abs() < 1e-9);
        assert_eq!(get(&p, "rnnoise:Dry Mix"), 0.0);
    }

    #[test]
    fn render_conf_contains_the_contract() {
        let (s, _, d) = setup();
        let full = render_conf(&d, &Deps { rnnoise: true, comp: true }, &s, "alsa_input.test");
        for needle in [
            "label = bq_highpass",
            "label = \"http://lsp-plug.in/plugins/ladspa/compressor_mono\"",
            "target.object      = \"alsa_input.test\"",
            "node.dont-fallback = true",
            "node.linger        = true",
            "audio.position = [ MONO ]",
            "audio.rate = 48000",
            "node.name   = \"maono_clean\"",
        ] {
            assert!(full.contains(needle), "missing {needle}");
        }
        assert_eq!(full.matches("{ output =").count(), 9); // 10 nodes
        let no_comp = render_conf(&d, &Deps { rnnoise: true, comp: false }, &s, "alsa_input.test");
        assert!(!no_comp.contains("compressor_mono"));
        assert_eq!(no_comp.matches("{ output =").count(), 8);
    }

    #[test]
    fn structure_key_tracks_graph_shape_only() {
        let (_, _, d) = setup();
        let deps = Deps { rnnoise: true, comp: true };
        let mut gain = d.clone();
        gain.eq.bands[0].gain = 6.0;
        assert_eq!(structure_key(&d, &deps), structure_key(&gain, &deps));
        let mut shelf = d.clone();
        shelf.eq.bands[0].kind = "lowshelf".into();
        assert_ne!(structure_key(&d, &deps), structure_key(&shelf, &deps));
        assert_ne!(structure_key(&d, &deps), structure_key(&d, &Deps { rnnoise: true, comp: false }));
    }

    #[test]
    fn flatten_roundtrips_through_with_changes() {
        let (s, p, d) = setup();
        let mut other = d.clone();
        other.rnnoise.vad = 40.0;
        other.eq.bands[2].gain = -3.0;
        other.eq.preset = None;
        let back = s.with_changes(&d, &flatten(&other), &p).unwrap();
        assert_eq!(back, other);
    }

    #[test]
    fn user_presets_save_load_delete() {
        let (_, mut p, d) = setup();
        let dir = tempdir("eq-user");
        let saved = p.save_user(&dir, "Mi Voz", &d).unwrap();
        assert_eq!(saved.id, "mi-voz");
        assert!(p.save_user(&dir, "mi voz", &d).is_err());
        assert!(p.delete_user(&dir, "original").is_err());
        let (reloaded, _) = Presets::load(&dir);
        assert!(reloaded.get("mi-voz").is_some());
        p.delete_user(&dir, "mi-voz").unwrap();
        assert!(Presets::load(&dir).0.get("mi-voz").is_none());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib filter`
Expected: compile errors `cannot find type 'FilterSchema'`.

- [ ] **Step 4: Implement**

Above the tests in `src/filter.rs`:

```rust
use crate::state::Values;
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pass {
    pub on: bool,
    pub freq: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Band {
    #[serde(rename = "type")]
    pub kind: String,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Eq {
    pub on: bool,
    #[serde(default)]
    pub preset: Option<String>,
    pub bands: Vec<Band>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RnNoise {
    pub on: bool,
    pub vad: f64,
    pub grace: f64,
    pub retro: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comp {
    pub on: bool,
    pub threshold: f64,
    pub ratio: f64,
    pub attack: f64,
    pub release: f64,
    pub makeup: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterState {
    pub version: u32,
    pub enabled: bool,
    pub hpf: Pass,
    pub eq: Eq,
    pub lpf: Pass,
    pub rnnoise: RnNoise,
    pub comp: Comp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMeta {
    pub node: String,
    pub capture: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterField {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Switch {
    pub port: String,
    pub on: f64,
    pub off: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub package: String,
    pub file: String,
    pub plugin: String,
    pub label: String,
    #[serde(rename = "in")]
    pub input: String,
    #[serde(rename = "out")]
    pub output: String,
    pub controls: BTreeMap<String, String>,
    pub switch: Switch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugins {
    pub rnnoise: Plugin,
    pub comp: Plugin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BypassMeta {
    pub hpf_freq: f64,
    pub lpf_freq: f64,
    pub pass_q: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterSchema {
    pub source: SourceMeta,
    pub fields: Vec<FilterField>,
    pub defaults: FilterState,
    pub band_labels: BTreeMap<String, String>,
    pub bypass: BypassMeta,
    pub plugins: Plugins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Deps {
    pub rnnoise: bool,
    pub comp: bool,
}

static NULL: Value = Value::Null;

/// Values at a dotted key; `*` walks every array element. Missing keys yield null.
fn expand<'a>(v: &'a Value, key: &str) -> Vec<(String, &'a Value)> {
    fn walk<'a>(v: &'a Value, parts: &[&str], path: String, out: &mut Vec<(String, &'a Value)>) {
        let Some((head, rest)) = parts.split_first() else {
            out.push((path, v));
            return;
        };
        let join = |p: &str| if path.is_empty() { p.to_string() } else { format!("{path}.{p}") };
        if *head == "*" {
            for (i, x) in v.as_array().into_iter().flatten().enumerate() {
                walk(x, rest, join(&i.to_string()), out);
            }
        } else {
            walk(v.get(*head).unwrap_or(&NULL), rest, join(head), out);
        }
    }
    let parts: Vec<&str> = key.split('.').collect();
    let mut out = Vec::new();
    walk(v, &parts, String::new(), &mut out);
    out
}

impl FilterSchema {
    pub fn load() -> Self {
        serde_json::from_str(include_str!("../devices/filter.json")).expect("bundled filter schema is valid")
    }

    pub fn validate(&self, s: &FilterState) -> Result<(), Vec<String>> {
        let v = serde_json::to_value(s).expect("FilterState serializes");
        let mut errs = Vec::new();
        if s.eq.bands.len() != 5 {
            errs.push("eq.bands: exactly 5 bands".into());
        }
        for f in &self.fields {
            for (path, val) in expand(&v, &f.key) {
                let ok = match f.kind.as_str() {
                    "bool" => val.is_boolean(),
                    "enum" => val.as_str().is_some_and(|x| f.options.iter().any(|o| o == x)),
                    _ => val.as_f64().is_some_and(|n| f.min.is_none_or(|m| n >= m) && f.max.is_none_or(|m| n <= m)),
                };
                if !ok {
                    errs.push(match f.kind.as_str() {
                        "bool" => format!("{path}: expected true or false"),
                        "enum" => format!("{path}: must be one of {:?}", f.options),
                        _ => format!("{path}: must be a number in {}..{}", f.min.unwrap_or(f64::MIN), f.max.unwrap_or(f64::MAX)),
                    });
                }
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    /// Apply `{"eq.bands.0.gain": 3, "eq.preset": "stream1"}`-style changes and
    /// validate. A preset goes first so explicit keys in the same set win; an
    /// explicit curve edit without a preset clears `eq.preset`.
    pub fn with_changes(&self, base: &FilterState, changes: &Values, presets: &Presets) -> Result<FilterState, Vec<String>> {
        let mut v = serde_json::to_value(base).expect("FilterState serializes");
        if let Some(id) = changes.get("eq.preset") {
            match id.as_str().and_then(|id| presets.get(id)) {
                Some(p) => p.apply_to(&mut v),
                None => return Err(vec![format!("eq.preset: unknown preset {id}")]),
            }
        }
        for (k, val) in changes.iter().filter(|(k, _)| *k != "eq.preset") {
            match v.pointer_mut(&format!("/{}", k.replace('.', "/"))) {
                Some(slot) => *slot = val.clone(),
                None => return Err(vec![format!("{k}: unknown filter setting")]),
            }
        }
        let curve_edit = changes.keys().any(|k| k.starts_with("eq.bands") || k.starts_with("hpf.") || k.starts_with("lpf."));
        if curve_edit && !changes.contains_key("eq.preset") {
            v["eq"]["preset"] = Value::Null;
        }
        let s: FilterState = serde_json::from_value(v).map_err(|e| vec![e.to_string()])?;
        self.validate(&s)?;
        Ok(s)
    }
}

/// Every leaf as a dotted key (arrays indexed), without `version` or nulls.
pub fn flatten(s: &FilterState) -> Values {
    fn walk(v: &Value, path: String, out: &mut Values) {
        let join = |k: &str| if path.is_empty() { k.to_string() } else { format!("{path}.{k}") };
        match v {
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(x, join(k), out)),
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(x, join(&i.to_string()), out)),
            Value::Null => {}
            _ => {
                out.insert(path, v.clone());
            }
        }
    }
    let mut out = Values::new();
    walk(&serde_json::to_value(s).expect("FilterState serializes"), String::new(), &mut out);
    out.remove("version");
    out
}

pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Control value of every node port, bypass included. Feeds both the conf and pw-cli.
pub fn params(s: &FilterState, d: &Deps, schema: &FilterSchema) -> Vec<(String, f64)> {
    let on = s.enabled;
    let by = &schema.bypass;
    let mut p = Vec::new();
    let hp = if on && s.hpf.on { s.hpf.freq } else { by.hpf_freq };
    for n in ["hp1", "hp2"] {
        p.push((format!("{n}:Freq"), hp));
        p.push((format!("{n}:Q"), by.pass_q));
    }
    for (i, b) in s.eq.bands.iter().enumerate() {
        p.push((format!("b{i}:Freq"), b.freq));
        p.push((format!("b{i}:Q"), b.q));
        p.push((format!("b{i}:Gain"), if on && s.eq.on { b.gain } else { 0.0 }));
    }
    p.push(("lpf:Freq".into(), if on && s.eq.on && s.lpf.on { s.lpf.freq } else { by.lpf_freq }));
    p.push(("lpf:Q".into(), by.pass_q));
    if d.rnnoise {
        let pl = &schema.plugins.rnnoise;
        let live = on && s.rnnoise.on;
        p.push((format!("rnnoise:{}", pl.controls["vad"]), if live { s.rnnoise.vad } else { 0.0 }));
        p.push((format!("rnnoise:{}", pl.controls["grace"]), s.rnnoise.grace));
        p.push((format!("rnnoise:{}", pl.controls["retro"]), s.rnnoise.retro));
        p.push((format!("rnnoise:{}", pl.switch.port), if live { pl.switch.on } else { pl.switch.off }));
    }
    if d.comp {
        let pl = &schema.plugins.comp;
        let live = on && s.comp.on;
        p.push((format!("comp:{}", pl.switch.port), if live { pl.switch.on } else { pl.switch.off }));
        p.push((format!("comp:{}", pl.controls["threshold"]), db_to_gain(s.comp.threshold)));
        p.push((format!("comp:{}", pl.controls["ratio"]), s.comp.ratio));
        p.push((format!("comp:{}", pl.controls["attack"]), s.comp.attack));
        p.push((format!("comp:{}", pl.controls["release"]), s.comp.release));
        p.push((format!("comp:{}", pl.controls["makeup"]), db_to_gain(s.comp.makeup)));
    }
    p
}

/// What forces a filter-chain restart: band filter types and which plugins exist.
pub fn structure_key(s: &FilterState, d: &Deps) -> String {
    let kinds: Vec<&str> = s.eq.bands.iter().map(|b| b.kind.as_str()).collect();
    format!("{}|rnnoise={}|comp={}", kinds.join(","), d.rnnoise, d.comp)
}

struct Node {
    decl: String,
    input: String,
    output: String,
}

pub fn render_conf(s: &FilterState, d: &Deps, schema: &FilterSchema, target: &str) -> String {
    let p = params(s, d, schema);
    let ctl = |name: &str| {
        let prefix = format!("{name}:");
        p.iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|port| format!("\"{port}\" = {v}")))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let builtin = |name: String, label: &str| Node {
        decl: format!("{{ type = builtin name = {name} label = {label} control = {{ {} }} }}", ctl(&name)),
        input: format!("{name}:In"),
        output: format!("{name}:Out"),
    };
    let mut nodes = vec![builtin("hp1".into(), "bq_highpass"), builtin("hp2".into(), "bq_highpass")];
    for (i, b) in s.eq.bands.iter().enumerate() {
        nodes.push(builtin(format!("b{i}"), &schema.band_labels[&b.kind]));
    }
    nodes.push(builtin("lpf".into(), "bq_lowpass"));
    for (name, pl, present) in [("rnnoise", &schema.plugins.rnnoise, d.rnnoise), ("comp", &schema.plugins.comp, d.comp)] {
        if present {
            nodes.push(Node {
                decl: format!(
                    "{{ type = ladspa name = {name} plugin = \"{}\" label = \"{}\" control = {{ {} }} }}",
                    pl.plugin,
                    pl.label,
                    ctl(name)
                ),
                input: format!("{name}:{}", pl.input),
                output: format!("{name}:{}", pl.output),
            });
        }
    }
    let decls = nodes.iter().map(|n| n.decl.as_str()).collect::<Vec<_>>().join("\n          ");
    let links = nodes
        .windows(2)
        .map(|w| format!("{{ output = \"{}\" input = \"{}\" }}", w[0].output, w[1].input))
        .collect::<Vec<_>>()
        .join("\n          ");
    let src = &schema.source;
    format!(
        r#"# Generated by maono from ~/.config/maono/filter.json; edits here are overwritten.
context.modules = [
  {{ name = libpipewire-module-filter-chain
    flags = [ nofail ]
    args = {{
      node.description = "{desc}"
      media.name       = "{desc}"
      filter.graph = {{
        nodes = [
          {decls}
        ]
        links = [
          {links}
        ]
      }}
      audio.rate = 48000
      audio.position = [ MONO ]
      capture.props = {{
        node.name          = "{capture}"
        target.object      = "{target}"
        node.passive       = true
        node.dont-fallback = true
        node.linger        = true
        stream.dont-remix  = true
      }}
      playback.props = {{
        node.name   = "{node}"
        media.class = Audio/Source
      }}
    }}
  }}
]
"#,
        desc = src.description,
        capture = src.capture,
        node = src.node,
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: String,
    pub hpf: f64,
    pub lpf: Option<f64>,
    pub bands: Vec<Band>,
}

impl Preset {
    fn apply_to(&self, v: &mut Value) {
        v["eq"]["preset"] = json!(self.id);
        v["eq"]["bands"] = json!(self.bands);
        v["hpf"]["on"] = json!(true);
        v["hpf"]["freq"] = json!(self.hpf);
        match self.lpf {
            Some(f) => {
                v["lpf"]["on"] = json!(true);
                v["lpf"]["freq"] = json!(f);
            }
            None => v["lpf"]["on"] = json!(false),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Presets {
    pub list: Vec<Preset>,
}

impl Presets {
    /// Bundled Maono curves plus the user's presets in `user_dir`.
    pub fn load(user_dir: &Path) -> (Presets, Vec<String>) {
        let mut list: Vec<Preset> = serde_json::from_str(include_str!("../presets/eq/maono.json")).expect("bundled presets are valid");
        let mut warn = Vec::new();
        let mut user = Vec::new();
        for e in fs::read_dir(user_dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match store::read_json::<Preset>(&path) {
                Ok(Some(mut p)) => {
                    p.source = "user".into();
                    if list.iter().any(|b| b.id == p.id) {
                        warn.push(format!("{}: id {} is taken by a built-in preset", path.display(), p.id));
                    } else {
                        user.push(p);
                    }
                }
                Ok(None) => {}
                Err(e) => warn.push(e.to_string()),
            }
        }
        user.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        list.extend(user);
        (Presets { list }, warn)
    }

    pub fn get(&self, id: &str) -> Option<&Preset> {
        self.list.iter().find(|p| p.id == id)
    }

    pub fn save_user(&mut self, dir: &Path, name: &str, s: &FilterState) -> Result<Preset, String> {
        let id = store::slugify(name);
        if id.is_empty() {
            return Err("the name needs at least one letter or digit".into());
        }
        if self.get(&id).is_some() {
            return Err(format!("a preset with id \"{id}\" already exists"));
        }
        let p = Preset {
            id,
            name: name.trim().into(),
            source: "user".into(),
            hpf: s.hpf.freq,
            lpf: s.lpf.on.then_some(s.lpf.freq),
            bands: s.eq.bands.clone(),
        };
        store::write_json(&dir.join(format!("{}.json", p.id)), &p).map_err(|e| e.to_string())?;
        self.list.push(p.clone());
        Ok(p)
    }

    pub fn delete_user(&mut self, dir: &Path, id: &str) -> Result<(), String> {
        match self.get(id) {
            Some(p) if p.source == "user" => {}
            Some(_) => return Err(format!("{id} is a built-in preset")),
            None => return Err(format!("preset {id} not found")),
        }
        fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
        self.list.retain(|p| p.id != id);
        Ok(())
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib filter`
Expected: `7 passed`.

- [ ] **Step 6: Commit**

```bash
git add devices/filter.json presets/eq/maono.json src/filter.rs src/lib.rs
git commit -m "feat: data-driven PipeWire filter model, Maono EQ presets, conf renderer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: PipeWire access (`pw.rs`) and verified apply (`filterctl.rs`)

**Files:**
- Create: `src/pw.rs`
- Create: `src/filterctl.rs`
- Create: `tests/fixtures/pw-dump.json`
- Modify: `src/lib.rs` (add `pub mod pw; pub mod filterctl;`)

**Interfaces:**
- Consumes: `filter::{Deps, FilterSchema, FilterState, params, render_conf, structure_key}`, `store`
- Produces:
  - `pw::Audio` (trait): `deps(&self) -> Deps`, `capture_target(&mut self) -> Option<String>`, `clean_params(&mut self) -> Option<BTreeMap<String,f64>>`, `set_params(&mut self, &[(String,f64)]) -> io::Result<()>`, `restart(&mut self) -> io::Result<()>`, `default_source(&mut self) -> Option<String>`, `set_default_source(&mut self, &str) -> io::Result<()>`
  - `pw::System { schema: FilterSchema }`, which implements `Audio`
  - `pw::{find_capture_node(&Value) -> Option<String>, node_params(&Value, &str) -> Option<(u64, BTreeMap<String,f64>)>, props_arg(&[(String,f64)]) -> String}`
  - test-only `pw::fake::FakeAudio`
  - `filterctl::{FilterCtl<A: Audio> { pub audio: A, pub schema, verify_tries: u32, verify_sleep: Duration }, Outcome { Live, Restarted }}` with `new(A, FilterSchema, conf_path, state_path)`, `load_state() -> (FilterState, Vec<String>)`, `adopt(&mut self, &FilterState) -> bool`, `is_applied() -> bool`, `apply(&mut self, &FilterState) -> Result<Outcome, String>`

- [ ] **Step 1: Fixture**

`tests/fixtures/pw-dump.json` is a trimmed `pw-dump`, with the receiver listed first so the test proves that wired wins:

```json
[
  {"id": 70, "type": "PipeWire:Interface:Device", "info": {"props": {"device.vendor.id": "0x352f", "device.product.id": "0x0414", "device.name": "alsa_card.rx"}}},
  {"id": 71, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "device.id": 70, "node.name": "alsa_input.usb-rx.mono-fallback"}}},
  {"id": 61, "type": "PipeWire:Interface:Device", "info": {"props": {"device.vendor.id": "0x352f", "device.product.id": "0x0417", "device.name": "alsa_card.usb-Maono"}}},
  {"id": 62, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "device.id": 61, "node.name": "alsa_output.usb-Maono.analog-stereo"}}},
  {"id": 63, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "device.id": 61, "node.name": "alsa_input.usb-Maono.mono-fallback"}}},
  {"id": 82, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "maono_clean_in"}, "params": {"Props": [
    {"params": ["monitor.channel-volumes", false]},
    {"params": ["hp1:Freq", 90.0, "hp1:Q", 0.707, "rnnoise:VAD Threshold (%)", 85, "comp:Bypass", true]}
  ]}}}
]
```

- [ ] **Step 2: Write the failing tests**

`src/pw.rs`:

```rust
//! Everything that touches PipeWire / systemd, behind the `Audio` trait.

#[cfg(test)]
mod tests {
    use super::*;

    fn dump() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/pw-dump.json")).unwrap()
    }

    #[test]
    fn capture_node_is_found_by_usb_ids_wired_first() {
        assert_eq!(find_capture_node(&dump()).as_deref(), Some("alsa_input.usb-Maono.mono-fallback"));
    }

    #[test]
    fn node_params_reads_props_pairs() {
        let (id, p) = node_params(&dump(), "maono_clean_in").unwrap();
        assert_eq!(id, 82);
        assert_eq!(p["hp1:Freq"], 90.0);
        assert_eq!(p["rnnoise:VAD Threshold (%)"], 85.0);
        assert_eq!(p["comp:Bypass"], 1.0);
        assert!(!p.contains_key("monitor.channel-volumes"));
        assert!(node_params(&dump(), "nope").is_none());
    }

    #[test]
    fn props_arg_formats_pw_cli_syntax() {
        assert_eq!(props_arg(&[("hp1:Freq".into(), 120.0), ("rnnoise:VAD Threshold (%)".into(), 80.0)]),
            "{ params = [ \"hp1:Freq\" 120 \"rnnoise:VAD Threshold (%)\" 80 ] }");
    }
}
```

`src/filterctl.rs`:

```rust
//! Bring PipeWire to a FilterState: live params when the graph shape is the
//! same, one verified restart otherwise, rollback when the node does not come up.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::fake::FakeAudio;
    use crate::testutil::tempdir;
    use std::path::Path;

    fn ctl(dir: &Path) -> FilterCtl<FakeAudio> {
        let conf = dir.join("pw/maono-clean.conf");
        let mut c = FilterCtl::new(FakeAudio::new(&conf), FilterSchema::load(), conf, dir.join("filter.json"));
        c.verify_sleep = Duration::ZERO;
        c.verify_tries = 3;
        c
    }

    #[test]
    fn first_apply_restarts_then_param_changes_are_live() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        assert_eq!(c.apply(&s), Ok(Outcome::Restarted));
        assert_eq!(c.audio.restarts, 1);
        assert!(dir.join("filter.json").exists());
        s.rnnoise.vad = 70.0;
        assert_eq!(c.apply(&s), Ok(Outcome::Live));
        assert_eq!((c.audio.restarts, c.audio.live_sets), (1, 1));
        assert_eq!(c.audio.node.as_ref().unwrap()["rnnoise:VAD Threshold (%)"], 70.0);
        assert!(fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap().contains("\"VAD Threshold (%)\" = 70"));
    }

    #[test]
    fn band_type_change_restarts() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        s.eq.bands[0].kind = "lowshelf".into();
        assert_eq!(c.apply(&s), Ok(Outcome::Restarted));
        assert_eq!(c.audio.restarts, 2);
    }

    #[test]
    fn broken_restart_restores_previous_conf() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        let before = fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap();
        let mut next = s.clone();
        next.eq.bands[0].kind = "highshelf".into();
        c.audio.broken = true;
        assert!(c.apply(&next).is_err());
        assert_eq!(fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap(), before);
        assert_eq!(c.audio.restarts, 3); // the failed one + the restore
        assert!(!c.is_applied());
    }

    #[test]
    fn adopt_skips_the_restart_when_disk_already_matches() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        let mut again = ctl(&dir);
        again.audio.node = c.audio.node.clone();
        assert!(again.adopt(&s));
        s.rnnoise.vad = 60.0;
        assert_eq!(again.apply(&s), Ok(Outcome::Live));
        assert_eq!(again.audio.restarts, 0);
    }

    #[test]
    fn invalid_state_or_missing_mic_touches_nothing() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut bad = c.schema.defaults.clone();
        bad.rnnoise.vad = 150.0;
        assert!(c.apply(&bad).is_err());
        c.audio.target = None;
        assert!(c.apply(&c.schema.defaults.clone()).is_err());
        assert_eq!(c.audio.restarts, 0);
        assert!(!dir.join("pw/maono-clean.conf").exists());
    }
}
```

Add `pub mod pw; pub mod filterctl;` to `src/lib.rs`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib pw filterctl`
Expected: compile errors `cannot find function 'find_capture_node'`, `cannot find type 'FilterCtl'`.

- [ ] **Step 4: Implement `pw.rs`**

Above the tests:

```rust
use crate::filter::{Deps, FilterSchema};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::Command;

pub trait Audio {
    fn deps(&self) -> Deps;
    /// node.name of the mic's raw PipeWire source, found by USB ids.
    fn capture_target(&mut self) -> Option<String>;
    /// Current Props of the chain's capture node; None when the node does not exist.
    fn clean_params(&mut self) -> Option<BTreeMap<String, f64>>;
    fn set_params(&mut self, params: &[(String, f64)]) -> io::Result<()>;
    fn restart(&mut self) -> io::Result<()>;
    fn default_source(&mut self) -> Option<String>;
    fn set_default_source(&mut self, name: &str) -> io::Result<()>;
}

fn props(o: &Value) -> &Value {
    o.pointer("/info/props").unwrap_or(&Value::Null)
}

pub fn find_capture_node(dump: &Value) -> Option<String> {
    let objs = dump.as_array()?;
    let mut devices: Vec<(u64, bool)> = objs
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Device" && props(o)["device.vendor.id"] == "0x352f")
        .filter_map(|o| {
            let wired = match props(o)["device.product.id"].as_str()? {
                "0x0417" => true,
                "0x0414" => false,
                _ => return None,
            };
            Some((o["id"].as_u64()?, wired))
        })
        .collect();
    devices.sort_by_key(|&(_, wired)| !wired);
    devices.iter().find_map(|&(dev, _)| {
        objs.iter().find_map(|o| {
            let p = props(o);
            if p["media.class"] == "Audio/Source" && p["device.id"].as_u64() == Some(dev) {
                p["node.name"].as_str().map(String::from)
            } else {
                None
            }
        })
    })
}

pub fn node_params(dump: &Value, node_name: &str) -> Option<(u64, BTreeMap<String, f64>)> {
    let o = dump.as_array()?.iter().find(|o| props(o)["node.name"] == node_name)?;
    let mut out = BTreeMap::new();
    for entry in o.pointer("/info/params/Props").and_then(Value::as_array).into_iter().flatten() {
        let Some(list) = entry.get("params").and_then(Value::as_array) else { continue };
        for kv in list.chunks(2) {
            if let [Value::String(k), v] = kv {
                if !k.contains(':') {
                    continue; // channelmix.* and friends, not ours
                }
                if let Some(n) = v.as_f64().or_else(|| v.as_bool().map(|b| b as u8 as f64)) {
                    out.insert(k.clone(), n);
                }
            }
        }
    }
    Some((o["id"].as_u64()?, out))
}

pub fn props_arg(params: &[(String, f64)]) -> String {
    let body = params.iter().map(|(k, v)| format!("\"{k}\" {v}")).collect::<Vec<_>>().join(" ");
    format!("{{ params = [ {body} ] }}")
}

fn run(cmd: &str, args: &[&str]) -> io::Result<String> {
    let out = Command::new(cmd).args(args).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(io::Error::other(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

fn dump() -> Option<Value> {
    serde_json::from_str(&run("pw-dump", &[]).ok()?).ok()
}

fn ladspa_dirs() -> Vec<PathBuf> {
    match std::env::var_os("LADSPA_PATH") {
        Some(p) => std::env::split_paths(&p).collect(),
        None => ["/usr/lib/ladspa", "/usr/lib64/ladspa", "/usr/local/lib/ladspa"].iter().map(PathBuf::from).collect(),
    }
}

pub struct System {
    pub schema: FilterSchema,
}

impl Audio for System {
    fn deps(&self) -> Deps {
        let dirs = ladspa_dirs();
        let has = |f: &str| dirs.iter().any(|d| d.join(f).is_file());
        Deps { rnnoise: has(&self.schema.plugins.rnnoise.file), comp: has(&self.schema.plugins.comp.file) }
    }

    fn capture_target(&mut self) -> Option<String> {
        find_capture_node(&dump()?)
    }

    fn clean_params(&mut self) -> Option<BTreeMap<String, f64>> {
        node_params(&dump()?, &self.schema.source.capture).map(|(_, p)| p)
    }

    fn set_params(&mut self, params: &[(String, f64)]) -> io::Result<()> {
        let d = dump().ok_or_else(|| io::Error::other("pw-dump failed"))?;
        let (id, _) = node_params(&d, &self.schema.source.capture).ok_or_else(|| io::Error::other("filter node not found"))?;
        run("pw-cli", &["set-param", &id.to_string(), "Props", &props_arg(params)]).map(drop)
    }

    fn restart(&mut self) -> io::Result<()> {
        run("systemctl", &["--user", "restart", "filter-chain.service"]).map(drop)
    }

    fn default_source(&mut self) -> Option<String> {
        run("pactl", &["get-default-source"]).ok().map(|s| s.trim().to_string())
    }

    fn set_default_source(&mut self, name: &str) -> io::Result<()> {
        run("pactl", &["set-default-source", name]).map(drop)
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::path::{Path, PathBuf};

    /// PipeWire stand-in: "loads" the conf on restart by parsing its control values.
    pub struct FakeAudio {
        pub deps: Deps,
        pub target: Option<String>,
        pub node: Option<BTreeMap<String, f64>>,
        pub restarts: u32,
        pub live_sets: u32,
        pub broken: bool,
        pub default: Option<String>,
        conf: PathBuf,
    }

    impl FakeAudio {
        pub fn new(conf: &Path) -> Self {
            FakeAudio {
                deps: Deps { rnnoise: true, comp: true },
                target: Some("alsa_input.test".into()),
                node: None,
                restarts: 0,
                live_sets: 0,
                broken: false,
                default: None,
                conf: conf.to_path_buf(),
            }
        }
    }

    fn parse_controls(conf: &str) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        for line in conf.lines() {
            let Some(name) = line.split("type = ").nth(1).and_then(|s| s.split("name = ").nth(1)).and_then(|s| s.split_whitespace().next()) else { continue };
            let Some(ctl) = line.split("control = {").nth(1) else { continue };
            let parts: Vec<&str> = ctl.split('"').collect();
            for pair in parts[1..].chunks(2) {
                if let [port, rest] = pair {
                    if let Some(v) = rest.trim_start_matches([' ', '=']).split_whitespace().next().and_then(|x| x.parse().ok()) {
                        out.insert(format!("{name}:{port}"), v);
                    }
                }
            }
        }
        out
    }

    impl Audio for FakeAudio {
        fn deps(&self) -> Deps {
            self.deps
        }
        fn capture_target(&mut self) -> Option<String> {
            self.target.clone()
        }
        fn clean_params(&mut self) -> Option<BTreeMap<String, f64>> {
            self.node.clone()
        }
        fn set_params(&mut self, params: &[(String, f64)]) -> io::Result<()> {
            let node = self.node.as_mut().ok_or_else(|| io::Error::other("no node"))?;
            for (k, v) in params {
                node.insert(k.clone(), *v);
            }
            self.live_sets += 1;
            Ok(())
        }
        fn restart(&mut self) -> io::Result<()> {
            self.restarts += 1;
            self.node = if self.broken { None } else { std::fs::read_to_string(&self.conf).ok().map(|c| parse_controls(&c)) };
            Ok(())
        }
        fn default_source(&mut self) -> Option<String> {
            self.default.clone()
        }
        fn set_default_source(&mut self, name: &str) -> io::Result<()> {
            self.default = Some(name.into());
            Ok(())
        }
    }
}
```

- [ ] **Step 5: Implement `filterctl.rs`**

Above the tests:

```rust
use crate::filter::{Deps, FilterSchema, FilterState, params, render_conf, structure_key};
use crate::pw::Audio;
use crate::store;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Live,
    Restarted,
}

pub struct FilterCtl<A: Audio> {
    pub audio: A,
    pub schema: FilterSchema,
    pub verify_tries: u32,
    pub verify_sleep: Duration,
    conf_path: PathBuf,
    state_path: PathBuf,
    applied: Option<String>,
    target: Option<String>,
}

impl<A: Audio> FilterCtl<A> {
    pub fn new(audio: A, schema: FilterSchema, conf_path: PathBuf, state_path: PathBuf) -> Self {
        FilterCtl { audio, schema, verify_tries: 25, verify_sleep: Duration::from_millis(200), conf_path, state_path, applied: None, target: None }
    }

    /// filter.json, or the schema defaults (with a warning when the file is bad).
    pub fn load_state(&self) -> (FilterState, Vec<String>) {
        match store::read_json::<FilterState>(&self.state_path) {
            Ok(Some(s)) => match self.schema.validate(&s) {
                Ok(()) => (s, Vec::new()),
                Err(e) => (self.schema.defaults.clone(), vec![format!("{}: {}", self.state_path.display(), e.join("; "))]),
            },
            Ok(None) => (self.schema.defaults.clone(), Vec::new()),
            Err(e) => (self.schema.defaults.clone(), vec![e.to_string()]),
        }
    }

    pub fn is_applied(&self) -> bool {
        self.applied.is_some()
    }

    fn key(s: &FilterState, d: &Deps, target: &str) -> String {
        format!("{}|{target}", structure_key(s, d))
    }

    /// At startup: when the conf on disk is exactly what we would write and the
    /// node is up, take it over without restarting (no audio drop).
    pub fn adopt(&mut self, s: &FilterState) -> bool {
        let d = self.audio.deps();
        let Some(target) = self.audio.capture_target() else { return false };
        let same = fs::read_to_string(&self.conf_path).is_ok_and(|c| c == render_conf(s, &d, &self.schema, &target));
        if same && self.audio.clean_params().is_some() {
            self.applied = Some(Self::key(s, &d, &target));
            self.target = Some(target);
            true
        } else {
            false
        }
    }

    pub fn apply(&mut self, next: &FilterState) -> Result<Outcome, String> {
        self.schema.validate(next).map_err(|e| e.join("; "))?;
        let d = self.audio.deps();
        let target = self.audio.capture_target().or_else(|| self.target.clone()).ok_or("the mic is not visible in PipeWire")?;
        let conf = render_conf(next, &d, &self.schema, &target);
        let key = Self::key(next, &d, &target);
        if self.applied.as_deref() == Some(key.as_str()) && self.audio.clean_params().is_some() {
            self.audio.set_params(&params(next, &d, &self.schema)).map_err(|e| e.to_string())?;
            store::write_atomic(&self.conf_path, conf.as_bytes()).map_err(|e| e.to_string())?;
            store::write_json(&self.state_path, next).map_err(|e| e.to_string())?;
            return Ok(Outcome::Live);
        }
        let previous = fs::read_to_string(&self.conf_path).ok();
        store::write_atomic(&self.conf_path, conf.as_bytes()).map_err(|e| e.to_string())?;
        if self.restart_and_verify(next, &d) {
            self.applied = Some(key);
            self.target = Some(target);
            store::write_json(&self.state_path, next).map_err(|e| e.to_string())?;
            return Ok(Outcome::Restarted);
        }
        match previous {
            Some(p) => store::write_atomic(&self.conf_path, p.as_bytes()).map_err(|e| e.to_string())?,
            None => {
                let _ = fs::remove_file(&self.conf_path);
            }
        }
        let _ = self.audio.restart();
        self.applied = None;
        Err("the filter did not come up with the new settings; the previous configuration was restored".into())
    }

    fn restart_and_verify(&mut self, s: &FilterState, d: &Deps) -> bool {
        if self.audio.restart().is_err() {
            return false;
        }
        let want = params(s, d, &self.schema);
        for _ in 0..self.verify_tries {
            if let Some(have) = self.audio.clean_params() {
                // PipeWire stores floats: compare with a relative tolerance.
                if want.iter().all(|(k, v)| have.get(k).is_some_and(|h| (h - v).abs() <= 1e-3 * v.abs().max(1.0))) {
                    return true;
                }
            }
            std::thread::sleep(self.verify_sleep);
        }
        false
    }
}

```

- [ ] **Step 6: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib pw filterctl`
Expected: `8 passed`.

- [ ] **Step 7: Commit**

```bash
git add src/pw.rs src/filterctl.rs tests/fixtures/pw-dump.json src/lib.rs
git commit -m "feat: PipeWire access behind a trait; verified filter apply with rollback

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Profiles (`profiles.rs`, `profiles/factory.json`)

**Files:**
- Create: `profiles/factory.json`
- Create: `src/profiles.rs`
- Modify: `src/lib.rs` (add `pub mod profiles;`)

**Interfaces:**
- Consumes: `Descriptor`, `state::{Values, check_color, same_color}`, `filter::{FilterSchema, FilterState, Presets}`, `store::{slugify, read_json, write_json}`
- Produces:
  - `profiles::Profile { version: u32, id: String, name: String, mic: Option<Values>, light: Option<Values>, filter: Option<Values> }`
  - `profiles::Store { dir: PathBuf }` with `list() -> (Vec<Profile>, Vec<String>)`, `get(&str) -> Option<Profile>`, `put(&Profile) -> io::Result<()>`, `create(&str, Option<Values>, Option<Values>, Option<Values>) -> Result<Profile,String>`, `rename(&str,&str) -> Result<Profile,String>`, `duplicate(&str) -> Result<Profile,String>`, `delete(&str) -> Result<(),String>`, `seed(Option<[f64;3]>) -> io::Result<bool>`
  - `profiles::{validate(&Profile, &Descriptor, &FilterSchema, &Presets, &FilterState) -> Result<(), Vec<String>>, dirty(&Profile, &Values, &FilterState, &FilterSchema, &Presets) -> bool, theme_accent_hsv() -> Option<[f64;3]>, hex_to_hsv(&str) -> Option<[f64;3]>}`

- [ ] **Step 1: Factory profiles (data)**

`profiles/factory.json`. `"light.color": "theme"` is a placeholder that `seed` replaces with the Omarchy accent; it falls back to blue.

```json
[
  {"version": 1, "id": "llamada", "name": "Llamada",
   "mic": {"mic.gain": 20, "mic.nr.on": true, "mic.nr.level": 0},
   "filter": {"enabled": true, "eq.preset": "original", "eq.on": true, "hpf.on": true, "hpf.freq": 90, "rnnoise.on": true, "rnnoise.vad": 85, "comp.on": true, "comp.threshold": -24, "comp.ratio": 3}},
  {"version": 1, "id": "streaming", "name": "Streaming / Discord",
   "mic": {"mic.gain": 20, "mic.nr.on": true, "mic.nr.level": 0},
   "light": {"light.on": true, "light.effect": 0, "light.brightness": 40, "light.color": "theme"},
   "filter": {"enabled": true, "eq.preset": "stream1", "eq.on": true, "hpf.on": true, "hpf.freq": 90, "rnnoise.on": true, "rnnoise.vad": 80, "comp.on": true, "comp.threshold": -20, "comp.ratio": 4}},
  {"version": 1, "id": "grabacion", "name": "Grabación",
   "mic": {"mic.gain": 18, "mic.nr.on": false},
   "filter": {"enabled": true, "eq.preset": "original", "eq.on": true, "hpf.on": true, "hpf.freq": 70, "rnnoise.on": true, "rnnoise.vad": 0, "comp.on": false}},
  {"version": 1, "id": "gaming", "name": "Gaming",
   "mic": {"mic.gain": 20, "mic.nr.on": true, "mic.nr.level": 1},
   "filter": {"enabled": true, "eq.preset": "game1", "eq.on": true, "hpf.on": true, "hpf.freq": 100, "rnnoise.on": true, "rnnoise.vad": 92, "comp.on": true, "comp.threshold": -20, "comp.ratio": 4}}
]
```

- [ ] **Step 2: Write the failing tests**

`src/profiles.rs`:

```rust
//! User profiles: partial snapshots of mic + light + filter settings, one JSON
//! file each in ~/.config/maono/profiles/. Profiles never contain `mic.mute`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;
    use serde_json::json;

    fn ctx() -> (Descriptor, FilterSchema, Presets) {
        (Descriptor::load(), FilterSchema::load(), Presets::load(&tempdir("eq")).0)
    }

    fn vals(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn seed_writes_factory_profiles_once_with_the_theme_colour() {
        let s = Store { dir: tempdir("p").join("profiles") };
        assert!(s.seed(Some([14.0, 0.54, 0.95])).unwrap());
        assert!(!s.seed(None).unwrap());
        let (list, warn) = s.list();
        assert!(warn.is_empty());
        assert_eq!(list.len(), 4);
        let streaming = s.get("streaming").unwrap();
        assert_eq!(streaming.light.unwrap()["light.color"], json!({"hsv": [14.0, 0.54, 0.95]}));
        let (d, f, p) = ctx();
        for prof in &list {
            validate(prof, &d, &f, &p, &f.defaults).unwrap_or_else(|e| panic!("{}: {e:?}", prof.id));
        }
    }

    #[test]
    fn crud_keeps_ids_stable_and_rejects_collisions() {
        let s = Store { dir: tempdir("p") };
        let a = s.create("Voz de Noche", None, None, None).unwrap();
        assert_eq!(a.id, "voz-de-noche");
        assert!(s.create("voz de noche", None, None, None).is_err());
        assert!(s.create("  ", None, None, None).is_err());
        let r = s.rename("voz-de-noche", "Noche").unwrap();
        assert_eq!((r.id.as_str(), r.name.as_str()), ("voz-de-noche", "Noche"));
        let d = s.duplicate("voz-de-noche").unwrap();
        assert_eq!((d.id.as_str(), d.name.as_str()), ("noche-2", "Noche 2"));
        s.delete("noche-2").unwrap();
        assert!(s.get("noche-2").is_none());
    }

    #[test]
    fn list_reports_corrupt_and_mismatched_files_without_deleting() {
        let s = Store { dir: tempdir("p") };
        s.create("Ok", None, None, None).unwrap();
        std::fs::write(s.dir.join("broken.json"), "{").unwrap();
        std::fs::write(s.dir.join("other.json"), r#"{"version":1,"id":"different","name":"x"}"#).unwrap();
        let (list, warn) = s.list();
        assert_eq!(list.len(), 1);
        assert_eq!(warn.len(), 2);
        assert!(s.dir.join("broken.json").exists());
    }

    #[test]
    fn validate_rejects_bad_profiles() {
        let (d, f, p) = ctx();
        let base = f.defaults.clone();
        let mk = |mic: Value, light: Value, filter: Value| Profile {
            version: 1, id: "x".into(), name: "X".into(),
            mic: mic.as_object().cloned(), light: light.as_object().cloned(), filter: filter.as_object().cloned(),
        };
        assert!(validate(&mk(json!({"mic.mute": true}), json!(null), json!(null)), &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!({"mic.gain": 99}), json!(null), json!(null)), &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!({"dsp.comp.on": true}), json!(null), json!(null)), &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!({"light.on": true}), json!(null), json!(null)), &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!(null), json!({"light.color": {"preset": "teal"}}), json!(null)), &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!(null), json!(null), json!({"rnnoise.vad": 300})), &d, &f, &p, &base).is_err());
        let mut bad_id = mk(json!({"mic.gain": 10}), json!(null), json!(null));
        bad_id.id = "../etc".into();
        assert!(validate(&bad_id, &d, &f, &p, &base).is_err());
        assert!(validate(&mk(json!({"mic.gain": 10}), json!({"light.color": {"preset": "red"}}), json!({"rnnoise.vad": 50})), &d, &f, &p, &base).is_ok());
    }

    #[test]
    fn dirty_only_looks_at_the_profiles_own_keys() {
        let (_, f, p) = ctx();
        let prof = Profile { version: 1, id: "x".into(), name: "X".into(), mic: Some(vals(json!({"mic.gain": 20}))), light: None, filter: Some(vals(json!({"rnnoise.vad": 85}))) };
        let mic = vals(json!({"mic.gain": 20, "light.brightness": 70}));
        assert!(!dirty(&prof, &mic, &f.defaults, &f, &p));
        let mic2 = vals(json!({"mic.gain": 14, "light.brightness": 70}));
        assert!(dirty(&prof, &mic2, &f.defaults, &f, &p));
        let mut fs2 = f.defaults.clone();
        fs2.rnnoise.vad = 50.0;
        assert!(dirty(&prof, &mic, &fs2, &f, &p));
        fs2.rnnoise.vad = 85.0;
        fs2.comp.ratio = 9.0; // not in the profile
        assert!(!dirty(&prof, &mic, &fs2, &f, &p));
    }

    #[test]
    fn hex_to_hsv_converts_theme_accents() {
        let [h, s, v] = hex_to_hsv("#f38d70").unwrap();
        assert!((h - 13.2).abs() < 0.5 && (s - 0.54).abs() < 0.01 && (v - 0.95).abs() < 0.01, "{h} {s} {v}");
        assert!(hex_to_hsv("nope").is_none());
    }
}
```

Add `pub mod profiles;` to `src/lib.rs`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib profiles`
Expected: compile errors `cannot find struct 'Store'`.

- [ ] **Step 4: Implement**

Above the tests:

```rust
use crate::descriptor::Descriptor;
use crate::filter::{FilterSchema, FilterState, Presets};
use crate::state::{self, Values};
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<Values>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<Values>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Values>,
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Sorted by name. Corrupt or mismatched files are reported, never deleted.
    pub fn list(&self) -> (Vec<Profile>, Vec<String>) {
        let mut out = Vec::new();
        let mut warn = Vec::new();
        for e in fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match store::read_json::<Profile>(&path) {
                Ok(Some(p)) if path.file_stem().is_some_and(|s| s == p.id.as_str()) => out.push(p),
                Ok(Some(_)) => warn.push(format!("{}: id does not match the file name", path.display())),
                Ok(None) => {}
                Err(e) => warn.push(e.to_string()),
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        (out, warn)
    }

    pub fn get(&self, id: &str) -> Option<Profile> {
        if store::slugify(id) != id {
            return None;
        }
        store::read_json(&self.path(id)).ok().flatten()
    }

    pub fn put(&self, p: &Profile) -> io::Result<()> {
        store::write_json(&self.path(&p.id), p)
    }

    pub fn create(&self, name: &str, mic: Option<Values>, light: Option<Values>, filter: Option<Values>) -> Result<Profile, String> {
        let id = store::slugify(name);
        if id.is_empty() {
            return Err("the name needs at least one letter or digit".into());
        }
        if self.path(&id).exists() {
            return Err(format!("a profile with id \"{id}\" already exists"));
        }
        let p = Profile { version: 1, id, name: name.trim().into(), mic, light, filter };
        self.put(&p).map_err(|e| e.to_string())?;
        Ok(p)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<Profile, String> {
        let mut p = self.get(id).ok_or_else(|| format!("profile {id} not found"))?;
        if name.trim().is_empty() {
            return Err("the name is empty".into());
        }
        p.name = name.trim().into();
        self.put(&p).map_err(|e| e.to_string())?;
        Ok(p)
    }

    pub fn duplicate(&self, id: &str) -> Result<Profile, String> {
        let src = self.get(id).ok_or_else(|| format!("profile {id} not found"))?;
        let mut n = 2;
        loop {
            let name = format!("{} {n}", src.name);
            if !self.path(&store::slugify(&name)).exists() {
                return self.create(&name, src.mic.clone(), src.light.clone(), src.filter.clone());
            }
            n += 1;
        }
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if store::slugify(id) != id {
            return Err(format!("profile {id} not found"));
        }
        fs::remove_file(self.path(id)).map_err(|e| format!("{id}: {e}"))
    }

    /// First run only: write the bundled profiles. False when the folder already existed.
    pub fn seed(&self, accent_hsv: Option<[f64; 3]>) -> io::Result<bool> {
        if self.dir.exists() {
            return Ok(false);
        }
        let list: Vec<Profile> = serde_json::from_str(include_str!("../profiles/factory.json")).expect("bundled profiles are valid");
        for mut p in list {
            if let Some(light) = p.light.as_mut() {
                if light.get("light.color") == Some(&json!("theme")) {
                    let c = accent_hsv.map_or(json!({"preset": "blue"}), |hsv| json!({"hsv": hsv}));
                    light.insert("light.color".into(), c);
                }
            }
            self.put(&p)?;
        }
        Ok(true)
    }
}

pub fn validate(p: &Profile, desc: &Descriptor, fschema: &FilterSchema, presets: &Presets, base: &FilterState) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    if p.id.is_empty() || store::slugify(&p.id) != p.id {
        errs.push(format!("id {:?} must look like \"mi-voz\"", p.id));
    }
    if p.name.trim().is_empty() {
        errs.push("name is empty".into());
    }
    for (is_light, map) in [(false, &p.mic), (true, &p.light)] {
        for (k, v) in map.iter().flatten() {
            if is_light && k == "light.color" {
                if let Err(e) = state::check_color(desc, v) {
                    errs.push(e);
                }
                continue;
            }
            match desc.field(k) {
                Some(f) if f.profile && k.starts_with("light.") == is_light => {
                    if let Err(e) = f.to_raw(v) {
                        errs.push(e);
                    }
                }
                _ => errs.push(format!("{k}: not a profile setting")),
            }
        }
    }
    if let Some(changes) = &p.filter {
        if let Err(e) = fschema.with_changes(base, changes, presets) {
            errs.extend(e);
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// True when applying the profile now would change something. Only the
/// profile's own keys count.
pub fn dirty(p: &Profile, mic: &Values, filter: &FilterState, fschema: &FilterSchema, presets: &Presets) -> bool {
    let differs = |k: &str, want: &Value| match (mic.get(k), want) {
        (Some(have), want) if k == "light.color" => !state::same_color(have, want),
        (Some(Value::Number(a)), Value::Number(b)) => (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() > 1e-6,
        (Some(have), want) => have != want,
        (None, _) => true,
    };
    let device = p.mic.iter().chain(p.light.iter()).flatten().any(|(k, v)| differs(k, v));
    let filt = p.filter.as_ref().is_some_and(|f| fschema.with_changes(filter, f, presets).map_or(true, |next| next != *filter));
    device || filt
}

pub fn hex_to_hsv(hex: &str) -> Option<[f64; 3]> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|x| x as f64 / 255.0);
    let (r, g, b) = (c(0)?, c(2)?, c(4)?);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let hue = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    Some([hue.round(), round2(if max == 0.0 { 0.0 } else { d / max }), round2(max)])
}

/// Omarchy's current theme accent as HSV, if the theme file has one.
pub fn theme_accent_hsv() -> Option<[f64; 3]> {
    let home = std::env::var_os("HOME")?;
    let text = fs::read_to_string(PathBuf::from(home).join(".local/state/omarchy/current/theme/colors.toml")).ok()?;
    let line = text.lines().find(|l| l.trim_start().starts_with("accent"))?;
    hex_to_hsv(line.split('=').nth(1)?.trim().trim_matches('"'))
}
```

The `hex_to_hsv` test expects hue ≈ 13 (the hue of `#f38d70`), so `round()` stays on the hue. The seed test passes `[14.0, 0.54, 0.95]` directly, so the two tests do not depend on each other.

- [ ] **Step 5: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib profiles`
Expected: `6 passed`.

- [ ] **Step 6: Commit**

```bash
git add profiles/factory.json src/profiles.rs src/lib.rs
git commit -m "feat: profiles with validation, dirty tracking and factory set

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Command core (`engine.rs`)

**Files:**
- Create: `src/engine.rs`
- Modify: `src/lib.rs` (add `pub mod engine;` — not `core`, which would shadow the `core` crate in macro expansions)

**Interfaces:**
- Consumes: everything above
- Produces:
  - `engine::PROTOCOL: u32 = 1`
  - `engine::Link { Connected, Disconnected, Permission }`
  - `engine::Core<A: Audio>` with:
    - public fields `desc`, `fschema`, `presets`, `profiles`, `filter`, `filterctl`, `config`, `dir`, `dev: Option<Device>`, `link`, `model`, `mic: Values`
    - `new(dir: PathBuf, conf_path: PathBuf, audio: A) -> Self`
    - `start_filter(&mut self)`, `hello(&mut self)`, `state_json(&mut self) -> Value`
    - `connect(&mut self, Device, reconnect: bool)`, `disconnect(&mut self, Link)`, `on_frame(&mut self, &Frame)`, `pump(&mut self, Duration)`
    - `handle_line(&mut self, &str)`, `take_events(&mut self) -> Vec<Value>`
  - Commands (JSON `cmd`, request id in `id`):
    - `status`, `refresh`, `profile.list`
    - `set {changes}`, `filter.set {changes}`, `config.set {changes}`
    - `source.default {which: clean|raw}`
    - `profile.apply {profile}`, `profile.save {name, groups?, overwrite?}`, `profile.rename {profile, name}`, `profile.duplicate {profile}`, `profile.delete {profile}`
    - `eq.preset.save {name}`, `eq.preset.delete {preset}`
    - `light.custom {op: add|update|delete, index?, hsv?}`
    - `recover`: after a reconnect reapplied the profile over unsaved edits, put those edits back (once)
  - Events (JSON `ev`): `hello`, `state`, `changed`, `ack`, `profiles`, `presets`, `reapplied`, `error`

(The spec's `filter.bypass` command is covered by `filter.set {"enabled": false}`, `{"eq.on": false}` and so on; one command fewer with the same effect.)

- [ ] **Step 1: Write the failing tests**

`src/engine.rs`:

```rust
//! All command logic. `serve` and the CLI feed it JSON lines and forward the
//! events it produces; nothing here does I/O except through Device and Audio.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeMic, pd100w_regs};
    use crate::pw::fake::FakeAudio;
    use crate::testutil::tempdir;

    fn core() -> (Core<FakeAudio>, PathBuf) {
        let dir = tempdir("core");
        let conf = dir.join("pw/maono-clean.conf");
        let mut c = Core::new(dir.clone(), conf.clone(), FakeAudio::new(&conf));
        c.filterctl.verify_sleep = Duration::ZERO;
        c.filterctl.verify_tries = 2;
        (c, dir)
    }

    fn connected() -> (Core<FakeAudio>, FakeMic) {
        let (mut c, _) = core();
        let (dev, fake) = FakeMic::new(&pd100w_regs());
        c.connect(dev, false);
        c.take_events();
        (c, fake)
    }

    fn ask(c: &mut Core<FakeAudio>, req: Value) -> Value {
        c.handle_line(&req.to_string());
        c.take_events().into_iter().find(|e| e["ev"] == "ack").expect("an ack")
    }

    #[test]
    fn hello_then_state() {
        let (mut c, _) = core();
        c.hello();
        let ev = c.take_events();
        assert_eq!(ev[0]["ev"], "hello");
        assert_eq!(ev[0]["protocol"], 1);
        assert_eq!(ev[0]["profiles"].as_array().unwrap().len(), 4);
        assert_eq!(ev[1]["ev"], "state");
        assert_eq!(ev[1]["device"], "disconnected");
    }

    #[test]
    fn connect_reads_state_and_silences_the_hid_meter() {
        let (c, fake) = connected();
        assert_eq!(c.link, Link::Connected);
        assert_eq!(c.mic["mic.gain"], json!(20));
        assert_eq!(fake.reg(0x0045), Some(1));
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn set_acks_effective_values_and_invalid_writes_nothing() {
        let (mut c, fake) = connected();
        let a = ask(&mut c, json!({"id": 1, "cmd": "set", "changes": {"mic.gain": 14}}));
        assert_eq!(a["ok"], true);
        assert_eq!(a["effective"]["mic.gain"], 14);
        let a = ask(&mut c, json!({"id": 2, "cmd": "set", "changes": {"mic.gain": 30}}));
        assert_eq!(a["ok"], false);
        assert_eq!(fake.writes(), vec![(0x207E, 14)]);
        let a = ask(&mut c, json!({"id": 3, "cmd": "nonsense"}));
        assert_eq!((a["ok"].clone(), a["id"].clone()), (json!(false), json!(3)));
    }

    #[test]
    fn button_press_becomes_a_changed_event() {
        let (mut c, fake) = connected();
        fake.press(0x207D, 1);
        c.pump(Duration::from_millis(50));
        let ev = c.take_events();
        assert!(ev.iter().any(|e| e["ev"] == "changed" && e["key"] == "mic.mute" && e["value"] == true && e["source"] == "button"));
        assert_eq!(c.mic["mic.mute"], json!(true));
    }

    #[test]
    fn apply_profile_sets_mic_filter_and_active() {
        let (mut c, fake) = connected();
        fake.press(0x207D, 1); // muted before applying
        c.pump(Duration::from_millis(20));
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "gaming"}));
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(fake.reg(0x2085), Some(1));
        assert_eq!(fake.reg(0x207D), Some(1), "applying a profile never unmutes");
        assert_eq!(c.filter.eq.preset.as_deref(), Some("game1"));
        assert_eq!(c.config.active_profile.as_deref(), Some("gaming"));
        assert_eq!(c.state_json()["dirty"], false);
        ask(&mut c, json!({"id": 2, "cmd": "set", "changes": {"mic.gain": 12}}));
        assert_eq!(c.state_json()["dirty"], true);
    }

    #[test]
    fn apply_profile_unplugged_midway_reports_partial() {
        let (mut c, fake) = connected();
        *fake.gone_after_writes.lock().unwrap() = Some(1);
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "llamada"}));
        assert_eq!(a["ok"], false);
        assert_eq!(a["failed"][0]["group"], "mic");
        assert_eq!(c.config.active_profile, None);
        assert_eq!(c.link, Link::Disconnected);
        assert_eq!(c.state_json()["partial"]["profile"], "llamada");
    }

    #[test]
    fn invalid_profile_writes_nothing() {
        let (mut c, fake) = connected();
        std::fs::write(c.profiles.dir.join("hack.json"), r#"{"version":1,"id":"hack","name":"Hack","mic":{"mic.gain":99,"mic.mute":false}}"#).unwrap();
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "hack"}));
        assert_eq!(a["ok"], false);
        assert_eq!(a["error"], "invalid profile");
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn profile_with_broken_filter_is_partial() {
        let (mut c, fake) = connected();
        c.filterctl.audio.broken = true; // gaming's game1 curve has a high shelf: a restart is needed
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "gaming"}));
        assert_eq!(a["ok"], false);
        assert_eq!(a["applied"], json!(["mic"]));
        assert_eq!(a["failed"][0]["group"], "filter");
        assert_eq!(fake.reg(0x207E), Some(20));
    }

    #[test]
    fn missing_compressor_is_a_warning_not_a_failure() {
        let (mut c, _fake) = connected();
        c.filterctl.audio.deps.comp = false;
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "streaming"}));
        assert_eq!(a["ok"], true, "{a}");
        assert!(a["warnings"][0].as_str().unwrap().contains("lsp-plugins-ladspa"));
    }

    #[test]
    fn reconnect_reapplies_without_unmuting_but_first_connect_does_not() {
        let (mut c, _) = core();
        c.config.active_profile = Some("grabacion".into());
        let (dev, fake) = FakeMic::new(&pd100w_regs());
        c.connect(dev, false);
        assert!(fake.writes().is_empty());
        let mut regs = pd100w_regs();
        regs.push((0x207D, 1));
        let (dev2, fake2) = FakeMic::new(&regs);
        c.connect(dev2, true);
        assert_eq!(fake2.reg(0x207E), Some(18));
        assert_eq!(fake2.reg(0x207D), Some(1));
        assert!(c.take_events().iter().any(|e| e["ev"] == "reapplied"));
    }

    #[test]
    fn reconnect_with_unsaved_changes_offers_recovery() {
        let (mut c, _fake) = connected();
        assert_eq!(ask(&mut c, json!({"id": 1, "cmd": "profile.apply", "profile": "grabacion"}))["ok"], true);
        ask(&mut c, json!({"id": 2, "cmd": "set", "changes": {"mic.gain": 7}}));
        assert_eq!(c.state_json()["dirty"], true);
        let (dev2, fake2) = FakeMic::new(&pd100w_regs());
        c.connect(dev2, true);
        assert_eq!(fake2.reg(0x207E), Some(18), "the profile is reapplied");
        let ev = c.take_events();
        assert!(ev.iter().any(|e| e["ev"] == "reapplied" && e["recoverable"] == true));
        assert_eq!(ask(&mut c, json!({"id": 3, "cmd": "recover"}))["ok"], true);
        assert_eq!(fake2.reg(0x207E), Some(7), "the unsaved gain is back");
        assert_eq!(ask(&mut c, json!({"id": 4, "cmd": "recover"}))["ok"], false, "only once");
    }

    #[test]
    fn save_rename_delete_profiles() {
        let (mut c, _fake) = connected();
        let a = ask(&mut c, json!({"id": 1, "cmd": "profile.save", "name": "Mi Voz", "groups": ["mic", "filter"]}));
        assert_eq!(a["id"], "mi-voz");
        let p = c.profiles.get("mi-voz").unwrap();
        assert!(p.light.is_none());
        assert_eq!(p.mic.unwrap()["mic.gain"], 20);
        assert!(!p.filter.unwrap().contains_key("version"));
        assert_eq!(ask(&mut c, json!({"id": 2, "cmd": "profile.save", "name": "mi voz"}))["ok"], false);
        ask(&mut c, json!({"id": 3, "cmd": "profile.apply", "profile": "mi-voz"}));
        assert_eq!(ask(&mut c, json!({"id": 4, "cmd": "profile.rename", "profile": "mi-voz", "name": "Noche"}))["ok"], true);
        assert_eq!(ask(&mut c, json!({"id": 5, "cmd": "profile.delete", "profile": "mi-voz"}))["ok"], true);
        assert_eq!(c.config.active_profile, None);
        let list = ask(&mut c, json!({"id": 6, "cmd": "profile.list"}));
        assert_eq!(list["profiles"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn filter_set_and_default_source() {
        let (mut c, _fake) = connected();
        let a = ask(&mut c, json!({"id": 1, "cmd": "filter.set", "changes": {"rnnoise.vad": 60}}));
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(c.filter.rnnoise.vad, 60.0);
        assert_eq!(ask(&mut c, json!({"id": 2, "cmd": "filter.set", "changes": {"rnnoise.vad": 600}}))["ok"], false);
        let a = ask(&mut c, json!({"id": 3, "cmd": "source.default", "which": "clean"}));
        assert_eq!(a["defaultSource"], "maono_clean");
        assert_eq!(ask(&mut c, json!({"id": 4, "cmd": "config.set", "changes": {"applyOnReconnect": false}}))["ok"], true);
        assert!(!c.config.apply_on_reconnect);
    }
}
```

`profile.*` commands name the profile with `"profile"` and `eq.preset.delete` with `"preset"`, because `id` is the request id. (The spec's examples reused `id` for both, and one JSON object cannot hold `id` twice.)

Add `pub mod engine;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib engine`
Expected: compile errors `cannot find struct 'Core'`.

- [ ] **Step 3: Implement**

Above the tests in `src/engine.rs`:

```rust
use crate::config::Config;
use crate::descriptor::Descriptor;
use crate::device::{DevError, Device, Model};
use crate::filter::{self, FilterSchema, FilterState, Presets};
use crate::filterctl::{FilterCtl, Outcome};
use crate::profiles::{self, Store};
use crate::proto::{Frame, GET, SET};
use crate::pw::Audio;
use crate::state::{self, ApplyError, Values};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Connected,
    Disconnected,
    Permission,
}

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<Value>,
    #[serde(flatten)]
    cmd: Cmd,
}

fn all_groups() -> Vec<String> {
    vec!["mic".into(), "filter".into(), "light".into()]
}

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
enum Cmd {
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "refresh")]
    Refresh,
    #[serde(rename = "set")]
    Set { changes: Values },
    #[serde(rename = "filter.set")]
    FilterSet { changes: Values },
    #[serde(rename = "source.default")]
    SourceDefault { which: String },
    #[serde(rename = "profile.list")]
    ProfileList,
    #[serde(rename = "profile.apply")]
    ProfileApply { profile: String },
    #[serde(rename = "profile.save")]
    ProfileSave {
        name: String,
        #[serde(default = "all_groups")]
        groups: Vec<String>,
        #[serde(default)]
        overwrite: Option<String>,
    },
    #[serde(rename = "profile.rename")]
    ProfileRename { profile: String, name: String },
    #[serde(rename = "profile.duplicate")]
    ProfileDuplicate { profile: String },
    #[serde(rename = "profile.delete")]
    ProfileDelete { profile: String },
    #[serde(rename = "eq.preset.save")]
    PresetSave { name: String },
    #[serde(rename = "eq.preset.delete")]
    PresetDelete { preset: String },
    #[serde(rename = "light.custom")]
    LightCustom {
        op: String,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        hsv: Option<Value>,
    },
    #[serde(rename = "config.set")]
    ConfigSet { changes: Values },
    #[serde(rename = "recover")]
    Recover,
}

fn err(msg: impl std::fmt::Display) -> Value {
    json!({ "error": msg.to_string() })
}

pub struct Core<A: Audio> {
    pub desc: Descriptor,
    pub fschema: FilterSchema,
    pub presets: Presets,
    pub profiles: Store,
    pub filter: FilterState,
    pub filterctl: FilterCtl<A>,
    pub config: Config,
    pub dir: PathBuf,
    pub dev: Option<Device>,
    pub link: Link,
    pub model: Option<Model>,
    pub mic: Values,
    partial: Option<Value>,
    /// Unsaved mic/filter values from before a reconnect reapplied the profile.
    recover: Option<(Values, FilterState)>,
    warnings: Vec<String>,
    out: Vec<Value>,
}

impl<A: Audio> Core<A> {
    pub fn new(dir: PathBuf, conf_path: PathBuf, audio: A) -> Self {
        let mut warnings = Vec::new();
        let profiles = Store { dir: dir.join("profiles") };
        if let Err(e) = profiles.seed(profiles::theme_accent_hsv()) {
            warnings.push(format!("profiles: {e}"));
        }
        let (presets, w) = Presets::load(&dir.join("eq"));
        warnings.extend(w);
        let (config, w) = Config::load(&dir);
        warnings.extend(w);
        let fschema = FilterSchema::load();
        let filterctl = FilterCtl::new(audio, fschema.clone(), conf_path, dir.join("filter.json"));
        let (filter, w) = filterctl.load_state();
        warnings.extend(w);
        Core {
            desc: Descriptor::load(),
            fschema,
            presets,
            profiles,
            filter,
            filterctl,
            config,
            dir,
            dev: None,
            link: Link::Disconnected,
            model: None,
            mic: Values::new(),
            partial: None,
            recover: None,
            warnings,
            out: Vec::new(),
        }
    }

    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.out)
    }

    fn emit(&mut self, v: Value) {
        self.out.push(v);
    }

    /// Take over a matching chain without a restart, or (re)build it once.
    pub fn start_filter(&mut self) {
        if !self.filterctl.adopt(&self.filter) {
            if let Err(e) = self.filterctl.apply(&self.filter) {
                self.warnings.push(format!("filter: {e}"));
            }
        }
    }

    pub fn hello(&mut self) {
        let (profiles, w) = self.profiles.list();
        let warnings: Vec<String> = self.warnings.iter().cloned().chain(w).collect();
        let hello = json!({
            "ev": "hello",
            "protocol": PROTOCOL,
            "schema": { "device": self.desc.schema(), "filter": self.fschema },
            "presets": self.presets.list,
            "profiles": profiles,
            "config": self.config,
            "warnings": warnings,
        });
        self.emit(hello);
        self.emit_state();
    }

    pub fn state_json(&mut self) -> Value {
        let deps = self.filterctl.audio.deps();
        let default_source = self.filterctl.audio.default_source();
        let active = self.config.active_profile.clone();
        let dirty = active
            .as_deref()
            .and_then(|id| self.profiles.get(id))
            .is_some_and(|p| profiles::dirty(&p, &self.mic, &self.filter, &self.fschema, &self.presets));
        json!({
            "ev": "state",
            "device": self.link,
            "model": self.model,
            "mic": self.mic,
            "filter": self.filter,
            "deps": { "rnnoise": deps.rnnoise, "comp": deps.comp },
            "activeProfile": active,
            "dirty": dirty,
            "partial": self.partial,
            "defaultSource": default_source,
            "cleanSource": self.fschema.source.node,
        })
    }

    fn emit_state(&mut self) {
        let s = self.state_json();
        self.emit(s);
    }

    fn emit_profiles(&mut self) {
        let (list, _) = self.profiles.list();
        self.emit(json!({ "ev": "profiles", "profiles": list }));
    }

    fn emit_presets(&mut self) {
        let list = self.presets.list.clone();
        self.emit(json!({ "ev": "presets", "presets": list }));
    }

    fn save_config(&mut self) -> Result<(), Value> {
        self.config.save(&self.dir).map_err(err)
    }

    /// A mic appeared. `reconnect` is true when one was connected earlier in this run.
    pub fn connect(&mut self, mut dev: Device, reconnect: bool) {
        let previous = std::mem::take(&mut self.mic);
        let _ = dev.set(&[(self.desc.meter.id, self.desc.meter.off as i64)]);
        let mut pending = Vec::new();
        match state::read_all(&mut dev, &self.desc, &mut |f| pending.push(f.clone())) {
            Ok(values) => {
                self.mic = values;
                self.model = Some(dev.model);
                self.dev = Some(dev);
                self.link = Link::Connected;
            }
            Err(e) => {
                self.emit(json!({ "ev": "error", "code": "device", "message": e.to_string() }));
                self.link = Link::Disconnected;
                return;
            }
        }
        for f in &pending {
            self.on_frame(f);
        }
        if !self.filterctl.is_applied() {
            self.start_filter();
        }
        if reconnect && self.config.apply_on_reconnect {
            if let Some(id) = self.config.active_profile.clone() {
                // `previous` holds what we knew before the unplug; dirty means it had unsaved edits.
                self.recover = self.profiles.get(&id).filter(|p| profiles::dirty(p, &previous, &self.filter, &self.fschema, &self.presets)).map(|_| {
                    let keep: Values = self.desc.fields.iter().filter(|f| f.profile).filter_map(|f| previous.get(&f.key).filter(|v| !v.is_null()).map(|v| (f.key.clone(), v.clone()))).collect();
                    (keep, self.filter.clone())
                });
                let r = match self.apply_profile(&id) {
                    Ok(v) | Err(v) => v,
                };
                self.emit(json!({ "ev": "reapplied", "profile": id, "result": r, "recoverable": self.recover.is_some() }));
            }
        }
        self.emit_state();
    }

    pub fn disconnect(&mut self, link: Link) {
        self.dev = None;
        self.link = link;
        self.emit_state();
    }

    /// Frames outside a request: button presses (SET) and answers to other readers (GET).
    pub fn on_frame(&mut self, f: &Frame) {
        if f.kind != SET && f.kind != GET {
            return;
        }
        for &(id, raw) in &f.fields {
            let Some(field) = self.desc.field_by_id(id) else { continue };
            let (key, v) = (field.key.clone(), field.from_raw(raw));
            if self.mic.get(&key) != Some(&v) {
                self.mic.insert(key.clone(), v.clone());
                if f.kind == SET {
                    self.emit(json!({ "ev": "changed", "key": key, "value": v, "source": "button" }));
                }
            }
        }
    }

    /// Drain device frames for up to `wait`. Called by serve between requests.
    pub fn pump(&mut self, wait: Duration) {
        let Some(dev) = self.dev.as_mut() else { return };
        let mut frames = Vec::new();
        let r = dev.poll(wait, &mut |f| frames.push(f.clone()));
        for f in &frames {
            self.on_frame(f);
        }
        if matches!(r, Err(DevError::Gone)) {
            self.disconnect(Link::Disconnected);
        }
    }

    /// Handle one JSONL request. Always emits exactly one ack, then a state.
    pub fn handle_line(&mut self, line: &str) {
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let id = serde_json::from_str::<Value>(line).ok().and_then(|v| v.get("id").cloned());
                self.emit(json!({ "ev": "ack", "id": id, "ok": false, "error": format!("bad request: {e}") }));
                return;
            }
        };
        let (ok, extra) = match self.handle(req.cmd) {
            Ok(v) => (true, v),
            Err(v) => (false, v),
        };
        let mut ack = json!({ "ev": "ack", "id": req.id, "ok": ok });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                ack[k] = v;
            }
        }
        self.emit(ack);
        self.emit_state();
    }

    fn handle(&mut self, cmd: Cmd) -> Result<Value, Value> {
        match cmd {
            Cmd::Status => Ok(json!({ "state": self.state_json() })),
            Cmd::Refresh => self.refresh(),
            Cmd::Set { changes } => self.set_mic(&changes),
            Cmd::FilterSet { changes } => {
                let next = self.fschema.with_changes(&self.filter, &changes, &self.presets).map_err(|e| json!({ "error": "invalid", "details": e }))?;
                let outcome = self.filterctl.apply(&next).map_err(err)?;
                self.filter = next;
                Ok(json!({ "restarted": outcome == Outcome::Restarted }))
            }
            Cmd::SourceDefault { which } => {
                let name = match which.as_str() {
                    "clean" => self.fschema.source.node.clone(),
                    "raw" => self.filterctl.audio.capture_target().ok_or_else(|| err("the mic is not visible in PipeWire"))?,
                    _ => return Err(err("which must be clean or raw")),
                };
                self.filterctl.audio.set_default_source(&name).map_err(err)?;
                if self.filterctl.audio.default_source().as_deref() != Some(name.as_str()) {
                    return Err(err("PipeWire did not switch the default source"));
                }
                Ok(json!({ "defaultSource": name }))
            }
            Cmd::ProfileList => {
                let (list, warnings) = self.profiles.list();
                Ok(json!({ "profiles": list, "warnings": warnings }))
            }
            Cmd::ProfileApply { profile } => self.apply_profile(&profile),
            Cmd::ProfileSave { name, groups, overwrite } => self.save_profile(&name, &groups, overwrite.as_deref()),
            Cmd::ProfileRename { profile, name } => {
                self.profiles.rename(&profile, &name).map_err(err)?;
                self.emit_profiles();
                Ok(json!({}))
            }
            Cmd::ProfileDuplicate { profile } => {
                let p = self.profiles.duplicate(&profile).map_err(err)?;
                self.emit_profiles();
                Ok(json!({ "profile": p.id }))
            }
            Cmd::ProfileDelete { profile } => {
                self.profiles.delete(&profile).map_err(err)?;
                if self.config.active_profile.as_deref() == Some(profile.as_str()) {
                    self.config.active_profile = None;
                    self.save_config()?;
                }
                self.emit_profiles();
                Ok(json!({}))
            }
            Cmd::PresetSave { name } => {
                let p = self.presets.save_user(&self.dir.join("eq"), &name, &self.filter).map_err(err)?;
                self.emit_presets();
                Ok(json!({ "preset": p.id }))
            }
            Cmd::PresetDelete { preset } => {
                self.presets.delete_user(&self.dir.join("eq"), &preset).map_err(err)?;
                self.emit_presets();
                Ok(json!({}))
            }
            Cmd::LightCustom { op, index, hsv } => self.light_custom(&op, index, hsv.as_ref()),
            Cmd::ConfigSet { changes } => {
                for (k, v) in &changes {
                    match (k.as_str(), v) {
                        ("applyOnReconnect", Value::Bool(b)) => self.config.apply_on_reconnect = *b,
                        _ => return Err(err(format!("{k}: unknown or invalid setting"))),
                    }
                }
                self.save_config()?;
                Ok(json!({}))
            }
            Cmd::Recover => {
                let (mic, filt) = self.recover.take().ok_or_else(|| err("nothing to recover"))?;
                self.set_mic(&mic)?;
                self.filterctl.apply(&filt).map_err(err)?;
                self.filter = filt;
                Ok(json!({}))
            }
        }
    }

    fn refresh(&mut self) -> Result<Value, Value> {
        let dev = self.dev.as_mut().ok_or_else(|| err("the mic is not connected"))?;
        let mut frames = Vec::new();
        let r = state::read_all(dev, &self.desc, &mut |f| frames.push(f.clone()));
        for f in &frames {
            self.on_frame(f);
        }
        match r {
            Ok(v) => {
                self.mic = v;
                Ok(json!({}))
            }
            Err(e) => Err(self.device_error(e)),
        }
    }

    fn device_error(&mut self, e: DevError) -> Value {
        if matches!(e, DevError::Gone) {
            self.disconnect(Link::Disconnected);
        }
        err(e)
    }

    fn apply_err(&mut self, e: ApplyError) -> Value {
        match e {
            ApplyError::Invalid(details) => json!({ "error": "invalid", "details": details }),
            ApplyError::Device(d) => self.device_error(d),
        }
    }

    fn set_mic(&mut self, changes: &Values) -> Result<Value, Value> {
        let dev = self.dev.as_mut().ok_or_else(|| err("the mic is not connected"))?;
        let mut frames = Vec::new();
        let r = state::apply(dev, &self.desc, changes, &self.mic, &mut |f| frames.push(f.clone()));
        for f in &frames {
            self.on_frame(f);
        }
        match r {
            Ok(eff) => {
                for (k, v) in &eff {
                    self.mic.insert(k.clone(), v.clone());
                }
                Ok(json!({ "effective": eff }))
            }
            Err(e) => Err(self.apply_err(e)),
        }
    }

    pub fn apply_profile(&mut self, id: &str) -> Result<Value, Value> {
        let p = self.profiles.get(id).ok_or_else(|| err(format!("profile {id} not found")))?;
        profiles::validate(&p, &self.desc, &self.fschema, &self.presets, &self.filter)
            .map_err(|e| json!({ "error": "invalid profile", "details": e }))?;
        let mut applied: Vec<&str> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let device: Values = p.mic.iter().chain(p.light.iter()).flatten().map(|(k, v)| (k.clone(), v.clone())).collect();
        if !device.is_empty() {
            match self.set_mic(&device) {
                Ok(_) => applied.push("mic"),
                Err(e) => failed.push(json!({ "group": "mic", "error": e })),
            }
        }
        if let Some(changes) = &p.filter {
            let deps = self.filterctl.audio.deps();
            let next = self.fschema.with_changes(&self.filter, changes, &self.presets).expect("validated above");
            if next.comp.on && !deps.comp {
                warnings.push(format!("compressor skipped: {} is not installed", self.fschema.plugins.comp.package));
            }
            if next.rnnoise.on && !deps.rnnoise {
                warnings.push(format!("noise suppression skipped: {} is not installed", self.fschema.plugins.rnnoise.package));
            }
            match self.filterctl.apply(&next) {
                Ok(_) => {
                    self.filter = next;
                    applied.push("filter");
                }
                Err(e) => failed.push(json!({ "group": "filter", "error": e })),
            }
        }
        if failed.is_empty() {
            self.config.active_profile = Some(id.to_string());
            self.partial = None;
            self.save_config()?;
            Ok(json!({ "applied": applied, "warnings": warnings }))
        } else {
            self.partial = Some(json!({ "profile": id, "applied": applied, "failed": failed }));
            Err(json!({ "error": "partially applied", "applied": applied, "failed": failed, "warnings": warnings }))
        }
    }

    fn save_profile(&mut self, name: &str, groups: &[String], overwrite: Option<&str>) -> Result<Value, Value> {
        let has = |g: &str| groups.iter().any(|x| x == g);
        let pick = |light: bool| -> Values {
            self.desc
                .fields
                .iter()
                .filter(|f| f.profile && f.key.starts_with("light.") == light)
                .filter_map(|f| self.mic.get(&f.key).filter(|v| !v.is_null()).map(|v| (f.key.clone(), v.clone())))
                .collect()
        };
        let mic = has("mic").then(|| pick(false));
        let light = has("light").then(|| {
            let mut l = pick(true);
            if let Some(c) = self.mic.get("light.color").filter(|c| c.get("preset").is_some() || c.get("hsv").is_some()) {
                l.insert("light.color".into(), c.clone());
            }
            l
        });
        let filt = has("filter").then(|| filter::flatten(&self.filter));
        let p = match overwrite {
            Some(id) => {
                let mut p = self.profiles.get(id).ok_or_else(|| err(format!("profile {id} not found")))?;
                p.mic = mic;
                p.light = light;
                p.filter = filt;
                self.profiles.put(&p).map_err(err)?;
                p
            }
            None => self.profiles.create(name, mic, light, filt).map_err(err)?,
        };
        self.emit_profiles();
        Ok(json!({ "profile": p.id }))
    }

    fn light_custom(&mut self, op: &str, index: Option<usize>, hsv: Option<&Value>) -> Result<Value, Value> {
        let dev = self.dev.as_mut().ok_or_else(|| err("the mic is not connected"))?;
        let mut frames = Vec::new();
        let r = match (op, index, hsv) {
            ("add", _, Some(h)) => {
                let change: Values = [("light.color".to_string(), json!({ "hsv": h }))].into_iter().collect();
                state::apply(dev, &self.desc, &change, &self.mic, &mut |f| frames.push(f.clone()))
            }
            ("update", Some(k), Some(h)) => state::custom_update(dev, &self.desc, &self.mic, k, h, &mut |f| frames.push(f.clone())),
            ("delete", Some(k), None) => state::custom_delete(dev, &self.desc, &self.mic, k, &mut |f| frames.push(f.clone())),
            _ => return Err(err("light.custom takes op add {hsv}, update {index, hsv} or delete {index}")),
        };
        for f in &frames {
            self.on_frame(f);
        }
        match r {
            Ok(eff) => {
                for (k, v) in &eff {
                    self.mic.insert(k.clone(), v.clone());
                }
                Ok(json!({ "effective": eff }))
            }
            Err(e) => Err(self.apply_err(e)),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib engine`
Expected: `14 passed`.

- [ ] **Step 5: Run the whole suite**

Run: `mise exec rust@stable -- cargo test --lib`
Expected: everything passes, with no warnings about unused imports.

- [ ] **Step 6: Commit**

```bash
git add src/engine.rs src/lib.rs
git commit -m "feat: command core: set, filter, profiles with partial-apply contract, reconnect

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: `maono serve` loop (`serve.rs`)

**Files:**
- Create: `src/serve.rs`
- Modify: `src/lib.rs` (add `pub mod serve;`)

**Interfaces:**
- Consumes: `Core`, `Link`, `device::{find, Device}`, `pw::System`, `store::{socket_path, lock_path, config_dir, pipewire_conf}`, `FilterSchema`
- Produces:
  - `serve::{Input { Line(String, Option<Sender<String>>), Eof }, Opened { Device(Device), Permission, None }}`
  - `serve::run<A: Audio>(core: &mut Core<A>, inputs: Receiver<Input>, out: &mut dyn Write, open: &mut dyn FnMut() -> Opened, tick: Duration, probe_every: Duration) -> io::Result<()>`
  - `serve::main() -> io::Result<()>`: the lock, the socket listener, the stdin reader and the real device opener

- [ ] **Step 1: Write the failing tests**

`src/serve.rs`:

```rust
//! The long-running owner of the mic. Requests arrive from stdin (the Omarchy
//! panel) and from a Unix socket (CLI, keybinds); one queue, one at a time.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeMic, pd100w_regs};
    use crate::pw::fake::FakeAudio;
    use crate::testutil::tempdir;
    use serde_json::Value;
    use std::sync::mpsc;

    fn core() -> Core<FakeAudio> {
        let dir = tempdir("serve");
        let conf = dir.join("pw/maono-clean.conf");
        let mut c = Core::new(dir, conf.clone(), FakeAudio::new(&conf));
        c.filterctl.verify_sleep = Duration::ZERO;
        c
    }

    fn lines(out: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(out).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn hello_first_then_acks() {
        let mut c = core();
        let (tx, rx) = mpsc::channel();
        tx.send(Input::Line(r#"{"id":1,"cmd":"set","changes":{"mic.gain":11}}"#.into(), None)).unwrap();
        tx.send(Input::Eof).unwrap();
        let (dev, fake) = FakeMic::new(&pd100w_regs());
        let mut devs = vec![Opened::Device(dev)];
        let mut out = Vec::new();
        run(&mut c, rx, &mut out, &mut || devs.pop().unwrap_or(Opened::None), Duration::from_millis(5), Duration::ZERO).unwrap();
        let ev = lines(&out);
        assert_eq!(ev[0]["ev"], "hello");
        assert!(ev.iter().any(|e| e["ev"] == "ack" && e["id"] == 1 && e["ok"] == true));
        assert_eq!(fake.reg(0x207E), Some(11));
    }

    #[test]
    fn permission_denied_is_reported_and_loop_survives() {
        let mut c = core();
        let (tx, rx) = mpsc::channel();
        tx.send(Input::Line(r#"{"id":7,"cmd":"status"}"#.into(), None)).unwrap();
        tx.send(Input::Eof).unwrap();
        let mut out = Vec::new();
        run(&mut c, rx, &mut out, &mut || Opened::Permission, Duration::from_millis(5), Duration::ZERO).unwrap();
        let ev = lines(&out);
        assert!(ev.iter().any(|e| e["ev"] == "state" && e["device"] == "permission"));
        assert!(ev.iter().any(|e| e["ev"] == "ack" && e["id"] == 7));
    }

    #[test]
    fn queued_requests_run_in_order_with_own_acks() {
        let mut c = core();
        let (tx, rx) = mpsc::channel();
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        tx.send(Input::Line(r#"{"id":"a","cmd":"set","changes":{"mic.gain":5,"light.brightness":50}}"#.into(), Some(a_tx))).unwrap();
        tx.send(Input::Line(r#"{"id":"b","cmd":"set","changes":{"mic.gain":9}}"#.into(), Some(b_tx))).unwrap();
        tx.send(Input::Eof).unwrap();
        let (dev, fake) = FakeMic::new(&pd100w_regs());
        let mut devs = vec![Opened::Device(dev)];
        let mut out = Vec::new();
        run(&mut c, rx, &mut out, &mut || devs.pop().unwrap_or(Opened::None), Duration::from_millis(5), Duration::ZERO).unwrap();
        let a: Vec<Value> = a_rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
        let b: Vec<Value> = b_rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
        assert!(a.iter().any(|e| e["ev"] == "ack" && e["id"] == "a"));
        assert!(b.iter().any(|e| e["ev"] == "ack" && e["id"] == "b"));
        assert!(!a.iter().any(|e| e["id"] == "b"));
        // serde_json maps are key-sorted: light.brightness goes out before mic.gain
        assert_eq!(fake.writes(), vec![(0x208A, 50), (0x207E, 5), (0x207E, 9)]);
    }
}
```

Add `pub mod serve;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib serve`
Expected: compile errors `cannot find function 'run'`.

- [ ] **Step 3: Implement**

Above the tests:

```rust
use crate::engine::{Core, Link};
use crate::device::{self, Device};
use crate::filter::FilterSchema;
use crate::pw::{self, Audio};
use crate::store;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub enum Input {
    Line(String, Option<Sender<String>>),
    Eof,
}

pub enum Opened {
    Device(Device),
    Permission,
    None,
}

pub fn run<A: Audio>(
    core: &mut Core<A>,
    inputs: Receiver<Input>,
    out: &mut dyn Write,
    open: &mut dyn FnMut() -> Opened,
    tick: Duration,
    probe_every: Duration,
) -> io::Result<()> {
    core.start_filter();
    core.hello();
    let mut was_connected = false;
    let mut next_probe = Instant::now();
    loop {
        if core.dev.is_none() && Instant::now() >= next_probe {
            match open() {
                Opened::Device(d) => {
                    core.connect(d, was_connected);
                    was_connected = true;
                }
                Opened::Permission if core.link != Link::Permission => core.disconnect(Link::Permission),
                Opened::None if core.link != Link::Disconnected => core.disconnect(Link::Disconnected),
                _ => {}
            }
            next_probe = Instant::now() + probe_every;
        }
        flush(core, out, None)?;
        match inputs.recv_timeout(tick) {
            Ok(Input::Line(line, reply)) => {
                core.handle_line(&line);
                flush(core, out, reply.as_ref())?;
            }
            Ok(Input::Eof) | Err(RecvTimeoutError::Disconnected) => {
                flush(core, out, None)?;
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        core.pump(Duration::ZERO);
    }
}

/// Every event goes to stdout; the requesting socket client also gets its ack and state.
fn flush<A: Audio>(core: &mut Core<A>, out: &mut dyn Write, reply: Option<&Sender<String>>) -> io::Result<()> {
    for ev in core.take_events() {
        let line = ev.to_string();
        writeln!(out, "{line}")?;
        if let Some(r) = reply {
            if ev["ev"] == "ack" || ev["ev"] == "state" {
                let _ = r.send(line);
            }
        }
    }
    out.flush()
}

fn open_device() -> Opened {
    match device::find().first() {
        None => Opened::None,
        Some(f) => match Device::open(f) {
            Ok(d) => Opened::Device(d),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Opened::Permission,
            Err(_) => Opened::None,
        },
    }
}

/// One socket client: each request line is answered with the lines up to and
/// including its own ack.
fn serve_client(conn: UnixStream, tx: Sender<Input>) {
    let Ok(mut writer) = conn.try_clone() else { return };
    for line in io::BufReader::new(conn).lines().map_while(Result::ok) {
        let id = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null);
        let (rtx, rrx) = mpsc::channel();
        if tx.send(Input::Line(line, Some(rtx))).is_err() {
            return;
        }
        for reply in rrx.iter() {
            let done = serde_json::from_str::<Value>(&reply).is_ok_and(|v| v["ev"] == "ack" && v["id"] == id);
            if writeln!(writer, "{reply}").is_err() {
                return;
            }
            if done {
                break;
            }
        }
    }
}

pub fn main() -> io::Result<()> {
    let lock = File::create(store::lock_path())?;
    if lock.try_lock().is_err() {
        eprintln!("maono: another `maono serve` is already running");
        std::process::exit(2);
    }
    let sock = store::socket_path();
    let _ = fs::remove_file(&sock); // we hold the lock, so any socket file is stale
    let listener = UnixListener::bind(&sock)?;
    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in io::stdin().lines() {
                match line {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => {
                        if tx.send(Input::Line(l, None)).is_err() {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(Input::Eof);
        });
    }
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            thread::spawn(move || serve_client(conn, tx));
        }
    });
    let mut core = Core::new(store::config_dir(), store::pipewire_conf(), pw::System { schema: FilterSchema::load() });
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let r = run(&mut core, rx, &mut out, &mut open_device, Duration::from_millis(50), Duration::from_secs(1));
    let _ = fs::remove_file(&sock);
    drop(lock);
    r
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mise exec rust@stable -- cargo test --lib serve`
Expected: `3 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/serve.rs src/lib.rs
git commit -m "feat: maono serve: single queue over stdin and a Unix socket, device probing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: CLI on top of serve (`main.rs`)

**Files:**
- Create: `src/client.rs`
- Modify: `src/lib.rs` (add `pub mod client;`)
- Modify: `src/main.rs` (rewrite the dispatch; keep `tui`, `shell`, `theme`, `widgets`)
- Modify: `src/tui.rs` (refuse to start while serve holds the lock)

**Interfaces:**
- Consumes: `serve::main`, `Core`, `pw::System`, `device`, `store`, `Descriptor`, `FilterSchema`
- Produces:
  - `client::request(Value) -> Result<Value /*ack*/, String>`: the socket first, then direct mode under the lock
  - `client::to_request(&[&str]) -> Result<Value, String>`: argv to request JSON (pure, tested)
  - `client::legacy_status_json(&Value /*state*/) -> Value`: the `status --json` shape the old bar widget reads
- CLI commands:
  - `serve`, `schema`
  - `status [--json]`
  - `set k=v…`, `filter set k=v…`
  - `profile list|apply <id>|save <name>|rename <id> <name>|duplicate <id>|delete <id>`
  - `source clean|raw`
  - kept from before: `mute|unmute|toggle`, `gain [n|+n|-n]`, `nr off|low|mid|high`, `light on|off|next|<colour>`
  - read-only: `get <id>`, `scan [lo] [hi]`
  - `shell install|uninstall`; TUI with no arguments

- [ ] **Step 1: Write the failing tests**

`src/client.rs`:

```rust
//! The CLI side of the protocol: argv -> request, and request -> ack via the
//! serve socket, or in-process under the same lock when serve is not running.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn argv_maps_to_requests() {
        assert_eq!(to_request(&["set", "mic.gain=12", "light.on=true"]).unwrap()["changes"], json!({"mic.gain": 12, "light.on": true}));
        assert_eq!(to_request(&["filter", "set", "eq.preset=stream1"]).unwrap(), json!({"cmd": "filter.set", "changes": {"eq.preset": "stream1"}}));
        assert_eq!(to_request(&["profile", "apply", "llamada"]).unwrap(), json!({"cmd": "profile.apply", "profile": "llamada"}));
        assert_eq!(to_request(&["profile", "save", "Mi", "Voz"]).unwrap(), json!({"cmd": "profile.save", "name": "Mi Voz"}));
        assert_eq!(to_request(&["source", "clean"]).unwrap(), json!({"cmd": "source.default", "which": "clean"}));
        assert_eq!(to_request(&["mute"]).unwrap(), json!({"cmd": "set", "changes": {"mic.mute": true}}));
        assert_eq!(to_request(&["nr", "high"]).unwrap()["changes"], json!({"mic.nr.on": true, "mic.nr.level": 2}));
        assert_eq!(to_request(&["nr", "off"]).unwrap()["changes"], json!({"mic.nr.on": false}));
        assert_eq!(to_request(&["light", "purple"]).unwrap_err(), "unknown colour purple (white, red, orange, yellow, green, cyan, blue, magenta)");
        assert!(to_request(&["set", "nonsense"]).is_err());
    }

    #[test]
    fn legacy_status_matches_the_old_widget_shape() {
        let state = json!({"device": "connected", "mic": {"mic.mute": false, "info.battery": 87, "mic.gain": 12, "mic.nr.on": true, "mic.nr.level": 1, "light.on": true, "light.color": {"preset": "green"}}});
        let s = legacy_status_json(&state);
        assert_eq!(s["class"], "live");
        assert_eq!(s["gain_max"], 20);
        assert_eq!(s["nr"], "on, mid");
        assert_eq!(s["light_mode_name"], "green");
        assert_eq!(s["light_mode"], 4);
    }
}
```

Add `pub mod client;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib client`
Expected: compile errors `cannot find function 'to_request'`.

- [ ] **Step 3: Implement `client.rs`**

Above the tests:

```rust
use crate::engine::Core;
use crate::descriptor::Descriptor;
use crate::device::{self, Device};
use crate::filter::FilterSchema;
use crate::pw;
use crate::store;
use serde_json::{Map, Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn parse_kv(args: &[&str]) -> Result<Map<String, Value>, String> {
    let mut m = Map::new();
    for a in args {
        let (k, v) = a.split_once('=').ok_or_else(|| format!("expected key=value, got {a}"))?;
        m.insert(k.to_string(), serde_json::from_str(v).unwrap_or_else(|_| json!(v)));
    }
    if m.is_empty() { Err("nothing to set".into()) } else { Ok(m) }
}

pub fn to_request(a: &[&str]) -> Result<Value, String> {
    let desc = Descriptor::load();
    Ok(match a {
        ["status"] | ["status", "--json"] => json!({"cmd": "status"}),
        ["set", kv @ ..] => json!({"cmd": "set", "changes": parse_kv(kv)?}),
        ["filter", "set", kv @ ..] => json!({"cmd": "filter.set", "changes": parse_kv(kv)?}),
        ["profile", "list"] => json!({"cmd": "profile.list"}),
        ["profile", "apply", id] => json!({"cmd": "profile.apply", "profile": id}),
        ["profile", "save", name @ ..] if !name.is_empty() => json!({"cmd": "profile.save", "name": name.join(" ")}),
        ["profile", "rename", id, name @ ..] if !name.is_empty() => json!({"cmd": "profile.rename", "profile": id, "name": name.join(" ")}),
        ["profile", "duplicate", id] => json!({"cmd": "profile.duplicate", "profile": id}),
        ["profile", "delete", id] => json!({"cmd": "profile.delete", "profile": id}),
        ["source", which @ ("clean" | "raw")] => json!({"cmd": "source.default", "which": which}),
        ["mute"] => json!({"cmd": "set", "changes": {"mic.mute": true}}),
        ["unmute"] => json!({"cmd": "set", "changes": {"mic.mute": false}}),
        ["nr", "off"] => json!({"cmd": "set", "changes": {"mic.nr.on": false}}),
        ["nr", level] => {
            let opts = &desc.field("mic.nr.level").unwrap().options;
            let o = opts.iter().find(|o| o.label == format!("nr.{level}")).ok_or_else(|| format!("nr takes off, {}", opts.iter().map(|o| o.label.trim_start_matches("nr.")).collect::<Vec<_>>().join(", ")))?;
            json!({"cmd": "set", "changes": {"mic.nr.on": true, "mic.nr.level": o.value}})
        }
        ["light", "on"] => json!({"cmd": "set", "changes": {"light.on": true}}),
        ["light", "off"] => json!({"cmd": "set", "changes": {"light.on": false}}),
        ["light", name] if *name != "next" => {
            let names: Vec<&str> = desc.light.presets.iter().map(|p| p.name.as_str()).collect();
            if !names.contains(name) {
                return Err(format!("unknown colour {name} ({})", names.join(", ")));
            }
            json!({"cmd": "set", "changes": {"light.on": true, "light.color": {"preset": name}}})
        }
        _ => return Err("unknown command - run `maono help`".into()),
    })
}

/// The one-line JSON the upstream bar widget and Waybar modules read.
pub fn legacy_status_json(state: &Value) -> Value {
    let desc = Descriptor::load();
    let m = &state["mic"];
    let muted = m["mic.mute"].as_bool().unwrap_or(false);
    let nr_on = m["mic.nr.on"].as_bool().unwrap_or(false);
    let nr_level = m["mic.nr.level"].as_i64().unwrap_or(0);
    let nr_name = desc.field("mic.nr.level").unwrap().options.iter().find(|o| o.value == nr_level).map(|o| o.label.trim_start_matches("nr.").to_string()).unwrap_or_default();
    let nr = if nr_on { format!("on, {nr_name}") } else { "off".into() };
    let colour = m["light.color"]["preset"].as_str().unwrap_or("custom");
    let mode = desc.light.presets.iter().find(|p| p.name == colour).map_or(8, |p| p.value);
    let gain_max = desc.field("mic.gain").unwrap().max.unwrap_or(20.0) as i64;
    let connected = state["device"] == "connected";
    json!({
        "text": if muted { "󰍭" } else { "󰍬" },
        "class": if !connected { "disconnected" } else if muted { "muted" } else { "live" },
        "tooltip": format!("Mic {} · battery {}% · gain {}/{gain_max} · NR {nr}", if muted { "muted" } else { "live" }, m["info.battery"], m["mic.gain"]),
        "muted": muted, "battery": m["info.battery"], "gain": m["mic.gain"], "gain_max": gain_max,
        "nr": nr, "nr_on": nr_on, "nr_level": nr_level,
        "light_on": m["light.on"].as_bool().unwrap_or(false), "light_mode": mode, "light_mode_name": colour,
    })
}

fn via_socket(stream: UnixStream, mut req: Value) -> Result<Value, String> {
    req["id"] = json!(std::process::id());
    let mut w = stream.try_clone().map_err(|e| e.to_string())?;
    writeln!(w, "{req}").map_err(|e| e.to_string())?;
    for line in BufReader::new(stream).lines() {
        let v: Value = serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if v["ev"] == "ack" && v["id"] == req["id"] {
            return Ok(v);
        }
    }
    Err("maono serve closed the connection".into())
}

/// Send one request: through `maono serve` when it runs, otherwise in-process
/// under the same lock (so the CLI never writes next to a running serve).
pub fn request(req: Value) -> Result<Value, String> {
    if let Ok(stream) = UnixStream::connect(store::socket_path()) {
        return via_socket(stream, req);
    }
    let lock = File::create(store::lock_path()).map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        return Err("maono serve is running but its socket does not answer".into());
    }
    let mut core = Core::new(store::config_dir(), store::pipewire_conf(), pw::System { schema: FilterSchema::load() });
    core.filterctl.adopt(&core.filter.clone());
    if let Some(f) = device::find().first() {
        match Device::open(f) {
            Ok(d) => core.connect(d, false),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(format!("{} needs the udev rule - see 99-maono.rules", f.hidraw.display()));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    core.take_events();
    core.handle_line(&req.to_string());
    core.take_events().into_iter().find(|e| e["ev"] == "ack").ok_or_else(|| "no answer".into())
}
```

- [ ] **Step 4: Rewrite `main.rs` dispatch**

Replace the body of `fn main()` and the old command handling in `src/main.rs`. Keep `mod theme; mod widgets; mod tui; mod shell;`, `fn fail` and `fn parse_id`; delete `struct State` and its impl. The new `main` is below. `get` and `scan` keep their previous bodies, because they are read-only and use `Mic` directly:

```rust
use maono::client::{self, legacy_status_json, to_request};
use maono::descriptor::Descriptor;
use maono::filter::FilterSchema;
use maono::mic::Mic;
use serde_json::{Value, json};
use std::process::ExitCode;

const USAGE: &str = "\
maono - control a Maono PD100W from the terminal, a status bar or the Omarchy panel

    maono                       live terminal UI (refuses while `maono serve` runs)
    maono serve                 own the mic; JSONL on stdin/stdout + $XDG_RUNTIME_DIR/maono.sock
    maono status [--json]       state (--json: the one-line shape status bars read)
    maono set key=value...      e.g. mic.gain=14 light.color='{\"preset\":\"blue\"}'
    maono filter set key=value  e.g. eq.preset=stream1 rnnoise.vad=80
    maono profile list | apply <id> | save <name> | rename <id> <name> | duplicate <id> | delete <id>
    maono source clean | raw    default PipeWire input
    maono mute | unmute | toggle | gain [n|+n|-n] | nr off|low|mid|high | light on|off|next|<colour>
    maono schema                every setting, range and option as JSON
    maono get <id> | scan [lo] [hi]   read raw fields (read-only)
    maono shell install | uninstall   the Omarchy bar widget
";

fn ack_or_fail(r: Result<Value, String>) -> Result<Value, ExitCode> {
    match r {
        Ok(a) if a["ok"] == true => Ok(a),
        Ok(a) => Err(fail(format!("{}{}", a["error"].as_str().unwrap_or("failed"), a.get("details").map(|d| format!(": {d}")).unwrap_or_default()))),
        Err(e) => Err(fail(e)),
    }
}

fn state() -> Result<Value, ExitCode> {
    ack_or_fail(client::request(json!({"cmd": "status"}))).map(|a| a["state"].clone())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = || -> Result<(), ExitCode> {
        match a.as_slice() {
            [] => tui::run().map_err(fail),
            ["help" | "-h" | "--help"] => {
                print!("{USAGE}");
                Ok(())
            }
            ["serve"] => maono::serve::main().map_err(fail),
            ["schema"] => {
                println!("{}", serde_json::to_string_pretty(&json!({"device": Descriptor::load().schema(), "filter": FilterSchema::load()})).unwrap());
                Ok(())
            }
            ["shell", rest @ ..] => {
                let force = rest.contains(&"--force");
                match rest.first().copied() {
                    Some("install") => shell::install(force).map_err(fail),
                    Some("uninstall") => shell::uninstall().map_err(fail),
                    _ => Err(fail("shell takes install or uninstall")),
                }
            }
            ["get", id] => {
                let id = parse_id(id).ok_or_else(|| fail("usage: maono get <id>"))?;
                let mut m = Mic::open().map_err(fail)?;
                match m.get(id).map_err(fail)? {
                    Some(v) => println!("  0x{id:04x} = {v}  (0x{v:04x})"),
                    None => println!("  0x{id:04x} = no answer"),
                }
                Ok(())
            }
            ["scan", rest @ ..] => scan(rest),
            ["status", "--json"] => {
                println!("{}", legacy_status_json(&state()?));
                Ok(())
            }
            ["status"] => {
                let s = state()?;
                println!("{}", serde_json::to_string_pretty(&json!({"device": s["device"], "activeProfile": s["activeProfile"], "dirty": s["dirty"], "mic": s["mic"], "filter": s["filter"]})).unwrap());
                Ok(())
            }
            ["toggle"] => {
                let muted = state()?["mic"]["mic.mute"].as_bool().unwrap_or(false);
                ack_or_fail(client::request(json!({"cmd": "set", "changes": {"mic.mute": !muted}}))).map(drop)
            }
            ["gain", n] if n.starts_with('+') || n.starts_with('-') => {
                let delta: i64 = n.parse().map_err(|_| fail("gain takes n, +n or -n"))?;
                let max = Descriptor::load().field("mic.gain").unwrap().max.unwrap_or(20.0) as i64;
                let cur = state()?["mic"]["mic.gain"].as_i64().unwrap_or(0);
                ack_or_fail(client::request(json!({"cmd": "set", "changes": {"mic.gain": (cur + delta).clamp(0, max)}}))).map(drop)
            }
            ["gain", n] => {
                let n: i64 = n.parse().map_err(|_| fail("gain takes n, +n or -n"))?;
                ack_or_fail(client::request(json!({"cmd": "set", "changes": {"mic.gain": n}}))).map(drop)
            }
            ["light", "next"] => {
                let d = Descriptor::load();
                let cur = state()?["mic"]["light.color"]["preset"].as_str().map(String::from);
                let i = cur.and_then(|c| d.light.presets.iter().position(|p| p.name == c)).map_or(0, |i| (i + 1) % d.light.presets.len());
                ack_or_fail(client::request(json!({"cmd": "set", "changes": {"light.on": true, "light.color": {"preset": d.light.presets[i].name}}}))).map(drop)
            }
            _ => {
                let req = to_request(&a).map_err(fail)?;
                let ack = ack_or_fail(client::request(req))?;
                let mut shown = ack.clone();
                for k in ["ev", "id", "ok"] {
                    shown.as_object_mut().unwrap().remove(k);
                }
                if shown.as_object().is_some_and(|m| !m.is_empty()) {
                    println!("{}", serde_json::to_string_pretty(&shown).unwrap());
                }
                Ok(())
            }
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}
```

Add the read-only range dump as its own function (this is the old `"scan"` arm, unchanged in behaviour). Delete every other old arm (`status`, `mute`, `gain`, `nr`, `light`, `set`), since `client` replaces them:

```rust
fn scan(rest: &[&str]) -> Result<(), ExitCode> {
    let lo = rest.first().and_then(|a| parse_id(a)).unwrap_or(0x2000);
    let hi = rest.get(1).and_then(|a| parse_id(a)).unwrap_or(0x20FF);
    let mut m = Mic::open().map_err(fail)?;
    println!("scanning 0x{lo:04x}..0x{hi:04x} (read-only)");
    for id in lo..=hi {
        if let Some(v) = m.get_quick(id).map_err(fail)? {
            println!("  0x{id:04x} = {v}");
        }
    }
    Ok(())
}
```

- [ ] **Step 5: TUI refuses while serve runs**

At the top of `pub fn run()` in `src/tui.rs`, add:

```rust
    let lock = std::fs::File::create(maono::store::lock_path())?;
    if lock.try_lock().is_err() {
        return Err(std::io::Error::other("`maono serve` owns the mic (the Omarchy panel); use the panel or stop serve first"));
    }
```

Keep `lock` alive for the rest of `run` by not dropping it: it is a local that lives until `run` returns.

- [ ] **Step 6: Build and test**

Run: `mise exec rust@stable -- cargo test --lib && mise exec rust@stable -- cargo build --release`
Expected: all tests pass and the release build succeeds.

Then check the CLI in direct mode. This only reads the mic, so no authorization is needed:

```bash
./target/release/maono status --json
./target/release/maono profile list | head -20
./target/release/maono schema | head -5
```

Expected:
- the first command prints the legacy one-liner with `"class":"live"` or `"muted"`;
- `profile list` shows 4 profiles;
- `schema` prints JSON.

`status` in direct mode will (re)write the filter conf only if `adopt` fails. It cannot fail silently: if the PipeWire node does not match, the next filter command rebuilds it. That is expected on first run.

- [ ] **Step 7: Commit**

```bash
git add src/client.rs src/lib.rs src/main.rs src/tui.rs
git commit -m "feat: CLI over serve socket with in-process fallback; legacy status kept

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Install and verify on the real mic (REQUIRES Agus's explicit go-ahead)

**Do not start this task until Agus has said yes in the conversation.** It writes to the mic and restarts `filter-chain.service`, so audio from the clean source drops for about 1 s each time.

**Files:**
- Modify: `devices/filter.json` (only if a polarity check below fails)
- Modify: `docs/protocol/pd100w.md` (append the results to §8)

- [ ] **Step 1: Snapshot and install**

```bash
cd ~/dev/maono
S=/tmp/claude-1000/-home-agus/ea1adf2a-09b5-41df-ac13-a30ca564669c/scratchpad
cp ~/.config/pipewire/filter-chain.conf.d/maono-clean.conf "$S/maono-clean.conf.before"
python3 "$S/cal.py" snapshot "$S/snap-before-task13.json"
mise exec rust@stable -- cargo install --path . --root ~/.local --force
maono status --json
```

Expected: the snapshot reports `76 ids, missing: []` and the status line prints. If `cal.py` is gone because the scratchpad was cleaned, run `maono status > "$S/status-before.json"` instead and restore from that by hand at the end.

- [ ] **Step 2: First apply builds the generated chain**

```bash
maono profile apply llamada
pw-dump | python3 -c "import json,sys;[print({k:v for k,v in zip(p['params'][::2],p['params'][1::2]) if ':' in str(k)}) for o in json.load(sys.stdin) if o.get('info',{}).get('props',{}).get('node.name')=='maono_clean_in' for p in o['info']['params'].get('Props',[]) if 'params' in p]"
pactl get-default-source
```

Expected:
- the ack has `"applied": ["mic", "filter"]`;
- the dump shows the nodes `hp1, hp2, b0..b4, lpf, rnnoise, comp` with `rnnoise:VAD Threshold (%)` 85 and `comp:Bypass` 1;
- the default source is still `maono_clean`.

- [ ] **Step 3: Verify the two switch polarities by measurement**

With Agus typing next to the mic (guided by `notify-send`), record raw and clean for 5 s in each of the four conditions below. Use `rec.sh` and `stats.py` from the scratchpad, or `pw-record` plus the RMS snippet from the calibration.

| Condition | Command | Expected clean-vs-raw typing level |
|---|---|---|
| A | `maono filter set rnnoise.on=true rnnoise.vad=85` | clean ≥ 30 dB below raw |
| B | `maono filter set rnnoise.on=false` | clean within ~3 dB of raw (Dry Mix 1 = fully dry) |
| C | `maono filter set rnnoise.on=true comp.on=true comp.threshold=-40 comp.ratio=10` | with speech: crest factor of clean drops vs A |
| D | `maono filter set comp.on=false` | crest factor back to A |

If B is still suppressed, flip `rnnoise.switch` in `devices/filter.json` to `{"port": "Dry Mix", "on": 1, "off": 0}`. If C shows no compression, flip `comp.switch`. Rebuild and repeat that row.

- [ ] **Step 4: Button, unplug and reconnect**

1. Run `maono serve` in a terminal.
2. Ask Agus to press the mute button: stdout must show `{"ev":"changed","key":"mic.mute",…,"source":"button"}` within 1 s.
3. Ask Agus to unplug the cable and plug it back in: stdout shows `"device":"disconnected"`, then `"device":"connected"` and `{"ev":"reapplied","profile":"llamada",…}`, and the mute state stays what it was.
4. Stop serve with Ctrl+D (stdin EOF): the process exits and `$XDG_RUNTIME_DIR/maono.sock` is gone.

- [ ] **Step 5: Restore what Agus had, record results, commit**

```bash
python3 "$S/cal.py" restore "$S/snap-before-task13.json"
maono filter set rnnoise.on=true rnnoise.vad=85 comp.on=false
```

Append a "Task 13 results" table to §8 of `docs/protocol/pd100w.md` covering polarities, button latency, reconnect behaviour and the final values. Then:

```bash
git add devices/filter.json docs/protocol/pd100w.md
git commit -m "test: hardware verification of the serve backend; switch polarities confirmed

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push fork omarchy-panel
```
