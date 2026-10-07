//! All command logic. `serve` and the CLI feed it JSON lines and forward the
//! events it produces; nothing here does I/O except through Device and Audio.

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
    /// True in `maono serve`: silence the HID meter and keep the filter chain up on
    /// connect. The one-shot CLI sets it false so a read never writes anything.
    pub owner: bool,
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
            owner: true,
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
        let mut pending = Vec::new();
        let id = self.desc.identity.clone();
        let vendor = dev.get(id.vendor_id, &mut |f| pending.push(f.clone()));
        let product = dev.get(id.product_id, &mut |f| pending.push(f.clone()));
        if !matches!(vendor, Ok(v) if v == id.vendor) || !matches!(product, Ok(p) if id.products.contains(&p)) {
            self.emit(json!({ "ev": "error", "code": "identity", "message": format!("not a PD100W (vendor {vendor:?}, product {product:?})") }));
            self.link = Link::Disconnected;
            return;
        }
        let previous = std::mem::take(&mut self.mic);
        if self.owner {
            let _ = dev.set(&[(self.desc.meter.id, self.desc.meter.off as i64)]);
        }
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
        if self.owner && !self.filterctl.is_applied() {
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
                for (k, v) in eff.iter().filter(|(_, v)| !v.is_null()) {
                    self.mic.insert(k.clone(), v.clone());
                }
                // A key the mic did not read back is not confirmed: the UI must not keep it.
                let missing: Vec<&String> = eff.iter().filter(|(_, v)| v.is_null()).map(|(k, _)| k).collect();
                if missing.is_empty() {
                    Ok(json!({ "effective": eff }))
                } else {
                    Err(json!({ "error": "timeout", "keys": missing, "effective": eff }))
                }
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
    fn a_device_with_the_wrong_identity_is_not_used() {
        let (mut c, _) = core();
        let mut regs = pd100w_regs();
        regs.push((0x0017, 0x0999));
        let (dev, fake) = FakeMic::new(&regs);
        c.connect(dev, false);
        assert_eq!(c.link, Link::Disconnected);
        assert!(c.take_events().iter().any(|e| e["ev"] == "error" && e["code"] == "identity"));
        assert_eq!(fake.reg(0x0045), Some(0), "nothing was written");
    }

    #[test]
    fn readback_timeout_is_not_ok() {
        let (mut c, fake) = connected();
        fake.silent.store(true, std::sync::atomic::Ordering::SeqCst);
        let a = ask(&mut c, json!({"id": 1, "cmd": "set", "changes": {"mic.gain": 9}}));
        assert_eq!((a["ok"].clone(), a["error"].clone()), (json!(false), json!("timeout")));
        assert_eq!(a["keys"], json!(["mic.gain"]));
        assert_eq!(fake.reg(0x207E), Some(9), "the write itself went out");
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
        assert_eq!(a["profile"], "mi-voz");
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
