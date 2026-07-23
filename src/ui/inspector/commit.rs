//! Inspector for a commit, stash or compare range: message, author, AI
//! explanation and the changed files.

use super::{file_items, file_row, open_items, permalink_items};
use crate::ai::Purpose;
use crate::git::{Change, CommitInfo};
use crate::repo::restore::restore_file;
use crate::repo::{Action, DiffSrc, RepoData, RepoTab, Sel};
use crate::theme::{self, icon};
use crate::ui::format::initials;
use egui::{Color32, RichText, ScrollArea, Sense, Ui};

/// Message text with `#123` / `GH-123` linked to the pull request (or issue) on `web`.
fn linked(ui: &mut Ui, text: &str, web: Option<&str>, style: impl Fn(&str) -> RichText, acts: &mut Vec<Action>) {
    let parts = crate::git::autolinks(text);
    let Some(web) = web.filter(|_| parts.iter().any(|p| p.1.is_some())) else {
        ui.label(style(text));
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (s, n) in parts {
            let Some(n) = n else {
                ui.label(style(s));
                continue;
            };
            let url = format!("{web}/pull/{n}");
            // An explicit color: `strong()` would otherwise override the link color.
            if ui.link(style(s).color(ui.visuals().hyperlink_color)).on_hover_text(&url).clicked() {
                acts.push(Action::Url(url));
            }
        }
    });
}

/// Restore a commit's file into the working tree: as it is in `id`, or as it
/// was before (`parent`). Undoable like a discard.
fn restore_items(ui: &mut Ui, d: &RepoData, id: &str, parent: Option<&str>, c: &Change, acts: &mut Vec<Action>) {
    let short = &id[..id.len().min(7)];
    if c.code != 'D'
        && ui.button(format!("{} Restore this file from this commit", icon::ARROW_U_UP_LEFT)).on_hover_text("Overwrite the working-tree copy with this commit's version (⌘Z undoes it)").clicked()
    {
        acts.push(Action::Git(restore_file(d, format!("Restore {}", c.path), id, &c.path, &format!("the version from {short}"))));
    }
    // An added file didn't exist before; a renamed one had its old name.
    if let Some(p) = parent.filter(|_| c.code != 'A')
        && ui.button(format!("{} Restore as it was before this commit", icon::ARROW_U_UP_LEFT)).on_hover_text("Overwrite the working-tree copy with the parent's version (⌘Z undoes it)").clicked()
    {
        let path = c.old_path.as_deref().unwrap_or(&c.path);
        acts.push(Action::Git(restore_file(d, format!("Restore {path}"), p, path, &format!("the version before {short}"))));
    }
    ui.separator();
}

/// The signature badge (`%G?`) with its tooltip: what git concluded, then signer and key.
fn sig_badge(ui: &mut Ui, info: &CommitInfo) {
    let (ic, text, color, why) = match info.sig {
        'G' => (icon::SEAL_CHECK, "Good signature", theme::ADD, "Git verified the signature with a trusted key."),
        'U' => (icon::SEAL_QUESTION, "Signed, unknown signer", theme::MOD, "The signature is valid, but the key isn't trusted (GPG) or isn't in gpg.ssh.allowedSignersFile (SSH)."),
        'X' => (icon::SEAL_WARNING, "Signed, signature expired", theme::MOD, "The signature is valid but has expired."),
        'Y' => (icon::SEAL_WARNING, "Signed, key expired", theme::MOD, "The signature is valid, made by a key that has since expired."),
        'R' => (icon::SEAL_WARNING, "Signed, key revoked", theme::DEL, "The signature was made by a revoked key."),
        'B' => (icon::SEAL_WARNING, "Bad signature", theme::DEL, "The signature doesn't match the commit: it was changed after signing, or forged."),
        'E' => (icon::SEAL_QUESTION, "Signed, can't verify", theme::dim(ui), "Git couldn't check the signature: the public key isn't in your keyring (GPG), or gpg.ssh.allowedSignersFile isn't set (SSH)."),
        'N' => (icon::SEAL, "Not signed", theme::dim(ui), "This commit has no signature."),
        _ => return,
    };
    let mut tip = why.to_owned();
    for (k, v) in [("Signer", &info.signer), ("Key", &info.key)] {
        if !v.is_empty() {
            tip += &format!("\n{k}: {v}");
        }
    }
    ui.label(RichText::new(format!("{ic} {text}")).small().color(color)).on_hover_text(tip);
}

