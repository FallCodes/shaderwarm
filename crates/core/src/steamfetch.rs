//! "Fetch from Steam": a short window with Steam's Shader Pre-Caching turned on, so Steam
//! downloads Valve's shader caches once while the setting otherwise stays off.
//!
//! Steam only downloads them through its own logged-in client ("Shader Swarm"), so this
//! drives the client: close Steam, turn pre-caching on, start Steam, follow
//! `logs/content_log.txt` until the shader jobs settle, close Steam and turn it off again.
//! A state file records the window so an interrupted fetch can be put right later.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::clean;
use crate::paths;
use crate::steam::{self, Steam};

/// Steam is done once no shader job has changed state for this long.
const QUIET: Duration = Duration::from_secs(90);
/// No shader job at all this long after Steam started means there is nothing to fetch.
const IDLE: Duration = Duration::from_secs(180);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(90);
/// How long Steam must stay gone before the window counts it as closed by the user.
const GONE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_secs(1);

const PROC: &str = "/proc";

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Give up (and still restore Steam's settings) after this long.
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(3 * 3600) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    ClosingSteam,
    StartingSteam,
    Waiting,
    Restoring,
}

impl Phase {
    pub fn describe(self) -> &'static str {
        match self {
            Phase::ClosingSteam => "closing Steam",
            Phase::StartingSteam => "starting Steam with pre-caching on",
            Phase::Waiting => "waiting for Steam's shader downloads",
            Phase::Restoring => "turning pre-caching back off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Phase(Phase),
    /// Games Steam is working on right now, with what it is doing.
    Active(Vec<(u32, &'static str)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Finished,
    NothingToFetch,
    Cancelled,
    TimedOut,
    SteamExited,
}

impl End {
    pub fn describe(self) -> &'static str {
        match self {
            End::Finished => "Steam finished its shader downloads",
            End::NothingToFetch => "Steam had no shader updates to download",
            End::Cancelled => "cancelled",
            End::TimedOut => "timed out waiting for Steam",
            End::SteamExited => "Steam was closed during the fetch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub end: End,
    /// Games Steam downloaded or committed shader data for.
    pub updated: Vec<u32>,
}

/// Runs the whole window. Steam's settings are restored even when the window fails or is
/// cancelled; only a failed restore leaves the state file behind for [`finish`].
pub fn run(steam: &Steam, opts: &Options, cancel: &AtomicBool, mut on_event: impl FnMut(Event)) -> Result<Report> {
    let proc_root = Path::new(PROC);
    match steam.precaching_enabled() {
        Some(false) => {}
        Some(true) => bail!("Steam Shader Pre-Caching is already on, so Steam downloads caches by itself"),
        None => bail!("Steam's pre-caching setting is not in config.vdf; toggle it once in Steam > Settings > Downloads"),
    }
    let games = clean::running_games(proc_root);
    if !games.is_empty() {
        bail!("a game is running (app {games:?}); fetching restarts Steam");
    }
    let was_running = steam::client_running(proc_root);
    let pending = Pending { pid: std::process::id(), started: unix_now(), steam_was_running: was_running };
    claim(&pending_path(), &pending)?;

    let result = window(steam, opts, cancel, &mut on_event, was_running);
    on_event(Event::Phase(Phase::Restoring));
    restore(steam, was_running).context("turning Steam Shader Pre-Caching back off (retry with `shaderwarm fetch --finish`)")?;
    remove_pending()?;
    result
}

fn window(steam: &Steam, opts: &Options, cancel: &AtomicBool, on_event: &mut impl FnMut(Event), was_running: bool) -> Result<Report> {
    let proc_root = Path::new(PROC);
    if was_running {
        on_event(Event::Phase(Phase::ClosingSteam));
        shutdown(steam)?;
    }
    steam.set_precaching(true)?;
    let mut log = LogTail::open(steam.root.join("logs/content_log.txt"));
    on_event(Event::Phase(Phase::StartingSteam));
    launch(steam)?;
    on_event(Event::Phase(Phase::Waiting));

    let start = Instant::now();
    let mut watch = Watch::new(start);
    let mut steam_seen = false;
    let mut gone_since: Option<Instant> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(watch.report(End::Cancelled));
        }
        let now = Instant::now();
        let mut changed = false;
        for line in log.read_new() {
            changed |= watch.feed(&line, now);
        }
        if changed {
            on_event(Event::Active(watch.active()));
        }
        if let Some(end) = watch.end(now) {
            return Ok(watch.report(end));
        }
        if now.duration_since(start) >= opts.timeout {
            return Ok(watch.report(End::TimedOut));
        }
        // The bootstrap takes a moment to appear, and a self-update restarts the client, so
        // Steam only counts as closed once it has been gone for a while.
        if steam::client_running(proc_root) {
            steam_seen = true;
            gone_since = None;
        } else {
            let since = *gone_since.get_or_insert(now);
            if (steam_seen && now.duration_since(since) >= GONE) || now.duration_since(start) >= SHUTDOWN_WAIT {
                return Ok(watch.report(End::SteamExited));
            }
        }
        thread::sleep(POLL);
    }
}

/// Closes Steam if it's open, turns pre-caching off (Steam rewrites config.vdf on exit,
/// so only after that) and starts Steam again when asked.
fn restore(steam: &Steam, relaunch: bool) -> Result<()> {
    if steam::client_running(Path::new(PROC)) {
        shutdown(steam)?;
    }
    steam.set_precaching(false)?;
    if relaunch {
        launch(steam)?;
    }
    Ok(())
}

fn shutdown(steam: &Steam) -> Result<()> {
    let launcher = steam.launcher();
    Command::new(&launcher[0])
        .args(&launcher[1..])
        .arg("-shutdown")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("running {} -shutdown", launcher.join(" ")))?;
    let deadline = Instant::now() + SHUTDOWN_WAIT;
    while steam::client_running(Path::new(PROC)) {
        if Instant::now() >= deadline {
            bail!("Steam did not close within {} s", SHUTDOWN_WAIT.as_secs());
        }
        thread::sleep(POLL);
    }
    Ok(())
}

/// Starts Steam minimised in its own session, so it outlives shaderwarm and the terminal.
fn launch(steam: &Steam) -> Result<()> {
    let launcher = steam.launcher();
    Command::new("setsid")
        .arg("-f")
        .args(&launcher)
        .arg("-silent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("starting {}", launcher.join(" ")))?;
    Ok(())
}

// ---- following Steam's content log -------------------------------------------------

/// Reads lines appended to a log, starting over when Steam rotates or truncates it.
struct LogTail {
    path: PathBuf,
    ino: u64,
    pos: u64,
    partial: String,
}

impl LogTail {
    fn open(path: PathBuf) -> Self {
        let (ino, pos) = fs::metadata(&path).map_or((0, 0), |m| (m.ino(), m.len()));
        Self { path, ino, pos, partial: String::new() }
    }

    fn read_new(&mut self) -> Vec<String> {
        let Ok(meta) = fs::metadata(&self.path) else { return Vec::new() };
        if meta.ino() != self.ino || meta.len() < self.pos {
            self.ino = meta.ino();
            self.pos = 0;
            self.partial.clear();
        }
        if meta.len() == self.pos {
            return Vec::new();
        }
        let mut buf = Vec::new();
        let read = File::open(&self.path).and_then(|mut f| {
            f.seek(SeekFrom::Start(self.pos))?;
            f.read_to_end(&mut buf)
        });
        if read.is_err() {
            return Vec::new();
        }
        self.pos += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = self.partial.split('\n').map(str::to_string).collect();
        self.partial = lines.pop().unwrap_or_default();
        lines
    }
}

/// `[date] AppID 1384160 Shader update changed : Running Update,Downloading,Staging,`
fn parse_line(line: &str) -> Option<(u32, &str)> {
    let rest = &line[line.find("AppID ")? + 6..];
    let (id, rest) = rest.split_once(' ')?;
    let state = rest.strip_prefix("Shader update changed : ")?;
    Some((id.parse().ok()?, state.trim().trim_end_matches(',')))
}

fn label(state: &str) -> &'static str {
    if state.contains("Committing") {
        "committing"
    } else if state.contains("Downloading") {
        "downloading"
    } else {
        "checking"
    }
}

/// Per-game shader job states, and when Steam last changed any of them.
struct Watch {
    started: Instant,
    states: BTreeMap<u32, String>,
    updated: BTreeSet<u32>,
    last_change: Option<Instant>,
}

impl Watch {
    fn new(started: Instant) -> Self {
        Self { started, states: BTreeMap::new(), updated: BTreeSet::new(), last_change: None }
    }

