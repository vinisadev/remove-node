use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Remove Node")
            .with_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "remove-node",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

// Data

struct Entry {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

enum Msg {
    Found {
        path: PathBuf,
        size: u64,
        modified: Option<SystemTime>,
    },
    ScanDone,
}

struct App {
    root: String,
    entries: Vec<Entry>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    scanning: bool,
    status: String,
}

impl Default for App {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        let root = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).display().to_string())
            .unwrap_or_else(|| "/".into());
        Self {
            root,
            entries: Vec::new(),
            rx,
            tx,
            scanning: false,
            status: "Pick a folder and press Scan.".into(),
        }
    }
}

// Background work

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

fn scan(root: PathBuf, tx: Sender<Msg>, ctx: egui::Context) {
    let mut stack = vec![root];
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
                let _ = tx.send(Msg::Found { path, size, modified });
                ctx.request_repaint();
            } else if !is_pseudo_fs(&path) {
                stack.push(path);
            }
        }
    }
    let _ = tx.send(Msg::ScanDone);
    ctx.request_repaint();
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

// UI

impl App {
    fn start_scan(&mut self, ctx: &egui::Context) {
        let root = PathBuf::from(self.root.trim());
        if !root.is_dir() {
            self.status = format!("Not a directory: {}", root.display());
            return;
        }
        self.entries.clear();
        self.scanning = true;
        self.status = "Scanning...".into();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || scan(root, tx, ctx));
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Found { path, size, modified } => {
                    self.entries.push(Entry { path, size, modified });
                    self.entries.sort_by(|a, b| b.size.cmp(&a.size));
                }
                Msg::ScanDone => {
                    self.scanning = false;
                    self.status = format!(
                        "Done. Found {} node_modules folder(s) using {}.",
                        self.entries.len(),
                        human_size(self.entries.iter().map(|e| e.size).sum()),
                    );
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();

        // --- Top: folder + scan controls
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Scan folder:");
                let edit = ui.add_enabled(
                    !self.scanning,
                    egui::TextEdit::singleline(&mut self.root).desired_width(ui.available_width() - 90.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(!self.scanning, egui::Button::new("Scan")).clicked() || (enter && !self.scanning) {
                    self.start_scan(&ctx);
                }
            });
            ui.add_space(6.0);
        });

        // --- Bottom: status
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.scanning {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
            ui.add_space(6.0);
        });

        // --- Center: results list
        egui::CentralPanel::default().show(ui, |ui| {
            if self.entries.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(if self.scanning { "Searching..." } else { "No node_modules folders yet." });
                });
                return;
            }
            let max = self.entries.iter().map(|e| e.size).max().unwrap_or(1).max(1);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("results").striped(true).num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.strong("Size");
                    ui.strong("Modified");
                    ui.strong("Path");
                    ui.end_row();
                    for e in &self.entries {
                        ui.add(
                            egui::ProgressBar::new(e.size as f32 / max as f32)
                                .desired_width(110.0)
                                .text(human_size(e.size)),
                        );
                        ui.label(age(e.modified));
                        ui.label(e.path.display().to_string());
                        ui.end_row();
                    }
                });
            });
        });
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

        let (tx, rx) = mpsc::channel();
        scan(root.clone(), tx, egui::Context::default());

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

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}