//! Right-hand inspector: what the graph selection shows. `commit` (commit or
//! compare details and files), `wip` (staging lists + commit box), `multi`
//! (several selected commits). File rows and their shared menu entries live here.

mod commit;
mod multi;
mod wip;

use crate::git::Change;
use crate::repo::history::Mode;
use crate::repo::{Action, Op, RepoData, RepoTab, Sel, tools};
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
            // A clean working tree (e.g. right after committing everything): a calm note.
            None => {
                let clean = self.data.as_ref().is_some_and(|d| !d.wip);
                ui.add_space(ui.available_height() / 3.0);
                ui.vertical_centered(|ui| {
                    if clean {
                        theme::empty_state(ui, icon::CHECK_CIRCLE, "Nothing to commit", "Your working tree is clean. Select a commit to see its details.");
                    } else {
                        theme::empty_state(ui, icon::CURSOR_CLICK, "Select a commit", "Its details, files and diffs show here.");
                    }
                });
            }
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

/// "Open in editor" (unless the file is deleted there) and the custom commands that take a {path}.
pub fn open_items(ui: &mut Ui, d: &RepoData, sha: &str, path: &str, deleted: bool, acts: &mut Vec<Action>) {
    if !deleted && ui.button(format!("{} Open in editor", icon::CODE)).on_hover_text("The working-tree file, in the editor from Settings → Tools").clicked() {
        acts.push(Action::Editor(path.into()));
    }
    let vars = tools::Vars { branch: d.status.branch.clone().unwrap_or_default(), sha: sha.into(), path: path.into() };
    custom_items(ui, tools::commands(Some("path")), &vars, acts);
}

/// Custom commands (Settings → Tools) as menu entries, run with `vars`.
pub fn custom_items(ui: &mut Ui, cmds: Vec<tools::Custom>, vars: &tools::Vars, acts: &mut Vec<Action>) {
    for c in cmds {
        if ui.button(format!("{} {}", icon::TERMINAL, c.name)).on_hover_text(&c.command).clicked() {
            acts.push(Action::Custom(c, vars.clone()));
        }
    }
}

/// Permalink to `path` at commit `sha` on the main remote: copy it or open it.
pub fn permalink_items(ui: &mut Ui, d: &RepoData, sha: &str, path: &str, acts: &mut Vec<Action>) {
    let Some(web) = d.web() else { return };
    let url = format!("{web}/blob/{sha}/{}", crate::git::url_path(path));
    let short = &sha[..sha.len().min(7)];
    let pushed = "Pinned to the full SHA: the link works once the commit is pushed";
    if ui.button(format!("{} Copy link to file at {short}", icon::LINK)).on_hover_text(pushed).clicked() {
        acts.push(Action::Copy(url.clone()));
    }
    let host = web.split_once("://").map_or(web.as_str(), |w| w.1).split('/').next().unwrap_or_default();
    if ui.button(format!("{} Open file on {host}", icon::GLOBE)).on_hover_text(pushed).clicked() {
        acts.push(Action::Url(url));
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

/// Pointer over `rect`; the screenshot harness can pin a pointer position (`row-hover:x,y`).
fn hovering(ui: &Ui, rect: egui::Rect) -> bool {
    ui.rect_contains_pointer(rect) || ui.data(|d| d.get_temp::<egui::Pos2>(egui::Id::new("shot_pointer"))).is_some_and(|p| rect.contains(p))
}

/// The hover button at a file row's right edge ("Stage" / "Unstage"): drawn only while the pointer
/// is over the row, true when clicked. Takes the click from the row beneath.
pub fn row_button(ui: &Ui, row: &egui::Response, glyph: &str, label: &str) -> bool {
    if !hovering(ui, row.rect) {
        return false;
    }
    let g = ui.painter().layout_no_wrap(format!("{glyph} {label}"), egui::FontId::proportional(11.5), egui::Color32::PLACEHOLDER);
    let rect = egui::Rect::from_center_size(egui::pos2(row.rect.right() - 6.0 - (g.size().x + 14.0) / 2.0, row.rect.center().y), g.size() + egui::vec2(14.0, 6.0));
    let resp = ui.interact(rect, row.id.with("quick"), Sense::click());
    let fill = theme::accent().gamma_multiply(if resp.hovered() { 1.0 } else { 0.8 });
    ui.painter().rect_filled(rect, 255.0, fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, egui::Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// A file row: colored status letter, file name, an LFS badge, dimmed directory.
pub fn file_row(ui: &mut Ui, c: &Change, selected: bool, lfs: bool) -> egui::Response {
    let (dir, file) = c.path.rsplit_once('/').map_or(("", c.path.as_str()), |(d, f)| (d, f));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 8.0, theme::selection(ui));
    } else if hovering(ui, rect) {
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
