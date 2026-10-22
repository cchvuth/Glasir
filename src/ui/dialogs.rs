//! Per-repo modal dialogs and popups: value prompt, confirmation, AI result,
//! branch cleanup, absorb preview, failed-op choices and the branch drop menu. Each reads its
//! request from the `RepoTab` and pushes the resulting `Action`s.

use crate::ui::format::{dismissed, dismissed_in, keys, on_top, plural};
use crate::ai;
use crate::git;
use crate::repo::{Action, Op, Prompt, RepoTab, Undo};
use crate::repo::merge_opts::{Merge, merge_op};
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, Key, RichText, TextEdit, Ui, vec2};

/// Asks for a value (`Action::Prompt`), then runs its op with `{}` / `{2}` filled in.
pub(super) fn prompt_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(p) = &mut t.prompt else { return };
    let mut done = None;
    egui::Modal::new(Id::new("prompt")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(&p.title);
        let top = on_top(ui);
        let first = ui.add(TextEdit::singleline(&mut p.value).margin(crate::theme::INPUT_MARGIN).desired_width(f32::INFINITY));
        if let Some((hint, v2)) = &mut p.second {
            ui.add(TextEdit::multiline(v2).margin(crate::theme::INPUT_MARGIN).hint_text(keys(hint)).desired_rows(3).desired_width(f32::INFINITY));
        } else if top {
            first.request_focus();
        }
        if let Some(sign) = &mut p.sign {
            ui.checkbox(sign, "Sign tag").on_hover_text("git tag --sign, with your configured key (gpg.format, user.signingkey); default from tag.gpgSign");
        }
        ui.horizontal(|ui| {
            let enter = top && ui.input(|i| i.key_pressed(Key::Enter) && (p.second.is_none() || i.modifiers.command));
            if ui.button("OK").clicked() || enter {
                done = Some(true);
            }
            if dismissed_in(ui, "Cancel", p.second.is_none().then_some(first.id)) {
                done = Some(false);
            }
        });
    });
    if let Some(ok) = done {
        let p = t.prompt.take().unwrap();
        let v = p.value.trim();
        // `-m ""` gives git's default message (a stash), `--reason ""` none (a worktree lock).
        let optional = p.op.cmds.iter().any(|c| c.windows(2).any(|w| w == ["-m", "{}"] || w == ["--reason", "{}"]));
        if ok && (!v.is_empty() || optional) {
            let mut op = p.op;
            if let Some(sign) = p.sign {
                op.cmds[0].insert(1, if sign { "--sign" } else { "--no-sign" }.into());
            }
            let v2 = p.second.as_ref().map(|s| s.1.trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| v.to_owned());
            let revert = op.revert.as_deref_mut().into_iter().flat_map(|r| r.cmds.iter_mut().flatten());
            for a in op.cmds.iter_mut().flatten().chain(op.then_open.as_mut()).chain(revert) {
                *a = fill_arg(a, v, &v2);
            }
            acts.push(Action::Git(op));
        }
    }
}

/// One argument of a prompt's op: `{}` / `{2}` as the whole argument, or [`Prompt::VALUE`] inside
/// one. Any other `{}` is data (a branch `x{}`, a path) and stays.
fn fill_arg(arg: &str, v: &str, v2: &str) -> String {
    match arg {
        "{}" | "{2}" => fill(arg, v, v2),
        _ => arg.replace(Prompt::VALUE, v),
    }
}

/// Substitutes `{}` and `{2}` in one pass: a value that contains "{2}" must not be substituted again.
fn fill(arg: &str, v: &str, v2: &str) -> String {
    arg.replace("{2}", "\0\x02").replace("{}", "\0\x01").replace("\0\x01", v).replace("\0\x02", v2)
}

#[cfg(test)]
mod fill_tests {
    #[test]
    fn value_with_placeholder_is_kept() {
        assert_eq!(super::fill("{}", "Support {2} in {}", "x"), "Support {2} in {}");
        assert_eq!(super::fill("{}-{2}", "a", "b"), "a-b");
        assert_eq!(super::fill("a{}b{2}{x", "1", "2"), "a1b2{x");
    }

