//! Game engine detection from install-folder markers.

use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::paths;
use crate::ue::iostore::{chunk_type, Toc, UcasFiles};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// `version` is the release (`4.25`) or a range from the IoStore format (`5.5+`).
    Unreal { major: u8, version: Option<String> },
    Unity,
    ReEngine,
    Source2,
    RedEngine,
    Godot,
    Unknown,
}

impl Engine {
    pub fn is_ue5(&self) -> bool {
        matches!(self, Engine::Unreal { major: 5, .. })
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Engine::Unreal { version: Some(v), .. } => write!(f, "Unreal Engine {v}"),
            Engine::Unreal { major, version: None } => write!(f, "Unreal Engine {major}"),
            Engine::Unity => f.write_str("Unity"),
            Engine::ReEngine => f.write_str("RE Engine"),
            Engine::Source2 => f.write_str("Source 2"),
            Engine::RedEngine => f.write_str("REDengine"),
            Engine::Godot => f.write_str("Godot"),
            Engine::Unknown => f.write_str("unknown"),
        }
    }
}

/// Detects the engine, caching the result per game until its executable changes.
pub fn detect_cached(appid: u32, install_dir: &Path) -> Engine {
    let shipping = unreal_shipping_exe(install_dir);
    let stamp = shipping.as_deref().and_then(stamp_of).unwrap_or_default();
    let cache = paths::state_dir().join("engine").join(appid.to_string());
    if let Ok(text) = fs::read_to_string(&cache) {
        if let Some((s, engine)) = text.split_once('\n') {
            if s == stamp {
                if let Some(e) = parse(engine.trim()) {
                    return e;
                }
            }
        }
    }
    let engine = detect(install_dir, shipping.as_deref());
    if let Some(parent) = cache.parent() {
        let _ = fs::create_dir_all(parent);
        let _ = fs::write(&cache, format!("{stamp}\n{}\n", serialize(&engine)));
    }
    engine
}

fn stamp_of(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}:{mtime}", meta.len()))
}