    /// Returns whether the line was a shader job update.
    fn feed(&mut self, line: &str, now: Instant) -> bool {
        let Some((appid, state)) = parse_line(line) else { return false };
        self.last_change = Some(now);
        if state.contains("Downloading") || state.contains("Committing") {
            self.updated.insert(appid);
        }
        if state == "None" {
            self.states.remove(&appid);
        } else {
            self.states.insert(appid, state.to_string());
        }
        true
    }

    fn active(&self) -> Vec<(u32, &'static str)> {
        self.states.iter().map(|(id, s)| (*id, label(s))).collect()
    }

    fn end(&self, now: Instant) -> Option<End> {
        if !self.states.is_empty() {
            return None;
        }
        match self.last_change {
            Some(t) => (now.duration_since(t) >= QUIET).then_some(End::Finished),
            None => (now.duration_since(self.started) >= IDLE).then_some(End::NothingToFetch),
        }
    }

    fn report(&self, end: End) -> Report {
        Report { end, updated: self.updated.iter().copied().collect() }
    }
}

// ---- the state file -----------------------------------------------------------------

/// A fetch in progress, or one that was interrupted with pre-caching possibly left on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub pid: u32,
    /// Unix time the fetch started.
    pub started: u64,
    pub steam_was_running: bool,
}

