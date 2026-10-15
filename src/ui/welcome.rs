//! The welcome / new-tab page (open, clone, init, recent repositories) and the
//! clone dialog.

use super::app::App;
use crate::git;
use crate::repo::Task;
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, RichText, TextEdit, Ui};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct CloneDlg {
    url: String,
    dir: String,
    task: Option<Task<git::Res<PathBuf>>>,
    cancel: git::Cancel,
    error: Option<String>,
}

impl App {
    /// Opens the clone dialog, defaulting to the last clone folder (or home).
    pub(super) fn open_clone(&mut self) {
        self.clone = Some(CloneDlg {
            dir: self.s.clone_dir.clone().or_else(home).map(|p| p.display().to_string()).unwrap_or_default(),
            ..Default::default()
        });
    }

    pub(super) fn welcome(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(70.0);
                ui.label(RichText::new("Glasir").size(44.0).strong());
                ui.label(RichText::new("A fast, free and open-source Git client").color(theme::dim(ui)));
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 420.0).max(0.0) / 2.0);
                    if ui.add(Button::new(format!("{}  Open repository…", icon::FOLDER_OPEN)).min_size(egui::vec2(130.0, 34.0))).clicked() {
                        self.pick_and_open(&ctx);
                    }
                    if ui.add(Button::new(format!("{}  Clone…", icon::DOWNLOAD_SIMPLE)).min_size(egui::vec2(130.0, 34.0))).clicked() {
                        self.open_clone();
                    }
                    if ui.add(Button::new(format!("{}  Init…", icon::PLUS_CIRCLE)).min_size(egui::vec2(130.0, 34.0))).clicked()
                        && let Some(p) = rfd::FileDialog::new().set_title("Initialize repository in…").pick_folder()
                    {
                        match git::run(&p, &["init"]) {
                            Ok(_) => self.open(&ctx, &p),
                            Err(e) => self.toast(&ctx, e, true),
                        }
                    }
                });
                ui.add_space(22.0);
                if self.s.tips {
                    self.s.tips = tips(ui);
                    ui.add_space(22.0);
                }
                if !self.s.recent.is_empty() {
                    ui.label(RichText::new("RECENT").small().strong().color(theme::dim(ui)));
                    ui.add_space(4.0);
                    let mut open = None;
                    let mut forget = None;
                    for p in &self.s.recent {
                        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
                        let r = ui.add(Button::new(RichText::new(name).strong()).frame(false)).on_hover_text(p.display().to_string());
                        if r.clicked() {
                            open = Some(p.clone());
                        }
                        r.context_menu(|ui| {
                            if ui.button("Remove from recent").clicked() {
                                forget = Some(p.clone());
                            }
                        });
                        ui.label(RichText::new(p.display().to_string()).small().color(theme::dim(ui)));
                        ui.add_space(4.0);
                    }
                    if let Some(p) = open {
                        self.open(&ctx, &p);
                    }
                    if let Some(p) = forget {
                        self.s.recent.retain(|r| *r != p);
                    }
                }
            });
        });
    }

    pub(super) fn clone_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.clone else { return };
        if let Some(res) = Task::poll(&mut c.task) {
            match res {
                Ok(p) => {
                    self.s.clone_dir = p.parent().map(Path::to_path_buf);
                    self.clone = None;
                    self.open(ctx, &p);
                    return;
                }
                Err(e) => c.error = (e != git::CANCELLED).then_some(e),
            }
        }
        let mut close = false;
        egui::Modal::new(Id::new("clone")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Clone a repository");
            ui.add_space(6.0);
            ui.label("Repository URL (HTTPS or SSH — github.com or GitHub Enterprise)");
            ui.add(TextEdit::singleline(&mut c.url).margin(crate::theme::INPUT_MARGIN).hint_text("https://github.example.com/org/repo.git").desired_width(f32::INFINITY));
            ui.label("Clone into");
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut c.dir).margin(crate::theme::INPUT_MARGIN).desired_width(360.0));
                if ui.button("Browse…").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    c.dir = p.display().to_string();
                }
            });
            let name = c.url.trim().trim_end_matches(['/', '\\']).trim_end_matches(".git").rsplit(['/', '\\', ':']).next().unwrap_or("").to_owned();
            let dest = clone_dir(&c.dir).join(&name);
            if !name.is_empty() {
                ui.label(RichText::new(format!("→ {}", dest.display())).small().color(theme::dim(ui)));
            }
            if let Some(e) = &c.error {
                ui.colored_label(theme::del(), e);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if c.task.is_some() {
                    ui.spinner();
                    theme::progress(ui, "Cloning…", Some(c.cancel.progress()).filter(|p| !p.is_empty()));
                    if ui.button("Cancel").clicked() {
                        c.cancel.cancel();
                    }
                } else {
                    if ui.add_enabled(!name.is_empty() && !c.dir.trim().is_empty(), Button::new("Clone")).clicked() {
                        let (url, parent, d2) = (c.url.trim().to_owned(), clone_dir(&c.dir), dest.clone());
                        c.error = None;
                        c.cancel = git::Cancel::default();
                        let cancel = c.cancel.clone();
                        c.task = Some(Task::spawn(ctx, move || {
                            std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
                            clone_cleaning_up(&d2, || cancel.scope(|| git::run(&parent, &["clone", "--progress", "--", &url, &d2.to_string_lossy()])))?;
                            Ok(d2)
                        }));
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                }
            });
        });
        if close {
            self.clone = None;
        }
    }

}

