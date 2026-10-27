use std::fs;
use std::path::Path;

/// Total size of a file or directory tree in bytes. Symlinks are not followed and
/// unreadable entries count as zero, since this only feeds size reports.
pub fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|e| tree_size(&e.path())).sum()
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(8_715_556_059), "8.1 GiB");
    }

    #[test]
    fn sums_nested_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("a/x"), [0u8; 10]).unwrap();
        fs::write(dir.path().join("a/b/y"), [0u8; 5]).unwrap();
        assert_eq!(tree_size(dir.path()), 15);
        assert_eq!(tree_size(&dir.path().join("missing")), 0);
    }
}
