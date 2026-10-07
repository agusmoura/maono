//! The mic's settings as a JSON map keyed by descriptor keys, read and written
//! through the descriptor (and therefore through `safety`).

use crate::descriptor::Descriptor;
use crate::device::{DevError, Device};
use crate::proto::Frame;
use serde_json::{Map, Value, json};

pub type Values = Map<String, Value>;

#[derive(Debug)]
pub enum ApplyError {
    Invalid(Vec<String>),
    Device(DevError),
}

/// `Ok(None)` when the mic did not answer; `Gone`/IO errors propagate.
fn get_opt(dev: &mut Device, id: u16, other: &mut dyn FnMut(&Frame)) -> Result<Option<u16>, DevError> {
    match dev.get(id, other) {
        Ok(v) => Ok(Some(v)),
        Err(DevError::Timeout(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn read_all(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<Values, DevError> {
    let mut out = Values::new();
    for f in desc.fields.iter().chain(&desc.readonly) {
        let v = get_opt(dev, f.id, other)?.map_or(Value::Null, |raw| f.from_raw(raw));
        out.insert(f.key.clone(), v);
    }
    out.insert("info.serial".into(), read_serial(dev, desc, other)?);
    let (color, custom) = read_color(dev, desc, other)?;
    out.insert("light.color".into(), color);
    out.insert("light.custom".into(), custom);
    Ok(out)
}

fn read_serial(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<Value, DevError> {
    let mut s = String::new();
    for i in 0..desc.serial.count {
        let Some(w) = get_opt(dev, desc.serial.start + i, other)? else { break };
        for b in [w as u8, (w >> 8) as u8] {
            if b == 0 || b == 0xFE {
                return Ok(json!(s));
            }
            s.push(b as char);
        }
    }
    Ok(json!(s))
}

fn read_color(dev: &mut Device, desc: &Descriptor, other: &mut dyn FnMut(&Frame)) -> Result<(Value, Value), DevError> {
    let l = &desc.light;
    let index = get_opt(dev, l.color, other)?;
    let count = get_opt(dev, l.count, other)?.unwrap_or(0).min(l.custom_max);
    let mut custom = Vec::new();
    for k in 0..count {
        let b = l.base + 3 * k;
        let hsv = (get_opt(dev, b, other)?, get_opt(dev, b + 1, other)?, get_opt(dev, b + 2, other)?);
        custom.push(match hsv {
            (Some(h), Some(s), Some(v)) => json!([h as f64 / 100.0, s as f64 / 100.0, v as f64 / 100.0]),
            _ => Value::Null,
        });
    }
    let color = match index {
        None => Value::Null,
        Some(i) if i < 8 => l.presets.iter().find(|p| p.value == i).map_or(Value::Null, |p| json!({"preset": p.name})),
        Some(i) => custom
            .get((i - 8) as usize)
            .filter(|c| !c.is_null())
            .map_or(Value::Null, |c| json!({"hsv": c})),
    };
    Ok((color, Value::Array(custom)))
}

/// `[h°, s, v]` with h in 0..360 and s, v in 0..1 -> raw `[h*100, s*100, v*100]`.
pub fn hsv_raw(v: Option<&Value>) -> Option<[i64; 3]> {
    let a = v?.as_array()?;
    let [h, s, x] = [a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?];
    if a.len() != 3 || !(0.0..360.0).contains(&h) || !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&x) {
        return None;
    }
    Some([((h * 100.0).round() as i64).min(35_900), (s * 100.0).round() as i64, (x * 100.0).round() as i64])
}

pub fn check_color(desc: &Descriptor, v: &Value) -> Result<(), String> {
    if let Some(name) = v.get("preset").and_then(Value::as_str) {
        return match desc.light.presets.iter().any(|p| p.name == name) {
            true => Ok(()),
            false => Err(format!("light.color: unknown preset {name}")),
        };
    }
    hsv_raw(v.get("hsv")).map(drop).ok_or_else(|| "light.color: expected {\"preset\": name} or {\"hsv\": [h, s, v]}".into())
}

pub fn same_color(a: &Value, b: &Value) -> bool {
    match (a.get("preset"), b.get("preset")) {
        (Some(x), Some(y)) => x == y,
        (None, None) => hsv_raw(a.get("hsv")).is_some() && hsv_raw(a.get("hsv")) == hsv_raw(b.get("hsv")),
        _ => false,
    }
}

fn custom_list(current: &Values) -> Vec<Value> {
    current.get("light.custom").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn plan_color(desc: &Descriptor, v: &Value, current: &Values) -> Result<Vec<(u16, i64)>, String> {
    check_color(desc, v)?;
    let l = &desc.light;
    if let Some(name) = v.get("preset").and_then(Value::as_str) {
        let p = l.presets.iter().find(|p| p.name == name).unwrap();
        return Ok(vec![(l.color, p.value as i64)]);
    }
    let raw = hsv_raw(v.get("hsv")).unwrap();
    let custom = custom_list(current);
    if let Some(k) = custom.iter().position(|c| hsv_raw(Some(c)) == Some(raw)) {
        return Ok(vec![(l.color, 8 + k as i64)]);
    }
    let k = custom.len() as u16;
    if k >= l.custom_max {
        return Err("light.color: all custom colour slots are taken".into());
    }
    let b = l.base + 3 * k;
    Ok(vec![(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2]), (l.count, k as i64 + 1), (l.color, 8 + k as i64)])
}

/// Validate everything first (nothing is written if any value is bad), write,
/// then read back every touched key. Returns the effective values.
pub fn apply(dev: &mut Device, desc: &Descriptor, changes: &Values, current: &Values, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    let mut pairs = Vec::new();
    let mut errors = Vec::new();
    let mut colour = None;
    for (k, v) in changes {
        if k == "light.color" {
            match plan_color(desc, v, current) {
                Ok(p) => colour = Some(p),
                Err(e) => errors.push(e),
            }
            continue;
        }
        match desc.field(k) {
            Some(f) => match f.to_raw(v) {
                Ok(raw) => pairs.push((f.id, raw as i64)),
                Err(e) => errors.push(e),
            },
            None => errors.push(format!("{k}: unknown setting")),
        }
    }
    if !errors.is_empty() {
        return Err(ApplyError::Invalid(errors));
    }
    if let Some(c) = &colour {
        pairs.extend(c.iter().copied());
    }
    dev.set(&pairs).map_err(ApplyError::Device)?;
    let mut effective = Values::new();
    for k in changes.keys().filter(|k| *k != "light.color") {
        let f = desc.field(k).unwrap();
        let v = get_opt(dev, f.id, other).map_err(ApplyError::Device)?.map_or(Value::Null, |raw| f.from_raw(raw));
        effective.insert(k.clone(), v);
    }
    if colour.is_some() {
        read_light_into(dev, desc, &mut effective, other)?;
    }
    Ok(effective)
}

fn read_light_into(dev: &mut Device, desc: &Descriptor, out: &mut Values, other: &mut dyn FnMut(&Frame)) -> Result<(), ApplyError> {
    let (c, custom) = read_color(dev, desc, other).map_err(ApplyError::Device)?;
    out.insert("light.color".into(), c);
    out.insert("light.custom".into(), custom);
    Ok(())
}

pub fn custom_update(dev: &mut Device, desc: &Descriptor, current: &Values, index: usize, hsv: &Value, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    if index >= custom_list(current).len() {
        return Err(ApplyError::Invalid(vec![format!("light.custom: no colour at {index}")]));
    }
    let raw = hsv_raw(Some(hsv)).ok_or_else(|| ApplyError::Invalid(vec!["light.custom: hsv must be [h, s, v]".into()]))?;
    let b = desc.light.base + 3 * index as u16;
    dev.set(&[(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2])]).map_err(ApplyError::Device)?;
    let mut out = Values::new();
    read_light_into(dev, desc, &mut out, other)?;
    Ok(out)
}

/// Remove custom colour `index`; later slots move down one, and the selected
/// colour index follows its colour (a deleted selection falls back to white).
pub fn custom_delete(dev: &mut Device, desc: &Descriptor, current: &Values, index: usize, other: &mut dyn FnMut(&Frame)) -> Result<Values, ApplyError> {
    let l = &desc.light;
    let custom = custom_list(current);
    if index >= custom.len() {
        return Err(ApplyError::Invalid(vec![format!("light.custom: no colour at {index}")]));
    }
    let mut shifted = Vec::new();
    for (k, c) in custom.iter().enumerate().skip(index + 1) {
        match hsv_raw(Some(c)) {
            Some(raw) => shifted.push((k, raw)),
            None => return Err(ApplyError::Invalid(vec![format!("light.custom: colour {k} could not be read; refresh and retry")])),
        }
    }
    let mut pairs = Vec::new();
    for (k, raw) in shifted {
        let b = l.base + 3 * (k as u16 - 1);
        pairs.extend([(b, raw[0]), (b + 1, raw[1]), (b + 2, raw[2])]);
    }
    pairs.push((l.count, custom.len() as i64 - 1));
    let selected = get_opt(dev, l.color, other).map_err(ApplyError::Device)?.unwrap_or(0) as usize;
    let deleted = 8 + index;
    if selected == deleted {
        pairs.push((l.color, 0));
    } else if selected > deleted {
        pairs.push((l.color, selected as i64 - 1));
    }
    dev.set(&pairs).map_err(ApplyError::Device)?;
    let mut out = Values::new();
    read_light_into(dev, desc, &mut out, other)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeMic, pd100w_regs};
    use serde_json::json;

    fn values(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn read_all_decodes_the_real_registers() {
        let d = Descriptor::load();
        let (mut dev, _fake) = FakeMic::new(&pd100w_regs());
        let v = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        assert_eq!(v["mic.gain"], json!(20));
        assert_eq!(v["mic.nr.on"], json!(true));
        assert_eq!(v["info.battery"], json!(70));
        assert_eq!(v["info.firmware"], json!("1.0.8"));
        assert_eq!(v["dsp.comp.threshold"], json!(-20));
        assert_eq!(v["light.color"], json!({"preset": "red"}));
        assert_eq!(v["light.custom"], json!([]));
        assert_eq!(v["info.serial"], json!("PD100W"));
    }

    #[test]
    fn invalid_changes_write_nothing() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let r = apply(&mut dev, &d, &values(json!({"mic.gain": 21, "light.brightness": 50, "nope": 1})), &cur, &mut |_| {});
        match r {
            Err(ApplyError::Invalid(e)) => assert_eq!(e.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn apply_writes_and_reads_back() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = apply(&mut dev, &d, &values(json!({"mic.gain": 14, "mic.nr.level": 2})), &cur, &mut |_| {}).unwrap();
        assert_eq!(eff, values(json!({"mic.gain": 14, "mic.nr.level": 2})));
        assert_eq!(fake.reg(0x207E), Some(14));
    }

    #[test]
    fn new_hsv_colour_takes_the_next_custom_slot() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = apply(&mut dev, &d, &values(json!({"light.color": {"hsv": [330, 1, 1]}})), &cur, &mut |_| {}).unwrap();
        assert_eq!(fake.writes(), vec![(0x208F, 33000), (0x2090, 100), (0x2091, 100), (0x208E, 1), (0x208C, 8)]);
        assert_eq!(eff["light.color"], json!({"hsv": [330.0, 1.0, 1.0]}));
    }

    #[test]
    fn existing_hsv_colour_reuses_its_slot() {
        let d = Descriptor::load();
        let mut regs = pd100w_regs();
        regs.extend([(0x208E, 1), (0x208F, 33000), (0x2090, 100), (0x2091, 100)]);
        let (mut dev, fake) = FakeMic::new(&regs);
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        apply(&mut dev, &d, &values(json!({"light.color": {"hsv": [330, 1, 1]}})), &cur, &mut |_| {}).unwrap();
        assert_eq!(fake.writes(), vec![(0x208C, 8)]);
    }

    #[test]
    fn deleting_a_custom_colour_shifts_the_rest() {
        let d = Descriptor::load();
        let mut regs = pd100w_regs();
        regs.extend([(0x208E, 2), (0x208F, 1000), (0x2090, 100), (0x2091, 100), (0x2092, 2000), (0x2093, 50), (0x2094, 50), (0x208C, 9)]);
        let (mut dev, fake) = FakeMic::new(&regs);
        let cur = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        let eff = custom_delete(&mut dev, &d, &cur, 0, &mut |_| {}).unwrap();
        assert_eq!(fake.reg(0x208F), Some(2000));
        assert_eq!(fake.reg(0x208E), Some(1));
        assert_eq!(fake.reg(0x208C), Some(8));
        assert_eq!(eff["light.custom"], json!([[20.0, 0.5, 0.5]]));
    }

    #[test]
    fn delete_refuses_and_writes_nothing_when_a_later_slot_is_unreadable() {
        let d = Descriptor::load();
        let (mut dev, fake) = FakeMic::new(&pd100w_regs());
        let cur = values(json!({"light.custom": [[10, 1, 1], null]}));
        let r = custom_delete(&mut dev, &d, &cur, 0, &mut |_| {});
        assert!(matches!(r, Err(ApplyError::Invalid(_))), "{r:?}");
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn colour_index_pointing_at_an_unreadable_slot_reads_as_null() {
        let d = Descriptor::load();
        let mut regs = pd100w_regs();
        regs.extend([(0x208C, 9), (0x208E, 0)]);
        let (mut dev, _fake) = FakeMic::new(&regs);
        let v = read_all(&mut dev, &d, &mut |_| {}).unwrap();
        assert_eq!(v["light.color"], Value::Null);
    }

    #[test]
    fn colour_helpers() {
        let d = Descriptor::load();
        assert!(check_color(&d, &json!({"preset": "blue"})).is_ok());
        assert!(check_color(&d, &json!({"preset": "teal"})).is_err());
        assert!(check_color(&d, &json!({"hsv": [400, 1, 1]})).is_err());
        assert!(same_color(&json!({"hsv": [330.0, 1.0, 1.0]}), &json!({"hsv": [330, 1, 1]})));
        assert!(!same_color(&json!({"preset": "red"}), &json!({"preset": "blue"})));
    }
}
