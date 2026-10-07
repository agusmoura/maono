//! Bring PipeWire to a FilterState: live params when the graph shape is the
//! same, one verified restart otherwise, rollback when the node does not come up.

use crate::filter::{Deps, FilterSchema, FilterState, params, render_conf, structure_key};
use crate::pw::Audio;
use crate::store;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Live,
    Restarted,
}

pub struct FilterCtl<A: Audio> {
    pub audio: A,
    pub schema: FilterSchema,
    pub verify_tries: u32,
    pub verify_sleep: Duration,
    conf_path: PathBuf,
    state_path: PathBuf,
    applied: Option<String>,
    target: Option<String>,
}

impl<A: Audio> FilterCtl<A> {
    pub fn new(audio: A, schema: FilterSchema, conf_path: PathBuf, state_path: PathBuf) -> Self {
        FilterCtl { audio, schema, verify_tries: 25, verify_sleep: Duration::from_millis(200), conf_path, state_path, applied: None, target: None }
    }

    /// filter.json, or the schema defaults (with a warning when the file is bad).
    pub fn load_state(&self) -> (FilterState, Vec<String>) {
        match store::read_json::<FilterState>(&self.state_path) {
            Ok(Some(s)) => match self.schema.validate(&s) {
                Ok(()) => (s, Vec::new()),
                Err(e) => (self.schema.defaults.clone(), vec![format!("{}: {}", self.state_path.display(), e.join("; "))]),
            },
            Ok(None) => (self.schema.defaults.clone(), Vec::new()),
            Err(e) => (self.schema.defaults.clone(), vec![e.to_string()]),
        }
    }

    pub fn is_applied(&self) -> bool {
        self.applied.is_some()
    }

    fn key(s: &FilterState, d: &Deps, target: &str) -> String {
        format!("{}|{target}", structure_key(s, d))
    }

    /// At startup: when the conf on disk is exactly what we would write and the
    /// node is up, take it over without restarting (no audio drop).
    pub fn adopt(&mut self, s: &FilterState) -> bool {
        let d = self.audio.deps();
        let Some(target) = self.audio.capture_target() else { return false };
        let same = fs::read_to_string(&self.conf_path).is_ok_and(|c| c == render_conf(s, &d, &self.schema, &target));
        if same && self.audio.clean_params().is_some() {
            self.applied = Some(Self::key(s, &d, &target));
            self.target = Some(target);
            true
        } else {
            false
        }
    }

    pub fn apply(&mut self, next: &FilterState) -> Result<Outcome, String> {
        self.schema.validate(next).map_err(|e| e.join("; "))?;
        let d = self.audio.deps();
        let target = self.audio.capture_target().or_else(|| self.target.clone()).ok_or("the mic is not visible in PipeWire")?;
        let conf = render_conf(next, &d, &self.schema, &target);
        let key = Self::key(next, &d, &target);
        if self.applied.as_deref() == Some(key.as_str()) && self.audio.clean_params().is_some() {
            self.audio.set_params(&params(next, &d, &self.schema)).map_err(|e| e.to_string())?;
            store::write_atomic(&self.conf_path, conf.as_bytes()).map_err(|e| e.to_string())?;
            store::write_json(&self.state_path, next).map_err(|e| e.to_string())?;
            return Ok(Outcome::Live);
        }
        let previous = fs::read_to_string(&self.conf_path).ok();
        store::write_atomic(&self.conf_path, conf.as_bytes()).map_err(|e| e.to_string())?;
        if self.restart_and_verify(next, &d) {
            self.applied = Some(key);
            self.target = Some(target);
            store::write_json(&self.state_path, next).map_err(|e| e.to_string())?;
            return Ok(Outcome::Restarted);
        }
        match previous {
            Some(p) => store::write_atomic(&self.conf_path, p.as_bytes()).map_err(|e| e.to_string())?,
            None => {
                let _ = fs::remove_file(&self.conf_path);
            }
        }
        let _ = self.audio.restart();
        self.applied = None;
        Err("the filter did not come up with the new settings; the previous configuration was restored".into())
    }

    fn restart_and_verify(&mut self, s: &FilterState, d: &Deps) -> bool {
        if self.audio.restart().is_err() {
            return false;
        }
        let want = params(s, d, &self.schema);
        for _ in 0..self.verify_tries {
            if let Some(have) = self.audio.clean_params() {
                // PipeWire stores floats: compare with a relative tolerance.
                if want.iter().all(|(k, v)| have.get(k).is_some_and(|h| (h - v).abs() <= 1e-3 * v.abs().max(1.0))) {
                    return true;
                }
            }
            std::thread::sleep(self.verify_sleep);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::fake::FakeAudio;
    use crate::testutil::tempdir;
    use std::path::Path;

    fn ctl(dir: &Path) -> FilterCtl<FakeAudio> {
        let conf = dir.join("pw/maono-clean.conf");
        let mut c = FilterCtl::new(FakeAudio::new(&conf), FilterSchema::load(), conf, dir.join("filter.json"));
        c.verify_sleep = Duration::ZERO;
        c.verify_tries = 3;
        c
    }

    #[test]
    fn first_apply_restarts_then_param_changes_are_live() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        assert_eq!(c.apply(&s), Ok(Outcome::Restarted));
        assert_eq!(c.audio.restarts, 1);
        assert!(dir.join("filter.json").exists());
        s.rnnoise.vad = 70.0;
        assert_eq!(c.apply(&s), Ok(Outcome::Live));
        assert_eq!((c.audio.restarts, c.audio.live_sets), (1, 1));
        assert_eq!(c.audio.node.as_ref().unwrap()["rnnoise:VAD Threshold (%)"], 70.0);
        assert!(fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap().contains("\"VAD Threshold (%)\" = 70"));
    }

    #[test]
    fn band_type_change_restarts() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        s.eq.bands[0].kind = "lowshelf".into();
        assert_eq!(c.apply(&s), Ok(Outcome::Restarted));
        assert_eq!(c.audio.restarts, 2);
    }

    #[test]
    fn broken_restart_restores_previous_conf() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        let before = fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap();
        let mut next = s.clone();
        next.eq.bands[0].kind = "highshelf".into();
        c.audio.broken = true;
        assert!(c.apply(&next).is_err());
        assert_eq!(fs::read_to_string(dir.join("pw/maono-clean.conf")).unwrap(), before);
        assert_eq!(c.audio.restarts, 3); // the failed one + the restore
        assert!(!c.is_applied());
    }

    #[test]
    fn adopt_skips_the_restart_when_disk_already_matches() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut s = c.schema.defaults.clone();
        c.apply(&s).unwrap();
        let mut again = ctl(&dir);
        again.audio.node = c.audio.node.clone();
        assert!(again.adopt(&s));
        s.rnnoise.vad = 60.0;
        assert_eq!(again.apply(&s), Ok(Outcome::Live));
        assert_eq!(again.audio.restarts, 0);
    }

    #[test]
    fn invalid_state_or_missing_mic_touches_nothing() {
        let dir = tempdir("fctl");
        let mut c = ctl(&dir);
        let mut bad = c.schema.defaults.clone();
        bad.rnnoise.vad = 150.0;
        assert!(c.apply(&bad).is_err());
        c.audio.target = None;
        assert!(c.apply(&c.schema.defaults.clone()).is_err());
        assert_eq!(c.audio.restarts, 0);
        assert!(!dir.join("pw/maono-clean.conf").exists());
    }
}
