//! Everything that touches PipeWire / systemd, behind the `Audio` trait.

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
        /// restart() returns Err once `restarts` exceeds this value.
        pub fail_restart_after: Option<u32>,
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
                fail_restart_after: None,
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
            if self.fail_restart_after.is_some_and(|n| self.restarts > n) {
                return Err(io::Error::other("systemctl restart failed"));
            }
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
