//! What the mic exposes, as data: keys, ids, ranges, encodings, options.
//! The UI renders from `schema()`; writes are validated here and then again by
//! `safety` (which this file can never widen).

use crate::safety;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use std::collections::HashMap;

fn hex_u16<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    let s = String::deserialize(d)?;
    u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
}

fn ser_hex<S: Serializer>(id: &u16, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("0x{id:04X}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Num,
    Enum,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Encoding {
    #[default]
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "db10_2000")]
    Db10_2000,
    #[serde(rename = "x100")]
    X100,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opt {
    pub value: i64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub id: u16,
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub encoding: Encoding,
    #[serde(default)]
    pub options: Vec<Opt>,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub profile: bool,
    #[serde(default)]
    pub mask: Option<String>,
}

impl Field {
    /// UI value -> raw device value. Checks the field's own domain, then `safety`.
    pub fn to_raw(&self, v: &Value) -> Result<u16, String> {
        let n = match (self.kind, v) {
            (Kind::Bool, Value::Bool(b)) => *b as u8 as f64,
            (Kind::Bool, Value::Number(n)) if matches!(n.as_i64(), Some(0 | 1)) => n.as_f64().unwrap(),
            (Kind::Int | Kind::Num | Kind::Enum, Value::Number(n)) => n.as_f64().unwrap(),
            _ => return Err(format!("{}: {v} is not a valid {:?}", self.key, self.kind)),
        };
        if let (Some(lo), Some(hi)) = (self.min, self.max) {
            if !(lo..=hi).contains(&n) {
                return Err(format!("{}: {n} is outside {lo}..{hi}", self.key));
            }
        }
        if matches!(self.kind, Kind::Int | Kind::Enum) && n.fract() != 0.0 {
            return Err(format!("{}: {n} is not a whole number", self.key));
        }
        if let (Kind::Int, Some(step), Some(lo)) = (self.kind, self.step, self.min) {
            if step >= 1.0 && ((n - lo) % step) != 0.0 {
                return Err(format!("{}: {n} is not a multiple of {step}", self.key));
            }
        }
        if self.kind == Kind::Enum && !self.options.iter().any(|o| o.value as f64 == n) {
            return Err(format!("{}: {n} is not one of the options", self.key));
        }
        let raw = match self.encoding {
            Encoding::Raw => n.round(),
            Encoding::Db10_2000 => (n * 10.0).round() + 2000.0,
            Encoding::X100 => (n * 100.0).round(),
        };
        safety::check(self.id, raw as i64).map_err(|e| format!("{}: {e}", self.key))
    }

    pub fn from_raw(&self, raw: u16) -> Value {
        let raw = if self.mask.as_deref() == Some("low") { raw & 0xFF } else { raw };
        match self.kind {
            Kind::Bool => Value::Bool(raw != 0),
            Kind::Version => json!(raw.to_string().chars().map(String::from).collect::<Vec<_>>().join(".")),
            _ => {
                let v = match self.encoding {
                    Encoding::Raw => raw as f64,
                    Encoding::Db10_2000 => (raw as f64 - 2000.0) / 10.0,
                    Encoding::X100 => raw as f64 / 100.0,
                };
                if v.fract() == 0.0 { json!(v as i64) } else { json!((v * 100.0).round() / 100.0) }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorPreset {
    pub value: u16,
    pub name: String,
    pub hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightMeta {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub color: u16,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub count: u16,
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub base: u16,
    pub custom_max: u16,
    pub presets: Vec<ColorPreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub start: u16,
    pub count: u16,
}

fn hex_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u16>, D::Error> {
    Vec::<String>::deserialize(d)?
        .iter()
        .map(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom))
        .collect()
}

/// Registers that must read back as this device before we touch it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(deserialize_with = "hex_u16")]
    pub vendor_id: u16,
    #[serde(deserialize_with = "hex_u16")]
    pub product_id: u16,
    #[serde(deserialize_with = "hex_u16")]
    pub vendor: u16,
    #[serde(deserialize_with = "hex_list")]
    pub products: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meter {
    #[serde(deserialize_with = "hex_u16", serialize_with = "ser_hex")]
    pub id: u16,
    pub off: u16,
}

#[derive(Debug, Clone, Deserialize)]
struct SlotField {
    name: String,
    #[serde(rename = "type")]
    kind: Kind,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    encoding: Encoding,
    #[serde(default)]
    options: Vec<Opt>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EqSlots {
    key_prefix: String,
    #[serde(deserialize_with = "hex_u16")]
    base: u16,
    count: u16,
    status: String,
    fields: Vec<SlotField>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    fields: Vec<Field>,
    eq_slots: EqSlots,
    readonly: Vec<Field>,
    serial: Span,
    meter: Meter,
    identity: Identity,
    light: LightMeta,
}

#[derive(Debug, Clone)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    pub readonly: Vec<Field>,
    pub light: LightMeta,
    pub serial: Span,
    pub meter: Meter,
    pub identity: Identity,
    by_key: HashMap<String, usize>,
    by_id: HashMap<u16, usize>,
}

impl Descriptor {
    pub fn load() -> Self {
        Self::parse(include_str!("../devices/pd100w.json")).expect("bundled descriptor is valid")
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawDescriptor = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut fields = raw.fields;
        let s = &raw.eq_slots;
        for slot in 0..s.count {
            for (i, sf) in s.fields.iter().enumerate() {
                fields.push(Field {
                    key: format!("{}.{slot}.{}", s.key_prefix, sf.name),
                    id: s.base + 5 * slot + i as u16,
                    kind: sf.kind,
                    min: sf.min,
                    max: sf.max,
                    step: sf.step,
                    unit: sf.unit.clone(),
                    encoding: sf.encoding,
                    options: sf.options.clone(),
                    group: "dsp".into(),
                    status: s.status.clone(),
                    profile: false,
                    mask: None,
                });
            }
        }
        // `fields` first, then `readonly`: indexes past fields.len() point into readonly.
        let mut by_key = HashMap::new();
        let mut by_id = HashMap::new();
        for (i, f) in fields.iter().chain(&raw.readonly).enumerate() {
            by_key.insert(f.key.clone(), i);
            by_id.insert(f.id, i);
        }
        Ok(Descriptor { fields, readonly: raw.readonly, light: raw.light, serial: raw.serial, meter: raw.meter, identity: raw.identity, by_key, by_id })
    }

    fn at(&self, i: usize) -> &Field {
        if i < self.fields.len() { &self.fields[i] } else { &self.readonly[i - self.fields.len()] }
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.by_key.get(key).map(|&i| self.at(i))
    }

    pub fn field_by_id(&self, id: u16) -> Option<&Field> {
        self.by_id.get(&id).map(|&i| self.at(i))
    }

    /// What the panel needs to render controls without hardcoding them.
    pub fn schema(&self) -> Value {
        json!({ "fields": self.fields, "readonly": self.readonly, "light": self.light })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_writable_value_the_descriptor_allows_passes_safety() {
        let d = Descriptor::load();
        for f in &d.fields {
            let candidates: Vec<serde_json::Value> = match f.kind {
                Kind::Bool => vec![json!(false), json!(true)],
                Kind::Enum => f.options.iter().map(|o| json!(o.value)).collect(),
                _ => vec![json!(f.min.unwrap()), json!(f.max.unwrap())],
            };
            for v in candidates {
                assert!(f.to_raw(&v).is_ok(), "{} = {v}: {:?}", f.key, f.to_raw(&v));
            }
        }
    }

    #[test]
    fn eq_slots_expand_to_35_fields() {
        let d = Descriptor::load();
        assert_eq!(d.fields.iter().filter(|f| f.key.starts_with("dsp.eq.")).count(), 35);
        assert_eq!(d.field("dsp.eq.6.freq").unwrap().id, 0x2023 + 30 + 2);
    }

    #[test]
    fn to_raw_rejects_bad_values() {
        let d = Descriptor::load();
        assert!(d.field("mic.gain").unwrap().to_raw(&json!(21)).is_err());
        assert!(d.field("mic.gain").unwrap().to_raw(&json!(1.5)).is_err());
        assert!(d.field("mic.nr.level").unwrap().to_raw(&json!(3)).is_err());
        assert!(d.field("light.brightness").unwrap().to_raw(&json!(0)).is_err());
        assert!(d.field("mic.mute").unwrap().to_raw(&json!("yes")).is_err());
        assert!(d.field("headphones.monitor").unwrap().to_raw(&json!(3)).is_err());
    }

    #[test]
    fn encodings_roundtrip() {
        let d = Descriptor::load();
        let thr = d.field("dsp.comp.threshold").unwrap();
        assert_eq!(thr.to_raw(&json!(-20)), Ok(1800));
        assert_eq!(thr.from_raw(1800), json!(-20));
        let gain = d.field("dsp.eq.0.gain").unwrap();
        assert_eq!(gain.to_raw(&json!(3.5)), Ok(2035));
        assert_eq!(gain.from_raw(2035), json!(3.5));
        assert_eq!(d.field("dsp.eq.0.q").unwrap().from_raw(71), json!(0.71));
        assert_eq!(d.field("info.firmware").unwrap().from_raw(108), json!("1.0.8"));
        assert_eq!(d.field("info.battery").unwrap().from_raw(0x3C46), json!(70));
        assert_eq!(d.field("mic.mute").unwrap().from_raw(1), json!(true));
    }

    #[test]
    fn light_and_profile_metadata() {
        let d = Descriptor::load();
        assert_eq!(d.light.presets.len(), 8);
        assert_eq!(d.light.custom_max, 5);
        assert!(!d.field("mic.mute").unwrap().profile);
        assert!(d.field("mic.gain").unwrap().profile);
        assert_eq!(d.field_by_id(0x207E).unwrap().key, "mic.gain");
    }
}
