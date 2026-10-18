//! Inspector for a commit, stash or compare range: message, author, AI
//! explanation and the changed files.

use crate::ui::format::{keys, nothing_was_focused};
use super::tree::{self, Item};
use super::{file_items, file_row, open_items, permalink_items};
use crate::ai::Purpose;
use crate::git::{Change, CommitInfo};
use crate::repo::restore::{apply_stash_file, restore_file};
use crate::repo::{Action, DiffSrc, MsgEdit, RepoData, RepoTab, Sel};
use crate::theme::{self, icon};
use crate::ui::format::{ago, change_summary, initials};
use egui::{Color32, Label, Response, RichText, ScrollArea, Sense, TextEdit, Ui};

/// Message text with `#123` / `GH-123` linked to the pull request (or issue) on `web`.
/// The response covers the plain text (not the links), for click-to-edit.
fn linked(ui: &mut Ui, text: &str, web: Option<&str>, style: impl Fn(&str) -> RichText, acts: &mut Vec<Action>) -> Response {
    let plain = |ui: &mut Ui, s: &str| ui.add(Label::new(style(s)).sense(Sense::click()));
    let parts = crate::git::autolinks(text);
    let Some(web) = web.filter(|_| parts.iter().any(|p| p.1.is_some())) else {
        return plain(ui, text);
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut resp: Option<Response> = None;
        for (s, n) in parts {
            let Some(n) = n else {
                let r = plain(ui, s);
                resp = Some(resp.take().map_or(r.clone(), |p| p.union(r)));
                continue;
            };
            let url = format!("{web}/pull/{n}");
            // An explicit color: `strong()` would otherwise override the link color.
            if ui.link(style(s).color(ui.visuals().hyperlink_color)).on_hover_text(&url).clicked() {
                acts.push(Action::Url(url));
            }
        }
        resp
    })
    .inner
    .unwrap_or_else(|| ui.label(""))
}

/// Click-to-edit affordance for a message part: a faint highlight and pencil on hover, and a
/// tooltip that says why editing isn't possible when it isn't. True when clicked and allowed.
fn editable(ui: &mut Ui, r: &Response, why_not: Option<&str>) -> bool {
    if r.hovered() {
        let rect = r.rect.expand2(egui::vec2(4.0, 2.0));
        ui.painter().rect_filled(rect, 4.0, theme::dim(ui).gamma_multiply(0.12));
        ui.painter().text(
            egui::pos2(ui.max_rect().right() - 4.0, rect.top() + 2.0),
            egui::Align2::RIGHT_TOP,
            icon::PENCIL_SIMPLE,
            egui::FontId::proportional(13.0),
            theme::dim(ui),
        );
        ui.ctx().set_cursor_icon(if why_not.is_some() { egui::CursorIcon::NotAllowed } else { egui::CursorIcon::Text });
        egui::Tooltip::for_enabled(r).show(|ui| {
            ui.label(why_not.unwrap_or("Click to edit message"));
        });
    }
    r.clicked() && why_not.is_none()
}

/// A round author picture: the GitHub avatar when loaded, else colored initials.
fn avatar(ui: &mut Ui, avatars: &crate::repo::avatars::Avatars, name: &str, email: &str, r: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(r * 2.0, r * 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), r, theme::avatar(email));
    match avatars.get(email) {
        Some(t) => theme::photo(ui.painter(), rect.center(), r, &t, 1.0),
        None => {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials(name), egui::FontId::proportional(r * 0.8), Color32::WHITE);
        }
    }
}

