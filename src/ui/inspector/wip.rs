//! Inspector for the WIP row: operation-in-progress banner, conflicted /
//! unstaged / staged file lists and the commit box.

use crate::ui::format::keys;
use super::tree::{self, Item};
use super::{file_items, file_row, lfs_track_item, open_items, permalink_items, row_button};
use crate::ai::Purpose;
use crate::git::{Change, State};
use crate::repo::{Action, DiffSrc, Op, RepoData, continue_op, RepoTab, Undo, discard_files_op, identity, prompt, sequencer_op, stage_files_op, unstage_files_op};
use crate::theme::{self, icon};
use egui::{Align, Layout, RichText, ScrollArea, TextEdit, Ui};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// "Stash this file" (or the selected files): their staged and unstaged changes (a rename
/// with its old path), ⌘Z pops them back. True when clicked.
fn stash_files_item(ui: &mut Ui, cs: &[Change], staged: bool, acts: &mut Vec<Action>) -> bool {
    let what = if cs.len() == 1 { "this file".to_owned() } else { format!("{} files", cs.len()) };
    // git can't stash a path that's gone from the index (it fails, perhaps after saving a stash).
    let gone = staged && cs.iter().any(|c| matches!(c.code, 'D' | 'R'));
    let clicked = ui
        .add_enabled(!gone, egui::Button::new(format!("{}  Stash {what}", icon::TRAY_ARROW_DOWN)))
        .on_disabled_hover_text("git can't stash a staged deletion or rename by path: stash all changes instead.")
        .clicked();
    if clicked {
        let paths: Vec<&str> = cs.iter().flat_map(|c| c.old_path.iter().filter(|_| c.code == 'R').chain([&c.path])).map(String::as_str).collect();
        acts.push(Action::Git(crate::repo::stash_paths_op(&paths, cs.iter().any(|c| c.code == '?'))));
    }
    clicked
}

fn gitignore_item(ui: &mut Ui, dir: &std::path::Path, path: &str, acts: &mut Vec<Action>) {
    if !ui.button("Add to .gitignore").clicked() {
        return;
    }
    match crate::repo::files::append_gitignore(dir, path) {
        Ok(()) => acts.push(Action::Reload),
        Err(e) => acts.push(Action::Error(format!("Could not update .gitignore: {e}"))),
    }
}

/// The rest of a WIP folder row's menu: stash, .gitignore (an untracked folder), file manager,
/// history and path. True when the stash was picked (the selection goes with it).
fn dir_items(ui: &mut Ui, dir: &std::path::Path, path: &str, files: &[Change], staged: bool, acts: &mut Vec<Action>) -> bool {
    let stashed = stash_files_item(ui, files, staged, acts);
    let untracked = files.iter().all(|c| c.code == '?');
    if untracked {
        gitignore_item(ui, dir, path, acts);
    }
    if ui.button("Show in file manager").clicked() {
        acts.push(Action::Reveal(path.into()));
    }
    if !untracked && ui.button(format!("{}  Folder history", icon::FOLDER_SIMPLE)).on_hover_text(format!("Commits that touched {path}/")).clicked() {
        acts.push(Action::FileView(format!("{path}/"), crate::repo::history::Mode::History));
    }
    if ui.button("Copy path").clicked() {
        acts.push(Action::Copy(path.into()));
    }
    stashed
}

/// Up to ten paths, one per line, and how many more.
fn shown(v: &[&str]) -> String {
    format!("{}{}", v[..v.len().min(10)].join("\n"), if v.len() > 10 { format!("\n…and {} more", v.len() - 10) } else { String::new() })
}

/// Mark the conflicted files `cs` resolved, only those without conflict markers (asking first
/// when some are left alone). None when none of them can be.
fn mark_resolved_op(label: &str, d: &RepoData, cs: &[Change]) -> Option<Op> {
    let res: std::collections::HashSet<&str> = d.resolvable.iter().map(String::as_str).collect();
    let (ok, rest): (Vec<&str>, Vec<&str>) = cs.iter().map(|c| c.path.as_str()).partition(|p| res.contains(p));
    if ok.is_empty() {
        return None;
    }
    let op = crate::repo::merge_editor::stage_many(label, &ok);
    Some(if rest.is_empty() { op } else { op.confirm(format!("Mark {} file(s) resolved? Not marked (conflict markers left, or a side to choose):\n{}", ok.len(), shown(&rest))) })
}

/// The external merge tools open on conflicted files, each with Cancel.
fn open_tools(ui: &mut Ui, tools: &[crate::repo::tools::Open], acts: &mut Vec<Action>) {
    for tool in tools.iter().filter(|o| o.kind == "merge") {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(format!("Merge tool open on {}", tool.path)).small());
            let b = theme::small(ui, true, format!("{}  Cancel", icon::STOP_CIRCLE));
            if b.on_hover_text(keys("Stop the merge tool. Changes it saved stay (⌘Z restores the file as before).")).clicked() {
                acts.push(Action::CloseTool(tool.path.clone()));
            }
        });
    }
}

