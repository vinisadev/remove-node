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

/// Directories that are never worth descending into (virtual filesystems).
fn is_pseudo_fs(path: &Path) -> bool {
    cfg!(unix)
        && ["/proc", "/sys", "/dev", "/run", "/tmp/.X11-unix"]
            .iter()
            .any(|p| path == Path::new(p))
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
            } else if !is_pseudo_fs(&path) {
                stack.push(path);
            }
        }
    }
    found
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

        let mut found = scan(&root);
        found.sort();
        assert_eq!(found, vec![b, root.join("proj-a/node_modules")]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}