/// Restore a commit's file into the working tree: as it is in `id`, or as it
/// was before (`parent`). Undoable like a discard.
fn restore_items(ui: &mut Ui, d: &RepoData, id: &str, parent: Option<&str>, c: &Change, acts: &mut Vec<Action>) {
    let short = &id[..id.len().min(7)];
    if c.code != 'D'
        && ui.button(format!("{}  Restore this file from this commit", icon::ARROW_U_UP_LEFT)).on_hover_text(keys("Overwrite the working-tree copy with this commit's version (⌘Z undoes it)")).clicked()
    {
        acts.push(Action::Git(restore_file(d, format!("Restore {}", c.path), id, &c.path, &format!("the version from {short}"))));
    }
    // An added file didn't exist before; a renamed one had its old name.
    if let Some(p) = parent.filter(|_| c.code != 'A')
        && ui.button(format!("{}  Restore as it was before this commit", icon::ARROW_U_UP_LEFT)).on_hover_text(keys("Overwrite the working-tree copy with the parent's version (⌘Z undoes it)")).clicked()
    {
        let path = c.old_path.as_deref().unwrap_or(&c.path);
        acts.push(Action::Git(restore_file(d, format!("Restore {path}"), p, path, &format!("the version before {short}"))));
    }
    ui.separator();
}