    /// "New branch from x{}", "Rename a{}b": the ref names baked into the op are data.
    #[test]
    fn ref_names_with_braces_are_kept() {
        let op = crate::repo::Op::new("Create branch", &["checkout", "-b", "{}", "x{}"]).then(&["branch", "-m", "a{}b", "{}"]).then(&["tag", "-a", "{}", "-m", "{2}", "{2}x"]);
        let filled: Vec<Vec<String>> = op.cmds.iter().map(|c| c.iter().map(|a| super::fill_arg(a, "new", "msg")).collect()).collect();
        assert_eq!(filled, [vec!["checkout", "-b", "new", "x{}"], vec!["branch", "-m", "a{}b", "new"], vec!["tag", "-a", "new", "-m", "msg", "{2}x"]]);
        let path = format!("/w/{{}}/repo.worktrees/{}", crate::repo::Prompt::VALUE);
        assert_eq!(super::fill_arg(&path, "feat", ""), "/w/{}/repo.worktrees/feat");
    }

    #[test]
    fn an_empty_lock_reason_still_locks() {
        use crate::repo::{Action, Op, Prompt};
        let mut sim = crate::ui::testkit::Sim::new(800.0, 600.0);
        let mut t = crate::ui::testkit::tab(&sim.ctx, "lock-reason", &["one"]);
        let op = Op::new("Lock worktree", &["worktree", "lock", "--reason", "{}", "/wt"]);
        t.prompt = Some(Prompt { title: "Lock reason (optional)".into(), value: String::new(), op, second: None, sign: None });
        let mut acts = vec![];
        let mut draw = |ui: &mut egui::Ui| super::prompt_dialog(&mut t, ui.ctx(), &mut acts);
        sim.frame(vec![], &mut draw);
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s == "OK").expect("the OK button");
        sim.click(at, egui::PointerButton::Primary, &mut draw);
        assert!(matches!(&acts[..], [Action::Git(op)] if op.cmds[0] == ["worktree", "lock", "--reason", "", "/wt"]), "{acts:?}");
    }
}

/// Confirmation for an op with `confirm` text (and an optional `\n$ command` preview).
pub(super) fn confirm_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(c) = &t.confirm else { return };
    let mut done = None;
    egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
        let text = c.confirm.as_deref().unwrap_or("");
        let (text, command) = text.split_once("\n$ ").map_or((text, None), |(t, c)| (t, Some(c)));
        let raw_id = Id::new("confirm_raw");
        let raw = ui.data(|d| d.get_temp::<bool>(raw_id)).unwrap_or(false);
        ui.set_width(if command.is_some() && raw { 640.0 } else { 380.0 });
        ui.heading(&c.label);
        ui.label(keys(text));
        if let Some(command) = command {
            ui.add_space(4.0);
            if ui.link(RichText::new(if raw { "Hide raw command" } else { "View raw command" }).small()).clicked() {
                ui.data_mut(|d| d.insert_temp(raw_id, !raw));
            }
            if raw {
                // One flag per line (still paste-able), so long leases don't wrap mid-word.
                let shown = format!("$ {}", command.replace(" --", " \\\n  --"));
                Frame::new().fill(theme::faint(ui)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(shown).monospace().size(11.5));
                });
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(Button::new(RichText::new("Confirm").strong()).fill(theme::del().gamma_multiply(0.6))).clicked() {
                done = Some(true);
            }
            if dismissed(ui, "Cancel") {
                done = Some(false);
            }
        });
    });
    if let Some(ok) = done {
        ctx.data_mut(|d| d.remove::<bool>(Id::new("confirm_raw")));
        let mut op = t.confirm.take().unwrap();
        if ok {
            op.confirm = None;
            acts.push(Action::Git(op));
        } else if op.parked {
            t.unpark_rebase();
        }
    }
}

