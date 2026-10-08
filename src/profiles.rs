//! User profiles: partial snapshots of mic + light + filter settings, one JSON
//! file each in ~/.config/maono/profiles/. Profiles never contain `mic.mute`.

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
    Some([hue.round() % 360.0, round2(if max == 0.0 { 0.0 } else { d / max }), round2(max)])
}

/// Omarchy's current theme accent as HSV, if the theme file has one.
pub fn theme_accent_hsv() -> Option<[f64; 3]> {
    let home = std::env::var_os("HOME")?;
    let text = fs::read_to_string(PathBuf::from(home).join(".local/state/omarchy/current/theme/colors.toml")).ok()?;
    let line = text.lines().find(|l| l.trim_start().starts_with("accent"))?;
    hex_to_hsv(line.split('=').nth(1)?.trim().trim_matches('"'))
}

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
        assert_eq!(hex_to_hsv("#ff0001").unwrap()[0], 0.0, "359.8 rounds to 360, which is 0");
    }
}
