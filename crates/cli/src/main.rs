use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use shaderwarm_core::cache;
use shaderwarm_core::clean::{self, PurgeOptions, Target};
use shaderwarm_core::engine;
use shaderwarm_core::fossilize;
use shaderwarm_core::fsutil::{format_bytes, tree_size};
use shaderwarm_core::gpu::{self, Gpu};
use shaderwarm_core::launch::{self, LaunchEnv};
use shaderwarm_core::paths;
use shaderwarm_core::record;
use shaderwarm_core::replay;
use shaderwarm_core::selfcompile::{self, Verdict};
use shaderwarm_core::steam::{Game, Steam};
use shaderwarm_core::steamfetch;

#[derive(Parser)]
#[command(name = "shaderwarm", version, about = "Pre-compile shaders for Proton games on Linux")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List installed Steam games with their cache sizes and replay status.
    List,
    /// Show the caches, recordings and Proton build of one game.
    Status { appid: u32 },
    /// Check the GPU, driver, Fossilize tools and Steam settings.
    Doctor,
    /// Install the recording layer and the auto-replay units; print the launch option.
    Setup {
        /// Only install the recording layer, without the systemd user units.
        #[arg(long)]
        no_units: bool,
        /// Remove the recording layer and the systemd user units.
        #[arg(long, conflicts_with = "no_units")]
        uninstall: bool,
        /// Only write the unit files into DIR (nothing is installed or enabled).
        #[arg(long, value_name = "DIR", conflicts_with_all = ["no_units", "uninstall"])]
        write_units: Option<PathBuf>,
    },
    /// Steam launch wrapper: `shaderwarm run -- %command%`.
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Compile recorded pipelines into the driver cache.
    Replay(ReplayArgs),
    /// Remove video transcodes, orphaned caches or one game's shader cache.
    Clean(CleanArgs),
    /// Show a game's Unreal IoStore containers and shader files (diagnostics).
    #[command(hide = true)]
    Inspect {
        appid: u32,
        /// Write chunk <INDEX> of the container whose file name contains <CONTAINER> to <OUT>.
        #[arg(long, num_args = 3, value_names = ["CONTAINER", "INDEX", "OUT"])]
        dump: Option<Vec<String>>,
    },
    /// Let Steam download its shader caches once, keeping Shader Pre-Caching off otherwise.
    ///
    /// Closes Steam, turns pre-caching on, starts Steam minimised until its shader downloads
    /// settle, then closes it, turns pre-caching off and starts it again if it was open.
    /// Steam fetches for every installed game while the window is open.
    Fetch(FetchArgs),
    /// Put a backup made by `clean --cache --backup` back in place.
    Restore {
        appid: u32,
        /// Backup timestamp (default: the newest backup).
        timestamp: Option<u64>,
    },
}

#[derive(Args)]
#[command(group = clap::ArgGroup::new("what").required(true).multiple(true).args(["video", "orphans", "cache"]))]
struct CleanArgs {
    /// Game to clean; `--video` without it cleans every game.
    appid: Option<u32>,
    /// Steam's transcoded video (not needed with Proton-CachyOS/GE).
    #[arg(long)]
    video: bool,
    /// Caches and recordings of games that are no longer installed.
    #[arg(long)]
    orphans: bool,
    /// The game's compiled shader caches (Mesa, NVIDIA, DXVK, vkd3d-proton).
    #[arg(long, requires = "appid")]
    cache: bool,
    /// With --cache: also delete shaderwarm's recordings of the game.
    #[arg(long, requires = "cache")]
    recordings: bool,
    /// With --cache: move the caches into a backup instead of deleting them.
    #[arg(long, requires = "cache")]
    backup: bool,
    /// Don't ask for confirmation.
    #[arg(long, short)]
    yes: bool,
}

