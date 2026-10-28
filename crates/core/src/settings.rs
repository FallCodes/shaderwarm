//! User preferences shared by the CLI, the GUI and the background service.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// Compile threads for replay; `None` means all cores.
    pub threads: Option<usize>,
    /// Let `replay --stale/--all` and auto-replay include self-compiling games.
    pub include_self_compiling: bool,
    /// GUI: skip transitions and reveal animations.
    pub reduce_motion: bool,
}

impl Settings {
    fn path() -> PathBuf {
        paths::config_dir().join("settings.toml")
    }

    /// Missing or unreadable settings fall back to defaults; a broken file must not stop
    /// the background service.
    pub fn load() -> Self {
        fs::read_to_string(Self::path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    pub fn effective_threads(&self) -> usize {
        self.threads
            .filter(|&n| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_use_defaults() {
        let s: Settings = toml::from_str("threads = 6\n").unwrap();
        assert_eq!(s.threads, Some(6));
        assert!(!s.include_self_compiling);
        assert_eq!(s.effective_threads(), 6);
        assert!(Settings { threads: Some(0), ..Default::default() }.effective_threads() > 0);
    }
}