/// Menu of people from the loaded log to credit with `Co-authored-by` trailers.
fn coauthors_menu(ui: &mut Ui, d: &RepoData, coauthors: &mut Vec<(String, String)>) {
    {
        let n = coauthors.len();
        let keep_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let button = egui::containers::menu::MenuButton::new(if n == 0 { "Co-authors".to_owned() } else { format!("Co-authors ({n})") }).config(keep_open);
        button.ui(ui, |ui| {
            let q = ui.id().with(("coauthor-filter", &d.git_dir));
            // Most active first, cached per loaded log: the scan is too slow for every frame.
            let log = Arc::downgrade(&d.log);
            let people: Arc<Vec<(String, String)>> = ui.data_mut(|m| {
                let cached = m.get_temp_mut_or_insert_with(q.with("people"), || (Weak::new(), Arc::default()));
                if !cached.0.ptr_eq(&log) {
                    let mut by: HashMap<&str, (&str, usize)> = HashMap::new();
                    for c in d.log.iter() {
                        by.entry(c.email.as_str()).or_insert((c.author.as_str(), 0)).1 += 1;
                    }
                    let mut v: Vec<_> = by.into_iter().filter(|(e, _)| !e.is_empty() && *e != d.author.1).collect();
                    v.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(b.0)));
                    *cached = (log, Arc::new(v.into_iter().map(|(e, (n, _))| (n.to_owned(), e.to_owned())).collect::<Vec<_>>()));
                }
                cached.1.clone()
            });
            let mut filter: String = ui.data_mut(|m| m.get_temp(q).unwrap_or_default());
            ui.add(TextEdit::singleline(&mut filter).hint_text("Find a person, or type Name <email>").desired_width(260.0));
            let f = filter.to_lowercase();
            let typed = crate::repo::message::parse_person(&filter).filter(|p| !coauthors.contains(p) && !people.contains(p));
            let mut filter = filter;
            if let Some(p) = typed
                && ui.button(format!("{}  Add {} <{}>", icon::PLUS, p.0, p.1)).clicked()
            {
                coauthors.push(p);
                filter.clear();
            }
            ui.data_mut(|m| m.insert_temp(q, filter));
            let shown = people.iter().filter(|(n, e)| f.is_empty() || n.to_lowercase().contains(&f) || e.to_lowercase().contains(&f)).take(12);
            // Picked people stay listed even when the filter hides them.
            let list: Vec<(String, String)> = coauthors.iter().cloned().chain(shown.filter(|p| !coauthors.contains(p)).cloned()).collect();
            for p in &list {
                let mut on = coauthors.contains(p);
                if ui.checkbox(&mut on, format!("{} <{}>", p.0, p.1)).changed() {
                    coauthors.retain(|c| c != p);
                    if on {
                        coauthors.push(p.clone());
                    }
                }
            }
        })
        .0
        .on_hover_text("Adds a Co-authored-by trailer for each person (GitHub credits them)");
    }
}

/// Who commits here (effective user.name / user.email), and a warning with a fix when a
/// Settings rule expects another email domain for one of the remotes' hosts.
fn identity_row(ui: &mut Ui, d: &RepoData, rules: &str, acts: &mut Vec<Action>) {
    let user = (d.cfg("user.name").unwrap_or_default(), d.cfg("user.email").unwrap_or_default());
    let (name, email) = if d.author.1.is_empty() { user } else { (d.author.0.as_str(), d.author.1.as_str()) };
    // author.* (or GIT_AUTHOR_*) overrides user.*: setting user.* would change nothing.
    let key = |what: &str| if (what == "name" && name != user.0) || (what == "email" && email != user.1) { format!("author.{what}") } else { format!("user.{what}") };
    let set = |what: &str, v: &str| prompt(format!("Commit {what} for this repository"), v, Op::new(format!("Set {}", key(what)), &["config", "--local", &key(what), "{}"]));
    let who = if email.is_empty() { format!("{name} (no email set)") } else { format!("{name} <{email}>") };
    let r = ui
        .add(egui::Label::new(RichText::new(format!("{}  {}", icon::USER_CIRCLE, who.trim())).small().color(theme::dim(ui))).truncate().sense(egui::Sense::click()))
        .on_hover_text("Commits are made as this identity (git config user.* / author.*, or GIT_AUTHOR_* in the environment).\nClick to change the email for this repository; right-click for the name.");
    if r.clicked() {
        acts.push(set("email", email));
    }
    r.context_menu(|ui| {
        if ui.button("Set name for this repository…").clicked() {
            acts.push(set("name", name));
        }
        if ui.button("Set email for this repository…").clicked() {
            acts.push(set("email", email));
        }
    });
    let hosts = d.remotes.iter().filter_map(|r| crate::git::host(&r.1));
    let Some((host, dom)) = identity::mismatch(email, hosts, &identity::rules(rules)) else { return };
    egui::Frame::new().fill(theme::changed().gamma_multiply(0.18)).inner_margin(6.0).corner_radius(6.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(format!("{}  {host} expects an @{dom} email", icon::WARNING)).small().strong().color(theme::changed()));
        ui.horizontal(|ui| {
            if theme::small(ui, true, "Set for this repo…").on_hover_text(format!("git config --local {}", key("email"))).clicked() {
                acts.push(set("email", &identity::suggest(email, &dom)));
            }
            ui.label(RichText::new("or commit anyway").small().color(theme::dim(ui)));
        });
    });
}

