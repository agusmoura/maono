//! Control a Maono PD100W wireless microphone from the command line or a TUI.

mod theme;
mod widgets;
mod tui;
mod shell;

use maono::client::{self, legacy_status_json, to_request};
use maono::descriptor::Descriptor;
use maono::filter::FilterSchema;
use maono::mic::Mic;
use serde_json::{Value, json};
use std::io::Write;
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

/// Accepts `0x208e` or plain decimal.
fn parse_id(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "maono: {msg}");
    ExitCode::FAILURE
}

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
                let s = state()?;
                println!("{}", legacy_status_json(&s));
                // Compat for the pre-plan-2 bar widget: it reads this JSON line but
                // detects "receiver not found" from the exit code, not from the
                // JSON's "device" field (plan 2 rewrites the widget and drops this).
                if s["device"] == "connected" { Ok(()) } else { Err(ExitCode::from(1)) }
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
