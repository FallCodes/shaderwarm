//! Everything the GUI shows or does, built on shaderwarm-core. All of it blocks, so the
//! UI calls it through [`blocking`] or a job thread.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

use iced::futures::channel::{mpsc, oneshot};
use shaderwarm_core::cache;
use shaderwarm_core::clean::{self, PurgeOptions};
use shaderwarm_core::engine;
use shaderwarm_core::fossilize;
use shaderwarm_core::gpu;
use shaderwarm_core::launch::LaunchEnv;
use shaderwarm_core::record;
use shaderwarm_core::replay;
use shaderwarm_core::selfcompile::{self, Verdict};
use shaderwarm_core::state::Staleness;
use shaderwarm_core::fsutil::tree_size;
use shaderwarm_core::paths;
use shaderwarm_core::steam::{Artwork, Game, Steam};
use shaderwarm_core::steamfetch::{self, Event as FetchEvent};
use shaderwarm_core::units;

pub const LAUNCH_OPTION: &str = "shaderwarm run -- %command%";

/// Runs `f` on its own thread and resolves when it returns.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("worker thread panicked")
}

#[derive(Debug, Clone)]
pub struct Row {
    pub game: Game,
    pub engine: String,
    pub proton: String,
    pub shaders: u64,
    pub video: u64,
    pub breakdown: cache::CacheBreakdown,
    pub recordings: u64,
    pub art: Artwork,
    pub staleness: Option<Staleness>,
    pub verdict: Verdict,
    pub backups: usize,
    /// Seconds since the game last launched through `shaderwarm run`.
    pub last_launch: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub gpu: Option<String>,
    pub precaching: Option<bool>,
    pub layer_installed: bool,
    pub auto_replay: bool,
    /// An earlier "Fetch from Steam" stopped with Steam open and pre-caching on.
    pub interrupted_fetch: bool,
    /// Bytes of caches and recordings left behind by games that are no longer installed.
    pub orphans: u64,
    pub rows: Vec<Row>,
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

pub fn load() -> Result<Snapshot, String> {
    let steam = Steam::locate().map_err(err)?;
    // With Steam closed an interrupted fetch is put right silently; otherwise it's flagged.
    let _ = steamfetch::recover(&steam);
    let gpus = gpu::enumerate().unwrap_or_default();
    let primary = gpu::primary(&gpus).cloned();
    let db = selfcompile::Database::load().map_err(err)?;
    let rows = steam
        .games()
        .into_iter()
        .map(|game| {
            let engine = engine::detect_cached(game.appid, &game.install_dir);
            let b = cache::breakdown(&game.shadercache());
            let last_launch = std::fs::metadata(LaunchEnv::path(game.appid))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .map(|d| d.as_secs());
            Row {
                verdict: db.assess(&game, &engine),
                engine: engine.to_string(),
                proton: game.proton().map(|p| p.name).unwrap_or_else(|| "native".into()),
                shaders: b.shaders(),
                video: b.video,
                recordings: tree_size(&paths::recordings_dir(game.appid)),
                art: steam.artwork(game.appid),
                breakdown: b,
                staleness: primary.as_ref().map(|g| replay::staleness(&game, g)),
                backups: clean::backups(game.appid).len(),
                last_launch,
                game,
            }
        })
        .collect();
    Ok(Snapshot {
        gpu: primary.map(|g| format!("{} · {}", g.name, g.driver_info)),
        precaching: steam.precaching_enabled(),
        layer_installed: fossilize::layer_manifest().is_some(),
        auto_replay: units::installed(),
        interrupted_fetch: steamfetch::interrupted().is_some(),
        orphans: clean::orphan_targets(&steam).iter().map(|t| t.size).sum(),
        rows,
    })
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    Started { name: String, index: usize, count: usize },
    Progress { done: u64, total: u64 },
    Finished { name: String, result: Result<(), String> },
    AllDone,
}

/// Replays the games one after another on a worker thread, streaming progress.
pub fn spawn_replay(appids: Vec<u32>, threads: usize) -> mpsc::UnboundedReceiver<JobEvent> {
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        let send = |e| {
            let _ = tx.unbounded_send(e);
        };
        let setup = Steam::locate().and_then(|steam| {
            let gpus = gpu::enumerate()?;
            let gpu = gpu::primary(&gpus).cloned().ok_or_else(|| anyhow::anyhow!("no hardware Vulkan device"))?;
            Ok((steam, gpu))
        });
        let (steam, gpu) = match setup {
            Ok(v) => v,
            Err(e) => {
                send(JobEvent::Finished { name: "replay".into(), result: Err(err(e)) });
                send(JobEvent::AllDone);
                return;
            }
        };
        let count = appids.len();
        for (index, appid) in appids.into_iter().enumerate() {
            let Some(game) = steam.game(appid) else { continue };
            send(JobEvent::Started { name: game.name.clone(), index, count });
            let result = replay::prepare(&steam, &game, &gpu, threads)
                .and_then(|job| job.run(|p| send(JobEvent::Progress { done: p.done, total: p.total })))
                .map_err(err);
            send(JobEvent::Finished { name: game.name.clone(), result });
        }
        send(JobEvent::AllDone);
    });
    rx
}

