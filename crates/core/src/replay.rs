//! Replaying recorded pipelines into a game's driver cache with `fossilize-replay`.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::cache;
use crate::clean;
use crate::fossilize;
use crate::gpu::Gpu;
use crate::launch::LaunchEnv;
use crate::record;
use crate::state::{self, Fingerprint, Staleness};
use crate::steam::{Game, Steam};

/// Steam's own recordings, kept when pre-caching was on. Other `.foz` files there are
/// whitelists or replayer bookkeeping, not pipeline sources.
const STEAM_DBS: &[&str] = &["steam_pipeline_cache.foz", "steamapp_pipeline_cache.foz"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

pub struct Job {
    pub appid: u32,
    pub inputs: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub threads: usize,
    pub fingerprint: Fingerprint,
    pub staleness: Staleness,
    shadercache: PathBuf,
    /// The recorder may be writing a session; compacting would delete it mid-write.
    game_running: bool,
    replay_bin: PathBuf,
    merge_bin: Option<PathBuf>,
}

/// Every non-empty pipeline database we can replay for a game.
pub fn inputs(game: &Game) -> Vec<PathBuf> {
    inputs_from(game.appid, &game.shadercache())
}

fn inputs_from(appid: u32, shadercache: &Path) -> Vec<PathBuf> {
    // Steam's databases are replayed in place rather than merged, so they aren't stored twice.
    let mut dbs = record::recorded_dbs(appid);
    collect_steam_dbs(shadercache, &mut dbs);
    dbs.retain(|p| fs::metadata(p).is_ok_and(|m| m.len() > 0));
    dbs
}

fn collect_steam_dbs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            // Only descend into the Fossilize part of the cache, not Mesa's files.
            let name = name.to_string_lossy();
            if name.starts_with("fozpipelines") || name.starts_with("steamapprun_pipeline_cache") {
                collect_steam_dbs(&path, out);
            }
        } else if STEAM_DBS.iter().any(|db| name == *db) {
            out.push(path);
        }
    }
}

/// Cache settings for replaying a game: the per-game Proton cache, overlaid with whatever
/// the game actually launched with last time.
pub fn env_for(game: &Game, launch: Option<&LaunchEnv>) -> Vec<(String, String)> {
    let shader_path = launch.and_then(LaunchEnv::shader_path);
    let mut env = if game.proton().is_some() || shader_path.is_some() {
        cache::local_cache_env(&shader_path.unwrap_or_else(|| game.shadercache()))
    } else {
        // Native games use the driver's default cache unless they launched with overrides.
        Vec::new()
    };
    for (k, v) in launch.map(|l| &l.vars).into_iter().flatten() {
        if k == "STEAM_COMPAT_SHADER_PATH" {
            continue;
        }
        match env.iter_mut().find(|(key, _)| key == k) {
            Some(slot) => slot.1 = v.clone(),
            None => env.push((k.clone(), v.clone())),
        }
    }
    env
}

pub fn current_fingerprint(game: &Game, gpu: &Gpu, inputs: &[PathBuf]) -> Fingerprint {
    Fingerprint {
        pipeline_cache_uuid: gpu.uuid_hex(),
        inputs: state::hash_inputs(inputs),
        proton: game.proton().map(|p| p.name).unwrap_or_default(),
    }
}

pub fn staleness(game: &Game, gpu: &Gpu) -> Staleness {
    let current = current_fingerprint(game, gpu, &inputs(game));
    Fingerprint::compare(Fingerprint::load(game.appid).as_ref(), &current)
}

pub fn prepare(steam: &Steam, game: &Game, gpu: &Gpu, threads: usize) -> Result<Job> {
    let (replay_bin, _) = fossilize::locate_replay(&steam.root)
        .context("fossilize-replay not found; run scripts/build-fossilize.sh")?;
    let inputs = inputs(game);
    let fingerprint = current_fingerprint(game, gpu, &inputs);
    let staleness = Fingerprint::compare(Fingerprint::load(game.appid).as_ref(), &fingerprint);
    Ok(Job {
        appid: game.appid,
        env: env_for(game, LaunchEnv::load(game.appid).as_ref()),
        inputs,
        threads: threads.max(1),
        fingerprint,
        staleness,
        shadercache: game.shadercache(),
        game_running: !clean::users_of(Path::new("/proc"), game).is_empty(),
        replay_bin,
        merge_bin: fossilize::locate_merge(),
    })
}

