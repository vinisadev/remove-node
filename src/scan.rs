//! Background work: finding, measuring and deleting `node_modules` folders.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui;

/// Messages sent from worker threads to the UI.
pub enum Msg {
    Found {
        path: PathBuf,
        size: u64,
        modified: Option<SystemTime>,
    },
    ScanDone,
    Deleted(PathBuf),
    DeleteFailed(PathBuf, String),
    DeleteDone,
}

/// Progress shared between the scanner thread and the UI.
#[derive(Default)]
pub struct Progress {
    pub dirs_scanned: AtomicU64,
    pub current: Mutex<String>,
}

/// Directories that are never worth descending into (virtual filesystems).
fn is_pseudo_fs(path: &Path) -> bool {
    cfg!(unix)
        && ["/proc", "/sys", "/dev", "/run", "/tmp/.X11-unix"]
            .iter()
            .any(|p| path == Path::new(p))
}

/// Bytes actually used on disk by a file (falls back to logical length).
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
fn dir_size(dir: &Path, cancel: &AtomicBool) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
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

pub fn scan(
    root: PathBuf,
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    ctx: egui::Context,
) {
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress.dirs_scanned.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut cur) = progress.current.lock() {
            *cur = dir.display().to_string();
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            // file_type() does not follow symlinks, so symlinked dirs are skipped.
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let path = entry.path();
            if entry.file_name() == "node_modules" {
                // Count it, but don't look for nested node_modules inside it.
                let size = dir_size(&path, &cancel);
                let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
                let _ = tx.send(Msg::Found {
                    path,
                    size,
                    modified,
                });
                ctx.request_repaint();
            } else if !is_pseudo_fs(&path) {
                stack.push(path);
            }
        }
    }
    let _ = tx.send(Msg::ScanDone);
    ctx.request_repaint();
}

pub fn delete(paths: Vec<PathBuf>, tx: Sender<Msg>, ctx: egui::Context) {
    for path in paths {
        let msg = match std::fs::remove_dir_all(&path) {
            Ok(()) => Msg::Deleted(path),
            Err(e) => Msg::DeleteFailed(path, e.to_string()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    }
    let _ = tx.send(Msg::DeleteDone);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_top_level_node_modules_only() {
        let root = std::env::temp_dir().join(format!("npmclean-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = root.join("proj-a/node_modules/lodash/node_modules");
        let b = root.join("deep/x/proj-b/node_modules");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("big.bin"), vec![1u8; 100_000]).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        scan(
            root.clone(),
            tx,
            Arc::default(),
            Arc::default(),
            egui::Context::default(),
        );
        let mut found: Vec<(PathBuf, u64)> = rx
            .try_iter()
            .filter_map(|m| match m {
                Msg::Found { path, size, .. } => Some((path, size)),
                _ => None,
            })
            .collect();
        found.sort();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, b);
        assert!(found[0].1 >= 100_000);
        assert_eq!(found[1].0, root.join("proj-a/node_modules"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
