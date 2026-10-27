//! Steam library discovery: install root, library folders, installed games and their Proton setup.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::paths;
use crate::vdf;

pub struct Steam {
    pub root: PathBuf,
    pub libraries: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    /// Library folder that holds the game (the parent of `steamapps`).
    pub library: PathBuf,
    pub install_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtonInfo {
    /// Build name from the tool's `version` file (e.g. `wineland-11.0-20261015`), or the
    /// first `config_info` line when the install is gone. That line is the Wine build,
    /// which forks such as Wineland share with their parent (`CachyOS-11.0-100`).
    pub name: String,
    /// Proton install directory (the one holding the `proton` script), if it still exists.
    pub root: Option<PathBuf>,
}

/// Apps that Steam installs as tools rather than games.
const TOOL_NAME_PREFIXES: &[&str] = &["Proton", "Steam Linux Runtime", "Steamworks Common"];

impl Steam {
    /// Finds the native or Flatpak Steam install in the usual places.
    pub fn locate() -> Result<Self> {
        let home = paths::home();
        let candidates = [
            home.join(".local/share/Steam"),
            home.join(".steam/steam"),
            home.join(".steam/root"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ];
        for candidate in candidates {
            if candidate.join("steamapps/libraryfolders.vdf").is_file() {
                return Self::from_root(&candidate);
            }
        }
        bail!("no Steam installation found (looked in ~/.local/share/Steam, ~/.steam and the Flatpak path)")
    }

    pub fn from_root(root: &Path) -> Result<Self> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let vdf_path = root.join("steamapps/libraryfolders.vdf");
        let text = fs::read_to_string(&vdf_path)
            .with_context(|| format!("reading {}", vdf_path.display()))?;
        let doc = vdf::parse(&text).with_context(|| format!("parsing {}", vdf_path.display()))?;

        let mut libraries = vec![root.clone()];
        if let Some(folders) = doc.get("libraryfolders") {
            for (_, folder) in folders.entries() {
                let Some(path) = folder.get_str("path") else { continue };
                let path = PathBuf::from(path);
                let path = path.canonicalize().unwrap_or(path);
                if !libraries.contains(&path) {
                    libraries.push(path);
                }
            }
        }
        // Unmounted drives stay in libraryfolders.vdf; skip them quietly.
        libraries.retain(|lib| lib.join("steamapps").is_dir());
        Ok(Self { root, libraries })
    }

    /// Installed games across all libraries, without Proton and runtime tools, sorted by name.
    pub fn games(&self) -> Vec<Game> {
        let mut games: Vec<Game> = self
            .libraries
            .iter()
            .flat_map(|lib| manifests(lib))
            .filter(|g| !TOOL_NAME_PREFIXES.iter().any(|p| g.name.starts_with(p)))
            .collect();
        games.sort_by_key(|g| g.name.to_lowercase());
        games
    }

    pub fn game(&self, appid: u32) -> Option<Game> {
        self.games().into_iter().find(|g| g.appid == appid)
    }

    /// Steam's "Enable Shader Pre-Caching" toggle, read from `config/config.vdf`.
    /// `None` when Steam never wrote the setting.
    pub fn precaching_enabled(&self) -> Option<bool> {
        let text = fs::read_to_string(self.root.join("config/config.vdf")).ok()?;
        let doc = vdf::parse(&text).ok()?;
        let manager = ["InstallConfigStore", "Software", "Valve", "Steam", "ShaderCacheManager"]
            .iter()
            .try_fold(&doc, |node, key| node.get(key))?;
        Some(manager.get_str("DisableShaderCache")? != "1")
    }
}

fn manifests(library: &Path) -> Vec<Game> {
    let Ok(entries) = fs::read_dir(library.join("steamapps")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("appmanifest_") && name.ends_with(".acf")
        })
        .filter_map(|p| parse_manifest(library, &p).ok())
        .collect()
}

fn parse_manifest(library: &Path, path: &Path) -> Result<Game> {
    let doc = vdf::parse(&fs::read_to_string(path)?)?;
    let state = doc.get("AppState").context("missing AppState")?;
    let appid = state.get_str("appid").context("missing appid")?.parse()?;
    let name = state.get_str("name").unwrap_or("").to_string();
    let installdir = state.get_str("installdir").context("missing installdir")?;
    Ok(Game {
        appid,
        name,
        library: library.to_path_buf(),
        install_dir: library.join("steamapps/common").join(installdir),
    })
}

impl Game {
    pub fn compatdata(&self) -> PathBuf {
        self.library.join("steamapps/compatdata").join(self.appid.to_string())
    }

    /// The directory Steam passes to Proton as `STEAM_COMPAT_SHADER_PATH`.
    pub fn shadercache(&self) -> PathBuf {
        self.library.join("steamapps/shadercache").join(self.appid.to_string())
    }

    /// The Proton build that last ran this game, from `compatdata/<appid>/config_info`.
    pub fn proton(&self) -> Option<ProtonInfo> {
        let text = fs::read_to_string(self.compatdata().join("config_info")).ok()?;
        parse_config_info(&text)
    }
}

/// `config_info` is line-based: the tool name first, then paths inside the Proton
/// install (fonts, libs, default prefix) and a few flags.
fn parse_config_info(text: &str) -> Option<ProtonInfo> {
    let mut lines = text.lines();
    let name = lines.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let root = lines.find_map(|line| {
        ["/files/share/fonts/", "/dist/share/fonts/"]
            .iter()
            .find_map(|suffix| line.trim().strip_suffix(suffix))
            .map(PathBuf::from)
    });
    let root = root.filter(|r| r.join("proton").is_file());
    let name = root.as_deref().and_then(version_name).unwrap_or(name);
    Some(ProtonInfo { name, root })
}

/// The `version` file holds `<timestamp> <build name>`.
fn version_name(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("version")).ok()?;
    text.split_whitespace().nth(1).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_info_without_existing_install_keeps_name() {
        let info = parse_config_info(
            "GE-Proton11-11\n/nonexistent/Proton-GE Latest/files/share/fonts/\n/nonexistent/files/lib/\n",
        )
        .unwrap();
        assert_eq!(info.name, "GE-Proton11-11");
        assert_eq!(info.root, None);
        assert_eq!(parse_config_info(""), None);
    }
}
