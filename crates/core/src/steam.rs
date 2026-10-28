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

    /// Flips the "Enable Shader Pre-Caching" toggle in `config/config.vdf`. Steam reads
    /// the file at startup and rewrites it on exit, so call this only while Steam is
    /// closed. Only an existing setting is changed; every other byte stays as it was.
    pub fn set_precaching(&self, enabled: bool) -> Result<()> {
        let path = self.root.join("config/config.vdf");
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let edited = set_disable_shader_cache(&text, !enabled)
            .context("Steam's pre-caching setting is not in config.vdf; toggle it once in Steam > Settings > Downloads")?;
        if edited == text {
            return Ok(());
        }
        let backup = path.with_extension("vdf.shaderwarm-bak");
        if !backup.exists() {
            fs::copy(&path, &backup).with_context(|| format!("backing up {}", path.display()))?;
        }
        let tmp = path.with_extension("vdf.shaderwarm-tmp");
        fs::write(&tmp, edited)?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The command that starts this Steam install (or talks to the running one).
    pub fn launcher(&self) -> Vec<String> {
        if self.root.components().any(|c| c.as_os_str() == "com.valvesoftware.Steam") {
            ["flatpak", "run", "com.valvesoftware.Steam"].map(String::from).to_vec()
        } else {
            vec!["steam".into()]
        }
    }
}

/// Whether the Steam client (or its bootstrap script) is running.
pub fn client_running(proc_root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(proc_root) else { return false };
    entries.flatten().any(|e| {
        e.file_name().to_str().is_some_and(|s| s.parse::<u32>().is_ok())
            && fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "steam")
    })
}

const SHADER_CACHE_MANAGER: [&str; 5] = ["InstallConfigStore", "Software", "Valve", "Steam", "ShaderCacheManager"];

/// Rewrites the value of `DisableShaderCache` in the ShaderCacheManager block. Steam
/// writes one token pair or brace per line, which is all this has to follow. `None`
/// when the setting isn't there.
fn set_disable_shader_cache(text: &str, disable: bool) -> Option<String> {
    let value = if disable { "1" } else { "0" };
    let mut stack: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    let mut found = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        let mut replacement = None;
        if t == "{" {
            stack.push(pending.take().unwrap_or_default());
        } else if t == "}" {
            stack.pop();
            pending = None;
        } else {
            match quoted_tokens(t).as_slice() {
                [key] => pending = Some(key.clone()),
                [key, _] => {
                    pending = None;
                    let in_manager = stack.len() == SHADER_CACHE_MANAGER.len()
                        && stack.iter().zip(SHADER_CACHE_MANAGER).all(|(a, b)| a.eq_ignore_ascii_case(b));
                    if !found && in_manager && key.eq_ignore_ascii_case("DisableShaderCache") {
                        found = true;
                        let end = line.rfind('"')?;
                        let start = line[..end].rfind('"')?;
                        replacement = Some(format!("{}{value}{}", &line[..=start], &line[end..]));
                    }
                }
                _ => pending = None,
            }
        }
        out.push_str(replacement.as_deref().unwrap_or(line));
    }
    found.then_some(out)
}

fn quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut token = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => token.extend(chars.next()),
                '"' => break,
                c => token.push(c),
            }
        }
        tokens.push(token);
    }
    tokens
}

#[cfg(test)]
mod precaching_tests {
    use super::*;

    const CONFIG: &str = "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\
        \t\t\t\t\"ShaderCacheManager\"\n\t\t\t\t{\n\t\t\t\t\t\"App\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"42\"\n\t\t\t\t\t\t{\n\
        \t\t\t\t\t\t\t\"DisableShaderCache\"\t\t\"1\"\n\t\t\t\t\t\t}\n\t\t\t\t\t}\n\
        \t\t\t\t\t\"DisableShaderCache\"\t\t\"1\"\n\t\t\t\t\t\"EnableShaderBackgroundProcessing\"\t\t\"0\"\n\
        \t\t\t\t}\n\t\t\t\t\"Other\"\n\t\t\t\t{\n\t\t\t\t\t\"DisableShaderCache\"\t\t\"1\"\n\t\t\t\t}\n\
        \t\t\t}\n\t\t}\n\t}\n}\n";

