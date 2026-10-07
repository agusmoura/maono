//! Backend settings in ~/.config/maono/config.json.

use crate::store;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,
    pub active_profile: Option<String>,
    pub apply_on_reconnect: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: 1, active_profile: None, apply_on_reconnect: true }
    }
}

impl Config {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("config.json")
    }

    /// A corrupt file is left on disk and reported; defaults are used for this run.
    pub fn load(dir: &Path) -> (Config, Vec<String>) {
        match store::read_json(&Self::path(dir)) {
            Ok(Some(c)) => (c, Vec::new()),
            Ok(None) => (Config::default(), Vec::new()),
            Err(e) => (Config::default(), vec![e.to_string()]),
        }
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        store::write_json(&Self::path(dir), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[test]
    fn roundtrip_and_corrupt_fallback() {
        let dir = tempdir("config");
        let (c, w) = Config::load(&dir);
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
        let c = Config { active_profile: Some("llamada".into()), ..Config::default() };
        c.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).0, c);
        std::fs::write(dir.join("config.json"), "garbage").unwrap();
        let (c, w) = Config::load(&dir);
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
    }
}
