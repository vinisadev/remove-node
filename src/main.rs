use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into());
    let mut found = scan(&root);
    found.sort_by(|a, b| b.size.cmp(&a.size));
    for f in &found {
        println!("{:>10}  {:>14}  {}", human_size(f.size), age(f.modified), f.path.display());
    }
    let total: u64 = found.iter().map(|f| f.size).sum();
    println!("Found {} node_modules folder(s) using {}.", found.len(), human_size(total));
}

// Scanning

struct Found {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

/// Directories that are never worth descending into (virtual filesystems).
fn is_pseudo_fs(path: &Path) -> bool {
    cfg!(unix)
        && ["/proc", "/sys", "/dev", "/run", "/tmp/.X11-unix"]
            .iter()
            .any(|p| path == Path::new(p))
}

/// Bytes actually used on disk by a file (falls back to logical length)
fn disk_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Total size of everything under `dir`, not following symlinks.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue }; // does not follow symlinks
            total += disk_size(&meta);
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    total
}

/// Every `node_modules` folder under `root`, with its size
fn scan(root: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            // file_type() does not follow symlinks, so symlinked dirs are skipped.
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let path = entry.path();
            if entry.file_name() == "node_modules" {
                // Count it, but don't look for nested node_modules inside it.
                let size = dir_size(&path);
                let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
                found.push(Found { path, size, modified });
            } else if !is_pseudo_fs(&path) {
                stack.push(path);
            }
        }
    }
    found
}

// Helpers

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "?".into() };
    let Ok(d) = SystemTime::now().duration_since(t) else { return "just now".into() };
    let days = d.as_secs() / 86_400;
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_top_level_node_modules_only() {
        let root = std::env::temp_dir().join(format!("remove-node-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = root.join("proj-a/node_modules/lodash/node_modules");
        let b = root.join("deep/x/proj-b/node_modules");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("big.bin"), vec![1u8; 100_000]).unwrap();

        let mut found: Vec<(PathBuf, u64)> = scan(&root).into_iter().map(|f| (f.path, f.size)).collect();
        found.sort();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, b);
        assert!(found[0].1 >= 100_000);
        assert_eq!(found[1].0, root.join("proj-a/node_modules"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}