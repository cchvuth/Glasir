//! Per-repo modal dialogs and popups: value prompt, confirmation, AI result,
//! branch cleanup, absorb preview, failed-op choices and the branch drop menu. Each reads its
//! request from the `RepoTab` and pushes the resulting `Action`s.

use crate::ai;
use crate::git;
use crate::repo::{Action, Op, RepoTab, Undo};
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, Key, RichText, TextEdit, Ui, vec2};

/// Asks for a value (`Action::Prompt`), then runs its op with `{}` / `{2}` filled in.
pub(super) fn prompt_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(p) = &mut t.prompt else { return };
    let mut done = None;
    egui::Modal::new(Id::new("prompt")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(&p.title);
        let first = ui.add(TextEdit::singleline(&mut p.value).margin(crate::theme::INPUT_MARGIN).desired_width(f32::INFINITY));
        if let Some((hint, v2)) = &mut p.second {
            ui.add(TextEdit::multiline(v2).margin(crate::theme::INPUT_MARGIN).hint_text(hint.as_str()).desired_rows(3).desired_width(f32::INFINITY));
        } else {
            first.request_focus();
        }
        ui.horizontal(|ui| {
            let enter = ui.input(|i| i.key_pressed(Key::Enter) && (p.second.is_none() || i.modifiers.command));
            if ui.button("OK").clicked() || enter {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                done = Some(false);
            }
        });
    });
    if let Some(ok) = done {
        let p = t.prompt.take().unwrap();
        let v = p.value.trim();
        if ok && !v.is_empty() {
            let mut op = p.op;
            let v2 = p.second.as_ref().map(|s| s.1.trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| v.to_owned());
            let revert = op.revert.as_deref_mut().into_iter().flat_map(|r| r.cmds.iter_mut().flatten());
            for a in op.cmds.iter_mut().flatten().chain(op.then_open.as_mut()).chain(revert) {
                *a = a.replace("{}", v).replace("{2}", &v2);
            }
            acts.push(Action::Git(op));
        }
    }
}

