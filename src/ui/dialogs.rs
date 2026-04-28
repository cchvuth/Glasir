//! Per-repo modal dialogs and popups: value prompt, confirmation,
//! failed-op choices and the branch drop menu. Each reads its
//! request from the `RepoTab` and pushes the resulting `Action`s.

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
        let first = ui.add(TextEdit::singleline(&mut p.value).desired_width(f32::INFINITY));
        if let Some((hint, v2)) = &mut p.second {
            ui.add(TextEdit::multiline(v2).hint_text(hint.as_str()).desired_rows(3).desired_width(f32::INFINITY));
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
            for a in op.cmds.iter_mut().flatten() {
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
    let dst_ref = d.refs.iter().find(|r| r.name == dst).map(|r| (r.kind, r.remote().to_owned(), r.remote_branch().to_owned()));
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
                    Op::new(format!("Merge {src}"), &["merge", "--no-edit", &src])
                } else {
                    Op::new(format!("Merge {src} into {dst}"), &["checkout", &dst, "--"]).then(&["merge", "--no-edit", &src])
                };
                pick(ui, format!("{} Merge {src} into {dst}", icon::GIT_MERGE), Action::Git(merge.undo(Undo::Keep)));
            }
            if matches!(dst_ref, Some((git::RefKind::Remote, ..))) && d.local(&src).is_some() {
                let (_, remote, branch) = dst_ref.clone().unwrap();
                let push = Op::new(format!("Push {src}"), &["push", &remote, &format!("{src}:{branch}")]);
                pick(ui, format!("{} Push {src} to {dst}", icon::ARROW_LINE_UP), Action::Git(push));
            }
            let rebase = Op::new(format!("Rebase {src} onto {dst}"), &["rebase", &dst, &src]).undo(Undo::Keep);
            pick(ui, format!("{} Rebase {src} onto {dst}", icon::GIT_COMMIT), Action::Git(rebase));
            if src == head {
                pick(ui, format!("{} Interactive rebase {src} onto {dst}…", icon::LIST), Action::Rebase { base: dst.clone(), label: dst.clone() });
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