#[derive(Args)]
struct FetchArgs {
    /// Keep the transcoded video Steam downloads along with the shaders.
    #[arg(long)]
    keep_video: bool,
    /// Don't replay the games Steam updated afterwards.
    #[arg(long)]
    no_replay: bool,
    /// Minutes to wait for Steam before giving up (settings are restored either way).
    #[arg(long, default_value_t = 180)]
    timeout: u64,
    /// Finish a fetch that was interrupted: turn pre-caching back off.
    #[arg(long, conflicts_with_all = ["keep_video", "no_replay"])]
    finish: bool,
    /// Don't ask for confirmation.
    #[arg(long, short)]
    yes: bool,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct ReplayTarget {
    appid: Option<u32>,
    /// Every game whose cache is stale (driver, Proton or recordings changed).
    #[arg(long)]
    stale: bool,
    /// Every game with recordings, stale or not.
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct ReplayArgs {
    #[command(flatten)]
    target: ReplayTarget,
    /// Compile threads (default: all cores; replay runs at idle priority).
    #[arg(long)]
    threads: Option<usize>,
    /// With --stale/--all: also replay games that compile their own shaders.
    #[arg(long)]
    include_self_compiling: bool,
    /// Do nothing while any Steam game is running (used by the background service).
    #[arg(long)]
    defer_if_gaming: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // `run` sits in the game's launch path: it must never need Steam discovery to succeed.
    if let Command::Run { command } = cli.command {
        return run(command);
    }
    let steam = Steam::locate()?;
    match steamfetch::recover(&steam) {
        Ok(true) => eprintln!("shaderwarm: turned Steam Shader Pre-Caching back off after an interrupted fetch"),
        Ok(false) => {}
        Err(e) => eprintln!("shaderwarm: could not finish an interrupted Steam fetch: {e:#}"),
    }
    match cli.command {
        Command::Run { .. } => unreachable!(),
        Command::Setup { write_units: Some(dir), .. } => {
            for unit in shaderwarm_core::units::write(&dir, &installed_exe()?)? {
                println!("Wrote {}", unit.display());
            }
            Ok(())
        }
        Command::Setup { no_units, uninstall, write_units: None } => setup(no_units, uninstall),
        Command::List => list(&steam),
        Command::Status { appid } => status(&steam, appid),
        Command::Doctor => doctor(&steam),
        Command::Replay(args) => replay_cmd(&steam, args),
        Command::Clean(args) => clean_cmd(&steam, args),
        Command::Inspect { appid, dump } => inspect(&steam, appid, dump),
        Command::Restore { appid, timestamp } => restore_cmd(&steam, appid, timestamp),
        Command::Fetch(args) => fetch_cmd(&steam, args),
    }
}

fn primary_gpu() -> Result<Gpu> {
    let gpus = gpu::enumerate()?;
    gpu::primary(&gpus).cloned().context("no hardware Vulkan device found")
}

fn find_game(steam: &Steam, appid: u32) -> Result<Game> {
    steam.game(appid).with_context(|| format!("app {appid} is not an installed game"))
}

fn verdict(db: &selfcompile::Database, game: &Game) -> Verdict {
    db.assess(game, &engine::detect_cached(game.appid, &game.install_dir))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn list(steam: &Steam) -> Result<()> {
    let gpu = primary_gpu().ok();
    let db = selfcompile::Database::load()?;
    println!(
        "{:>8}  {:<32}  {:<26}  {:>10}  {:>10}  STATUS",
        "APPID", "NAME", "PROTON", "SHADERS", "VIDEO"
    );
    for game in steam.games() {
        let b = cache::breakdown(&game.shadercache());
        let proton = game.proton().map(|p| p.name).unwrap_or_else(|| "-".into());
        let status = if verdict(&db, &game).self_compiles {
            "self-compiles: shaderwarm probably unnecessary"
        } else {
            gpu.as_ref().map_or("unknown GPU", |g| replay::staleness(&game, g).describe())
        };
        println!(
            "{:>8}  {:<32}  {:<26}  {:>10}  {:>10}  {status}",
            game.appid,
            truncate(&game.name, 32),
            truncate(&proton, 26),
            format_bytes(b.shaders()),
            format_bytes(b.video),
        );
    }
    Ok(())
}

fn status(steam: &Steam, appid: u32) -> Result<()> {
    let game = find_game(steam, appid)?;
    println!("{} ({})", game.name, game.appid);
    println!("  install      {}", game.install_dir.display());
    let engine = engine::detect_cached(appid, &game.install_dir);
    println!("  engine       {engine}");
    let v = selfcompile::Database::load()?.assess(&game, &engine);
    match (v.self_compiles, v.ue5_exempt) {
        (true, _) => println!("  self-compile yes: {}. shaderwarm is probably unnecessary", v.reason()),
        (false, true) => println!("  self-compile no ({})", v.reason()),
        (false, false) => println!("  self-compile no"),
    }
    match game.proton() {
        Some(p) => println!(
            "  proton       {} ({})",
            p.name,
            p.root.map(|r| r.display().to_string()).unwrap_or_else(|| "install not found".into())
        ),
        None => println!("  proton       none recorded (native game or never launched)"),
    }

    let shadercache = game.shadercache();
    let b = cache::breakdown(&shadercache);
    println!("  shadercache  {}", shadercache.display());
    for (label, size) in [
        ("mesa", b.mesa),
        ("nvidia", b.nvidia),
        ("amdvlk", b.amdvlk),
        ("dxvk", b.dxvk),
        ("vkd3d", b.vkd3d),
        ("fossilize", b.fossilize),
        ("video", b.video),
        ("other", b.other),
    ] {
        if size > 0 {
            println!("    {label:<10} {:>10}", format_bytes(size));
        }
    }

    let recordings = paths::recordings_dir(appid);
    if recordings.is_dir() {
        println!("  recordings   {} ({})", recordings.display(), format_bytes(tree_size(&recordings)));
    } else {
        println!("  recordings   none");
    }
    // `run` rewrites launch.env on every launch, so its age shows whether the wrapper ran.
    let last_launch = std::fs::metadata(LaunchEnv::path(appid))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());
    match last_launch {
        Some(age) => println!("  last launch  {} ago through `shaderwarm run`", format_age(age.as_secs())),
        None => println!("  last launch  never through `shaderwarm run` (check the Steam launch option)"),
    }
    let inputs = replay::inputs(&game);
    println!("  replay input {} database(s)", inputs.len());
    for db in &inputs {
        println!("    {} ({})", db.display(), format_bytes(tree_size(db)));
    }
    if let Ok(gpu) = primary_gpu() {
        println!("  status       {}", replay::staleness(&game, &gpu).describe());
    }
    Ok(())
}

fn doctor(steam: &Steam) -> Result<()> {
    println!("Steam");
    println!("  root         {}", steam.root.display());
    for lib in &steam.libraries {
        println!("  library      {}", lib.display());
    }
    match steam.precaching_enabled() {
        Some(false) => println!("  pre-caching  off (good: no shader or video downloads)"),
        Some(true) => println!("  pre-caching  ON: Steam downloads shaders and transcoded video; turn it off in Settings > Downloads"),
        None => println!("  pre-caching  unknown (setting not found in config.vdf)"),
    }
    if steamfetch::interrupted().is_some() {
        println!("  fetch        an interrupted Steam fetch left pre-caching on: run `shaderwarm fetch --finish`");
    }

    println!("GPU");
    match gpu::enumerate() {
        Ok(gpus) => {
            let primary = gpu::primary(&gpus).map(|g| g.pipeline_cache_uuid);
            for g in &gpus {
                let mark = if Some(g.pipeline_cache_uuid) == primary { "*" } else { " " };
                println!("  {mark} {} [{}, {:?}]", g.name, g.vendor, g.device_type);
                println!("      driver     {} ({})", g.driver_name, g.driver_info);
                println!("      cache uuid {}", g.uuid_hex());
            }
            if primary.is_none() {
                println!("  no hardware Vulkan device found");
            }
        }
        Err(e) => println!("  Vulkan unavailable: {e:#}"),
    }

    println!("Fossilize");
    match fossilize::locate_replay(&steam.root) {
        Some((path, source)) => println!("  replay       {} ({source:?})", path.display()),
        None => println!("  replay       not found (run scripts/build-fossilize.sh)"),
    }
    match fossilize::layer_manifest() {
        Some(path) => println!("  layer        {}", path.display()),
        None => println!("  layer        not installed (run `shaderwarm setup`)"),
    }
    if shaderwarm_core::units::installed() {
        println!("  auto-replay  enabled (systemctl --user status shaderwarm-replay.path)");
    } else {
        println!("  auto-replay  off (run `shaderwarm setup`)");
    }
    println!("  recordings   {}", paths::data_dir().join("recordings").display());
    Ok(())
}

/// The command the units should run: the `shaderwarm` on PATH when there is one, so a
/// rebuilt or reinstalled binary keeps working, else this executable.
fn installed_exe() -> Result<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("shaderwarm"))
        .find(|p| p.is_file());
    match on_path {
        Some(p) => Ok(p),
        None => std::env::current_exe().context("locating the shaderwarm executable"),
    }
}

