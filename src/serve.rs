//! The long-running owner of the mic. Requests arrive from stdin (the Omarchy
//! panel) and from a Unix socket (CLI, keybinds); one queue, one at a time.

use crate::device::{self, Device};
use crate::engine::{Core, Link};
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