impl Job {
    /// Compacts the recordings, replays everything and records the fingerprint on success.
    pub fn run(&self, mut on_progress: impl FnMut(Progress)) -> Result<()> {
        if self.inputs.is_empty() {
            bail!("nothing recorded for app {}", self.appid);
        }
        for (k, v) in &self.env {
            if k.ends_with("_DIR") || k.ends_with("_PATH") {
                fs::create_dir_all(v).with_context(|| format!("creating {v}"))?;
            }
        }
        if let (Some(merge), false) = (&self.merge_bin, self.game_running) {
            record::compact(self.appid, merge)?;
        }
        // Compaction replaced the session files, so fingerprint what is actually replayed.
        let dbs = inputs_from(self.appid, &self.shadercache);
        for db in &dbs {
            self.replay(db, &mut on_progress)?;
        }
        Fingerprint { inputs: state::hash_inputs(&dbs), ..self.fingerprint.clone() }.save(self.appid)
    }

    fn replay(&self, db: &Path, on_progress: &mut impl FnMut(Progress)) -> Result<()> {
        let mut cmd = low_priority_command();
        cmd.arg(&self.replay_bin)
            .args(["--progress", "--num-threads", &self.threads.to_string()])
            .arg(db)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            // The replayer must not record itself into the game's recordings.
            .env_remove(record::ENABLE_ENV)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("starting fossilize-replay")?;
        let stderr = child.stderr.take().expect("stderr is piped");
        let mut errors = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(p) = parse_progress(&line) {
                on_progress(p);
            } else if line.contains("ERROR") {
                errors.push(line);
            }
        }
        let status = child.wait()?;
        if !status.success() {
            bail!("fossilize-replay failed ({status}): {}", errors.join("; "));
        }
        Ok(())
    }
}

/// `nice`/`ionice` so replay never competes with a running game, inside a systemd scope
/// with a memory ceiling when a user session is available (Fossilize issue #312).
fn low_priority_command() -> Command {
    let have = |bin: &str| {
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .any(|d| d.join(bin).is_file())
    };
    let mut prefix: Vec<&str> = Vec::new();
    if have("systemd-run") && std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        prefix.extend(["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryHigh=60%"]);
    }
    if have("ionice") {
        prefix.extend(["ionice", "-c", "3"]);
    }
    prefix.extend(["nice", "-n", "19"]);
    let mut cmd = Command::new(prefix[0]);
    cmd.args(&prefix[1..]);
    cmd
}

/// Fossilize prints `Overall <done> / <total>` in each progress report.
fn parse_progress(line: &str) -> Option<Progress> {
    let rest = line.split("Overall").nth(1)?;
    let (done, total) = rest.split_once('/')?;
    Some(Progress { done: done.trim().parse().ok()?, total: total.trim().parse().ok()? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_lines() {
        assert_eq!(parse_progress("Fossilize INFO:    Overall 2 / 5"), Some(Progress { done: 2, total: 5 }));
        assert_eq!(parse_progress("Fossilize INFO:    Parsed graphics 1 / 1, failed 0"), None);
    }

    #[test]
    fn finds_only_steam_pipeline_dbs() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("fozpipelinesv6/steamapprun_pipeline_cache.049c");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(dir.path().join("mesa_shader_cache_sf")).unwrap();
        for f in ["steam_pipeline_cache.foz", "steam_pipeline_cache_whitelist.foz", "replay_cache.abc.foz"] {
            fs::write(run.join(f), b"x").unwrap();
        }
        fs::write(dir.path().join("fozpipelinesv6/steamapp_pipeline_cache.foz"), b"x").unwrap();
        fs::write(dir.path().join("mesa_shader_cache_sf/steam_pipeline_cache.foz"), b"x").unwrap();

        let mut found = Vec::new();
        collect_steam_dbs(dir.path(), &mut found);
        found.sort();
        assert_eq!(
            found,
            [dir.path().join("fozpipelinesv6/steamapp_pipeline_cache.foz"), run.join("steam_pipeline_cache.foz")]
        );
    }
}
