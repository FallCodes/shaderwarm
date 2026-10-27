//! Recording pipelines while a game runs, through our implicit Fossilize layer.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fossilize;
use crate::paths;

/// The loader enables our layer only when this is set, like Steam's own Fossilize layer.
pub const ENABLE_ENV: &str = "SHADERWARM_RECORD";
pub const LAYER_NAME: &str = "VK_LAYER_shaderwarm_fossilize";
/// Fossilize writes `<prefix>.<hash>.<index>.foz` next to this prefix.
const DUMP_PREFIX: &str = "session";
/// Merged database of earlier sessions; the layer skips entries already in it.
pub const MERGED_DB: &str = "merged.foz";

pub fn layer_manifest_dir() -> Option<PathBuf> {
    Some(paths::data_dir().parent()?.join("vulkan/implicit_layer.d"))
}

pub fn layer_manifest_json(library: &Path) -> String {
    format!(
        r#"{{
    "file_format_version": "1.0.0",
    "layer": {{
        "name": "{LAYER_NAME}",
        "type": "GLOBAL",
        "library_path": "{}",
        "api_version": "1.4.303",
        "implementation_version": "1",
        "description": "shaderwarm pipeline recorder (Fossilize)",
        "functions": {{
            "vkGetInstanceProcAddr": "VK_LAYER_fossilize_GetInstanceProcAddr",
            "vkGetDeviceProcAddr": "VK_LAYER_fossilize_GetDeviceProcAddr"
        }},
        "enable_environment": {{ "{ENABLE_ENV}": "1" }},
        "disable_environment": {{ "DISABLE_{ENABLE_ENV}": "1" }}
    }}
}}
"#,
        library.display()
    )
}

/// Writes the layer manifest pointing at our Fossilize build. Returns its path.
pub fn install_layer_manifest() -> Result<PathBuf> {
    let library = fossilize::layer_library()
        .context("Fossilize layer not built; run scripts/build-fossilize.sh first")?;
    let dir = layer_manifest_dir().context("cannot determine the XDG data directory")?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(fossilize::LAYER_MANIFEST);
    fs::write(&path, layer_manifest_json(&library)).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// With a valid disk cache, vkd3d-proton creates pipelines from shader module identifiers
/// instead of SPIR-V. Fossilize can't record those, and the mix crashed the GPU in a UE5
/// game, so recording hides the extension: vkd3d-proton then always passes full SPIR-V.
const VKD3D_IDENTIFIER_EXT: &str = "VK_EXT_shader_module_identifier";

fn vkd3d_disabled_extensions(existing: Option<&str>) -> String {
    match existing.map(str::trim).filter(|s| !s.is_empty()) {
        Some(list) if list.split([',', ';']).any(|e| e.trim() == VKD3D_IDENTIFIER_EXT) => list.to_string(),
        Some(list) => format!("{list},{VKD3D_IDENTIFIER_EXT}"),
        None => VKD3D_IDENTIFIER_EXT.to_string(),
    }
}

/// Environment that makes a game record into `recordings/<appid>/`. `get_env` reads the
/// game's current environment, so user settings that we extend are kept.
pub fn record_env(appid: u32, get_env: impl Fn(&str) -> Option<String>) -> Result<Vec<(String, String)>> {
    let dir = paths::recordings_dir(appid);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut env = vec![
        (ENABLE_ENV.to_string(), "1".to_string()),
        ("FOSSILIZE_DUMP_PATH".to_string(), dir.join(DUMP_PREFIX).display().to_string()),
        (
            "VKD3D_DISABLE_EXTENSIONS".to_string(),
            vkd3d_disabled_extensions(get_env("VKD3D_DISABLE_EXTENSIONS").as_deref()),
        ),
    ];
    let merged = dir.join(MERGED_DB);
    if merged.is_file() {
        env.push(("FOSSILIZE_DUMP_PATH_READ_ONLY".to_string(), merged.display().to_string()));
    }
    Ok(env)
}

/// Everything recorded for a game: the merged database, then sessions not merged yet.
pub fn recorded_dbs(appid: u32) -> Vec<PathBuf> {
    let merged = paths::recordings_dir(appid).join(MERGED_DB);
    std::iter::once(merged)
        .chain(session_dbs(appid))
        .filter(|p| fs::metadata(p).is_ok_and(|m| m.len() > 0))
        .collect()
}

/// Folds every session into `merged.foz` and deletes them. Each launch re-records the
/// pipelines the game builds, so without this the recordings grow by the full set each
/// time; with it, the next session only holds pipelines `merged.foz` lacks.
/// Sessions cut short by a crash still contribute their complete entries.
pub fn compact(appid: u32, merge_bin: &Path) -> Result<()> {
    compact_dir(&paths::recordings_dir(appid), merge_bin)
}

fn compact_dir(dir: &Path, merge_bin: &Path) -> Result<()> {
    let sessions = sessions_in(dir);
    if sessions.is_empty() {
        return Ok(());
    }
    let merged = dir.join(MERGED_DB);
    let inputs: Vec<PathBuf> = std::iter::once(merged.clone())
        .chain(sessions.iter().cloned())
        .filter(|p| fs::metadata(p).is_ok_and(|m| m.len() > 0))
        .collect();
    if !inputs.is_empty() {
        // merge-db appends into its first argument, so build a fresh file and swap it in.
        let tmp = dir.join(format!("{MERGED_DB}.tmp"));
        let _ = fs::remove_file(&tmp);
        let status = std::process::Command::new(merge_bin)
            .arg(&tmp)
            .args(&inputs)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .with_context(|| format!("running {}", merge_bin.display()))?;
        if !status.success() {
            let _ = fs::remove_file(&tmp);
            anyhow::bail!("fossilize-merge-db failed ({status})");
        }
        fs::rename(&tmp, &merged)?;
    }
    for session in sessions {
        fs::remove_file(&session).with_context(|| format!("removing {}", session.display()))?;
    }
    Ok(())
}

/// Recorded session databases for a game, oldest first.
fn session_dbs(appid: u32) -> Vec<PathBuf> {
    sessions_in(&paths::recordings_dir(appid))
}

fn sessions_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dbs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(DUMP_PREFIX) && name.ends_with(".foz")
        })
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    dbs.sort();
    dbs.into_iter().map(|(_, p)| p).collect()
}