/// The signature badge (`%G?`) with its tooltip: what git concluded, then signer and key.
fn sig_badge(ui: &mut Ui, info: &CommitInfo) {
    let (ic, text, color, why) = match info.sig {
        'G' => (icon::SEAL_CHECK, "Good signature", theme::add(), "Git verified the signature with a trusted key."),
        'U' => (icon::SEAL_QUESTION, "Signed, unknown signer", theme::changed(), "The signature is valid, but the key isn't trusted (GPG) or isn't in gpg.ssh.allowedSignersFile (SSH)."),
        'X' => (icon::SEAL_WARNING, "Signed, signature expired", theme::changed(), "The signature is valid but has expired."),
        'Y' => (icon::SEAL_WARNING, "Signed, key expired", theme::changed(), "The signature is valid, made by a key that has since expired."),
        'R' => (icon::SEAL_WARNING, "Signed, key revoked", theme::del(), "The signature was made by a revoked key."),
        'B' => (icon::SEAL_WARNING, "Bad signature", theme::del(), "The signature doesn't match the commit: it was changed after signing, or forged."),
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
    pub(super) fn commit_panel(&mut self, ui: &mut Ui, as_tree: &mut bool, acts: &mut Vec<Action>) {
        let compare = match &self.sel {
            Some(Sel::Compare { from, to, three_dot }) => Some((from.clone(), to.clone(), *three_dot)),
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
                ui.colored_label(theme::del(), e);
                return;
            }
        };
        let busy = self.busy().is_some();
        let open = self.diff.as_ref().map(|d| d.src.clone());
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if compare.is_some() { "to" } else { "commit" }).color(theme::dim(ui)));
                if wt {
                    ui.label(RichText::new("working tree").monospace());
                } else {
                    let short = &info.id[..info.id.len().min(7)];
                    if ui.link(RichText::new(short).monospace()).on_hover_text("Copy short SHA").clicked() {
                        acts.push(Action::Copy(short.into()));
                    }
                    if ui.link(icon::COPY).on_hover_text(format!("Copy full SHA\n{}", info.id)).clicked() {
                        acts.push(Action::Copy(info.id.clone()));
                    }
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
            if self.edit_msg.as_ref().is_some_and(|e| e.id == info.id && !e.quiet) && compare.is_none() {
                msg_editor(ui, &mut self.edit_msg, busy, acts);
            } else {
                // Eligibility is only worked out while the message is hovered or clicked.
                let why_not = |hit: &Response| match (&compare, hit.hovered() || hit.clicked()) {
                    (Some(_), _) | (_, false) => None,
                    _ if busy => Some("Wait for the running operation to finish."),
                    _ => self.data.as_ref().and_then(|d| d.reword_path(&info.id).err()),
                };
                let mut edit = false;
                let r = linked(ui, summary, web.as_deref(), |s| RichText::new(s).heading().strong(), acts);
                edit |= compare.is_none() && editable(ui, &r, why_not(&r));
                if !body.trim().is_empty() {
                    ui.add_space(4.0);
                    let body = body.trim();
                    let more_id = egui::Id::new(("msg_more", &info.id));
                    let open = ui.data(|d| d.get_temp(more_id).unwrap_or(false));
                    let (font, width) = (egui::TextStyle::Body.resolve(ui.style()), ui.available_width());
                    let g = ui.fonts_mut(|f| f.layout(body.to_owned(), font, Color32::PLACEHOLDER, width));
                    let rows = g.rows.len();
                    let r = if rows <= 5 || open {
                        linked(ui, body, web.as_deref(), |s| RichText::new(s), acts)
                    } else {
                        let at = |n: usize| {
                            let c: usize = g.rows[..n].iter().map(|r| r.char_count_including_newline().0).sum();
                            body.char_indices().nth(c).map_or(body.len(), |(i, _)| i)
                        };
                        let (mut a, mut b) = (at(3), at(4));
                        if body[a..b].trim().is_empty() {
                            (a, b) = (at(2), at(3));
                        }
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let faded = ui.visuals().text_color().gamma_multiply(0.35);
                            let r = linked(ui, body[..a].trim_end(), web.as_deref(), |s| RichText::new(s), acts);
                            r.union(linked(ui, body[a..b].trim_end(), web.as_deref(), |s| RichText::new(s).color(faded), acts))
                        })
                        .inner
                    };
                    edit |= compare.is_none() && editable(ui, &r, why_not(&r));
                    if rows > 5 {
                        let label = if open { "Show less".to_owned() } else { format!("Show more ({} more lines)", rows - 4) };
                        if ui.link(RichText::new(label).small()).clicked() {
                            ui.data_mut(|d| d.insert_temp(more_id, !open));
                        }
                    }
                }
                if edit && let Some(at) = self.data.as_ref().and_then(|d| d.reword_path(&info.id).ok()) {
                    self.edit_msg = Some(MsgEdit::new(&info.id, &info.message, at));
                }
            }
            ui.add_space(6.0);
            let explain = match &compare {
                Some((from, to, three_dot)) => Purpose::ExplainRange { from: from.clone(), to: to.clone(), three_dot: *three_dot },
                None => Purpose::Explain(info.id.clone()),
            };
            match self.ai.as_ref().filter(|j| j.purpose == explain) {
                Some(job) => {
                    egui::Frame::new().fill(theme::faint(ui)).inner_margin(8.0).corner_radius(6.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}  {}", icon::SPARKLE, crate::ai::name())).small().strong());
                            if job.running() {
                                ui.spinner();
                                if theme::small(ui, true, "Cancel").clicked() {
                                    acts.push(Action::AiCancel);
                                }
                            }
                        });
                        if !job.text.is_empty() {
                            ui.label(job.text.trim());
                        }
                    });
                }
                None if crate::ai::enabled() && !wt => {
                    let label = if compare.is_some() { "Explain these changes" } else { "Explain this commit" };
                    if theme::small(ui, true, format!("{}  {label}", icon::SPARKLE)).clicked() {
                        acts.push(Action::Ai(explain));
                    }
                }
                None => {}
            }
            ui.add_space(8.0);
            if compare.is_none() {
                ui.horizontal(|ui| {
                    avatar(ui, &self.avatars, &info.author, &info.email, 15.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&info.author).strong()).on_hover_text(&info.email);
                        ui.label(RichText::new(format!("authored {} · {}", info.date, ago(info.time))).small().color(theme::dim(ui)));
                        if info.committer != info.author || info.committer_email != info.email {
                            ui.label(RichText::new(format!("committed by {} {} · {}", info.committer, info.committer_date, ago(info.committer_time))).small().color(theme::dim(ui)))
                                .on_hover_text(&info.committer_email);
                        }
                        sig_badge(ui, info);
                    });
                });
                for (k, (name, email)) in info.coauthors.iter().enumerate() {
                    let without = crate::repo::message::remove_coauthor(&info.message, name, email);
                    let hover_id = ui.id().with(("coauthor", &info.id, email));
                    let hot = ui.data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
                    let why = || {
                        if busy {
                            Some("Wait for the running operation to finish.")
                        } else if self.edit_msg.as_ref().is_some_and(|e| e.id == info.id && !e.quiet) {
                            Some("Save or cancel the message edit first.")
                        } else {
                            self.data.as_ref().and_then(|d| d.reword_path(&info.id).err())
                        }
                    };
                    let mut remove = false;
                    let row = theme::keyed(ui, ("coauthor", &info.id, k), |ui| ui.horizontal(|ui| {
                        avatar(ui, &self.avatars, name, email, 9.0);
                        let label = ui.add(Label::new(RichText::new(format!("co-authored by {name}")).small().color(theme::dim(ui))).sense(Sense::click())).on_hover_text(email);
                        if without.is_none() {
                            return;
                        }
                        let x = ui.add_visible(hot, Label::new(RichText::new(icon::X).small().color(theme::dim(ui))).sense(Sense::click()));
                        let reason = if x.hovered() { why() } else { None };
                        if x.hovered() {
                            ui.ctx().set_cursor_icon(if reason.is_some() { egui::CursorIcon::NotAllowed } else { egui::CursorIcon::PointingHand });
                        }
                        let x = x.on_hover_text(reason.unwrap_or("Remove this co-author from the commit message"));
                        remove |= x.clicked() && why().is_none();
                        label.context_menu(|ui| {
                            let why = why();
                            let b = ui.add_enabled(why.is_none(), egui::Button::new(format!("{}  Remove co-author", icon::X)));
                            if b.on_hover_text("Rewrites the commit message without this Co-authored-by line").on_disabled_hover_text(why.unwrap_or_default()).clicked() {
                                remove = true;
                                ui.close();
                            }
                        });
                    }));
                    let over = ui.rect_contains_pointer(row.response.rect);
                    ui.data_mut(|d| d.insert_temp(hover_id, over));
                    if let Some(message) = without.filter(|_| remove) {
                        if let Some(path) = self.data.as_ref().and_then(|d| d.reword_path(&info.id).ok()) {
                            self.edit_msg = Some(MsgEdit::quiet(&info.id, &info.message, path));
                        }
                        acts.push(Action::Reword { commit: info.id.clone(), message });
                    }
                }
            }
            ui.add_space(8.0);
            ui.add_space(2.0);
            let n = info.files.len() + info.untracked.len();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{n} changed file{}", if n == 1 { "" } else { "s" })).small().strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| tree::toggle(ui, as_tree));
            });
            let stash = self.data.as_ref().filter(|d| d.stashes.iter().any(|s| s.id == info.id));
            let mut summary = change_summary(info.files.iter().chain(&info.untracked));
            let (add, del) = info.stats.values().flatten().fold((0, 0), |t, s| (t.0 + s.0, t.1 + s.1));
            if add + del > 0 {
                summary += &format!(" · +{add} -{del}");
            }
            ui.label(RichText::new(summary).small().color(theme::dim(ui)));
            if compare.is_none() && stash.is_none() && info.parents.len() > 1 {
                ui.label(RichText::new("Merge commit: changes are against the first parent").small().color(theme::dim(ui)));
            }
            // A stash's untracked files live in its third parent, a root commit.
            let untracked_id = format!("{}^3", info.id);
            let parent = info.parents.first().cloned();
            let file = |ui: &mut Ui, c: &Change, depth: Option<usize>, acts: &mut Vec<Action>| {
                let (id, parent) = if info.untracked.contains(c) { (&untracked_id, None) } else { (&info.id, parent.clone()) };
                // A deleted file is blamed as it was just before.
                let blame_rev = if c.code == 'D' { parent.clone() } else { Some(id.clone()) };
                let src = DiffSrc::Commit { id: id.clone(), parent, change: c.clone() };
                let resp = theme::keyed(ui, ("commit-file", id, &c.path), |ui| file_row(ui, c, open.as_ref() == Some(&src), self.data.as_ref().is_some_and(|d| d.lfs.files.contains(&c.path)), info.stats.get(&c.path).copied(), depth));
                if resp.clicked() {
                    acts.push(Action::Diff(src));
                }
                resp.context_menu(|ui| {
                    if let Some(d) = stash.filter(|_| c.code != 'D')
                        && ui.button(format!("{}  Apply this file", icon::ARROW_LINE_DOWN)).on_hover_text("Write this file from the stash into the working tree; the stash is kept").clicked()
                    {
                        let label = format!("Apply {} from stash", c.path);
                        acts.push(Action::Git(if *id == untracked_id { restore_file(d, label, id, &c.path, "the stash's version") } else { apply_stash_file(d, label, id, &c.path) }));
                    }
                    if let Some(d) = self.data.as_ref().filter(|_| stash.is_none() && !wt) {
                        restore_items(ui, d, &info.id, info.parents.first().map(String::as_str), c, acts);
                    }
                    let at = if stash.is_some() { info.parents.first() } else { Some(&info.id) };
                    file_items(ui, &c.path, &c.path, at.map(String::as_str), blame_rev.as_deref(), acts);
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
            };
            let all = info.files.iter().chain(&info.untracked);
            if *as_tree {
                let closed = self.tree_closed.entry("commit").or_default();
                tree::show(ui, all, self.info_gen, ui.id().with(("filetree", "commit")), closed, false, |ui, item| {
                    match item {
                        Item::File { c, depth, .. } => file(ui, c, depth, acts),
                        Item::Dir { path, resp, .. } => {
                            resp.context_menu(|ui| {
                                if ui.button(format!("{}  Folder history", icon::FOLDER_SIMPLE)).on_hover_text(format!("Commits that touched {path}/")).clicked() {
                                    let at = if stash.is_some() { info.parents.first() } else { Some(&info.id) };
                                    acts.push(Action::FileView(format!("{path}/"), crate::repo::history::Mode::History(at.filter(|_| !wt).cloned())));
                                }
                                if ui.button("Copy path").clicked() {
                                    acts.push(Action::Copy(path.into()));
                                }
                            });
                        }
                    }
                    false
                });
                return;
            }
            // Only the rows in view are built, so a 10k-file commit stays cheap.
            let all: Vec<&Change> = all.collect();
            let total = all.len();
            let step = 24.0 + ui.spacing().item_spacing.y;
            let (top, clip) = (ui.cursor().top(), ui.clip_rect());
            let first = (((clip.top() - top) / step).floor().max(0.0) as usize).min(total);
            let last = (((clip.bottom() - top) / step).ceil().max(0.0) as usize + 1).min(total);
            ui.add_space(first as f32 * step);
            for c in &all[first..last] {
                file(ui, c, None, acts);
            }
            ui.add_space((total - last) as f32 * step);
        });
    }
}

