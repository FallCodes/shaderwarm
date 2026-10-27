//! Removing caches: Steam's video transcodes, orphaned caches and per-game shader purges
//! (optionally moved to a backup for cold/warm A-B testing).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::fsutil::tree_size;
use crate::paths;
use crate::state::Fingerprint;
use crate::steam::{Game, Steam};

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub path: PathBuf,
    pub size: u64,
}

fn target(path: PathBuf) -> Target {
    Target { size: tree_size(&path), path }
}

fn children_matching(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<Target> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Target> = entries
        .flatten()
        .filter(|e| pred(&e.file_name().to_string_lossy()))
        .map(|e| target(e.path()))
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Steam's transcoded video for a game.
pub fn video_targets(game: &Game) -> Vec<Target> {
    children_matching(&game.shadercache(), |n| {
        n.starts_with("fozmedia") || n == "transcoded_video.foz" || n == "h264-used"
    })
}

/// Compiled caches only. Steam's `fozpipelines*` recordings are kept: they are replay inputs.
fn is_compiled_cache(name: &str) -> bool {
    name.starts_with("mesa_shader_cache")
        || matches!(
            name,
            "radv_builtin_shaders" | "nvidiav1" | "GLCache" | "AMDv1" | "DXVK_state_cache" | "VKD3D_shader_cache"
        )
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PurgeOptions {
    /// Also delete shaderwarm's recordings (they are what rebuilds the cache).
    pub recordings: bool,
}

/// What a per-game purge removes.
pub fn purge_targets(game: &Game, opts: PurgeOptions) -> Vec<Target> {
    let mut out = children_matching(&game.shadercache(), is_compiled_cache);
    // Without VKD3D_SHADER_CACHE_PATH, vkd3d-proton writes its cache next to the game exe.
    out.extend(find_files(&game.install_dir, "vkd3d-proton.cache", 6).into_iter().map(target));
    out.extend(find_files(&game.compatdata(), "vkd3d-proton.cache", 2).into_iter().map(target));
    if opts.recordings {
        let rec = paths::recordings_dir(game.appid);
        if rec.exists() {
            out.push(target(rec));
        }
    }
    out
}

fn find_files(dir: &Path, prefix: &str, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() && depth > 0 {
            out.extend(find_files(&entry.path(), prefix, depth - 1));
        } else if kind.is_file() && entry.file_name().to_string_lossy().starts_with(prefix) {
            out.push(entry.path());
        }
    }
    out
}

/// Caches and recordings of games that are no longer installed.
pub fn orphan_targets(steam: &Steam) -> Vec<Target> {
    let installed: HashSet<String> = steam.games().iter().map(|g| g.appid.to_string()).collect();
    // Tools such as Proton have shader caches too; they are installed, just not games.
    let is_orphan = |n: &str| n.parse::<u32>().is_ok_and(|id| id != 0) && !installed.contains(n) && !is_tool(steam, n);
    let mut out: Vec<Target> = steam
        .libraries
        .iter()
        .flat_map(|lib| children_matching(&lib.join("steamapps/shadercache"), is_orphan))
        .collect();
    out.extend(children_matching(&paths::data_dir().join("recordings"), is_orphan));
    out
}

fn is_tool(steam: &Steam, appid: &str) -> bool {
    steam.libraries.iter().any(|lib| lib.join(format!("steamapps/appmanifest_{appid}.acf")).is_file())
}

/// Processes that use the game's caches: the game itself (Steam exports its app id to
/// every child) or a replay/warm job writing to its shader cache.
pub fn users_of(proc_root: &Path, game: &Game) -> Vec<(u32, String)> {
    let appid_vars = [
        format!("SteamAppId={}", game.appid),
        format!("STEAM_COMPAT_APP_ID={}", game.appid),
    ];
    let cache_var = format!("MESA_SHADER_CACHE_DIR={}", game.shadercache().display());
    let Ok(entries) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(environ) = fs::read(entry.path().join("environ")) else { continue };
        let uses = environ.split(|&b| b == 0).any(|var| {
            let var = String::from_utf8_lossy(var);
            appid_vars.iter().any(|v| *v == var) || var == cache_var
        });
        if uses {
            let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            out.push((pid, comm.trim().to_string()));
        }
    }
    out.sort();
    out
}

/// Deletes the targets, or moves them into a new backup when `backup_for` names the game.
/// Returns the backup directory when one was made.
pub fn remove(targets: &[Target], backup_for: Option<u32>) -> Result<Option<PathBuf>> {
    let Some(appid) = backup_for else {
        for t in targets {
            remove_path(&t.path).with_context(|| format!("removing {}", t.path.display()))?;
        }
        return Ok(None);
    };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let dir = paths::backups_dir().join(format!("{appid}-{stamp}"));
    fs::create_dir_all(&dir)?;
    let mut manifest = String::new();
    for (i, t) in targets.iter().enumerate() {
        let name = t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let slot = format!("{i:03}-{name}");
        move_path(&t.path, &dir.join(&slot)).with_context(|| format!("backing up {}", t.path.display()))?;
        manifest.push_str(&format!("{slot}\t{}\n", t.path.display()));
    }
    fs::write(dir.join(RESTORE_LIST), manifest)?;
    // The backed-up cache matches the last replay, so restoring it restores that state too.
    if let Some(fp) = Fingerprint::load(appid) {
        fp.save_to(&dir.join(FINGERPRINT))?;
    }
    Ok(Some(dir))
}