/// A click on row `i` of a WIP list (`staged` picks which): a plain click selects just it (true: open
/// its diff), ⌘ toggles it, ⇧ selects the range from the selection's first row of that list.
fn pick_file(sel: &mut Vec<(bool, String)>, list: &[Change], staged: bool, i: usize, m: egui::Modifiers) -> bool {
    let path = list[i].path.clone();
    if m.shift {
        let from = sel.first().filter(|(s, _)| *s == staged).and_then(|(_, p)| list.iter().position(|c| c.path == *p)).unwrap_or(i);
        let anchor = list[from].path.clone();
        *sel = list[from.min(i)..=from.max(i)].iter().map(|c| (staged, c.path.clone())).collect();
        sel.sort_by_key(|(_, p)| *p != anchor);
    } else if m.command {
        sel.retain(|(s, _)| *s == staged);
        match sel.iter().position(|(_, p)| *p == path) {
            Some(k) => drop(sel.remove(k)),
            None => sel.push((staged, path)),
        }
    } else {
        *sel = vec![(staged, path)];
        return true;
    }
    false
}

/// The WIP selection resolved against the status, rebuilt only when either changes (a 10k-file
/// select-all must not hash the whole selection every frame).
struct SelCache {
    key: (u64, usize, u64),
    paths: [std::collections::HashSet<String>; 2],
    un: Vec<Change>,
    st: Vec<Change>,
}

impl SelCache {
    fn get(ui: &Ui, id: egui::Id, d: &RepoData, sel: &[(bool, String)], edits: u64) -> Arc<Self> {
        let key = (d.generation, sel.len(), edits);
        if let Some(c) = ui.data(|x| x.get_temp::<Arc<Self>>(id)).filter(|c| c.key == key) {
            return c;
        }
        let mut paths: [std::collections::HashSet<String>; 2] = Default::default();
        for (s, p) in sel {
            paths[*s as usize].insert(p.clone());
        }
        let pick = |list: &[Change], staged: bool| list.iter().filter(|c| paths[staged as usize].contains(&c.path)).cloned().collect();
        let (un, st) = (pick(&d.status.unstaged, false), pick(&d.status.staged, true));
        let c = Arc::new(Self { key, paths, un, st });
        ui.data_mut(|x| x.insert_temp(id, c.clone()));
        c
    }

    fn has(&self, staged: bool, path: &str) -> bool {
        self.paths[staged as usize].contains(path)
    }
}

/// The changes of `list` that are selected (`staged` picks the list), in one pass over both.
fn selected(sel: &[(bool, String)], list: &[Change], staged: bool) -> Vec<Change> {
    if sel.is_empty() {
        return vec![];
    }
    let set: std::collections::HashSet<(bool, &str)> = sel.iter().map(|(s, p)| (*s, p.as_str())).collect();
    list.iter().filter(|c| set.contains(&(staged, c.path.as_str()))).cloned().collect()
}

impl RepoTab {
    /// S / U with WIP files selected: stage the selected unstaged files / unstage the selected staged ones.
    pub fn file_key(&mut self, stage: bool) -> Option<Action> {
        let d = self.data.as_ref().filter(|_| self.sel == Some(crate::repo::Sel::Wip) && self.multi.len() < 2 && self.busy().is_none())?;
        // The rebase editor and other center views have their own S / U / letter keys.
        if self.rebase.is_some() || self.merge.is_some() || self.file_view.is_some() || self.reflog.is_some() {
            return None;
        }
        let list = if stage { &d.status.unstaged } else { &d.status.staged };
        let cs: Vec<Change> = selected(&self.file_sel, list, !stage);
        let op = if stage { stage_files_op("Stage", &cs) } else { unstage_files_op(&cs) }?;
        self.file_sel.clear();
        Some(Action::Git(op))
    }