/// Confirmation for an op with `confirm` text (and an optional `\n$ command` preview).
pub(super) fn confirm_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(c) = &t.confirm else { return };
    let mut done = None;
    egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
        let text = c.confirm.as_deref().unwrap_or("");
        let (text, command) = text.split_once("\n$ ").map_or((text, None), |(t, c)| (t, Some(c)));
        ui.set_width(if command.is_some() { 640.0 } else { 380.0 });
        ui.heading(&c.label);
        ui.label(text);
        if let Some(command) = command {
            ui.add_space(4.0);
            // One flag per line (still paste-able), so long leases don't wrap mid-word.
            let shown = format!("$ {}", command.replace(" --", " \\\n  --"));
            Frame::new().fill(theme::faint(ui)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(shown).monospace().size(11.5));
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(Button::new(RichText::new("Confirm").strong()).fill(theme::DEL.gamma_multiply(0.6))).clicked() {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                done = Some(false);
            }
        });
    });
    if let Some(ok) = done {
        let mut op = t.confirm.take().unwrap();
        if ok {
            op.confirm = None;
            acts.push(Action::Git(op));
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
        ai::Purpose::ExplainRange { from: f, to: tt } => !matches!(&t.sel, Some(crate::repo::Sel::Compare { from, to, .. }) if f == from && tt == to),
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
            ui.heading(format!("{} {}", icon::SPARKLE, job.purpose.title()));
            if job.running() {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        if let Some(Err(e)) = &job.done {
            ui.colored_label(theme::DEL, e);
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
                    if let Some(web) = t.data.as_ref().and_then(|d| d.web())
                        && ui.add_enabled(ready, Button::new(format!("{} Open pull request page", icon::GIT_PULL_REQUEST))).clicked()
                    {
                        let strip = |b: &str| b.split_once('/').filter(|(r, _)| *r == "origin").map_or(b.to_owned(), |(_, n)| n.to_owned());
                        let mut url = format!(
                            "{web}/compare/{}...{}?expand=1&title={}",
                            git::url_path(&strip(base)),
                            git::url_path(&strip(branch)),
                            git::url_encode(title.trim())
                        );
                        let body = git::url_encode(body.trim());
                        if body.len() < 6000 {
                            url += &format!("&body={body}");
                        } else {
                            acts.push(Action::Copy(job.text.clone()));
                        }
                        acts.push(Action::Url(url));
                    }
                    if ui.add_enabled(ready, Button::new(format!("{} Copy", icon::COPY))).clicked() {
                        acts.push(Action::Copy(job.text.trim().to_owned()));
                    }
                }
                ai::Purpose::Resolve(path) => {
                    if ui.add_enabled(ready, Button::new(format!("{} Apply & mark resolved", icon::CHECK))).clicked() {
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
            if !job.running() && ui.button(format!("{} Regenerate", icon::ARROWS_CLOCKWISE)).clicked() {
                acts.push(Action::Ai(job.purpose.clone()));
            }
            if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
    });
    if close {
        t.ai = None;
    }
}

pub(super) fn cleanup_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let (Some(list), Some(d)) = (&mut t.cleanup, &t.data) else { return };
    let mut close = false;
    egui::Modal::new(Id::new("cleanup")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.heading(format!("{} Clean up branches", icon::BROOM));
        if list.is_empty() {
            ui.label(RichText::new("Nothing to clean up — no merged or orphaned local branches.").color(theme::dim(ui)));
        } else {
            ui.label(RichText::new("Local branches you probably don't need anymore. Deleting is undoable (⌘Z).").small().color(theme::dim(ui)));
            ui.add_space(4.0);
            // Full width: reasons line up on the right, long names end in "…".
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                for (name, why, on) in list.iter_mut() {
                    theme::bar(ui, |ui| {
                        ui.label(RichText::new(why.as_str()).small().color(theme::dim(ui)));
                        theme::rest(ui, |ui| ui.checkbox(on, RichText::new(name.as_str()).strong()).on_hover_text(name.as_str()));
                    });
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let names: Vec<&str> = list.iter().filter(|c| c.2).map(|c| c.0.as_str()).collect();
            let label = format!("Delete {} branch{}", names.len(), if names.len() == 1 { "" } else { "es" });
            if theme::primary(ui, !names.is_empty(), &label, 170.0).clicked() {
                acts.push(Action::Git(crate::repo::delete_branches(d, "Clean up branches".into(), &names, true)));
                close = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
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
        ui.heading(format!("{} Absorb staged changes", icon::ARROWS_IN));
        ui.label(
            RichText::new("Each hunk is folded into the unpushed commit that last changed its lines. Undo with ⌘Z.").small().color(theme::dim(ui)),
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
                            ui.add(egui::Label::new(RichText::new(format!("{}  {} {summary}", icon::ARROW_RIGHT, &id[..7.min(id.len())])).color(theme::ADD)).truncate());
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
            let s = |k: usize| if k == 1 { "" } else { "s" };
            if theme::primary(ui, n > 0, &format!("Absorb {hunks} hunk{} into {n} commit{}", s(hunks), s(n)), 250.0).clicked() {
                acts.push(p.op(&t.dir, &d.git_dir).map_or_else(Action::Error, Action::Git));
                close = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
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
        ui.heading(format!("{} {}", icon::WARNING, c.title));
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
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
    });
    if close {
        t.choice = None;
    }
}

/// The menu shown after dropping one branch onto another.
pub(super) fn drop_menu(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some((src, dst, at)) = t.drop_menu.clone() else { return };
    let Some(d) = &t.data else { return };
    let head = d.status.branch.clone().unwrap_or_default();
    let (src_rev, dst_rev) = (d.branch_rev(&src), d.branch_rev(&dst));
    let dst_ref = d.refs.iter().find(|r| r.full == dst_rev).map(|r| (r.kind, r.remote().to_owned(), r.remote_branch().to_owned()));
    let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
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
            if dst == head || matches!(dst_ref, Some((git::RefKind::Local, ..))) {
                let merge = if dst == head {
                    Op::new(format!("Merge {src}"), &["merge", "--no-edit", src_rev])
                } else {
                    Op::new(format!("Merge {src} into {dst}"), &["checkout", &dst, "--"]).then(&["merge", "--no-edit", src_rev])
                };
                pick(ui, format!("{} Merge {src} into {dst}", icon::GIT_MERGE), Action::Git(merge.undo(Undo::Keep)));
            }
            if matches!(dst_ref, Some((git::RefKind::Remote, ..))) && d.local(&src).is_some() {
                let (_, remote, branch) = dst_ref.clone().unwrap();
                let push = Op::new(format!("Push {src}"), &["push", &remote, &format!("{src}:{branch}")]);
                pick(ui, format!("{} Push {src} to {dst}", icon::ARROW_LINE_UP), Action::Git(push));
            }
            // `git rebase <dst> <remote ref>` (or `refs/heads/<src>`) would rebase a detached HEAD
            // and move no branch; a short `<branch>` argument is looked up as a branch first.
            if d.local(&src).is_some() {
                let rebase = Op::new(format!("Rebase {src} onto {dst}"), &["rebase", dst_rev, &src]).undo(Undo::Keep);
                pick(ui, format!("{} Rebase {src} onto {dst}", icon::GIT_COMMIT), Action::Git(rebase));
            }
            if src == head {
                pick(ui, format!("{} Interactive rebase {src} onto {dst}…", icon::LIST), Action::Rebase { base: dst_rev.into(), label: dst.clone() });
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
