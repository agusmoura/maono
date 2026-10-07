//! The PipeWire cleanup chain: its state, schema (data), EQ presets, the
//! generated filter-chain fragment and the live control values.

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
        assert_eq!(get(&p, "rnnoise:Dry Mix"), s.plugins.rnnoise.switch.off);
        assert_eq!(get(&p, "comp:Bypass"), s.plugins.comp.switch.off);
        d.enabled = true;
        d.comp.on = true;
        d.comp.threshold = -20.0;
        let p = params(&d, &deps, &s);
        assert_eq!(get(&p, "comp:Bypass"), s.plugins.comp.switch.on);
        assert!((get(&p, "comp:Attack threshold (G)") - 0.1).abs() < 1e-9);
        assert_eq!(get(&p, "rnnoise:Dry Mix"), s.plugins.rnnoise.switch.on);
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