    /// Guided split: Continue / Abort, above whatever the inspector shows.
    pub(super) fn split_banner(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(sp) = self.split.as_ref().filter(|sp| !sp.replaying) else { return };
        let busy = self.busy().is_some();
        egui::Frame::new().fill(theme::accent().gamma_multiply(0.2)).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{}  Splitting {}", icon::SCISSORS, &sp.id[..7.min(sp.id.len())])).strong());
            ui.label("Stage a part and commit it, repeat, then Continue.");
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("Continue")).on_hover_text("Replay the rest of the branch on top of the parts").clicked() {
                    acts.push(Action::SplitEnd { abort: false });
                }
                if ui.add_enabled(!busy, egui::Button::new("Abort")).on_hover_text("Put the original commit back").clicked() {
                    acts.push(Action::SplitEnd { abort: true });
                }
            });
        });
        ui.add_space(6.0);
    }

    pub(super) fn wip_panel(&mut self, ui: &mut Ui, as_tree: &mut bool, acts: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let s = &d.status;
        let open = self.diff.as_ref().map(|d| d.src.clone());
        let busy = self.busy().is_some();
        let head = s.head.as_deref().unwrap_or_default();
        let sc = SelCache::get(ui, ui.id().with((&self.dir, "wipsel")), d, &self.file_sel, self.file_sel_gen);

        // A bisect has its own banner above the graph.
        // A guided split has its own banner (`split_banner`), shown for any selection.
        if self.split.as_ref().is_none_or(|sp| sp.replaying) && !matches!(d.state, State::Clean | State::Bisecting) {
            let (what, cmd) = match d.state {
                State::Merging => ("Merge", "merge"),
                State::Rebasing => ("Rebase", "rebase"),
                State::CherryPicking => ("Cherry-pick", "cherry-pick"),
                State::Reverting => ("Revert", "revert"),
                _ => ("Patch (git am)", "am"),
            };
            egui::Frame::new().fill(theme::changed().gamma_multiply(0.2)).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.label(RichText::new(format!("{what} in progress")).strong());
                if !s.conflicted.is_empty() {
                    ui.label(format!("{} conflicted file(s) — resolve, then continue.", s.conflicted.len()));
                }
                open_tools(ui, &self.ext_tools, acts);
                ui.horizontal(|ui| {
                    let cont = ui.add_enabled(s.conflicted.is_empty(), egui::Button::new("Continue"));
                    if cont.on_disabled_hover_text(format!("{} file(s) still have unresolved conflicts: resolve them (or mark them resolved) first", s.conflicted.len())).clicked() {
                        acts.push(Action::Git(continue_op(cmd)));
                    }
                    // An empty step (its changes are already in HEAD) can't --continue.
                    // At an `edit` stop the commit is already applied: skip would only discard work.
                    let skip = sequencer_op(cmd, false);
                    if cmd != "merge" && !d.git_dir.join("rebase-merge/amend").exists() && ui.button(&skip.label).clicked() {
                        acts.push(Action::Git(skip));
                    }
                    if ui.button("Abort").clicked() {
                        acts.push(Action::Git(sequencer_op(cmd, true)));
                    }
                });
            });
            ui.add_space(6.0);
        }

        let generating = matches!(self.ai_busy(), Some(Purpose::CommitMessage { .. }));
        let ai_on = crate::ai::enabled();
        let (sum_id, body_id) = (egui::Id::new(("commit_summary", &self.dir)), egui::Id::new(("commit_description", &self.dir)));
        let (mut commit, mut stage_commit) = (false, false);
        egui::Panel::bottom("commit_box").show_separator_line(false).frame(egui::Frame::NONE.inner_margin(egui::Margin { top: 8, ..Default::default() })).resizable(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Commit").small().strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let absorb = !s.staged.is_empty() && d.state == State::Clean;
                    if theme::small(ui, absorb, format!("{}  Absorb", icon::ARROWS_IN))
                        .on_hover_text("Turn staged hunks into fixups of the unpushed commits they belong to (preview first)")
                        .on_disabled_hover_text("Stage changes first (not during a merge or rebase)")
                        .clicked()
                    {
                        acts.push(Action::Absorb);
                    }
                    if ai_on && generating {
                        ui.spinner();
                        ui.label(RichText::new("Writing…").small().color(theme::dim(ui)));
                        if theme::small(ui, true, "Cancel").clicked() {
                            acts.push(Action::AiCancel);
                        }
                    } else if ai_on && theme::small(ui, !s.staged.is_empty() || self.amend, format!("{}  Generate", icon::SPARKLE))
                        .on_hover_text(if self.amend {
                            format!("Write the amended commit's message (its changes plus the staged ones) with {}", crate::ai::name())
                        } else {
                            format!("Write the commit message from the staged changes with {}", crate::ai::name())
                        })
                        .on_disabled_hover_text("Stage changes first")
                        .clicked()
                    {
                        acts.push(Action::Ai(Purpose::CommitMessage { amend: self.amend }));
                    }
                });
            });
            // A rebase or bisect detaches HEAD on purpose; only a plain detached checkout loses commits.
            if s.branch.is_none() && s.head.is_some() && d.state == State::Clean {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{}  Detached HEAD: commits made here are easy to lose.", icon::WARNING)).small().color(theme::changed()));
                    if theme::pill(ui, format!("{}  Create branch here…", icon::GIT_BRANCH)).on_hover_text("A new branch at HEAD; your staged changes come along").clicked() {
                        acts.push(prompt("New branch at HEAD", "", Op::new("Create branch", &["checkout", "-b", "{}"]).undo(Undo::Keep)));
                    }
                });
            }
            identity_row(ui, d, &self.email_rules, acts);
            let next = ui.horizontal(|ui| {
                let n = self.summary.chars().count();
                let r = ui.add(TextEdit::singleline(&mut self.summary).id(sum_id).interactive(!generating).margin(crate::theme::INPUT_MARGIN).hint_text("Summary").desired_width(ui.available_width() - 30.0));
                if std::mem::take(&mut self.focus_summary) {
                    r.request_focus();
                }
                ui.label(RichText::new(format!("{n}")).small().color(if n > 72 { theme::del() } else { theme::dim(ui) }));
                r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
            })
            .inner;
            theme::description(ui, &mut self.body, body_id, !generating);
            if next {
                ui.memory_mut(|m| m.request_focus(body_id));
            }
            let was = self.amend;
            ui.horizontal_wrapped(|ui| {
                // Nothing to amend before the first commit.
                if s.head.is_some() {
                    ui.checkbox(&mut self.amend, "Amend previous commit");
                }
                let mut sign = self.sign.unwrap_or_else(|| d.cfg_bool("commit.gpgsign"));
                let how = d.cfg("gpg.format").unwrap_or("openpgp");
                if ui.checkbox(&mut sign, "Sign commit").on_hover_text(format!("Sign the commit ({how} key from user.signingkey; default from commit.gpgSign). Keys are set up in git, not in Glasir.")).changed() {
                    self.sign = Some(sign);
                }
            });
            // An untouched template counts as empty.
            let untouched = self.template.as_ref().is_some_and(|t| (&t.summary, &t.body) == (&self.summary, &self.body));
            if self.amend && !was {
                acts.push(Action::AmendToggled(true));
            } else if was && !self.amend {
                acts.push(Action::AmendToggled(false));
            }
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.no_verify, "Skip hooks").on_hover_text("git commit --no-verify: skips the pre-commit and commit-msg hooks");
                let can_push = !self.amend && d.push_op(false).is_some();
                ui.add_enabled(can_push, egui::Checkbox::new(&mut self.push_after, "Push after commit"))
                    .on_disabled_hover_text("Needs a branch with a remote, and isn't offered for an amend (that push would need a force)");
                coauthors_menu(ui, d, &mut self.coauthors);
            });
            let n = s.staged.len();
            let named = !self.summary.trim().is_empty() && !untouched && !busy && !generating;
            let can = (n > 0 || self.amend) && named;
            let push = self.push_after && !self.amend && d.push_op(false).is_some();
            let label = if self.amend {
                "Amend commit".to_owned()
            } else {
                format!("Commit{} changes to {n} file{}", if push { " and push" } else { "" }, if n == 1 { "" } else { "s" })
            };
            let btn = theme::primary(ui, can, &label, ui.available_width()).on_hover_text(if untouched { "Edit the template's message first".to_owned() } else { keys("⌘↩ commits; ⇧⌘↩ stages everything first, then commits") });
            // Not while a dialog is open: ⌘↩ there confirms the dialog (e.g. an annotated tag).
            // Nor from another field (graph search, terminal): only the commit box or no focus.
            let ours = ui.memory(|m| m.top_modal_layer().is_none() && m.focused().is_none_or(|f| f == sum_id || f == body_id));
            let (enter, shift) = match ours {
                true => ui.input_mut(|i| {
                    let hit = i.modifiers.command && i.key_pressed(egui::Key::Enter);
                    if hit {
                        i.consume_key(i.modifiers, egui::Key::Enter);
                    }
                    (hit, i.modifiers.shift)
                }),
                false => (false, false),
            };
            if can && (btn.clicked() || (enter && !shift)) {
                commit = true;
            }
            // Stage everything first; a conflicted file would be committed with its markers.
            if enter && shift && named && s.conflicted.is_empty() && (n > 0 || self.amend || !s.unstaged.is_empty()) {
                stage_commit = true;
            }
        });
        if stage_commit {
            match crate::repo::stage_all_op(s) {
                Some(o) => {
                    self.commit_next = true;
                    acts.push(Action::Git(o));
                }
                None => commit = true,
            }
        }
        if commit {
            acts.push(Action::Git(self.commit_op()));
        }
        if ai_on {
            ui.horizontal_wrapped(|ui| {
                let any = !s.staged.is_empty() || !s.unstaged.is_empty();
                if self.composing() {
                    ui.spinner();
                    ui.label(RichText::new("Planning commits…").small().color(theme::dim(ui)));
                } else if theme::small(ui, any && s.conflicted.is_empty() && d.state == State::Clean && !busy, format!("{}  Compose commits", icon::SPARKLE))
                    .on_hover_text(format!("Let {} split all uncommitted changes into a series of focused commits (you review the plan first)", crate::ai::name()))
                    .on_disabled_hover_text("Needs uncommitted changes, and no merge or rebase in progress")
                    .clicked()
                {
                    acts.push(Action::Ai(Purpose::Compose));
                }
                let explaining = self.ai_busy() == Some(&Purpose::ExplainWip);
                if theme::small(ui, any && !explaining, format!("{}  Explain changes", icon::SPARKLE)).on_hover_text(format!("What do these uncommitted changes do? Asks {}", crate::ai::name())).clicked() {
                    acts.push(Action::Ai(Purpose::ExplainWip));
                }
            });
            ui.add_space(4.0);
        }
        {
            ScrollArea::vertical().id_salt("wip_files").auto_shrink(false).show(ui, |ui| {
                if !s.conflicted.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Conflicts  {}", s.conflicted.len())).small().strong().color(theme::del()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            tree::toggle(ui, as_tree);
                            let b = theme::small(ui, !d.resolvable.is_empty() && !busy, if ui.available_width() < 230.0 { "Mark all" } else { "Mark all resolved" });
                            // Built only while hovered: it compares every conflicted path with every resolvable one.
                            let b = b.on_hover_ui(|ui| {
                                let rest: Vec<&str> = s.conflicted.iter().map(|c| c.path.as_str()).filter(|p| !d.resolvable.iter().any(|r| r == p)).collect();
                                ui.label(if rest.is_empty() { "Stage every conflicted file".to_owned() } else { format!("Only files without conflict markers are marked. Left alone:\n{}", shown(&rest)) });
                            });
                            if b.on_disabled_hover_text("No conflicted file is free of conflict markers yet").clicked() {
                                acts.extend(mark_resolved_op("Mark all resolved", d, &s.conflicted).map(Action::Git));
                            }
                        });
                    });
                    // No banner outside a merge / rebase (a conflicted stash apply): list the tools here.
                    if matches!(d.state, State::Clean | State::Bisecting) {
                        open_tools(ui, &self.ext_tools, acts);
                    }
                    // The type label is painted under its row, in the gap: rows keep one height, so the list is culled.
                    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 16.0);
                    tree::list(ui, &s.conflicted, d.status_key, *as_tree, "conflicts", &mut self.tree_closed, false, |ui, item| {
                        let (c, depth) = match item {
                            Item::File { c, depth, .. } => (c, depth),
                            Item::Dir { path, files, resp } => {
                                resp.context_menu(|ui| {
                                    let op = mark_resolved_op("Mark resolved", d, files);
                                    let b = ui.add_enabled(op.is_some() && !busy, egui::Button::new("Mark folder resolved"));
                                    if b.on_hover_text("Stage its conflicted files that have no conflict markers left").clicked() {
                                        acts.extend(op.map(Action::Git));
                                    }
                                    if ui.button("Show in file manager").clicked() {
                                        acts.push(Action::Reveal(path.into()));
                                    }
                                    if ui.button("Copy path").clicked() {
                                        acts.push(Action::Copy(path.into()));
                                    }
                                });
                                return false;
                            }
                        };
                        let resp = file_row(ui, c, self.merge.as_ref().is_some_and(|m| m.path == c.path), d.lfs.files.contains(&c.path), None, depth);
                        let (kind, has) = crate::git::conflict_kind(s.conflict_xy.get(&c.path).map_or("", String::as_str));
                        let font = egui::TextStyle::Small.resolve(ui.style());
                        ui.painter().text(egui::pos2(ui.max_rect().left() + 22.0 + tree::indent(depth), resp.rect.bottom() + 1.0), egui::Align2::LEFT_TOP, kind, font, theme::dim(ui));
                        resp.context_menu(|ui| {
                            if ai_on && ui.button(format!("{}  Resolve…", icon::SPARKLE)).clicked() {
                                acts.push(Action::Ai(Purpose::Resolve(c.path.clone())));
                            }
                            let op = mark_resolved_op("Mark resolved", d, std::slice::from_ref(c));
                            if ui.add_enabled(op.is_some() && !busy, egui::Button::new("Mark resolved")).on_disabled_hover_text("Conflict markers are left, or a side is still to choose").clicked() {
                                acts.extend(op.map(Action::Git));
                            }
                            for (i, (side, word)) in [("--ours", "current"), ("--theirs", "incoming")].into_iter().enumerate() {
                                let label = &d.conflict_sides[i];
                                let name = if label.is_empty() { format!("Use {word}") } else { format!("Use {word} ({})", label.chars().take(40).collect::<String>()) };
                                let b = ui.add_enabled(has[i], egui::Button::new(&name));
                                if b.on_disabled_hover_text(format!("{kind}: this side has no version of the file. Open the merge editor to keep or delete it.")).clicked() {
                                    let file = std::fs::symlink_metadata(self.dir.join(&c.path)).map_or(true, |m| m.is_file());
                                    acts.push(Action::Git(crate::repo::merge_editor::take_side(&name, side, &c.path, file, crate::repo::merge_editor::gitlinks(&crate::git::run(&self.dir, &["--literal-pathspecs", "ls-files", "-u", "-z", "--", &c.path]).unwrap_or_default())[i].clone())));
                                }
                            }
                            let tool = crate::repo::tools::tool(d, false);
                            let b = ui.add_enabled(tool.is_ok() && has == [true, true], egui::Button::new(format!("{}  Open in external merge tool", icon::ARROW_SQUARE_OUT)));
                            let b = b.on_hover_text(keys(&format!("git mergetool ({}). Your current file is backed up first (⌘Z); other actions keep working while the tool is open.", tool.as_deref().unwrap_or_default())));
                            let why = tool.err().unwrap_or_else(|| "One side deleted this file; use the merge editor instead.".into());
                            if b.on_disabled_hover_text(why).clicked() {
                                acts.push(Action::Git(crate::repo::tools::mergetool_op(&c.path)));
                            }
                            if ui.button("Show in file manager").clicked() {
                                acts.push(Action::Reveal(c.path.clone()));
                            }
                            open_items(ui, d, head, &c.path, false, acts);
                            if ui.button("Restore conflict markers").clicked() {
                                acts.push(Action::Git(crate::repo::tools::restore_markers_op(&c.path)));
                            }
                        });
                        if resp.clicked() {
                            acts.push(Action::MergeEditor(c.path.clone()));
                        }
                        false
                    });
                    ui.spacing_mut().item_spacing.y = gap;
                    ui.add_space(6.0);
                }
                let mods = ui.input(|i| i.modifiers);
                let mut clear = false;
                let sel_un = &sc.un;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Unstaged files  {}", s.unstaged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        tree::toggle(ui, as_tree);
                        if sel_un.len() > 1 {
                            let n = sel_un.len();
                            if theme::small(ui, true, format!("Stage {n}")).on_hover_text("Stage the selected files (S)").clicked() {
                                acts.extend(stage_files_op("Stage", &sel_un).map(Action::Git));
                                clear = true;
                            }
                            if theme::small(ui, true, format!("Discard {n}")).on_hover_text(keys("Discard changes to the selected files (one confirmation, one ⌘Z)")).clicked() {
                                acts.extend(discard_files_op(&sel_un, &s.submodules).map(Action::Git));
                                clear = true;
                            }
                        } else if !s.unstaged.is_empty() && theme::small(ui, true, "Stage all").clicked() {
                            // Built on click only: its path list is the whole list.
                            acts.extend(crate::repo::stage_all_op(s).map(Action::Git));
                        }
                    });
                });
                if s.unstaged.is_empty() {
                    ui.label(RichText::new("No unstaged changes").small().color(theme::dim(ui)));
                }
                tree::list(ui, &s.unstaged, d.status_key, *as_tree, "unstaged", &mut self.tree_closed, false, |ui, item| {
                    let (c, depth, order) = match item {
                        Item::File { c, depth, order } => (c, depth, order),
                        Item::Dir { path, files, resp } => {
                            if !busy && row_button(ui, resp, icon::PLUS, "Stage") {
                                acts.extend(stage_files_op("Stage", files).map(Action::Git));
                                clear = true;
                                return true;
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Stage folder").clicked() {
                                    acts.extend(stage_files_op("Stage", files).map(Action::Git));
                                    clear = true;
                                }
                                if ui.button("Discard changes in folder").clicked() {
                                    acts.extend(discard_files_op(files, &s.submodules).map(Action::Git));
                                    clear = true;
                                }
                                clear |= dir_items(ui, &self.dir, path, files, false, acts);
                            });
                            return false;
                        }
                    };
                    let src = DiffSrc::Worktree { change: c.clone(), staged: false };
                    let in_sel = sc.has(false, &c.path);
                    let resp = file_row(ui, c, open.as_ref() == Some(&src) || in_sel, d.lfs.files.contains(&c.path), None, depth);
                    let stage_one = || stage_files_op("Stage", std::slice::from_ref(c)).map(Action::Git);
                    if !busy && row_button(ui, &resp, icon::PLUS, "Stage") {
                        acts.extend(stage_one());
                    } else if resp.clicked() && {
                        let (list, i) = order.place(c);
                        self.file_sel_gen += 1;
                        pick_file(&mut self.file_sel, &list, false, i, mods)
                    } {
                        acts.push(Action::Diff(src));
                    }
                    if resp.double_clicked() && !busy {
                        acts.extend(stage_one());
                    }
                    resp.context_menu(|ui| {
                        if in_sel && sel_un.len() > 1 {
                            let n = sel_un.len();
                            if ui.button(format!("Stage {n} files")).clicked() {
                                acts.extend(stage_files_op("Stage", &sel_un).map(Action::Git));
                                clear = true;
                            }
                            if ui.button(format!("Discard {n} files")).clicked() {
                                acts.extend(discard_files_op(&sel_un, &s.submodules).map(Action::Git));
                                clear = true;
                            }
                            clear |= stash_files_item(ui, &sel_un, false, acts);
                            return;
                        }
                        if ui.button("Stage").clicked() {
                            acts.extend(stage_one());
                        }
                        if ui.button("Discard changes").clicked() {
                            acts.extend(discard_files_op(std::slice::from_ref(c), &s.submodules).map(Action::Git));
                        }
                        stash_files_item(ui, std::slice::from_ref(c), false, acts);
                        lfs_track_item(ui, d, &c.path, acts);
                        if c.code == '?' {
                            gitignore_item(ui, &self.dir, &c.path, acts);
                        }
                        if ui.button("Show in file manager").clicked() {
                            acts.push(Action::Reveal(c.path.clone()));
                        }
                        open_items(ui, d, head, &c.path, c.code == 'D', acts);
                        if c.code != '?' {
                            file_items(ui, &c.path, c.old_path.as_deref().unwrap_or(&c.path), (c.code == 'D').then_some("HEAD"), acts);
                        }
                        if let Some(h) = d.status.head.as_deref().filter(|_| !matches!(c.code, '?' | 'A')) {
                            permalink_items(ui, d, h, c.old_path.as_deref().unwrap_or(&c.path), acts);
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(c.path.clone()));
                        }
                    });
                    false
                });
                ui.add_space(6.0);
                let sel_st = &sc.st;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Staged files  {}", s.staged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        tree::toggle(ui, as_tree);
                        if sel_st.len() > 1 {
                            if theme::small(ui, true, format!("Unstage {}", sel_st.len())).on_hover_text("Unstage the selected files (U)").clicked() {
                                acts.extend(unstage_files_op(&sel_st).map(Action::Git));
                                clear = true;
                            }
                        } else if !s.staged.is_empty() && theme::small(ui, true, "Unstage all").clicked() {
                            acts.push(Action::Git(crate::repo::unstage_all_op()));
                        }
                    });
                });
                if s.staged.is_empty() {
                    ui.add(egui::Label::new(RichText::new("Nothing staged yet: double-click a file above, or Stage all").small().color(theme::dim(ui))).wrap());
                }
                tree::list(ui, &s.staged, d.status_key, *as_tree, "staged", &mut self.tree_closed, false, |ui, item| {
                    let (c, depth, order) = match item {
                        Item::File { c, depth, order } => (c, depth, order),
                        Item::Dir { path, files, resp } => {
                            if !busy && row_button(ui, resp, icon::MINUS, "Unstage") {
                                acts.extend(unstage_files_op(files).map(Action::Git));
                                clear = true;
                                return true;
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Unstage folder").clicked() {
                                    acts.extend(unstage_files_op(files).map(Action::Git));
                                    clear = true;
                                }
                                clear |= dir_items(ui, &self.dir, path, files, true, acts);
                            });
                            return false;
                        }
                    };
                    let src = DiffSrc::Worktree { change: c.clone(), staged: true };
                    let in_sel = sc.has(true, &c.path);
                    let resp = file_row(ui, c, open.as_ref() == Some(&src) || in_sel, d.lfs.files.contains(&c.path), None, depth);
                    if !busy && row_button(ui, &resp, icon::MINUS, "Unstage") {
                        acts.push(Action::Git(crate::repo::unstage_op(c)));
                    } else if resp.clicked() && {
                        let (list, i) = order.place(c);
                        self.file_sel_gen += 1;
                        pick_file(&mut self.file_sel, &list, true, i, mods)
                    } {
                        acts.push(Action::Diff(src));
                    }
                    if resp.double_clicked() && !busy {
                        acts.push(Action::Git(crate::repo::unstage_op(c)));
                    }
                    resp.context_menu(|ui| {
                        if in_sel && sel_st.len() > 1 {
                            if ui.button(format!("Unstage {} files", sel_st.len())).clicked() {
                                acts.extend(unstage_files_op(&sel_st).map(Action::Git));
                                clear = true;
                            }
                            clear |= stash_files_item(ui, &sel_st, true, acts);
                            return;
                        }
                        if ui.button("Unstage").clicked() {
                            acts.push(Action::Git(crate::repo::unstage_op(c)));
                        }
                        stash_files_item(ui, std::slice::from_ref(c), true, acts);
                        lfs_track_item(ui, d, &c.path, acts);
                        open_items(ui, d, head, &c.path, c.code == 'D', acts);
                        if c.code != 'A' {
                            file_items(ui, &c.path, c.old_path.as_deref().unwrap_or(&c.path), (c.code == 'D').then_some("HEAD"), acts);
                        }
                        if let Some(h) = d.status.head.as_deref().filter(|_| c.code != 'A') {
                            permalink_items(ui, d, h, c.old_path.as_deref().unwrap_or(&c.path), acts);
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(c.path.clone()));
                        }
                    });
                    false
                });
                if clear {
                    self.file_sel.clear();
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Commit;
    use crate::ui::testkit::{Sim, tab};

    #[test]
    fn co_author_suggestions_follow_the_loaded_log() {
        let mut sim = Sim::new(420.0, 500.0);
        let mut t = tab(&sim.ctx, "coauthors", &["x"]);
        let mut d = t.data.take().unwrap();
        let by = |who: &str| vec![Commit { id: "0".repeat(40), parents: vec![], author: who.into(), email: format!("{who}@x"), time: 0, date: String::new(), summary: String::new() }];
        d.log = Arc::new(by("ann"));
        let mut coauthors = vec![];
        let mut draw = |ui: &mut Ui, d: &RepoData| coauthors_menu(ui, d, &mut coauthors);
        sim.frame(vec![], |ui| draw(ui, &d));
        let button = sim.text(|t| t.starts_with("Co-authors")).unwrap();
        sim.click(button, egui::PointerButton::Primary, |ui| draw(ui, &d));
        assert!(sim.text(|t| t == "ann <ann@x>").is_some(), "the menu lists the log's people");
        let (stale, mut spare) = (Arc::as_ptr(&d.log), vec![]);
        d.log = Arc::new(vec![]);
        d.log = loop {
            let newer = Arc::new(vec![]);
            if Arc::as_ptr(&newer) == stale || spare.len() == 5000 {
                break newer;
            }
            spare.push(newer);
        };
        Arc::get_mut(&mut d.log).unwrap().extend(by("bob"));
        for _ in 0..2 {
            sim.frame(vec![], |ui| draw(ui, &d));
        }
        assert!(sim.text(|t| t == "bob <bob@x>").is_some() && sim.text(|t| t == "ann <ann@x>").is_none(), "the people of the log now loaded");
    }

    #[test]
    fn a_renamed_files_history_and_a_deleted_files_blame_look_at_head() {
        use crate::repo::history::Mode;
        let mut sim = Sim::new(420.0, 900.0);
        let mut t = tab(&sim.ctx, "wip-renamed-deleted", &["x"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        std::fs::write(dir.join("gone.txt"), "g\n").unwrap();
        git(&["add", "gone.txt"]);
        git(&["commit", "-q", "-m", "gone"]);
        git(&["mv", "f", "renamed.txt"]);
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        t.reload(&sim.ctx, true);
        crate::ui::testkit::load(&mut t, &sim.ctx);
        let mut opened = vec![];
        for (row, item) in [("renamed.txt", "File history"), ("gone.txt", "Blame")] {
            let mut acts = vec![];
            let mut draw = |ui: &mut Ui| t.wip_panel(ui, &mut false, &mut acts);
            sim.open_menu(row, &mut draw);
            sim.frame(vec![], &mut draw);
            let at = sim.text(|s| s.contains(item)).unwrap_or_else(|| panic!("{item} is not in {row}'s menu"));
            sim.click(at, egui::PointerButton::Primary, draw);
            opened.extend(acts.into_iter().filter_map(|a| if let Action::FileView(p, m) = a { Some((p, m)) } else { None }));
        }
        assert_eq!(opened, [("f".into(), Mode::History), ("gone.txt".into(), Mode::Blame(Some("HEAD".into())))]);
    }
}