    #[test]
    fn flips_only_the_manager_setting() {
        let on = set_disable_shader_cache(CONFIG, false).unwrap();
        let changed: Vec<(&str, &str)> = CONFIG.lines().zip(on.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed, [("\t\t\t\t\t\"DisableShaderCache\"\t\t\"1\"", "\t\t\t\t\t\"DisableShaderCache\"\t\t\"0\"")]);
        assert_eq!(set_disable_shader_cache(&on, true).unwrap(), CONFIG, "round trip is byte-identical");
        assert_eq!(set_disable_shader_cache(CONFIG, true).unwrap(), CONFIG);
    }

    #[test]
    fn missing_setting_is_reported() {
        assert_eq!(set_disable_shader_cache("\"InstallConfigStore\"\n{\n}\n", false), None);
    }

    #[test]
    fn writes_config_with_backup() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("config")).unwrap();
        fs::write(tmp.path().join("config/config.vdf"), CONFIG).unwrap();
        let steam = Steam { root: tmp.path().to_path_buf(), libraries: Vec::new() };
        assert_eq!(steam.precaching_enabled(), Some(false));
        steam.set_precaching(true).unwrap();
        assert_eq!(steam.precaching_enabled(), Some(true));
        steam.set_precaching(false).unwrap();
        assert_eq!(fs::read_to_string(tmp.path().join("config/config.vdf")).unwrap(), CONFIG);
        assert_eq!(fs::read_to_string(tmp.path().join("config/config.vdf.shaderwarm-bak")).unwrap(), CONFIG);
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

/// Library artwork Steam has cached for a game. Any of it may be missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Artwork {
    /// Portrait 600x900 capsule.
    pub cover: Option<PathBuf>,
    /// Wide banner shown behind the game page.
    pub hero: Option<PathBuf>,
    /// Transparent title logo drawn over the hero.
    pub logo: Option<PathBuf>,
    /// 460x215 landscape header, the fallback when there's no cover.
    pub header: Option<PathBuf>,
}

impl Steam {
    pub fn artwork(&self, appid: u32) -> Artwork {
        artwork_in(&self.root.join("appcache/librarycache"), appid)
    }
}

/// Steam keeps artwork in `librarycache/<appid>/`, either as named files or inside
/// hash-named subfolders; older clients used flat `<appid>_library_600x900.jpg` names.
fn artwork_in(cache: &Path, appid: u32) -> Artwork {
    let dir = cache.join(appid.to_string());
    let mut found: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                found.extend(fs::read_dir(&path).into_iter().flatten().flatten().map(|f| f.path()));
            } else {
                found.push(path);
            }
        }
    }
    let pick = |names: &[&str]| {
        names.iter().find_map(|name| {
            found
                .iter()
                .find(|p| p.file_name().is_some_and(|n| n == *name))
                .cloned()
                .or_else(|| Some(cache.join(format!("{appid}_{name}"))).filter(|p| p.is_file()))
        })
    };
    Artwork {
        cover: pick(&["library_600x900.jpg", "library_capsule.jpg"]),
        hero: pick(&["library_hero.jpg"]),
        logo: pick(&["logo.png"]),
        header: pick(&["library_header.jpg", "header.jpg"]),
    }
}

#[cfg(test)]
mod artwork_tests {
    use super::*;

    #[test]
    fn finds_named_hashed_and_flat_artwork() {
        let tmp = tempfile::tempdir().unwrap();
        let c = tmp.path();
        fs::create_dir_all(c.join("1/abc")).unwrap();
        fs::write(c.join("1/library_hero.jpg"), b"").unwrap();
        fs::write(c.join("1/abc/library_capsule.jpg"), b"").unwrap();
        fs::write(c.join("1/abc/logo.png"), b"").unwrap();
        fs::write(c.join("2_library_600x900.jpg"), b"").unwrap();

        let a = artwork_in(c, 1);
        assert_eq!(a.cover, Some(c.join("1/abc/library_capsule.jpg")));
        assert_eq!(a.hero, Some(c.join("1/library_hero.jpg")));
        assert_eq!(a.logo, Some(c.join("1/abc/logo.png")));
        assert_eq!(a.header, None);
        assert_eq!(artwork_in(c, 2).cover, Some(c.join("2_library_600x900.jpg")));
        assert_eq!(artwork_in(c, 3), Artwork::default());
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
