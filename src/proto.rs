//! Frame codec for the vendor HID channel: report id 0xC4, 64-byte reports.
//!
//! ```text
//! c4 <len> 00 00 <kind> [id_lo id_hi val_lo val_hi]... <ck_lo ck_hi>   padded to 64
//! ```
//! `len` counts the whole frame including `c4` and the checksum; `ck` is the
//! two's complement of the sum of every byte before it.

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

#[cfg(test)]
mod tests {
    use crate::proto::*;

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