/// Modal for AI results that need a decision (pull request text, conflict resolution).
pub(super) fn ai_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(job) = &t.ai else { return };
    let modal = match &job.purpose {
        ai::Purpose::PullRequest { .. } => true,
        // From the merge editor, the answer fills its output pane instead.
        ai::Purpose::Resolve(p) => t.merge.as_ref().is_none_or(|m| m.path != *p),
        // The inspector shows it inline only while that same range is selected.
        ai::Purpose::ExplainRange { from: f, to: tt, .. } => !matches!(&t.sel, Some(crate::repo::Sel::Compare { from, to, .. }) if f == from && tt == to),
        ai::Purpose::ExplainWip => true,
        _ => false,
    };
    if !modal {
        return;
    }
    let mut close = false;
    egui::Modal::new(Id::new("ai_dialog")).show(ctx, |ui| {
        ui.set_width(720.0);
        ui.horizontal(|ui| {
            ui.heading(format!("{}  {}", icon::SPARKLE, job.purpose.title()));
            if job.running() {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        if let Some(Err(e)) = &job.done {
            ui.colored_label(theme::del(), e);
        }
        egui::ScrollArea::vertical().max_height(460.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut text = job.text.as_str();
            let font = if matches!(job.purpose, ai::Purpose::Resolve(_)) { egui::TextStyle::Monospace } else { egui::TextStyle::Body };
            ui.add(TextEdit::multiline(&mut text).margin(crate::theme::INPUT_MARGIN).font(font).desired_width(f32::INFINITY));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ready = job.done == Some(Ok(())) && !job.text.trim().is_empty();
            match &job.purpose {
                ai::Purpose::PullRequest { branch, base } => {
                    let (title, body) = job.text.trim().split_once('\n').unwrap_or((job.text.trim(), ""));
                    if let Some(page) = t.data.as_ref().and_then(|d| super::sidebar::ref_menus::pull_request_url(d, branch, base))
                        && ui.add_enabled(ready, Button::new(format!("{}  Open pull request page", icon::GIT_PULL_REQUEST))).clicked()
                    {
                        let mut url = format!("{page}&title={}", git::url_encode(title.trim()));
                        let body = git::url_encode(body.trim());
                        if body.len() < 6000 {
                            url += &format!("&body={body}");
                        } else {
                            acts.push(Action::Copy(job.text.clone()));
                        }
                        acts.push(Action::Url(url));
                    }
                    if ui.add_enabled(ready, Button::new(format!("{}  Copy", icon::COPY))).clicked() {
                        acts.push(Action::Copy(job.text.trim().to_owned()));
                    }
                }
                ai::Purpose::Resolve(path) => {
                    let tool = t.merge_tool_open(path);
                    let b = ui.add_enabled(ready && !tool, Button::new(format!("{}  Apply & mark resolved", icon::CHECK)));
                    if (if tool { b.on_disabled_hover_text("The external merge tool is open on this file and would overwrite the save. Close it first.") } else { b }).clicked() {
                        match crate::repo::merge_editor::apply_ai(&t.dir, path, job.draft.as_bytes(), &job.text) {
                            Ok(op) => {
                                acts.push(Action::Git(op));
                                close = true;
                            }
                            Err(e) => acts.push(Action::Error(e)),
                        }
                    }
                }
                _ => {}
            }
            if !job.running() && ui.button(format!("{}  Regenerate", icon::ARROWS_CLOCKWISE)).clicked() {
                acts.push(Action::Ai(job.purpose.clone()));
            }
            if dismissed(ui, "Close") {
                close = true;
            }
        });
    });
    if close {
        t.ai = None;
    }
}

/// Clean up branches: candidates against a chosen target, grouped by why they're offered.
pub(super) fn cleanup_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>, days: &mut u32) {
    use crate::repo::cleanup::{Cleanup, Why};
    let (Some(c), Some(d)) = (&mut t.cleanup, &t.data) else { return };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_secs() as i64);
    let mut close = false;
    egui::Modal::new(Id::new("cleanup")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading(format!("{}  Clean up branches", icon::BROOM));
        ui.horizontal(|ui| {
            ui.label("Target");
            let before = c.target.clone();
            egui::ComboBox::from_id_salt("cleanup_target").selected_text(c.target.as_str()).width(200.0).show_ui(ui, |ui| {
                for n in &c.targets {
                    ui.selectable_value(&mut c.target, n.clone(), n.as_str());
                }
            });
            if c.target != before {
                c.rescan();
                acts.push(Action::Cleanup);
            }
            ui.add_space(12.0);
            ui.label("Stale after");
            ui.add(egui::DragValue::new(days).range(1..=3650).suffix(" days"));
        });
        ui.add_space(4.0);
        match &mut c.rows {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking branches…").color(theme::dim(ui)));
                });
            }
            Some(Err(e)) => {
                ui.colored_label(theme::del(), e.as_str());
            }
            Some(Ok(rows)) if !rows.iter().any(|r| Cleanup::group(r, *days, now).is_some()) => {
                ui.label(RichText::new("Nothing to clean up: no merged, orphaned or stale local branches.").color(theme::dim(ui)));
            }
            Some(Ok(rows)) => {
                ui.label(RichText::new(keys("Local branches you probably don't need anymore. Deleting is undoable (⌘Z).")).small().color(theme::dim(ui)));
                egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                    for g in [Why::Merged, Why::Squashed, Why::Gone, Why::Stale] {
                        let mut first = true;
                        for r in rows.iter_mut().filter(|r| Cleanup::group(r, *days, now) == Some(g)) {
                            if std::mem::take(&mut first) {
                                ui.add_space(4.0);
                                ui.label(RichText::new(match g {
                                    Why::Merged => format!("Merged into {}", c.target),
                                    Why::Squashed => "Squash- or rebase-merged (same changes in the target)".into(),
                                    Why::Gone => "Upstream deleted".into(),
                                    Why::Stale => format!("Stale: no commits in {days} days"),
                                }).small().strong());
                            }
                            // Full width: details line up on the right, long names end in "…".
                            theme::bar(ui, |ui| {
                                let mut info = vec![super::format::ago(r.date), format!("↑ {}  ↓ {}", r.ahead, r.behind)];
                                if !r.note.is_empty() {
                                    info.insert(0, r.note.clone());
                                }
                                if c.remote && !r.kept.is_empty() {
                                    info.insert(0, r.kept.clone());
                                }
                                ui.label(RichText::new(info.join(" · ")).small().color(theme::dim(ui)))
                                    .on_hover_text(format!("{} commits not in {}, {} commits of {} missing", r.ahead, c.target, r.behind, c.target));
                                theme::rest(ui, |ui| ui.checkbox(&mut r.on, RichText::new(r.name.as_str()).strong()).on_hover_text(r.name.as_str()));
                            });
                        }
                    }
                });
            }
        }
        ui.add_space(6.0);
        let remote = if c.remote { format!(" ({})", c.remote_deletes(*days, now).len()) } else { String::new() };
        if ui
            .checkbox(&mut c.remote, format!("Also delete on the remote{remote}"))
            .on_hover_text("Fetches, then deletes the ticked branches' upstream branches whose own commits are all in the target, each only if nobody pushed to it since. Can't be undone on the remote.")
            .changed()
            && c.remote
        {
            c.rescan();
            acts.push(Action::Cleanup);
        }
        ui.horizontal(|ui| {
            if ui.checkbox(&mut c.prune, "Prune stale remote-tracking refs").changed() && c.prune {
                acts.push(Action::Cleanup);
            }
            match (&c.stale_refs, c.prune) {
                (_, false) => {}
                (None, true) => {
                    ui.spinner();
                }
                (Some(Ok(v)), true) => {
                    let n = v.len();
                    ui.label(RichText::new(format!("{n} ref{} gone from the remote", plural(n))).small().color(theme::dim(ui)))
                        .on_hover_text(v.iter().map(|r| r.0.trim_start_matches("refs/remotes/")).collect::<Vec<_>>().join("\n"));
                }
                (Some(Err(e)), true) => {
                    ui.label(RichText::new(e.lines().next().unwrap_or("")).small().color(theme::del())).on_hover_text(e.as_str());
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (n, p) = (c.chosen(*days, now).len(), c.pruned().len());
            let mut label = format!("Delete {n} branch{}", if n == 1 { "" } else { "es" });
            if p > 0 {
                label += &format!(", prune {p}");
            }
            if theme::primary(ui, n + p > 0, &label, 190.0).clicked() {
                acts.push(c.op(d, *days, now).map_or_else(Action::Error, Action::Git));
                close = true;
            }
            if dismissed(ui, "Cancel") {
                close = true;
            }
        });
    });
    if close {
        t.cleanup = None;
    }
}

