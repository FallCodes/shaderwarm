use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use shaderwarm_core::cache;
use shaderwarm_core::clean::{self, PurgeOptions, Target};
use shaderwarm_core::fossilize;
use shaderwarm_core::fsutil::{format_bytes, tree_size};
use shaderwarm_core::gpu::{self, Gpu};
use shaderwarm_core::launch::{self, LaunchEnv};
use shaderwarm_core::paths;
use shaderwarm_core::record;
use shaderwarm_core::replay;
use shaderwarm_core::steam::{Game, Steam};

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
    /// Install the recording layer and print the Steam launch option.
    Setup,
    /// Steam launch wrapper: `shaderwarm run -- %command%`.
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Compile recorded pipelines into the driver cache.
    Replay(ReplayArgs),
    /// Remove video transcodes, orphaned caches or one game's shader cache.
    Clean(CleanArgs),
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // `run` sits in the game's launch path: it must never need Steam discovery to succeed.
    if let Command::Run { command } = cli.command {
        return run(command);
    }
    let steam = Steam::locate()?;
    match cli.command {
        Command::Run { .. } => unreachable!(),
        Command::Setup => setup(),
        Command::List => list(&steam),
        Command::Status { appid } => status(&steam, appid),
        Command::Doctor => doctor(&steam),
        Command::Replay(args) => replay_cmd(&steam, args),
        Command::Clean(args) => clean_cmd(&steam, args),
        Command::Restore { appid, timestamp } => restore_cmd(&steam, appid, timestamp),
    }
}

fn primary_gpu() -> Result<Gpu> {
    let gpus = gpu::enumerate()?;
    gpu::primary(&gpus).cloned().context("no hardware Vulkan device found")
}

fn find_game(steam: &Steam, appid: u32) -> Result<Game> {
    steam.game(appid).with_context(|| format!("app {appid} is not an installed game"))
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
    println!(
        "{:>8}  {:<32}  {:<26}  {:>10}  {:>10}  STATUS",
        "APPID", "NAME", "PROTON", "SHADERS", "VIDEO"
    );
    for game in steam.games() {
        let b = cache::breakdown(&game.shadercache());
        let proton = game.proton().map(|p| p.name).unwrap_or_else(|| "-".into());
        let status = gpu.as_ref().map_or("unknown GPU", |g| replay::staleness(&game, g).describe());
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
    println!("  recordings   {}", paths::data_dir().join("recordings").display());
    Ok(())
}

fn setup() -> Result<()> {
    let manifest = record::install_layer_manifest()?;
    println!("Installed recording layer: {}", manifest.display());
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
    let gpu = primary_gpu()?;
    let threads = args
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
    let games: Vec<Game> = match args.target.appid {
        Some(appid) => vec![find_game(steam, appid)?],
        None => steam
            .games()
            .into_iter()
            .filter(|g| {
                let s = replay::staleness(g, &gpu);
                if args.target.stale { s.needs_replay() } else { !replay::inputs(g).is_empty() }
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
