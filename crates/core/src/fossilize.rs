//! Locating the Fossilize tools: our bundled build first, Steam's copy as a fallback.

use std::env;
use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// `SHADERWARM_FOSSILIZE_REPLAY` points at it.
    Env,
    Bundled,
    Path,
    /// `ubuntu12_64/fossilize_replay` from the Steam client.
    Steam,
}

/// Name of the layer manifest `shaderwarm setup` installs.
pub const LAYER_MANIFEST: &str = "shaderwarm_fossilize.json";

pub fn locate_replay(steam_root: &Path) -> Option<(PathBuf, Source)> {
    if let Some(p) = env::var_os("SHADERWARM_FOSSILIZE_REPLAY").map(PathBuf::from) {
        if p.is_file() {
            return Some((p, Source::Env));
        }
    }
    if let Some(p) = bundled_dirs().iter().find_map(|d| find_replay_in(d)) {
        return Some((p, Source::Bundled));
    }
    if let Some(p) = env::var_os("PATH").iter().flat_map(env::split_paths).find_map(|d| find_replay_in(&d)) {
        return Some((p, Source::Path));
    }
    let steam = steam_root.join("ubuntu12_64/fossilize_replay");
    steam.is_file().then_some((steam, Source::Steam))
}

/// `fossilize-merge-db` from our build or PATH. Steam doesn't ship one.
pub fn locate_merge() -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = env::var_os("PATH").iter().flat_map(env::split_paths).collect();
    bundled_dirs()
        .into_iter()
        .chain(path_dirs)
        .flat_map(|d| ["fossilize-merge-db", "fossilize_merge_db"].map(|n| d.join(n)))
        .find(|p| p.is_file())
}

/// Upstream installs `fossilize-replay`; Steam ships `fossilize_replay`.
fn find_replay_in(dir: &Path) -> Option<PathBuf> {
    ["fossilize-replay", "fossilize_replay"].iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Where a release or `scripts/build-fossilize.sh` puts our own build.
pub fn bundled_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("../lib/shaderwarm"));
        dirs.push(exe_dir.clone());
    }
    dirs.push(prefix().join("bin"));
    dirs
}

/// Install prefix used by `scripts/build-fossilize.sh`.
pub fn prefix() -> PathBuf {
    paths::data_dir().join("fossilize")
}

/// Our build of the capture layer. Steam's copy can't be used: Steam controls its dump path.
pub fn layer_library() -> Option<PathBuf> {
    let p = prefix().join("lib/libVkLayer_fossilize.so");
    p.is_file().then_some(p)
}

/// The installed recording-layer manifest, if `shaderwarm setup` has run.
pub fn layer_manifest() -> Option<PathBuf> {
    let data_home = paths::data_dir().parent()?.to_path_buf();
    let p = data_home.join("vulkan/implicit_layer.d").join(LAYER_MANIFEST);
    p.is_file().then_some(p)
}
