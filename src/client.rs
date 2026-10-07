//! The CLI side of the protocol: argv -> request, and request -> ack via the
//! serve socket, or in-process under the same lock when serve is not running.

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
        // Numeric mode, as the pre-plan-2 bar widget still sends (shell/Panel.qml:157).
        ["light", name] if *name != "next" && name.parse::<i64>().is_ok() => {
            let n = name.parse::<i64>().unwrap();
            let preset = desc.light.presets.iter().find(|p| p.value as i64 == n)
                .ok_or_else(|| format!("light takes 0-{} or a colour name", desc.light.presets.len().saturating_sub(1)))?;
            json!({"cmd": "set", "changes": {"light.on": true, "light.color": {"preset": preset.name}}})
        }
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
        "text": if muted { "\u{f036d}" } else { "\u{f036c}" },
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
    core.owner = false; // one-shot: reads write nothing; only filter/profile/source commands touch PipeWire
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
        // Numeric light mode: what the pre-plan-2 bar widget still sends.
        assert_eq!(to_request(&["light", "4"]).unwrap(), json!({"cmd": "set", "changes": {"light.on": true, "light.color": {"preset": "green"}}}));
        assert!(to_request(&["light", "8"]).is_err());
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