impl Pending {
    fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let mut p = Pending { pid: 0, started: 0, steam_was_running: false };
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            match k {
                "pid" => p.pid = v.parse().ok()?,
                "started" => p.started = v.parse().ok()?,
                "steam_was_running" => p.steam_was_running = v == "1",
                _ => {}
            }
        }
        Some(p)
    }

    fn to_text(&self) -> String {
        format!("pid={}\nstarted={}\nsteam_was_running={}\n", self.pid, self.started, u8::from(self.steam_was_running))
    }

    /// Whether the shaderwarm process that started the fetch is still running.
    fn owner_alive(&self, proc_root: &Path) -> bool {
        fs::read_to_string(proc_root.join(self.pid.to_string()).join("comm")).is_ok_and(|c| c.starts_with("shaderwarm"))
    }
}

fn pending_path() -> PathBuf {
    paths::state_dir().join("steam-fetch.state")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn claim(path: &Path, pending: &Pending) -> Result<()> {
    if let Some(p) = Pending::load_from(path) {
        if p.owner_alive(Path::new(PROC)) {
            bail!("a Steam fetch is already running (pid {})", p.pid);
        }
        bail!("an earlier Steam fetch did not finish; run `shaderwarm fetch --finish` first");
    }
    fs::create_dir_all(path.parent().expect("state path has a parent"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(pending.to_text().as_bytes())?;
    Ok(())
}

fn remove_pending() -> Result<()> {
    match fs::remove_file(pending_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// A fetch whose shaderwarm process is gone: Steam may still have pre-caching on.
pub fn interrupted() -> Option<Pending> {
    Pending::load_from(&pending_path()).filter(|p| !p.owner_alive(Path::new(PROC)))
}

/// Puts an interrupted fetch right when that needs nothing from the user: Steam is
/// closed, so the setting can simply be turned back off. Returns whether it did.
pub fn recover(steam: &Steam) -> Result<bool> {
    if interrupted().is_none() || steam::client_running(Path::new(PROC)) {
        return Ok(false);
    }
    steam.set_precaching(false)?;
    remove_pending()?;
    Ok(true)
}

/// Finishes an interrupted fetch: closes Steam if it is open, turns pre-caching off and
/// starts Steam again if it was open.
pub fn finish(steam: &Steam) -> Result<()> {
    if interrupted().is_none() {
        bail!("no interrupted Steam fetch to finish");
    }
    restore(steam, steam::client_running(Path::new(PROC)))?;
    remove_pending()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &[&str] = &[
        "[2026-10-23 22:23:20] AppID 1422450 Shader update changed : Running Update,",
        "[2026-10-23 22:23:20] AppID 1422450 Shader update changed : Running Update,Reconfiguring,",
        "[2026-10-23 22:23:21] AppID 1422450 Shader update changed : Running Update,Downloading,Staging,",
        "[2026-10-23 22:23:28] Detected write gap 131 MB in file \"mesa_shader_cache_sf\\x\\RADV\\steam_cache.foz\"",
        "[2026-10-23 22:24:41] AppID 1422450 Shader update changed : Running Update,Committing,",
        "[2026-10-23 22:24:41] AppID 1422450 Shader update changed : None",
    ];

    #[test]
    fn parses_shader_job_lines() {
        assert_eq!(parse_line(LOG[2]), Some((1422450, "Running Update,Downloading,Staging")));
        assert_eq!(parse_line(LOG[5]), Some((1422450, "None")));
        assert_eq!(parse_line(LOG[3]), None);
        assert_eq!(parse_line("[x] AppID 7 update started : download 0/1"), None);
    }

    #[test]
    fn watch_settles_after_quiet_period() {
        let t0 = Instant::now();
        let mut w = Watch::new(t0);
        assert_eq!(w.end(t0 + IDLE), Some(End::NothingToFetch));

        let t1 = t0 + Duration::from_secs(5);
        for line in &LOG[..3] {
            w.feed(line, t1);
        }
        assert_eq!(w.active(), [(1422450, "downloading")]);
        assert_eq!(w.end(t1 + QUIET * 2), None, "busy jobs never end the window");
        assert!(!w.feed(LOG[3], t1));
        for line in &LOG[4..] {
            w.feed(line, t1);
        }
        assert!(w.active().is_empty());
        assert_eq!(w.end(t1 + QUIET / 2), None);
        assert_eq!(w.end(t1 + QUIET), Some(End::Finished));
        assert_eq!(w.report(End::Finished).updated, [1422450]);
    }

    #[test]
    fn checks_without_downloads_update_nothing() {
        let t0 = Instant::now();
        let mut w = Watch::new(t0);
        w.feed("AppID 9 Shader update changed : Running Update,Reconfiguring,", t0);
        w.feed("AppID 9 Shader update changed : None", t0);
        assert_eq!(w.report(End::Finished).updated, Vec::<u32>::new());
    }

    #[test]
    fn log_tail_follows_appends_and_rotation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("content_log.txt");
        fs::write(&path, "old line\n").unwrap();
        let mut tail = LogTail::open(path.clone());
        assert!(tail.read_new().is_empty(), "starts at the end");

        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"one\ntw").unwrap();
        assert_eq!(tail.read_new(), ["one"]);
        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"o\n").unwrap();
        assert_eq!(tail.read_new(), ["two"]);

        // Steam moves the log aside and starts a new one.
        fs::rename(&path, tmp.path().join("content_log.previous.txt")).unwrap();
        fs::write(&path, "fresh\n").unwrap();
        assert_eq!(tail.read_new(), ["fresh"]);
    }

    #[test]
    fn claim_refuses_a_second_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state/steam-fetch.state");
        let p = Pending { pid: std::process::id(), started: 5, steam_was_running: true };
        claim(&path, &p).unwrap();
        assert_eq!(Pending::load_from(&path), Some(p.clone()));
        assert!(claim(&path, &p).is_err());
    }
}