fn serialize(e: &Engine) -> String {
    match e {
        Engine::Unreal { major, version } => format!("unreal {major} {}", version.as_deref().unwrap_or("-")),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn parse(s: &str) -> Option<Engine> {
    let mut words = s.split(' ');
    Some(match words.next()? {
        "unreal" => {
            let major = words.next()?.parse().ok()?;
            let version = words.next().filter(|v| *v != "-").map(str::to_string);
            Engine::Unreal { major, version }
        }
        "unity" => Engine::Unity,
        "reengine" => Engine::ReEngine,
        "source2" => Engine::Source2,
        "redengine" => Engine::RedEngine,
        "godot" => Engine::Godot,
        "unknown" => Engine::Unknown,
        _ => return None,
    })
}

pub fn detect(install_dir: &Path, shipping: Option<&Path>) -> Engine {
    if let Some(exe) = shipping {
        return detect_unreal(install_dir, exe);
    }
    let has = |rel: &str| install_dir.join(rel).exists();
    let any_entry = |pred: &dyn Fn(&str) -> bool| {
        fs::read_dir(install_dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| pred(&e.file_name().to_string_lossy()))
    };
    if has("UnityPlayer.dll") || any_entry(&|n| n.ends_with("_Data") && install_dir.join(n).join("globalgamemanagers").exists()) {
        Engine::Unity
    } else if any_entry(&|n| n.starts_with("re_chunk_") && n.ends_with(".pak")) {
        Engine::ReEngine
    } else if has("game/bin/win64/engine2.dll") {
        Engine::Source2
    } else if has("archive/pc/content") && has("bin/x64") {
        Engine::RedEngine
    } else if any_entry(&|n| n.ends_with(".pck")) {
        Engine::Godot
    } else {
        Engine::Unknown
    }
}

/// `<Project>/Binaries/Win64/<Project>-Win64-Shipping.exe`, Unreal's packaged game binary.
pub fn unreal_shipping_exe(install_dir: &Path) -> Option<PathBuf> {
    for project in fs::read_dir(install_dir).ok()?.flatten() {
        let bin = project.path().join("Binaries/Win64");
        let Ok(entries) = fs::read_dir(&bin) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with("-shipping.exe") {
                return Some(e.path());
            }
        }
    }
    None
}

fn detect_unreal(install_dir: &Path, exe: &Path) -> Engine {
    if let Some((major, version)) = version_from_exe(exe) {
        return Engine::Unreal { major, version: Some(version) };
    }
    // Packed executables hide the build string; IoStore containers only exist in UE5 builds.
    match iostore_header_version(install_dir) {
        Some(v) => Engine::Unreal { major: 5, version: Some(ue5_range(v).to_string()) },
        None => Engine::Unreal { major: 4, version: None },
    }
}

/// Finds Unreal's `++UE5+Release-5.4` build string (stored as UTF-16) in an executable.
fn version_from_exe(exe: &Path) -> Option<(u8, String)> {
    let needle: Vec<u8> = "++UE".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut file = File::open(exe).ok()?;
    let mut buf = vec![0u8; 8 << 20];
    let mut carry = 0;
    loop {
        let n = file.read(&mut buf[carry..]).ok()?;
        if n == 0 {
            return None;
        }
        let filled = carry + n;
        let window = &buf[..filled];
        for at in memchr::memmem::find_iter(window, &needle) {
            let tail: Vec<u16> = window[at..filled.min(at + 96)]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            if let Some(v) = parse_build_string(&String::from_utf16_lossy(&tail)) {
                return Some(v);
            }
        }
        // Keep a tail so a string split across reads is still found.
        carry = 128.min(filled);
        buf.copy_within(filled - carry..filled, 0);
    }
}

/// `++UE4+Release-4.25Plus` → (4, "4.25").
pub fn parse_build_string(s: &str) -> Option<(u8, String)> {
    let rest = s.strip_prefix("++UE")?;
    let major: u8 = rest.get(..1)?.parse().ok()?;
    let release = rest.split("Release-").nth(1)?;
    let version: String = release.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let version = version.trim_end_matches('.');
    (version.starts_with(&major.to_string()) && version.contains('.')).then(|| (major, version.to_string()))
}

/// Version of the first IoStore container header (`IoCn`) found in the install.
fn iostore_header_version(install_dir: &Path) -> Option<u32> {
    let mut tocs = Vec::new();
    collect_ext(install_dir, "utoc", 4, &mut tocs);
    tocs.sort();
    for path in tocs {
        let Ok(toc) = Toc::open(&path) else { continue };
        let Some(index) = toc.chunks.iter().position(|c| c.id.kind() == chunk_type::CONTAINER_HEADER) else {
            continue;
        };
        let Ok(data) = toc.read_chunk(index, &mut UcasFiles::default()) else { continue };
        if data.get(..4) == Some(&0x496f_436e_u32.to_le_bytes()) {
            return data.get(4..8).map(|v| u32::from_le_bytes(v.try_into().unwrap()));
        }
    }
    None
}

/// `EIoContainerHeaderVersion` → the UE5 releases that write it.
fn ue5_range(version: u32) -> &'static str {
    match version {
        0 | 1 => "5.0",
        2 => "5.1",
        3 => "5.2-5.3",
        4 => "5.4",
        _ => "5.5+",
    }
}

fn collect_ext(dir: &Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && depth > 0 {
            collect_ext(&path, ext, depth - 1, out);
        } else if path.extension().is_some_and(|x| x == ext) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_build_strings() {
        assert_eq!(parse_build_string("++UE4+Release-4.25Plus"), Some((4, "4.25".into())));
        assert_eq!(parse_build_string("++UE5+Release-5.6"), Some((5, "5.6".into())));
        assert_eq!(parse_build_string("++UE5+Release-5.4-CL-0"), Some((5, "5.4".into())));
        assert_eq!(parse_build_string("++UE5+Main"), None);
        assert_eq!(parse_build_string("++UE5+Release-4.27"), None);
    }

    #[test]
    fn cache_format_round_trips() {
        for e in [
            Engine::Unreal { major: 5, version: Some("5.5+".into()) },
            Engine::Unreal { major: 4, version: None },
            Engine::Unity,
            Engine::ReEngine,
            Engine::Unknown,
        ] {
            assert_eq!(parse(&serialize(&e)), Some(e));
        }
    }

    #[test]
    fn finds_build_string_in_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Game-Win64-Shipping.exe");
        let mut data = vec![0u8; 9 << 20]; // spans more than one read buffer
        let s: Vec<u8> = "++UE4+Release-4.27\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let at = (8 << 20) - 10; // straddles the first buffer boundary
        data[at..at + s.len()].copy_from_slice(&s);
        fs::write(&exe, data).unwrap();
        assert_eq!(version_from_exe(&exe), Some((4, "4.27".into())));
    }

    #[test]
    fn detects_by_markers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert_eq!(detect(p, None), Engine::Unknown);
        fs::write(p.join("re_chunk_000.pak"), b"").unwrap();
        assert_eq!(detect(p, None), Engine::ReEngine);
        fs::write(p.join("UnityPlayer.dll"), b"").unwrap();
        assert_eq!(detect(p, None), Engine::Unity);
    }
}
