//! Right-hand inspector: what the graph selection shows. `commit` (commit or
//! compare details and files), `wip` (staging lists + commit box), `multi`
//! (several selected commits). File rows and their shared menu entries live here.

mod commit;
mod multi;
mod wip;

use crate::git::Change;
use crate::repo::history::Mode;
use crate::repo::{Action, Op, RepoData, RepoTab, Sel};
use crate::theme::{self, icon};
use egui::{Sense, Ui};

impl RepoTab {
    pub fn details(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        self.split_banner(ui, acts);
        if self.multi.len() > 1 {
            return self.multi_panel(ui, acts);
        }
        match self.sel.clone() {
            Some(Sel::Wip) => self.wip_panel(ui, acts),
            Some(Sel::Commit(_) | Sel::Compare { .. }) => self.commit_panel(ui, acts),
            None => {}
        }
    }
}

/// History/Blame entries shared by every file context menu.
pub fn file_items(ui: &mut Ui, path: &str, acts: &mut Vec<Action>) {
    if ui.button(format!("{} File history", icon::CLOCK_COUNTER_CLOCKWISE)).clicked() {
        acts.push(Action::FileView(path.into(), Mode::History));
    }
    if ui.button(format!("{} Blame", icon::USER_LIST)).clicked() {
        acts.push(Action::FileView(path.into(), Mode::Blame));
    }
}

/// "Track *.ext with LFS": adds the pattern to .gitattributes and stages it.
pub fn lfs_track_item(ui: &mut Ui, d: &RepoData, path: &str, acts: &mut Vec<Action>) {
    let Some(ext) = std::path::Path::new(path).extension().map(|e| e.to_string_lossy()) else { return };
    if d.lfs.files.contains(path) {
        return;
    }
    let pat = format!("*.{ext}");
    let b = ui.add_enabled(d.lfs.installed, egui::Button::new(format!("{} Track {pat} with LFS", icon::DATABASE)));
    if b.on_hover_text("Adds the pattern to .gitattributes and stages it; matching files you add from now on are stored in LFS")
        .on_disabled_hover_text(crate::repo::LFS_MISSING)
        .clicked()
    {
        acts.push(Action::Git(Op::new(format!("Track {pat} with LFS"), &["lfs", "track", &pat]).then(&["add", ".gitattributes"])));
    }
}

/// A file row: colored status letter, file name, an LFS badge, dimmed directory.
pub fn file_row(ui: &mut Ui, c: &Change, selected: bool, lfs: bool) -> egui::Response {
    let (dir, file) = c.path.rsplit_once('/').map_or(("", c.path.as_str()), |(d, f)| (d, f));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 8.0, theme::selection(ui));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, theme::hover(ui));
    }
    let p = ui.painter().with_clip_rect(rect);
    let y = rect.center().y;
    let code = if c.code == '?' { 'A' } else { c.code };
    p.text(egui::pos2(rect.left() + 6.0, y), egui::Align2::LEFT_CENTER, code, egui::FontId::monospace(12.0), theme::change_color(c.code));
    let mut r = p.text(egui::pos2(rect.left() + 22.0, y), egui::Align2::LEFT_CENTER, file, egui::FontId::proportional(13.0), theme::text(ui));
    if lfs {
        let g = p.layout_no_wrap("LFS".into(), egui::FontId::proportional(9.5), theme::accent());
        let badge = egui::Rect::from_min_size(egui::pos2(r.right() + 6.0, y - 7.0), g.size() + egui::vec2(8.0, 14.0 - g.size().y));
        p.rect_filled(badge, 4.0, theme::accent().gamma_multiply(0.18));
        p.galley(badge.center() - g.size() / 2.0, g, theme::accent());
        r = badge;
    }
    if !dir.is_empty() {
        p.text(egui::pos2(r.right() + 6.0, y), egui::Align2::LEFT_CENTER, dir, egui::FontId::proportional(11.5), theme::dim(ui));
    }
    resp.on_hover_text(match &c.old_path {
        Some(o) => format!("{o} → {}", c.path),
        None if lfs => format!("{}\nStored with Git LFS", c.path),
        None => c.path.clone(),
    })
}
