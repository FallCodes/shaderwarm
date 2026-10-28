//! Games that compile their own shaders up front, where shaderwarm adds little.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::engine::Engine;
use crate::paths;
use crate::steam::Game;

const BUILTIN: &str = include_str!("../../../data/self-precompile.toml");
const OVERRIDE_FILE: &str = "self-precompile.toml";
/// Smaller files are configs or markers, not caches.
const MIN_CACHE_BYTES: u64 = 1 << 20;

#[derive(Debug, Deserialize)]
struct ListFile {
    #[serde(default)]
    game: Vec<ListEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListEntry {
    pub appid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default = "yes")]
    pub self_compiles: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    /// Listed in the shipped database or the user's override file.
    Listed { note: String, user: bool },
    /// The game wrote its own shader/PSO cache into its prefix.
    PrefixCache(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub self_compiles: bool,
    pub evidence: Option<Evidence>,
    /// Set when a UE5 game was kept unflagged despite evidence.
    pub ue5_exempt: bool,
}

impl Verdict {
    /// Short reason for list views.
    pub fn reason(&self) -> String {
        match (&self.evidence, self.ue5_exempt) {
            (Some(_), true) => "UE5: own shader compile misses most pipelines".into(),
            (Some(Evidence::Listed { note, user }), _) => {
                format!("{}{note}", if *user { "your list: " } else { "" })
            }
            (Some(Evidence::PrefixCache(p)), _) => format!(
                "likely: game keeps its own shader cache ({})",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
            (None, _) => String::new(),
        }
    }
}

pub struct Database {
    builtin: Vec<ListEntry>,
    user: Vec<ListEntry>,
}

impl Database {
    pub fn load() -> Result<Self> {
        let path = paths::config_dir().join(OVERRIDE_FILE);
        let user = match fs::read_to_string(&path) {
            Ok(text) => parse(&text).with_context(|| format!("parsing {}", path.display()))?,
            Err(_) => Vec::new(),
        };
        Ok(Self::with_user(user))
    }

    fn with_user(user: Vec<ListEntry>) -> Self {
        Self { builtin: parse(BUILTIN).expect("built-in list is valid"), user }
    }

    fn lookup(&self, appid: u32) -> Option<(&ListEntry, bool)> {
        // The user's file wins, including `self_compiles = false` to un-flag a game.
        self.user
            .iter()
            .find(|e| e.appid == appid)
            .map(|e| (e, true))
            .or_else(|| self.builtin.iter().find(|e| e.appid == appid).map(|e| (e, false)))
    }

    pub fn assess(&self, game: &Game, engine: &Engine) -> Verdict {
        let listed = self.lookup(game.appid);
        if let Some((entry, true)) = listed {
            if !entry.self_compiles {
                return Verdict { self_compiles: false, evidence: None, ue5_exempt: false };
            }
        }
        let evidence = match listed {
            Some((entry, user)) if entry.self_compiles => {
                Some(Evidence::Listed { note: entry.note.clone(), user })
            }
            _ => prefix_cache(&game.compatdata()).map(Evidence::PrefixCache),
        };
        // A user entry is a deliberate choice, so it overrides the UE5 rule.
        let user_listed = matches!(evidence, Some(Evidence::Listed { user: true, .. }));
        let ue5_exempt = evidence.is_some() && engine.is_ue5() && !user_listed;
        Verdict { self_compiles: evidence.is_some() && !ue5_exempt, evidence, ue5_exempt }
    }
}

fn parse(text: &str) -> Result<Vec<ListEntry>> {
    Ok(toml::from_str::<ListFile>(text)?.game)
}

/// A shader or PSO cache the game wrote under its Windows user profile.
fn prefix_cache(compatdata: &Path) -> Option<PathBuf> {
    let user = compatdata.join("pfx/drive_c/users/steamuser");
    ["AppData/Local", "AppData/LocalLow", "AppData/Roaming", "Documents", "Saved Games"]
        .iter()
        .find_map(|d| find_cache(&user.join(d), 6))
}

fn looks_like_cache(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["shadercache", "shader_cache", "pipelinecache", "pipeline_cache", "psocache", "pso_cache"]
        .iter()
        .any(|k| n.contains(k))
        || n.ends_with(".upipelinecache")
        || n.ends_with(".ushaderprecache")
}

fn find_cache(dir: &Path, depth: usize) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if looks_like_cache(&name) && crate::fsutil::tree_size(&path) >= MIN_CACHE_BYTES {
            return Some(path);
        }
        if depth > 0 && e.file_type().is_ok_and(|t| t.is_dir()) {
            subdirs.push(path);
        }
    }
    subdirs.into_iter().find_map(|d| find_cache(&d, depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(root: &Path, appid: u32) -> Game {
        Game { appid, name: "G".into(), library: root.to_path_buf(), install_dir: root.join("common/G") }
    }

    fn write_prefix_cache(g: &Game, size: usize) {
        let p = g.compatdata().join("pfx/drive_c/users/steamuser/AppData/Local/G/Saved/G_PCD3D_SM6.upipelinecache");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; size]).unwrap();
    }

    const UE5: Engine = Engine::Unreal { major: 5, version: None };
    const UE4: Engine = Engine::Unreal { major: 4, version: None };

    #[test]
    fn builtin_list_flags_listed_games() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::with_user(Vec::new());
        let v = db.assess(&game(tmp.path(), 1151640), &Engine::Unknown);
        assert!(v.self_compiles);
        assert!(matches!(v.evidence, Some(Evidence::Listed { user: false, .. })));
        assert!(!db.assess(&game(tmp.path(), 42), &Engine::Unknown).self_compiles);
    }

    #[test]
    fn prefix_cache_flags_but_ue5_is_exempt() {
        let tmp = tempfile::tempdir().unwrap();
        let g = game(tmp.path(), 42);
        let db = Database::with_user(Vec::new());
        write_prefix_cache(&g, 1024);
        assert!(!db.assess(&g, &UE4).self_compiles, "tiny files are not caches");

        write_prefix_cache(&g, 2 << 20);
        let v = db.assess(&g, &UE4);
        assert!(v.self_compiles);
        assert!(matches!(v.evidence, Some(Evidence::PrefixCache(_))));

        let v = db.assess(&g, &UE5);
        assert!(!v.self_compiles);
        assert!(v.ue5_exempt);
    }

    #[test]
    fn user_list_overrides_both_ways() {
        let tmp = tempfile::tempdir().unwrap();
        let user = parse(
            "[[game]]\nappid = 1151640\nself_compiles = false\n\n[[game]]\nappid = 7\nnote = \"mine\"\n",
        )
        .unwrap();
        let db = Database::with_user(user);
        assert!(!db.assess(&game(tmp.path(), 1151640), &Engine::Unknown).self_compiles);
        let v = db.assess(&game(tmp.path(), 7), &UE5);
        assert!(v.self_compiles, "an explicit user entry beats the UE5 rule");
        assert_eq!(v.reason(), "your list: mine");
    }
}
