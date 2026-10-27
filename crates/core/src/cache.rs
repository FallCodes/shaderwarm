//! What sits in a game's `shadercache/<appid>` directory, grouped by owner.

use std::fs;
use std::path::Path;

use crate::fsutil::tree_size;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CacheBreakdown {
    /// Mesa (RADV/ANV) disk cache, including RADV's built-in shaders.
    pub mesa: u64,
    pub nvidia: u64,
    pub amdvlk: u64,
    pub dxvk: u64,
    pub vkd3d: u64,
    /// Fossilize pipeline databases Steam recorded or downloaded.
    pub fossilize: u64,
    /// Steam's transcoded video, which Proton-CachyOS/GE don't need.
    pub video: u64,
    pub other: u64,
}

impl CacheBreakdown {
    pub fn total(&self) -> u64 {
        self.mesa + self.nvidia + self.amdvlk + self.dxvk + self.vkd3d + self.fossilize + self.video + self.other
    }

    /// Everything that is a compiled or recorded shader, i.e. all but video.
    pub fn shaders(&self) -> u64 {
        self.total() - self.video - self.other
    }
}

/// Driver and translation-layer cache settings for a per-game cache in `shader_path`.
/// Mirrors Proton's `setup_local_shader_cache` (Proton-CachyOS, GE and Wineland), which
/// only fills in unset variables, so the launch wrapper and replay can both apply it and
/// the game and the replayer agree on cache location and keys.
pub fn local_cache_env(shader_path: &Path) -> Vec<(String, String)> {
    const APP_CACHE: &str = "steamapp_shader_cache";
    let path = shader_path.display().to_string();
    let sub = |name: &str| shader_path.join(name).display().to_string();
    [
        ("__GL_SHADER_DISK_CACHE_APP_NAME", APP_CACHE.to_string()),
        ("__GL_SHADER_DISK_CACHE_PATH", sub("nvidiav1")),
        ("__GL_SHADER_DISK_CACHE_READ_ONLY_APP_NAME", "steam_shader_cache;steamapp_merged_shader_cache".into()),
        ("__GL_SHADER_DISK_CACHE_SIZE", "10737418240".into()),
        ("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS", "steam_cache,steam_precompiled".into()),
        ("MESA_DISK_CACHE_SINGLE_FILE", "1".into()),
        ("MESA_GLSL_CACHE_DIR", path.clone()),
        ("MESA_GLSL_CACHE_MAX_SIZE", "10G".into()),
        ("MESA_SHADER_CACHE_DIR", path),
        ("MESA_SHADER_CACHE_MAX_SIZE", "10G".into()),
        ("AMD_VK_PIPELINE_CACHE_FILENAME", APP_CACHE.to_string()),
        ("AMD_VK_PIPELINE_CACHE_PATH", sub("AMDv1")),
        ("AMD_VK_USE_PIPELINE_CACHE", "1".into()),
        ("DXVK_STATE_CACHE_PATH", sub("DXVK_state_cache")),
        ("VKD3D_SHADER_CACHE_PATH", sub("VKD3D_shader_cache")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Cache directories that must exist before the game starts (Proton creates the same set).
pub fn local_cache_dirs(shader_path: &Path) -> Vec<std::path::PathBuf> {
    ["nvidiav1", "AMDv1", "DXVK_state_cache", "VKD3D_shader_cache"]
        .iter()
        .map(|d| shader_path.join(d))
        .chain(std::iter::once(shader_path.to_path_buf()))
        .collect()
}

pub fn breakdown(shadercache: &Path) -> CacheBreakdown {
    let mut b = CacheBreakdown::default();
    let Ok(entries) = fs::read_dir(shadercache) else {
        return b;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let size = tree_size(&entry.path());
        let slot = match name.as_str() {
            n if n.starts_with("mesa_shader_cache") || n == "radv_builtin_shaders" => &mut b.mesa,
            "nvidiav1" | "GLCache" => &mut b.nvidia,
            "AMDv1" => &mut b.amdvlk,
            "DXVK_state_cache" => &mut b.dxvk,
            "VKD3D_shader_cache" => &mut b.vkd3d,
            n if n.starts_with("fozpipelines") => &mut b.fossilize,
            n if n.starts_with("fozmedia") || n.contains("video") || n == "h264-used" => &mut b.video,
            _ => &mut b.other,
        };
        *slot += size;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_steam_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for d in ["mesa_shader_cache_sf/x", "fozpipelinesv6", "fozmediav1", "nvidiav1"] {
            fs::create_dir_all(p.join(d)).unwrap();
        }
        fs::write(p.join("mesa_shader_cache_sf/x/RADV"), [0u8; 4]).unwrap();
        fs::write(p.join("fozpipelinesv6/steam_pipeline_cache.foz"), [0u8; 8]).unwrap();
        fs::write(p.join("fozmediav1/video.foz"), [0u8; 2]).unwrap();
        fs::write(p.join("transcoded_video.foz"), [0u8; 100]).unwrap();
        fs::write(p.join("h264-used"), []).unwrap();

        let b = breakdown(p);
        assert_eq!(b.mesa, 4);
        assert_eq!(b.fossilize, 8);
        assert_eq!(b.video, 102);
        assert_eq!(b.shaders(), 12);
        assert_eq!(b.total(), 114);
    }

    #[test]
    fn local_env_points_mesa_at_the_game_dir() {
        let env = local_cache_env(Path::new("/lib/steamapps/shadercache/42"));
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("MESA_SHADER_CACHE_DIR"), Some("/lib/steamapps/shadercache/42"));
        assert_eq!(get("MESA_DISK_CACHE_SINGLE_FILE"), Some("1"));
        assert_eq!(get("VKD3D_SHADER_CACHE_PATH"), Some("/lib/steamapps/shadercache/42/VKD3D_shader_cache"));
    }
}
