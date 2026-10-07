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
        (0x000B, 108), (0x0042, 70), (0x0049, 0), (0x0045, 0), (0x0016, 0x352F), (0x0017, 0x0417),
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