impl RepoTab {
    pub(super) fn commit_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let compare = match &self.sel {
            Some(Sel::Compare { from, to, .. }) => Some((from.clone(), to.clone())),
            _ => None,
        };
        let wt = compare.as_ref().is_some_and(|c| c.1 == crate::git::WORKTREE);
        let Some(info) = &self.info else {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        };
        let info = match info {
            Ok(i) => i,
            Err(e) => {
                ui.colored_label(theme::DEL, e);
                return;
            }
        };
        let open = self.diff.as_ref().map(|d| d.src.clone());
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if compare.is_some() { "to" } else { "commit" }).color(theme::dim(ui)));
                if wt {
                    ui.label(RichText::new("working tree").monospace());
                } else if ui.link(RichText::new(&info.id[..info.id.len().min(10)]).monospace()).on_hover_text("Copy full SHA").clicked() {
                    acts.push(Action::Copy(info.id.clone()));
                }
                if !info.parents.is_empty() {
                    ui.label(RichText::new(if compare.is_some() { "base" } else { "parent" }).color(theme::dim(ui)));
                    for p in &info.parents {
                        if ui.link(RichText::new(&p[..p.len().min(7)]).monospace()).clicked() {
                            acts.push(Action::Jump(p.clone()));
                        }
                    }
                }
            });
            ui.add_space(6.0);
            let (summary, body) = info.message.split_once('\n').unwrap_or((&info.message, ""));
            let web = self.data.as_ref().and_then(|d| d.web());
            linked(ui, summary, web.as_deref(), |s| RichText::new(s).heading().strong(), acts);
            if !body.trim().is_empty() {
                ui.add_space(4.0);
                linked(ui, body.trim(), web.as_deref(), |s| RichText::new(s), acts);
            }
            ui.add_space(6.0);
            let explain = match &compare {
                Some((from, to)) => Purpose::ExplainRange { from: from.clone(), to: to.clone() },
                None => Purpose::Explain(info.id.clone()),
            };
            match self.ai.as_ref().filter(|j| j.purpose == explain) {
                Some(job) => {
                    egui::Frame::new().fill(theme::faint(ui)).inner_margin(8.0).corner_radius(6.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} {}", icon::SPARKLE, crate::ai::name())).small().strong());
                            if job.running() {
                                ui.spinner();
                            }
                        });
                        if !job.text.is_empty() {
                            ui.label(job.text.trim());
                        }
                    });
                }
                None if crate::ai::enabled() && !wt => {
                    let label = if compare.is_some() { "Explain these changes" } else { "Explain this commit" };
                    if theme::small(ui, true, format!("{} {label}", icon::SPARKLE)).clicked() {
                        acts.push(Action::Ai(explain));
                    }
                }
                None => {}
            }
            ui.add_space(8.0);
            if compare.is_none() { ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 15.0, theme::avatar(&info.email));
                match self.avatars.get(&info.email) {
                    Some(t) => theme::photo(ui.painter(), rect.center(), 15.0, &t, 1.0),
                    None => {
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials(&info.author), egui::FontId::proportional(12.0), Color32::WHITE);
                    }
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(&info.author).strong()).on_hover_text(&info.email);
                    ui.label(RichText::new(format!("authored {}", info.date)).small().color(theme::dim(ui)));
                    if info.committer != info.author {
                        ui.label(RichText::new(format!("committed by {} {}", info.committer, info.committer_date)).small().color(theme::dim(ui)));
                    }
                    sig_badge(ui, info);
                });
            }); }
            ui.add_space(8.0);
            ui.add_space(2.0);
            let n = info.files.len() + info.untracked.len();
            ui.label(RichText::new(format!("{n} changed file{}", if n == 1 { "" } else { "s" })).small().strong());
            let stash = self.data.as_ref().filter(|d| d.stashes.iter().any(|s| s.id == info.id));
            // A stash's untracked files live in its third parent, a root commit.
            let untracked_id = format!("{}^3", info.id);
            let files = info.files.iter().map(|c| (c, &info.id, info.parents.first().cloned())).chain(info.untracked.iter().map(|c| (c, &untracked_id, None)));
            for (c, id, parent) in files {
                let src = DiffSrc::Commit { id: id.clone(), parent, change: c.clone() };
                let resp = file_row(ui, c, open.as_ref() == Some(&src), self.data.as_ref().is_some_and(|d| d.lfs.files.contains(&c.path)));
                if resp.clicked() {
                    acts.push(Action::Diff(src));
                }
                resp.context_menu(|ui| {
                    if let Some(d) = stash.filter(|_| c.code != 'D')
                        && ui.button(format!("{} Apply this file", icon::ARROW_LINE_DOWN)).on_hover_text("Write this file from the stash into the working tree; the stash is kept").clicked()
                    {
                        acts.push(Action::Git(restore_file(d, format!("Apply {} from stash", c.path), id, &c.path, "the stash's version")));
                    }
                    if let Some(d) = self.data.as_ref().filter(|_| stash.is_none() && !wt) {
                        restore_items(ui, d, &info.id, info.parents.first().map(String::as_str), c, acts);
                    }
                    file_items(ui, &c.path, acts);
                    if let Some(d) = self.data.as_ref().filter(|_| !wt) {
                        open_items(ui, d, &info.id, &c.path, c.code == 'D', acts);
                    }
                    if let Some(d) = self.data.as_ref().filter(|_| stash.is_none() && !wt && c.code != 'D') {
                        permalink_items(ui, d, &info.id, &c.path, acts);
                    }
                    if ui.button("Copy path").clicked() {
                        acts.push(Action::Copy(c.path.clone()));
                    }
                });
            }
        });
    }
}