fn setup(no_units: bool, uninstall: bool) -> Result<()> {
    use shaderwarm_core::units;

    if uninstall {
        units::uninstall()?;
        if let Some(manifest) = fossilize::layer_manifest() {
            std::fs::remove_file(&manifest).with_context(|| format!("removing {}", manifest.display()))?;
        }
        println!("Removed the recording layer and the auto-replay units.");
        println!("Recordings and caches are kept; remove `shaderwarm run --` from your launch options.");
        return Ok(());
    }

    let manifest = record::install_layer_manifest()?;
    println!("Installed recording layer: {}", manifest.display());
    if !no_units {
        let exe = installed_exe()?;
        for unit in units::install(&exe)? {
            println!("Installed {}", unit.display());
        }
        println!("Auto-replay enabled: after GPU driver or Proton updates, and hourly, stale games are");
        println!("replayed at idle priority (never while a game is running).");
    }
    println!();
    println!("Set this as the Steam launch option of each game you want to record:");
    println!("    shaderwarm run -- %command%");
    Ok(())
}

/// Adds the cache and recording environment, saves it for replay and execs the game.
/// Any problem only disables recording; the game always starts.
fn run(command: Vec<String>) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let (program, args) = command.split_first().context("no command given")?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    let mut effective: BTreeMap<String, String> = std::env::vars().collect();
    let mut set_default = |cmd: &mut std::process::Command, k: String, v: String| {
        if let std::collections::btree_map::Entry::Vacant(slot) = effective.entry(k) {
            cmd.env(slot.key(), &v);
            slot.insert(v);
        }
    };

    // Same per-game cache as Proton's PROTON_LOCAL_SHADER_CACHE, set here so it also holds
    // on Proton builds without that option. Proton keeps values that are already set.
    set_default(&mut cmd, "PROTON_LOCAL_SHADER_CACHE".into(), "1".into());
    if let Some(shader_path) = std::env::var_os("STEAM_COMPAT_SHADER_PATH").map(PathBuf::from) {
        for dir in cache::local_cache_dirs(&shader_path) {
            let _ = std::fs::create_dir_all(dir);
        }
        for (k, v) in cache::local_cache_env(&shader_path) {
            set_default(&mut cmd, k, v);
        }
    }

    let appid = record::detect_appid(|k| std::env::var(k).ok(), command.iter().map(String::as_str));
    match (appid, fossilize::layer_manifest()) {
        (Some(appid), Some(_)) => match compact_then_record_env(appid) {
            Ok(env) => {
                cmd.envs(env);
            }
            Err(e) => eprintln!("shaderwarm: recording disabled: {e:#}"),
        },
        (None, _) => eprintln!("shaderwarm: no Steam app id found; recording disabled"),
        (_, None) => eprintln!("shaderwarm: recording layer not installed (run `shaderwarm setup`)"),
    }
    if let Some(appid) = appid {
        let exe = launch::exe_name(command.iter().map(String::as_str));
        if let Err(e) = LaunchEnv::capture(effective, exe.as_deref()).save(appid) {
            eprintln!("shaderwarm: could not save launch environment: {e:#}");
        }
    }
    Err(cmd.exec()).with_context(|| format!("starting {program}"))
}