#[derive(Debug, Clone)]
pub enum FetchMsg {
    Event(FetchEvent),
    /// The games Steam updated, or what went wrong, plus a summary line.
    Done(Result<(Vec<u32>, String), String>),
}

/// Runs a "Fetch from Steam" window on a worker thread. Setting `cancel` ends the window
/// early; Steam's settings are restored either way.
pub fn spawn_fetch(delete_video: bool, cancel: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<FetchMsg> {
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        let result = Steam::locate().and_then(|steam| {
            let report = steamfetch::run(&steam, &steamfetch::Options::default(), &cancel, |e| {
                let _ = tx.unbounded_send(FetchMsg::Event(e));
            })?;
            let mut summary = capitalize(report.end.describe());
            if delete_video {
                let targets: Vec<_> = report.updated.iter().filter_map(|&id| steam.game(id)).flat_map(|g| clean::video_targets(&g)).collect();
                let total: u64 = targets.iter().map(|t| t.size).sum();
                clean::remove(&targets, None)?;
                if total > 0 {
                    summary.push_str(&format!("; deleted {} of video", size(total)));
                }
            }
            Ok((report.updated, summary))
        });
        let _ = tx.unbounded_send(FetchMsg::Done(result.map_err(err)));
    });
    rx
}

/// Turns pre-caching back off after an interrupted fetch (restarting Steam if it's open).
pub fn finish_fetch() -> Result<String, String> {
    let steam = Steam::locate().map_err(err)?;
    steamfetch::finish(&steam).map_err(err)?;
    Ok("Steam Shader Pre-Caching is off again".into())
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn with_game<T>(appid: u32, f: impl FnOnce(&Game) -> anyhow::Result<T>) -> Result<T, String> {
    let steam = Steam::locate().map_err(err)?;
    let game = steam.game(appid).ok_or_else(|| format!("app {appid} is not installed"))?;
    f(&game).map_err(err)
}

/// Number of items and bytes a cache purge would remove.
pub fn purge_preview(appid: u32, recordings: bool) -> Result<(usize, u64), String> {
    with_game(appid, |game| {
        let targets = clean::purge_targets(game, PurgeOptions { recordings });
        Ok((targets.len(), targets.iter().map(|t| t.size).sum()))
    })
}

pub fn purge(appid: u32, recordings: bool, backup: bool) -> Result<String, String> {
    with_game(appid, |game| {
        clean::ensure_not_in_use(game)?;
        let targets = clean::purge_targets(game, PurgeOptions { recordings });
        let total: u64 = targets.iter().map(|t| t.size).sum();
        let dir = clean::remove(&targets, backup.then_some(appid))?;
        clean::forget_replay(appid)?;
        Ok(match dir {
            Some(_) => format!("{}: moved {} of caches to a backup", game.name, size(total)),
            None => format!("{}: deleted {} of caches", game.name, size(total)),
        })
    })
}

pub fn restore_latest(appid: u32) -> Result<String, String> {
    with_game(appid, |game| {
        let (stamp, dir) = clean::backups(appid).pop().ok_or_else(|| anyhow::anyhow!("no backups"))?;
        clean::ensure_not_in_use(game)?;
        clean::restore(appid, &dir)?;
        Ok(format!("{}: restored backup {stamp}", game.name))
    })
}

/// Deletes Steam's transcoded video for every installed game and/or the caches of games
/// that are no longer installed.
pub fn free_space(video: bool, orphans: bool) -> Result<String, String> {
    let steam = Steam::locate().map_err(err)?;
    let mut targets = Vec::new();
    if video {
        targets.extend(steam.games().iter().flat_map(clean::video_targets));
    }
    if orphans {
        targets.extend(clean::orphan_targets(&steam));
    }
    let total: u64 = targets.iter().map(|t| t.size).sum();
    clean::remove(&targets, None).map_err(err)?;
    Ok(format!("Freed {}", size(total)))
}

pub fn clean_video(appid: u32) -> Result<String, String> {
    with_game(appid, |game| {
        let targets = clean::video_targets(game);
        let total: u64 = targets.iter().map(|t| t.size).sum();
        clean::remove(&targets, None)?;
        Ok(format!("{}: freed {} of Steam video", game.name, size(total)))
    })
}

/// Turns recording + auto-replay on or off, like `shaderwarm setup [--uninstall]`.
pub fn set_auto_replay(on: bool) -> Result<String, String> {
    if on {
        record::install_layer_manifest().map_err(err)?;
        units::install(&cli_exe()).map_err(err)?;
        Ok("Recording layer and auto-replay enabled".into())
    } else {
        units::uninstall().map_err(err)?;
        Ok("Auto-replay disabled (recording layer kept)".into())
    }
}

/// The units run the CLI; prefer the one on PATH, else the one next to this binary.
fn cli_exe() -> std::path::PathBuf {
    let on_path = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("shaderwarm"))
        .find(|p| p.is_file());
    on_path.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("shaderwarm")))
            .unwrap_or_else(|| Path::new("shaderwarm").to_path_buf())
    })
}

pub fn size(bytes: u64) -> String {
    shaderwarm_core::fsutil::format_bytes(bytes)
}