fn clone_cleaning_up(dest: &Path, clone: impl FnOnce() -> git::Res<String>) -> git::Res<String> {
    let fresh = std::fs::symlink_metadata(dest).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    let res = clone();
    if fresh && res.as_ref().is_err_and(|e| e == git::CANCELLED) {
        for _ in 0..10 {
            if std::fs::remove_dir_all(dest).map_or_else(|e| e.kind() == std::io::ErrorKind::NotFound, |_| true) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    res
}

/// First-run tips in a glass strip; returns false once dismissed.
fn tips(ui: &mut Ui) -> bool {
    let mut keep = true;
    ui.horizontal(|ui| {
        // Centred on last frame's width: the Linux / Windows key labels are longer.
        let id = ui.id().with("tips_w");
        ui.add_space((ui.available_width() - ui.data(|d| d.get_temp(id)).unwrap_or(760.0)).max(0.0) / 2.0);
        let x0 = ui.cursor().min.x;
        theme::group(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(icon::LIGHTBULB).color(theme::accent()).size(16.0));
            for (keys, what) in [("⌘P", "run any command"), ("Drop a folder", "here to open it"), ("⌘Z", "undoes commits, rebases and discards"), ("?", "shows every shortcut")] {
                ui.label(RichText::new(super::format::keys(keys)).strong());
                ui.label(RichText::new(what).color(theme::dim(ui)));
                ui.add_space(8.0);
            }
            keep = !theme::pill(ui, icon::X).on_hover_text("Hide these tips").clicked();
        });
        let w = ui.min_rect().right() - x0;
        ui.data_mut(|d| d.insert_temp(id, w));
    });
    keep
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// The typed clone folder as an absolute path: a leading `~` is the home folder, a relative one is under the cwd.
fn clone_dir(dir: &str) -> PathBuf {
    let dir = dir.trim();
    let p = match dir.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\'])).zip(home()) {
        Some((rest, h)) => h.join(rest.trim_start_matches(['/', '\\'])),
        None => PathBuf::from(dir),
    };
    std::path::absolute(&p).unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use crate::git;

    #[test]
    fn a_cancelled_clone_removes_only_the_folder_it_made() {
        let base = std::env::temp_dir().join(format!("glasir-clone-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cancelled = || Err::<String, _>(git::CANCELLED.to_owned());
        let fresh = base.join("fresh");
        let res = super::clone_cleaning_up(&fresh, || {
            std::fs::create_dir_all(fresh.join(".git/objects/pack")).unwrap();
            cancelled()
        });
        assert_eq!(res, cancelled());
        assert!(!fresh.exists(), "what the killed git left behind goes");
        let theirs = base.join("theirs");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("keep.txt"), "x").unwrap();
        let _ = super::clone_cleaning_up(&theirs, || {
            std::fs::create_dir_all(theirs.join(".git")).unwrap();
            cancelled()
        });
        assert!(theirs.join("keep.txt").exists() && theirs.join(".git").exists(), "a folder that was already there stays");
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let _ = super::clone_cleaning_up(&empty, cancelled);
        assert!(empty.exists());
        let failed = base.join("failed");
        let res = super::clone_cleaning_up(&failed, || {
            std::fs::create_dir_all(&failed).unwrap();
            Err("fatal: repository not found".into())
        });
        assert!(res.is_err() && failed.exists(), "git cleans up after its own failures");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn clone_dir_is_absolute() {
        let h = super::home().unwrap();
        assert_eq!(super::clone_dir(" ~/code "), h.join("code"));
        assert_eq!(super::clone_dir("~"), h);
        assert!(super::clone_dir("code").is_absolute());
    }
}