/// Merges earlier sessions first (well under a second) so this session records only
/// pipelines that are new.
fn compact_then_record_env(appid: u32) -> Result<Vec<(String, String)>> {
    if let Some(merge) = fossilize::locate_merge() {
        if let Err(e) = record::compact(appid, &merge) {
            eprintln!("shaderwarm: could not merge earlier recordings: {e:#}");
        }
    }
    record::record_env(appid, |k| std::env::var(k).ok())
}

fn replay_cmd(steam: &Steam, args: ReplayArgs) -> Result<()> {
    if args.defer_if_gaming {
        let running = clean::running_games(std::path::Path::new("/proc"));
        if !running.is_empty() {
            println!("A game is running ({running:?}); deferring replay.");
            return Ok(());
        }
    }
    let gpu = primary_gpu()?;
    let settings = shaderwarm_core::settings::Settings::load();
    let threads = args.threads.unwrap_or_else(|| settings.effective_threads());
    let include_self_compiling = args.include_self_compiling || settings.include_self_compiling;
    let db = selfcompile::Database::load()?;
    let games: Vec<Game> = match args.target.appid {
        Some(appid) => {
            let game = find_game(steam, appid)?;
            let v = verdict(&db, &game);
            if v.self_compiles {
                println!("Note: {} compiles its own shaders ({}); replaying anyway.", game.name, v.reason());
            }
            vec![game]
        }
        None => steam
            .games()
            .into_iter()
            .filter(|g| {
                let s = replay::staleness(g, &gpu);
                let wanted = if args.target.stale { s.needs_replay() } else { !replay::inputs(g).is_empty() };
                wanted && (include_self_compiling || !verdict(&db, g).self_compiles)
            })
            .collect(),
    };
    if games.is_empty() {
        println!("Nothing to replay.");
        return Ok(());
    }

    let mut failed = 0;
    for game in &games {
        let job = replay::prepare(steam, game, &gpu, threads)?;
        println!("{} ({}): {}, {} database(s)", game.name, game.appid, job.staleness.describe(), job.inputs.len());
        let result = job.run(|p| {
            eprint!("\r  compiling {}/{}   ", p.done, p.total);
            let _ = std::io::stderr().flush();
        });
        eprintln!();
        match result {
            Ok(()) => println!("  done"),
            Err(e) => {
                failed += 1;
                println!("  failed: {e:#}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} replay(s) failed", games.len());
    }
    Ok(())
}

fn fetch_cmd(steam: &Steam, args: FetchArgs) -> Result<()> {
    if args.finish {
        steamfetch::finish(steam)?;
        println!("Steam Shader Pre-Caching is off again.");
        return Ok(());
    }
    println!("Steam will be closed, started minimised with Shader Pre-Caching on until its shader");
    println!("downloads finish, then closed and started again if it was open. Steam downloads for");
    println!("every installed game, video included; pipelines are recorded with Valve's Proton builds.");
    if !args.yes && !confirm("Fetch shader caches from Steam now?")? {
        println!("Cancelled.");
        return Ok(());
    }
    let names: BTreeMap<u32, String> = steam.games().into_iter().map(|g| (g.appid, g.name)).collect();
    let name = |id: u32| names.get(&id).cloned().unwrap_or_else(|| format!("app {id}"));
    let opts = steamfetch::Options { timeout: std::time::Duration::from_secs(args.timeout * 60) };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let report = steamfetch::run(steam, &opts, &cancel, |event| match event {
        steamfetch::Event::Phase(p) => println!("{}…", capitalize(p.describe())),
        steamfetch::Event::Active(apps) if apps.is_empty() => println!("  idle"),
        steamfetch::Event::Active(apps) => {
            let list: Vec<String> = apps.iter().map(|(id, what)| format!("{} ({what})", name(*id))).collect();
            println!("  {}", list.join(", "));
        }
    })?;
    println!("{}.", capitalize(report.end.describe()));
    if report.updated.is_empty() {
        return Ok(());
    }
    let games: Vec<Game> = report.updated.iter().filter_map(|&id| steam.game(id)).collect();
    println!("Updated: {}", games.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", "));
    if !args.keep_video {
        let targets: Vec<Target> = games.iter().flat_map(clean::video_targets).collect();
        let total: u64 = targets.iter().map(|t| t.size).sum();
        if total > 0 {
            clean::remove(&targets, None)?;
            println!("Deleted {} of Steam video.", format_bytes(total));
        }
    }
    if args.no_replay {
        return Ok(());
    }
    let replay = ReplayArgs {
        target: ReplayTarget { appid: None, stale: true, all: false },
        threads: None,
        include_self_compiling: false,
        defer_if_gaming: false,
    };
    replay_cmd(steam, replay)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn clean_cmd(steam: &Steam, args: CleanArgs) -> Result<()> {
    let game = args.appid.map(|id| find_game(steam, id)).transpose()?;
    let mut targets: Vec<Target> = Vec::new();
    if args.video {
        match &game {
            Some(g) => targets.extend(clean::video_targets(g)),
            None => steam.games().iter().for_each(|g| targets.extend(clean::video_targets(g))),
        }
    }
    if args.orphans {
        targets.extend(clean::orphan_targets(steam));
    }
    if args.cache {
        let game = game.as_ref().expect("clap requires appid with --cache");
        clean::ensure_not_in_use(game)?;
        targets.extend(clean::purge_targets(game, PurgeOptions { recordings: args.recordings }));
    }
    if targets.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }

    for t in &targets {
        println!("  {:>10}  {}", format_bytes(t.size), t.path.display());
    }
    let total: u64 = targets.iter().map(|t| t.size).sum();
    let verb = if args.backup { "Move to backup" } else { "Delete" };
    if !args.yes && !confirm(&format!("{verb} {} ({})?", targets.len(), format_bytes(total)))? {
        println!("Cancelled.");
        return Ok(());
    }

    let backup_for = if args.backup { args.appid } else { None };
    let backup = clean::remove(&targets, backup_for)?;
    if args.cache {
        clean::forget_replay(args.appid.expect("clap requires appid with --cache"))?;
    }
    match backup {
        Some(dir) => println!("Backed up {} to {}", format_bytes(total), dir.display()),
        None => println!("Freed {}", format_bytes(total)),
    }
    Ok(())
}

fn restore_cmd(steam: &Steam, appid: u32, timestamp: Option<u64>) -> Result<()> {
    let game = find_game(steam, appid)?;
    let backups = clean::backups(appid);
    if backups.is_empty() {
        bail!("no backups for {}", game.name);
    }
    let (stamp, dir) = match timestamp {
        Some(t) => backups.iter().find(|(s, _)| *s == t).with_context(|| {
            let known: Vec<String> = backups.iter().map(|(s, _)| s.to_string()).collect();
            format!("no backup {t}; available: {}", known.join(", "))
        })?,
        None => backups.last().expect("checked non-empty"),
    };
    clean::ensure_not_in_use(&game)?;
    clean::restore(appid, dir)?;
    println!("Restored backup {stamp} of {}", game.name);
    Ok(())
}

fn format_age(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s if s < 172_800 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn find_files(dir: &std::path::Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

fn inspect(steam: &Steam, appid: u32, dump: Option<Vec<String>>) -> Result<()> {
    use shaderwarm_core::ue::iostore::{Toc, UcasFiles};

    let game = find_game(steam, appid)?;
    let mut tocs = Vec::new();
    find_files(&game.install_dir, "utoc", &mut tocs);
    tocs.sort();
    for path in tocs {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let toc = match Toc::open(&path) {
            Ok(t) => t,
            Err(e) => {
                println!("{name}: {e:#}");
                continue;
            }
        };
        let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
        for c in &toc.chunks {
            *kinds.entry(c.id.kind()).or_default() += 1;
        }
        if let Some([container, index, out]) = dump.as_deref() {
            if name.contains(container.as_str()) {
                let data = toc.read_chunk(index.parse()?, &mut UcasFiles::default())?;
                std::fs::write(out, &data)?;
                println!("wrote {} bytes of chunk {index} of {name} to {out}", data.len());
            }
            continue;
        }
        println!("{name}: v{} {} chunks, {} files, types {kinds:?}", toc.version, toc.chunks.len(), toc.files.len());
        for (file, chunk) in &toc.files {
            let lower = file.to_ascii_lowercase();
            if lower.ends_with(".ushaderbytecode") || lower.ends_with(".upipelinecache") {
                let c = toc.chunks.get(*chunk);
                println!("  {file} (chunk {chunk}, type {:?}, {} bytes)", c.map(|c| c.id.kind()), c.map_or(0, |c| c.length));
                if lower.ends_with(".ushaderbytecode") {
                    if let Err(e) = inspect_library(&toc, *chunk) {
                        println!("    unreadable: {e:#}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Decodes a sample of a shader library to show what pre-warming could use.
fn inspect_library(toc: &shaderwarm_core::ue::iostore::Toc, chunk: usize) -> Result<()> {
    use shaderwarm_core::dxbc;
    use shaderwarm_core::ue::{iostore::UcasFiles, shaderlib::ShaderLibrary};

    let mut files = UcasFiles::default();
    let lib = ShaderLibrary::parse(&toc.read_chunk(chunk, &mut files)?)?;
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for s in &lib.shaders {
        *freq.entry(format!("{:?}", s.frequency)).or_default() += 1;
    }
    println!("    {} shaders in {} groups: {freq:?}", lib.shaders.len(), lib.groups.len());

    let sample = lib.groups.len().min(50);
    let (mut shaders, mut containers, mut dxil, mut rts0) = (0, 0, 0, 0);
    // SFI0 feature bits for SM6.6 dynamic resources: set when a shader indexes the
    // descriptor heap directly, i.e. Unreal's bindless mode with one global root signature.
    const HEAP_INDEXING: u64 = 0x0200_0000 | 0x0400_0000;
    let mut by_freq: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let mut prefix_shown = false;
    for g in 0..sample {
        for s in lib.read_group(g, toc, &mut files)? {
            shaders += 1;
            let found = dxbc::find_all(&s.code);
            containers += found.len();
            dxil += found.iter().filter(|c| c.is_dxil()).count();
            let with_rs = found.iter().any(|c| c.root_signature().is_some());
            let bindless = found.iter().any(|c| {
                c.part(b"SFI0")
                    .and_then(|p| p.get(..8))
                    .is_some_and(|f| u64::from_le_bytes(f.try_into().unwrap()) & HEAP_INDEXING != 0)
            });
            rts0 += usize::from(with_rs);
            let e = by_freq.entry(format!("{:?}", s.frequency)).or_default();
            e.0 += 1;
            e.1 += usize::from(with_rs);
            e.2 += usize::from(bindless);
            if !prefix_shown {
                if let Some(c) = found.first() {
                    let at = c.bytes.as_ptr() as usize - s.code.as_ptr() as usize;
                    let tail = s.code.len() - at - c.bytes.len();
                    println!("    first shader: {at} bytes before container, {tail} after, {} code bytes", s.code.len());
                    prefix_shown = true;
                }
            }
        }
    }
    println!("    sample of {sample} groups: {shaders} shaders, {containers} containers ({dxil} DXIL), {rts0} with root signature");
    println!("    by stage (shaders, with RTS0, heap-indexed): {by_freq:?}");
    Ok(())
}