/// The inline message editor: summary (72-char hint) and body, Save / Cancel, Esc / ⌘↩.
fn msg_editor(ui: &mut Ui, edit: &mut Option<MsgEdit>, busy: bool, acts: &mut Vec<Action>) {
    let Some(e) = edit else { return };
    let (sum_id, body_id) = (ui.id().with(("msg_summary", &e.id)), ui.id().with(("msg_body", &e.id)));
    let next = ui.horizontal(|ui| {
        let n = e.summary.chars().count();
        let w = ui.available_width() - 30.0;
        let field = ui.add(TextEdit::singleline(&mut e.summary).id(sum_id).interactive(!busy).margin(theme::INPUT_MARGIN).hint_text("Summary").desired_width(w));
        if std::mem::take(&mut e.focus) {
            field.request_focus();
        }
        ui.label(RichText::new(n.to_string()).small().color(if n > 72 { theme::del() } else { theme::dim(ui) }));
        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    })
    .inner;
    theme::description(ui, &mut e.body, body_id, !busy);
    if next {
        ui.memory_mut(|m| m.request_focus(body_id));
    }
    let can = !busy && !e.summary.trim().is_empty();
    let (mut save, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        save = theme::small(ui, can, "Save").on_hover_text(keys("Save the message (⌘↩)")).clicked();
        cancel = theme::small(ui, true, "Cancel").on_hover_text("Discard the edit (Esc)").clicked();
    });
    // Not while a dialog is open: Esc and ⌘↩ there belong to the dialog.
    let (free, focus, held) = ui.ctx().memory(|m| (m.top_modal_layer().is_none(), m.focused(), m.had_focus_last_frame(sum_id) || m.had_focus_last_frame(body_id)));
    if free && (held || nothing_was_focused(ui.ctx())) {
        cancel |= ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    }
    if free && focus.is_none_or(|f| f == sum_id || f == body_id) {
        save |= can && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
    }
    if cancel {
        *edit = None;
    } else if save && can {
        if e.changed() {
            acts.push(Action::Reword { commit: e.id.clone(), message: e.message() });
        } else {
            *edit = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Id, Key, Modifiers, RawInput};

    /// Esc cancels the edit while one of its fields or none has the keyboard, ⌘↩ saves it then too:
    /// another field (the graph search, the terminal) keeps both keys, and a dialog keeps them from both.
    #[test]
    fn esc_and_enter_belong_to_the_focused_field() {
        let ctx = egui::Context::default();
        let other = Id::new("other_field");
        let (mut n, dialog) = (0.0, std::cell::Cell::new(false));
        let mut frame = |edit: &mut Option<MsgEdit>, key: Option<(Key, Modifiers)>, focus_other: bool| {
            n += 0.05;
            let mods = key.map_or(Modifiers::NONE, |k| k.1);
            let events = [Event::ModifiersChanged(mods)].into_iter().chain(key.map(|(key, modifiers)| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers })).collect();
            let raw = RawInput { time: Some(n), events, ..Default::default() };
            let mut acts = vec![];
            let mut out = ctx.run_ui(raw, |ui| {
                msg_editor(ui, edit, false, &mut acts);
                let mut text = String::new();
                let field = ui.add(TextEdit::singleline(&mut text).id(other));
                if focus_other {
                    field.request_focus();
                }
                if dialog.get() {
                    egui::Modal::new(Id::new("dialog")).show(ui.ctx(), |ui| ui.label("Confirm"));
                }
                crate::ui::format::note_focus(ui.ctx());
            });
            out.textures_delta.clear();
            acts
        };
        let esc = Some((Key::Escape, Modifiers::NONE));
        let save = Some((Key::Enter, Modifiers::COMMAND));
        let fresh = || {
            let mut e = MsgEdit::new("c1", "one", (None, vec![]));
            e.summary = "two".into();
            Some(e)
        };

        let mut edit = fresh();
        frame(&mut edit, None, false);
        frame(&mut edit, esc, false);
        assert!(edit.is_none(), "Esc in the editor's own field cancels");

        let mut edit = fresh();
        frame(&mut edit, None, false);
        frame(&mut edit, None, true);
        frame(&mut edit, esc, true);
        assert!(edit.is_some(), "Esc that leaves another field isn't the editor's");
        assert!(frame(&mut edit, save, true).is_empty(), "⌘↩ in another field isn't either");

        let mut edit = fresh();
        frame(&mut edit, None, false);
        ctx.memory_mut(|m| m.stop_text_input());
        dialog.set(true);
        for _ in 0..3 {
            frame(&mut edit, None, false);
        }
        frame(&mut edit, esc, false);
        assert!(edit.is_some(), "Esc with nothing focused belongs to a dialog that is open");
        dialog.set(false);
        frame(&mut edit, None, false);
        frame(&mut edit, None, false);
        frame(&mut edit, esc, false);
        assert!(edit.is_none(), "Esc with nothing focused cancels the edit");

        let mut edit = fresh();
        frame(&mut edit, None, false);
        let acts = frame(&mut edit, save, false);
        assert!(matches!(&acts[..], [Action::Reword { commit, message }] if commit == "c1" && message == "two"));

        frame(&mut edit, None, false);
        ctx.memory_mut(|m| m.stop_text_input());
        assert_eq!(frame(&mut edit, save, false).len(), 1, "⌘↩ with nothing focused saves");
    }
}
