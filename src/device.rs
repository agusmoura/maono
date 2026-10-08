//! Finding the mic and talking to it. Every write goes through `safety`.

use crate::proto::{self, Frame, GET, MAX_PAIRS, REPORT_LEN};
use crate::safety::{self, Refused};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
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

/// Feed every report from a nonblocking `reader` into `tx`; `Gone` at end of
/// file or on an error. With nothing to read it exits once `alive` is cleared
/// (the Device was dropped), so a silent mic never strands the thread and fd.
// ponytail: a 10 ms sleep-poll instead of poll(2) plus a wakeup fd; enough for
// one mic, revisit if the idle wakeups ever matter.
fn spawn_reader<R: Read + Send + 'static>(mut reader: R, alive: Arc<AtomicBool>, tx: Sender<Incoming>) -> JoinHandle<()> {
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
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !alive.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = tx.send(Incoming::Gone);
                    return;
                }
            }
        }
    })
}

pub struct Device {
    link: Box<dyn Link>,
    rx: Receiver<Incoming>,
    pub model: Model,
    /// Cleared on drop to stop the hidraw reader thread (unused by fakes).
    alive: Arc<AtomicBool>,
}

impl Drop for Device {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Device {
    pub fn new(link: Box<dyn Link>, rx: Receiver<Incoming>, model: Model) -> Self {
        Device { link, rx, model, alive: Arc::new(AtomicBool::new(true)) }
    }

    /// Open the hidraw node nonblocking. A reader thread feeds every report
    /// into the channel, sends `Gone` when the node disappears, and ends when
    /// this Device is dropped. Writes share the fd; a write that would block
    /// surfaces as `DevError::Io` (EAGAIN), never as a silent retry.
    pub fn open(found: &Found) -> io::Result<Device> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(crate::mic::libc_o_nonblock()).open(&found.hidraw)?;
        let reader = file.try_clone()?;
        let (tx, rx) = mpsc::channel();
        let dev = Device::new(Box::new(file), rx, found.model);
        spawn_reader(reader, dev.alive.clone(), tx);
        Ok(dev)
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

    /// Waits up to `ms` for the reader thread to end.
    fn finishes_within(h: &std::thread::JoinHandle<()>, ms: u64) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while !h.is_finished() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        h.is_finished()
    }

    #[test]
    fn reader_reports_end_of_file_as_gone() {
        let path = tempdir("reader").join("empty");
        fs::write(&path, b"").unwrap();
        let (tx, rx) = mpsc::channel();
        let h = spawn_reader(File::open(&path).unwrap(), Arc::new(AtomicBool::new(true)), tx);
        assert!(matches!(rx.recv_timeout(Duration::from_millis(200)), Ok(Incoming::Gone)));
        assert!(finishes_within(&h, 200));
    }

    /// A nonblocking hidraw node with nothing to read, as when the mic is off.
    struct NoData;
    impl Read for NoData {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    #[test]
    fn dropping_the_device_stops_its_reader() {
        let (dev, _fake) = FakeMic::new(&pd100w_regs());
        let (tx, _rx) = mpsc::channel();
        let h = spawn_reader(NoData, dev.alive.clone(), tx);
        assert!(!finishes_within(&h, 50), "keeps waiting while the Device lives");
        drop(dev);
        assert!(finishes_within(&h, 200), "the reader thread ended after the drop");
    }

    #[test]
    fn unplug_is_reported_as_gone() {
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        fake.unplug();
        assert!(matches!(dev.poll(Duration::ZERO, &mut |_| {}), Err(DevError::Gone)));
    }
}
