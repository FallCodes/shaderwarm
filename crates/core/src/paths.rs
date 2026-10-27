//! Where shaderwarm keeps its own files (XDG base directories).

use std::env;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

/// `$XDG_DATA_HOME/shaderwarm`: recordings and backups.
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("shaderwarm")
}

/// `$XDG_STATE_HOME/shaderwarm`: per-game fingerprints and the stale marker.
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("shaderwarm")
}

/// `$XDG_CONFIG_HOME/shaderwarm`: settings and the self-compile override list.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("shaderwarm")
}

/// Fossilize dump directory for one game.
pub fn recordings_dir(appid: u32) -> PathBuf {
    data_dir().join("recordings").join(appid.to_string())
}

pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}
