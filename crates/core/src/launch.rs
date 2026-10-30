//! The driver environment a game last ran with, captured by `shaderwarm run` so replay
//! compiles with the same cache location and the same options that feed driver cache keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths;

const FILE: &str = "launch.env";
const PROCESS_NAME: &str = "MESA_PROCESS_NAME";

/// Variables that change how drivers compile or where they cache. Lower-case prefixes are
/// driconf options, which Mesa also reads from the environment.
const DRIVER_PREFIXES: &[&str] = &[
    "RADV_", "ACO_", "MESA_", "NIR_", "ANV_", "INTEL_", "AMD_", "__GL_", "radv_", "anv_", "vk_", "mesa_",
];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LaunchEnv {
    pub vars: BTreeMap<String, String>,
}

impl LaunchEnv {
    /// Picks the driver variables out of a full environment, plus Mesa's process name so
    /// per-executable driconf entries match the game rather than `fossilize-replay`.
    pub fn capture(env: impl IntoIterator<Item = (String, String)>, exe: Option<&str>) -> Self {
        let mut vars: BTreeMap<String, String> = env
            .into_iter()
            .filter(|(k, _)| DRIVER_PREFIXES.iter().any(|p| k.starts_with(p)) || k == "STEAM_COMPAT_SHADER_PATH")
            .collect();
        if let Some(exe) = exe {
            vars.entry(PROCESS_NAME.to_string()).or_insert_with(|| exe.to_string());
        }
        Self { vars }
    }

    pub fn shader_path(&self) -> Option<PathBuf> {
        self.vars.get("STEAM_COMPAT_SHADER_PATH").map(PathBuf::from)
    }

    pub fn path(appid: u32) -> PathBuf {
        paths::recordings_dir(appid).join(FILE)
    }

    pub fn save(&self, appid: u32) -> Result<()> {
        let path = Self::path(appid);
        fs::create_dir_all(path.parent().unwrap())?;
        let text: String = self.vars.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(appid: u32) -> Option<Self> {
        Self::load_from(&Self::path(appid))
    }

    fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let vars = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(Self { vars })
    }
}

/// The Windows executable in a Proton launch command (`… proton waitforexitandrun C:\…\Game.exe`).
pub fn exe_name<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<String> {
    args.into_iter()
        .filter(|a| a.to_ascii_lowercase().ends_with(".exe"))
        .last()
        .and_then(|a| a.rsplit(['/', '\\']).next())
        .map(str::to_string)
}

/// Splits leading `NAME=value` words off a command, as `env` does.
pub fn split_assignments(command: &[String]) -> (Vec<(String, String)>, &[String]) {
    let is_name = |n: &str| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let count = command
        .iter()
        .take_while(|a| a.split_once('=').is_some_and(|(n, _)| is_name(n)))
        .count();
    let vars = command[..count]
        .iter()
        .filter_map(|a| a.split_once('='))
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    (vars, &command[count..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_leading_assignments_only() {
        let cmd: Vec<String> = ["PROTON_ENABLE_WAYLAND=1", "A_B=x=y", "/bin/bgmod", "FOO=1", "reaper"]
            .map(String::from)
            .to_vec();
        let (vars, rest) = split_assignments(&cmd);
        assert_eq!(vars, [("PROTON_ENABLE_WAYLAND".into(), "1".into()), ("A_B".into(), "x=y".into())]);
        assert_eq!(rest, &cmd[2..]);
        let plain: Vec<String> = ["/usr/bin/env", "=1"].map(String::from).to_vec();
        assert_eq!(split_assignments(&plain).1, &plain[..]);
    }

    #[test]
    fn captures_only_driver_vars_and_process_name() {
        let env = [
            ("RADV_PERFTEST", "gpl"),
            ("radv_zero_vram", "true"),
            ("HOME", "/home/x"),
            ("STEAM_COMPAT_SHADER_PATH", "/s/42"),
            ("DXVK_HUD", "fps"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let launch = LaunchEnv::capture(env, Some("Game-Win64-Shipping.exe"));
        let keys: Vec<_> = launch.vars.keys().map(String::as_str).collect();
        assert_eq!(keys, ["MESA_PROCESS_NAME", "RADV_PERFTEST", "STEAM_COMPAT_SHADER_PATH", "radv_zero_vram"]);
        assert_eq!(launch.shader_path(), Some(PathBuf::from("/s/42")));
    }

    #[test]
    fn round_trips_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let launch = LaunchEnv::capture([("MESA_X".to_string(), "a=b".to_string())], None);
        fs::write(&path, launch.vars.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<String>()).unwrap();
        assert_eq!(LaunchEnv::load_from(&path), Some(launch));
    }

    #[test]
    fn finds_windows_exe() {
        let args = ["/proton", "waitforexitandrun", "/games/GGST/RED/Binaries/Win64/GGST-Win64-Shipping.exe", "-dx11"];
        assert_eq!(exe_name(args).as_deref(), Some("GGST-Win64-Shipping.exe"));
        assert_eq!(exe_name(["C:\\Games\\a.EXE"]).as_deref(), Some("a.EXE"));
        assert_eq!(exe_name(["./native_game"]), None);
    }
}
