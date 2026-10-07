//! Paths and atomic file I/O shared by every module that persists something.

use serde::{Serialize, de::DeserializeOwned};
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg_config() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

/// ~/.config/maono (override with MAONO_CONFIG_DIR).
pub fn config_dir() -> PathBuf {
    env::var_os("MAONO_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| xdg_config().join("maono"))
}

/// The filter-chain fragment we own (override with MAONO_PW_CONF).
pub fn pipewire_conf() -> PathBuf {
    env::var_os("MAONO_PW_CONF")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg_config().join("pipewire/filter-chain.conf.d/maono-clean.conf"))
}

pub fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(env::temp_dir)
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("maono.sock")
}

pub fn lock_path() -> PathBuf {
    runtime_dir().join("maono.lock")
}

/// Write to a temp file in the same directory, fsync, then rename over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// `Ok(None)` when missing; `InvalidData` when the file exists but does not parse.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// "Voz de Noche!" -> "voz-de-noche": lowercase ASCII, digits and single dashes, at most 40 chars.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[test]
    fn write_atomic_creates_parents_and_leaves_no_temp_file() {
        let dir = tempdir("store");
        let path = dir.join("a/b/c.json");
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn read_json_missing_is_none_and_corrupt_is_an_error_that_keeps_the_file() {
        let dir = tempdir("store");
        assert!(read_json::<serde_json::Value>(&dir.join("nope.json")).unwrap().is_none());
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert_eq!(read_json::<serde_json::Value>(&bad).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert!(bad.exists());
    }

    #[test]
    fn slugify_makes_file_safe_ids() {
        assert_eq!(slugify("Voz de Noche!"), "voz-de-noche");
        assert_eq!(slugify("Ñandú  Grabación"), "nandu-grabacion");
        assert_eq!(slugify("  "), "");
        assert_eq!(slugify("Streaming / Discord"), "streaming-discord");
    }
}