/// Finds the Steam app id of the game being launched. Steam exports it, and its launch
/// command carries `AppId=<id>` for the reaper as a fallback.
pub fn detect_appid<'a>(env: impl Fn(&str) -> Option<String>, args: impl IntoIterator<Item = &'a str>) -> Option<u32> {
    ["SteamAppId", "STEAM_COMPAT_APP_ID", "SteamGameId"]
        .iter()
        .filter_map(|k| env(k))
        .find_map(|v| v.parse().ok().filter(|&id: &u32| id != 0))
        .or_else(|| args.into_iter().find_map(|a| a.strip_prefix("AppId=")?.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appid_from_env_then_reaper_args() {
        let env = |k: &str| (k == "STEAM_COMPAT_APP_ID").then(|| "1384160".to_string());
        assert_eq!(detect_appid(env, []), Some(1384160));

        let none = |_: &str| None;
        let args = ["/steam/reaper", "SteamLaunch", "AppId=2050650", "--", "proton"];
        assert_eq!(detect_appid(none, args), Some(2050650));
        assert_eq!(detect_appid(none, ["game.sh"]), None);

        let zero = |k: &str| (k == "SteamAppId").then(|| "0".to_string());
        assert_eq!(detect_appid(zero, ["AppId=7"]), Some(7));
    }

    #[test]
    fn hides_module_identifiers_from_vkd3d_keeping_user_list() {
        assert_eq!(vkd3d_disabled_extensions(None), "VK_EXT_shader_module_identifier");
        assert_eq!(vkd3d_disabled_extensions(Some(" ")), "VK_EXT_shader_module_identifier");
        assert_eq!(
            vkd3d_disabled_extensions(Some("VK_EXT_mesh_shader")),
            "VK_EXT_mesh_shader,VK_EXT_shader_module_identifier"
        );
        let already = "VK_EXT_shader_module_identifier;VK_KHR_x";
        assert_eq!(vkd3d_disabled_extensions(Some(already)), already);
    }

    /// Stand-in for fossilize-merge-db: concatenates inputs into the first argument.
    fn fake_merge(dir: &Path, exit: u8) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("merge");
        fs::write(&bin, format!("#!/bin/sh\nout=$1; shift; cat \"$@\" >> \"$out\"; exit {exit}\n")).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn compaction_folds_sessions_into_merged_db() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rec");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(MERGED_DB), "M").unwrap();
        fs::write(dir.join("session.a.1.foz"), "A").unwrap();
        fs::write(dir.join("session.b.1.foz"), "").unwrap();
        fs::write(dir.join("launch.env"), "X=1").unwrap();

        compact_dir(&dir, &fake_merge(tmp.path(), 0)).unwrap();
        assert_eq!(fs::read_to_string(dir.join(MERGED_DB)).unwrap(), "MA");
        assert!(sessions_in(&dir).is_empty(), "sessions, empty ones included, are removed");
        assert!(dir.join("launch.env").is_file());
    }

    #[test]
    fn failed_compaction_keeps_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rec");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session.a.1.foz"), "A").unwrap();

        assert!(compact_dir(&dir, &fake_merge(tmp.path(), 1)).is_err());
        assert_eq!(sessions_in(&dir).len(), 1);
        assert!(!dir.join(MERGED_DB).exists());
        assert!(!dir.join(format!("{MERGED_DB}.tmp")).exists());
    }

    #[test]
    fn manifest_points_at_library_and_uses_own_enable_env() {
        let json = layer_manifest_json(Path::new("/x/libVkLayer_fossilize.so"));
        assert!(json.contains(r#""library_path": "/x/libVkLayer_fossilize.so""#));
        assert!(json.contains(r#""SHADERWARM_RECORD": "1""#));
    }
}