const RESTORE_LIST: &str = "restore.list";
const FINGERPRINT: &str = "fingerprint.state";

/// Backups of a game, oldest first, as (timestamp, directory).
pub fn backups(appid: u32) -> Vec<(u64, PathBuf)> {
    let prefix = format!("{appid}-");
    let Ok(entries) = fs::read_dir(paths::backups_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let stamp = e.file_name().to_str()?.strip_prefix(&prefix)?.parse().ok()?;
            e.path().join(RESTORE_LIST).is_file().then(|| (stamp, e.path()))
        })
        .collect();
    out.sort();
    out
}

/// Moves a backup back into place, replacing whatever the game built since, along with
/// the replay fingerprint it was taken with.
pub fn restore(appid: u32, backup: &Path) -> Result<()> {
    let list = fs::read_to_string(backup.join(RESTORE_LIST)).context("backup has no restore list")?;
    for line in list.lines() {
        let Some((slot, original)) = line.split_once('\t') else { continue };
        let original = Path::new(original);
        if original.exists() {
            remove_path(original)?;
        }
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent)?;
        }
        move_path(&backup.join(slot), original).with_context(|| format!("restoring {}", original.display()))?;
    }
    match Fingerprint::load_from(&backup.join(FINGERPRINT)) {
        Some(fp) => fp.save(appid)?,
        None => Fingerprint::reset(appid)?,
    }
    fs::remove_dir_all(backup)?;
    Ok(())
}

/// After a purge or restore the stored fingerprint no longer describes the cache.
pub fn forget_replay(appid: u32) -> Result<()> {
    Fingerprint::reset(appid)
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Rename, falling back to copy + delete across filesystems (libraries on other drives).
fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            copy_tree(from, to)?;
            remove_path(from)
        }
        other => other,
    }
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

/// Refuses to touch caches a running process is using.
pub fn ensure_not_in_use(game: &Game) -> Result<()> {
    let users = users_of(Path::new("/proc"), game);
    if !users.is_empty() {
        let list: Vec<String> = users.iter().map(|(pid, name)| format!("{name} ({pid})")).collect();
        bail!("{} is running or being compiled: {}", game.name, list.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(root: &Path) -> Game {
        Game {
            appid: 42,
            name: "Test".into(),
            library: root.to_path_buf(),
            install_dir: root.join("steamapps/common/Test"),
        }
    }

    fn touch(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; len]).unwrap();
    }

    #[test]
    fn purge_keeps_fossilize_inputs_and_video() {
        let tmp = tempfile::tempdir().unwrap();
        let g = game(tmp.path());
        let sc = g.shadercache();
        touch(&sc.join("mesa_shader_cache_sf/a/RADV"), 4);
        touch(&sc.join("DXVK_state_cache/x"), 1);
        touch(&sc.join("fozpipelinesv6/steam_pipeline_cache.foz"), 8);
        touch(&sc.join("transcoded_video.foz"), 9);
        touch(&g.install_dir.join("Binaries/Win64/vkd3d-proton.cache"), 3);

        let names: Vec<String> = purge_targets(&g, PurgeOptions::default())
            .iter()
            .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["DXVK_state_cache", "mesa_shader_cache_sf", "vkd3d-proton.cache"]);

        let video: Vec<_> = video_targets(&g).into_iter().map(|t| t.size).collect();
        assert_eq!(video, [9]);
    }

    #[test]
    fn detects_processes_by_app_id_or_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let g = game(tmp.path());
        let proc_root = tmp.path().join("proc");
        let write_proc = |pid: u32, env: &str, comm: &str| {
            touch(&proc_root.join(format!("{pid}/environ")), 0);
            fs::write(proc_root.join(format!("{pid}/environ")), env.replace(' ', "\0")).unwrap();
            fs::write(proc_root.join(format!("{pid}/comm")), comm).unwrap();
        };
        write_proc(100, "HOME=/h SteamAppId=42", "Game.exe\n");
        write_proc(101, "SteamAppId=420", "Other.exe\n");
        write_proc(102, &format!("MESA_SHADER_CACHE_DIR={}", g.shadercache().display()), "fossilize-repl\n");
        touch(&proc_root.join("self/environ"), 0);

        let users = users_of(&proc_root, &g);
        assert_eq!(users, [(100, "Game.exe".to_string()), (102, "fossilize-repl".to_string())]);
    }

    #[test]
    fn copy_tree_handles_nested_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("src/a/b"), 2);
        copy_tree(&tmp.path().join("src"), &tmp.path().join("dst")).unwrap();
        assert_eq!(fs::read(tmp.path().join("dst/a/b")).unwrap().len(), 2);
    }
}
