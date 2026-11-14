use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Remove Node")
            .with_app_id("remove-node")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([600.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "remove-node",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

// ---------------------------------------------------------------------------
// Data

struct Entry {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    selected: bool,
}

enum Msg {
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
struct Progress {
    dirs_scanned: AtomicU64,
    current: Mutex<String>,
}

#[derive(PartialEq, Clone, Copy)]
enum SortBy {
    Size,
    Path,
    Modified,
}

struct App {
    root: String,
    entries: Vec<Entry>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    scanning: bool,
    deleting: bool,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    sort_by: SortBy,
    filter: String,
    confirm_delete: bool,
    errors: Vec<String>,
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
            deleting: false,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::default()),
            sort_by: SortBy::Size,
            filter: String::new(),
            confirm_delete: false,
            errors: Vec::new(),
            status: "Pick a folder and press Scan.".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Background work

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

fn scan(
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

fn delete(paths: Vec<PathBuf>, tx: Sender<Msg>, ctx: egui::Context) {
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

// ---------------------------------------------------------------------------
// Helpers

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "?".into() };
    let Ok(d) = SystemTime::now().duration_since(t) else {
        return "just now".into();
    };
    let days = d.as_secs() / 86_400;
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

// ---------------------------------------------------------------------------
// UI

impl App {
    fn start_scan(&mut self, ctx: &egui::Context) {
        let root = PathBuf::from(self.root.trim());
        if !root.is_dir() {
            self.status = format!("Not a directory: {}", root.display());
            return;
        }
        self.entries.clear();
        self.errors.clear();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(Progress::default());
        self.scanning = true;
        let (tx, cancel, progress, ctx) = (
            self.tx.clone(),
            self.cancel.clone(),
            self.progress.clone(),
            ctx.clone(),
        );
        thread::spawn(move || scan(root, tx, cancel, progress, ctx));
    }

    fn start_delete(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|e| e.selected)
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.deleting = true;
        self.errors.clear();
        self.status = format!("Deleting {} folder(s)...", paths.len());
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || delete(paths, tx, ctx));
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Found {
                    path,
                    size,
                    modified,
                } => {
                    self.entries.push(Entry {
                        path,
                        size,
                        modified,
                        selected: false,
                    });
                    self.sort();
                }
                Msg::ScanDone => {
                    self.scanning = false;
                    let cancelled = self.cancel.load(Ordering::Relaxed);
                    self.status = format!(
                        "{} {} node_modules folder(s) using {}.",
                        if cancelled {
                            "Cancelled. Found"
                        } else {
                            "Done. Found"
                        },
                        self.entries.len(),
                        human_size(self.entries.iter().map(|e| e.size).sum()),
                    );
                }
                Msg::Deleted(path) => self.entries.retain(|e| e.path != path),
                Msg::DeleteFailed(path, err) => {
                    self.errors.push(format!("{}: {err}", path.display()));
                }
                Msg::DeleteDone => {
                    self.deleting = false;
                    self.status = if self.errors.is_empty() {
                        "Deleted.".into()
                    } else {
                        format!("Finished with {} error(s).", self.errors.len())
                    };
                }
            }
        }
    }

    fn sort(&mut self) {
        match self.sort_by {
            SortBy::Size => self.entries.sort_by(|a, b| b.size.cmp(&a.size)),
            SortBy::Path => self.entries.sort_by(|a, b| a.path.cmp(&b.path)),
            SortBy::Modified => self.entries.sort_by(|a, b| a.modified.cmp(&b.modified)),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        let busy = self.scanning || self.deleting;

        // --- Top: folder + scan controls
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Scan folder:");
                let edit = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.root)
                        .desired_width(ui.available_width() - 90.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if self.scanning {
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                } else if ui
                    .add_enabled(!self.deleting, egui::Button::new("Scan"))
                    .clicked()
                    || (enter && !busy)
                {
                    self.start_scan(&ctx);
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Filter:");
                ui.text_edit_singleline(&mut self.filter);
                ui.separator();
                ui.label("Sort by:");
                let before = self.sort_by;
                ui.selectable_value(&mut self.sort_by, SortBy::Size, "Size");
                ui.selectable_value(&mut self.sort_by, SortBy::Path, "Path");
                ui.selectable_value(&mut self.sort_by, SortBy::Modified, "Oldest");
                if before != self.sort_by {
                    self.sort();
                }
            });
            ui.add_space(6.0);
        });

        // --- Bottom: totals, selection actions, status
        let total: u64 = self.entries.iter().map(|e| e.size).sum();
        let (sel_count, sel_size) = self
            .entries
            .iter()
            .filter(|e| e.selected)
            .fold((0, 0), |(n, s), e| (n + 1, s + e.size));

        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "{} folders · {}",
                    self.entries.len(),
                    human_size(total)
                ));
                ui.separator();
                ui.label(format!("{sel_count} selected · {}", human_size(sel_size)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let del = egui::Button::new(
                        egui::RichText::new(format!("Delete selected ({})", human_size(sel_size)))
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(190, 50, 50));
                    if ui.add_enabled(!busy && sel_count > 0, del).clicked() {
                        self.confirm_delete = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Select none"))
                        .clicked()
                    {
                        self.entries.iter_mut().for_each(|e| e.selected = false);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Select all shown"))
                        .clicked()
                    {
                        let filter = self.filter.to_lowercase();
                        for e in &mut self.entries {
                            if filter.is_empty()
                                || e.path.to_string_lossy().to_lowercase().contains(&filter)
                            {
                                e.selected = true;
                            }
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                if self.scanning {
                    ui.spinner();
                    let cur = self
                        .progress
                        .current
                        .lock()
                        .map(|c| c.clone())
                        .unwrap_or_default();
                    ui.label(format!(
                        "Scanned {} dirs — {cur}",
                        self.progress.dirs_scanned.load(Ordering::Relaxed)
                    ));
                } else {
                    if self.deleting {
                        ui.spinner();
                    }
                    ui.label(&self.status);
                }
            });
            for err in &self.errors {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
            }
            ui.add_space(6.0);
        });

        // --- Center: results list
        egui::CentralPanel::default().show(ui, |ui| {
            if self.entries.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(if self.scanning {
                        "Searching..."
                    } else {
                        "No node_modules folders yet."
                    });
                });
                return;
            }
            let max = self
                .entries
                .iter()
                .map(|e| e.size)
                .max()
                .unwrap_or(1)
                .max(1);
            let filter = self.filter.to_lowercase();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("results")
                        .striped(true)
                        .num_columns(4)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("");
                            ui.strong("Size");
                            ui.strong("Modified");
                            ui.strong("Path");
                            ui.end_row();
                            for e in &mut self.entries {
                                if !filter.is_empty()
                                    && !e.path.to_string_lossy().to_lowercase().contains(&filter)
                                {
                                    continue;
                                }
                                ui.add_enabled(
                                    !busy,
                                    egui::Checkbox::without_text(&mut e.selected),
                                );
                                ui.add(
                                    egui::ProgressBar::new(e.size as f32 / max as f32)
                                        .desired_width(110.0)
                                        .text(human_size(e.size)),
                                );
                                ui.label(age(e.modified));
                                let path = e.path.display().to_string();
                                if ui
                                    .add(egui::Label::new(&path).sense(egui::Sense::click()))
                                    .clicked()
                                    && !busy
                                {
                                    e.selected = !e.selected;
                                }
                                ui.end_row();
                            }
                        });
                });
        });

        // --- Confirmation dialog
        if self.confirm_delete {
            let modal = egui::Modal::new(egui::Id::unique("confirm")).show(&ctx, |ui| {
                ui.set_max_width(420.0);
                ui.heading("Delete node_modules?");
                ui.add_space(6.0);
                ui.label(format!(
                    "This permanently deletes {sel_count} folder(s), freeing about {}. \
                     You can reinstall them with npm/yarn/pnpm install.",
                    human_size(sel_size)
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        self.confirm_delete = false;
                        self.start_delete(&ctx);
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = false;
                    }
                });
            });
            if modal.should_close() {
                self.confirm_delete = false;
            }
        }

        if self.scanning {
            // Keep the progress line ticking.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
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

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
