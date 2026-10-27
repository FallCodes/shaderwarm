//! Per-game fingerprint of the last successful replay, used to tell when a cache went stale.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fingerprint {
    /// Hex `pipelineCacheUUID` of the GPU the cache was built for.
    pub pipeline_cache_uuid: String,
    /// Hash over the replay inputs (paths, sizes, mtimes).
    pub inputs: String,
    pub proton: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    Fresh,
    NeverReplayed,
    DriverChanged,
    NewRecordings,
    ProtonChanged,
    NoInputs,
}

impl Staleness {
    pub fn needs_replay(self) -> bool {
        !matches!(self, Staleness::Fresh | Staleness::NoInputs)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Staleness::Fresh => "fresh",
            Staleness::NeverReplayed => "never replayed",
            Staleness::DriverChanged => "stale: driver changed",
            Staleness::NewRecordings => "stale: new recordings",
            Staleness::ProtonChanged => "stale: Proton changed",
            Staleness::NoInputs => "nothing recorded",
        }
    }
}

impl Fingerprint {
    pub fn compare(stored: Option<&Fingerprint>, current: &Fingerprint) -> Staleness {
        if current.inputs.is_empty() {
            return Staleness::NoInputs;
        }
        let Some(stored) = stored else {
            return Staleness::NeverReplayed;
        };
        if stored.pipeline_cache_uuid != current.pipeline_cache_uuid {
            Staleness::DriverChanged
        } else if stored.inputs != current.inputs {
            Staleness::NewRecordings
        } else if stored.proton != current.proton {
            Staleness::ProtonChanged
        } else {
            Staleness::Fresh
        }
    }

    fn path(appid: u32) -> PathBuf {
        paths::state_dir().join("games").join(format!("{appid}.state"))
    }

    pub fn load(appid: u32) -> Option<Self> {
        Self::load_from(&Self::path(appid))
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let mut fp = Fingerprint::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            match k {
                "pipeline_cache_uuid" => fp.pipeline_cache_uuid = v.to_string(),
                "inputs" => fp.inputs = v.to_string(),
                "proton" => fp.proton = v.to_string(),
                _ => {}
            }
        }
        Some(fp)
    }

    pub fn save(&self, appid: u32) -> Result<()> {
        self.save_to(&Self::path(appid))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path.parent().unwrap())?;
        let text = format!(
            "pipeline_cache_uuid={}\ninputs={}\nproton={}\n",
            self.pipeline_cache_uuid, self.inputs, self.proton
        );
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Forgets the last replay so the game shows as stale (after a cache purge).
    pub fn reset(appid: u32) -> Result<()> {
        match fs::remove_file(Self::path(appid)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Stable hash of a set of input files. FNV-1a, so it doesn't change between Rust releases.
pub fn hash_inputs(files: &[PathBuf]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let mut sorted: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    sorted.sort();
    for f in sorted {
        feed(f.as_os_str().as_encoded_bytes());
        if let Ok(meta) = fs::metadata(f) {
            feed(&meta.len().to_le_bytes());
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            feed(&mtime.map_or(0, |d| d.as_nanos()).to_le_bytes());
        }
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(uuid: &str, inputs: &str, proton: &str) -> Fingerprint {
        Fingerprint { pipeline_cache_uuid: uuid.into(), inputs: inputs.into(), proton: proton.into() }
    }

    #[test]
    fn staleness_order() {
        let cur = fp("u1", "i1", "p1");
        assert_eq!(Fingerprint::compare(None, &cur), Staleness::NeverReplayed);
        assert_eq!(Fingerprint::compare(Some(&cur), &cur), Staleness::Fresh);
        assert_eq!(Fingerprint::compare(Some(&fp("u0", "i0", "p0")), &cur), Staleness::DriverChanged);
        assert_eq!(Fingerprint::compare(Some(&fp("u1", "i0", "p1")), &cur), Staleness::NewRecordings);
        assert_eq!(Fingerprint::compare(Some(&fp("u1", "i1", "p0")), &cur), Staleness::ProtonChanged);
        assert_eq!(Fingerprint::compare(None, &fp("u1", "", "p1")), Staleness::NoInputs);
    }

    #[test]
    fn input_hash_tracks_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.foz");
        fs::write(&a, b"1").unwrap();
        let first = hash_inputs(std::slice::from_ref(&a));
        assert_eq!(first, hash_inputs(std::slice::from_ref(&a)));
        fs::write(&a, b"12").unwrap();
        assert_ne!(first, hash_inputs(&[a]));
        assert_eq!(hash_inputs(&[]), "");
    }
}
