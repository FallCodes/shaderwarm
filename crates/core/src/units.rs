//! systemd user units that replay stale caches after driver or Proton updates.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::paths;

pub const SERVICE: &str = "shaderwarm-replay.service";
pub const PATH_UNIT: &str = "shaderwarm-replay.path";
pub const TIMER: &str = "shaderwarm-replay.timer";

/// Files that a GPU driver or Proton update rewrites. Missing paths are fine: systemd
/// watches for them to appear.
const WATCHED: &[&str] = &[
    "/usr/share/vulkan/icd.d",
    "/usr/lib/libvulkan_radeon.so",
    "/usr/lib/libvulkan_intel.so",
    "/usr/lib/libvulkan_lvp.so",
    "/usr/share/steam/compatibilitytools.d",
    "%h/.local/share/Steam/compatibilitytools.d",
];

pub fn unit_dir() -> PathBuf {
    paths::config_dir().parent().map_or_else(|| paths::home().join(".config"), Path::to_path_buf).join("systemd/user")
}

pub fn service_unit(exe: &Path) -> String {
    format!(
        "[Unit]
Description=Replay stale shader caches (shaderwarm)

[Service]
Type=oneshot
# Let package upgrades finish before compiling against the new driver.
ExecStartPre=/usr/bin/sleep 60
ExecStart=\"{}\" replay --stale --defer-if-gaming
Nice=19
IOSchedulingClass=idle
CPUSchedulingPolicy=idle
MemoryHigh=60%
",
        exe.display()
    )
}

pub fn path_unit() -> String {
    let watches: String = WATCHED.iter().map(|p| format!("PathChanged={p}\n")).collect();
    format!(
        "[Unit]
Description=Watch GPU drivers and Proton for shaderwarm

[Path]
{watches}Unit={SERVICE}

[Install]
WantedBy=default.target
"
    )
}

pub fn timer_unit() -> String {
    format!(
        "[Unit]
Description=Periodic stale shader cache check (shaderwarm)

[Timer]
OnBootSec=10min
OnCalendar=hourly
Persistent=true
Unit={SERVICE}

[Install]
WantedBy=timers.target
"
    )
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl --user")?;
    if !status.success() {
        bail!("systemctl --user {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// Writes the three unit files into `dir` without enabling anything (packaging, review).
pub fn write(dir: &Path, exe: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let units = [(SERVICE, service_unit(exe)), (PATH_UNIT, path_unit()), (TIMER, timer_unit())];
    let mut written = Vec::new();
    for (name, text) in units {
        let path = dir.join(name);
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Writes the units for `exe` and enables the watcher and timer. Returns the unit paths.
pub fn install(exe: &Path) -> Result<Vec<PathBuf>> {
    let written = write(&unit_dir(), exe)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", PATH_UNIT, TIMER])?;
    Ok(written)
}

pub fn uninstall() -> Result<()> {
    let dir = unit_dir();
    if !dir.join(SERVICE).exists() {
        return Ok(());
    }
    // Units may already be disabled; removing the files is what matters.
    let _ = systemctl(&["disable", "--now", PATH_UNIT, TIMER]);
    for name in [SERVICE, PATH_UNIT, TIMER] {
        let path = dir.join(name);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    systemctl(&["daemon-reload"])
}

pub fn installed() -> bool {
    unit_dir().join(SERVICE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_reference_each_other_and_quote_exe() {
        let service = service_unit(Path::new("/home/me/bin/shader warm"));
        assert!(service.contains("ExecStart=\"/home/me/bin/shader warm\" replay --stale --defer-if-gaming"));
        let path = path_unit();
        assert!(path.contains("PathChanged=/usr/share/vulkan/icd.d\n"));
        assert!(path.contains(&format!("Unit={SERVICE}")));
        assert!(timer_unit().contains("OnCalendar=hourly"));
    }
}