/// Absorb preview: each staged hunk and the commit it goes to, or why it stays staged.
pub(super) fn absorb_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let (Some(p), Some(d)) = (&t.absorb, &t.data) else { return };
    let mut close = false;
    egui::Modal::new(Id::new("absorb")).show(ctx, |ui| {
        ui.set_width(640.0);
        ui.heading(format!("{}  Absorb staged changes", icon::ARROWS_IN));
        ui.label(
            RichText::new(keys("Each hunk is folded into the unpushed commit that last changed its lines. Undo with ⌘Z.")).small().color(theme::dim(ui)),
        );
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("absorb_rows").num_columns(3).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
                for r in &p.rows {
                    ui.label(RichText::new(&r.path).strong());
                    ui.label(RichText::new(&r.what).monospace().small().color(theme::dim(ui)));
                    match &r.target {
                        Ok(i) => {
                            let (id, summary) = &p.cands[*i];
                            ui.add(egui::Label::new(RichText::new(format!("{}  {} {summary}", icon::ARROW_RIGHT, &id[..7.min(id.len())])).color(theme::add())).truncate());
                        }
                        Err(why) => {
                            ui.add(egui::Label::new(RichText::new(format!("stays staged: {why}")).italics().color(theme::dim(ui))).truncate());
                        }
                    }
                    ui.end_row();
                }
            });
        });
        ui.add_space(8.0);
        let (n, hunks) = (p.targets().len(), p.rows.iter().filter(|r| r.target.is_ok()).count());
        ui.horizontal(|ui| {
            if theme::primary(ui, n > 0, &format!("Absorb {hunks} hunk{} into {n} commit{}", plural(hunks), plural(n)), 250.0).clicked() {
                acts.push(p.op(&t.dir, &d.git_dir).map_or_else(Action::Error, Action::Git));
                close = true;
            }
            if dismissed(ui, "Cancel") {
                close = true;
            }
        });
    });
    if close {
        t.absorb = None;
    }
}

