use std::path::{Path, PathBuf};

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into());
    for path in scan(&root) {
        println!("{}", path.display());
    }
}

/// Every `node_modules` folder under `root`.
fn scan(root: &Path) -> Vec<PathBuf> {
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
                found.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    found
}