/// A failed op's one-click fixes (stash and switch, pull when diverged...).
pub(super) fn choice_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(c) = &t.choice else { return };
    let mut close = false;
    egui::Modal::new(Id::new("choice")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(format!("{}  {}", icon::WARNING, c.title));
        ui.add_space(2.0);
        ui.label(&c.text);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (i, (label, op)) in c.ops.iter().enumerate() {
                let r = if i == 0 { theme::primary(ui, true, label, 120.0) } else { ui.add(Button::new(label).min_size(vec2(0.0, 32.0)).corner_radius(255)) };
                if r.clicked() {
                    acts.push(Action::Git(op.clone()));
                    close = true;
                }
            }
            if dismissed(ui, "Cancel") {
                close = true;
            }
        });
    });
    if close {
        t.choice = None;
    }
}

/// The menu shown after dropping a commit onto a commit or a branch.
pub(super) fn commit_drop_menu(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some((title, items, at)) = &t.commit_drop else { return };
    let mut close = false;
    let area = egui::Area::new(Id::new("commit_drop_menu")).order(egui::Order::Foreground).fixed_pos(*at).show(ctx, |ui| {
        Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(260.0);
            ui.set_max_width(380.0);
            ui.label(RichText::new(format!("{}  {title}", icon::GIT_COMMIT)).strong());
            ui.separator();
            for (label, a) in items {
                match a {
                    Some(a) if ui.button(label).clicked() => {
                        acts.push(a.clone());
                        close = true;
                    }
                    Some(_) => {}
                    None => {
                        ui.label(RichText::new(label).weak());
                    }
                }
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    let outside = ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
    if close || outside {
        t.commit_drop = None;
    }
}

/// The menu shown after dropping one branch onto another.
pub(super) fn drop_menu(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some((src, dst, at)) = t.drop_menu.clone() else { return };
    let Some(d) = &t.data else { return };
    let find = |full: &str| d.refs.iter().find(|r| r.full == full);
    let (Some(s), Some(dr)) = (find(&src), find(&dst)) else {
        t.drop_menu = None;
        return;
    };
    let (src, dst, src_rev, dst_rev) = (&s.name, &dr.name, d.rev(s), d.rev(dr));
    let local = |r: &git::Ref| r.kind == git::RefKind::Local;
    let mut close = false;
    let area = egui::Area::new(Id::new("drop_menu")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(260.0);
            ui.label(RichText::new(format!("{src}  {}  {dst}", icon::ARROW_RIGHT)).strong());
            ui.separator();
            let mut pick = |ui: &mut Ui, label: String, a: Action| {
                if ui.button(label).clicked() {
                    acts.push(a);
                    close = true;
                }
            };
            // Merging needs a local target branch (checking out a remote ref would detach HEAD).
            if local(dr) {
                let hows = [None, Some(Merge::NoFf), Some(Merge::FfOnly), Some(Merge::Squash)];
                // A squash merge stays staged on the checked-out branch, so only there.
                for how in hows.into_iter().filter(|h| dr.head || *h != Some(Merge::Squash)) {
                    let mut merge = merge_op(src, src_rev, how);
                    if !dr.head {
                        let mut o = Op::new(format!("Merge {src} into {dst}"), &crate::repo::checkout_args(dst, false)).undo(Undo::Keep);
                        o.cmds.extend(merge.cmds);
                        merge = o;
                    }
                    let how = how.map_or(String::new(), |h| format!(" ({})", h.name()));
                    pick(ui, format!("{}  Merge {src} into {dst}{how}", icon::GIT_MERGE), Action::Git(merge));
                }
            }
            if dr.kind == git::RefKind::Remote
                && local(s)
                && let Some((remote, branch)) = d.split_remote(dst)
            {
                let push = Op::new(format!("Push {src}"), &["push", remote, &format!("{}:refs/heads/{branch}", s.full)]);
                pick(ui, format!("{}  Push {src} to {dst}", icon::ARROW_LINE_UP), Action::Git(push));
            }
            // `git rebase <dst> <remote ref>` (or `refs/heads/<src>`) would rebase a detached HEAD
            // and move no branch; a short `<branch>` argument is looked up as a branch first.
            if local(s) {
                let rebase = Op::new(format!("Rebase {src} onto {dst}"), &["rebase", "--end-of-options", dst_rev, src]).undo(Undo::Keep);
                let rebase = Action::RunRebase { op: rebase, onto: dst_rev.into(), branch: Some(src.clone()), first: None };
                pick(ui, format!("{}  Rebase {src} onto {dst}", icon::GIT_COMMIT), rebase);
            }
            if local(s) && s.head {
                pick(ui, format!("{}  Interactive rebase {src} onto {dst}…", icon::LIST), Action::Rebase { base: dst_rev.into(), label: dst.clone() });
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    let outside = ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
    if close || outside {
        t.drop_menu = None;
    }
}
