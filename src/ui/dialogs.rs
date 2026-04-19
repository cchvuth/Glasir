//! Per-repo modal dialogs: value prompt, confirmation and failed-op
//! choices. Each reads its request from the `RepoTab` and pushes the resulting `Action`s.

use crate::repo::{Action, RepoTab};
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, Key, RichText, TextEdit, vec2};

/// Asks for a value (`Action::Prompt`), then runs its op with `{}` filled in.
pub(super) fn prompt_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(p) = &mut t.prompt else { return };
    let mut done = None;
    egui::Modal::new(Id::new("prompt")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(&p.title);
        ui.add(TextEdit::singleline(&mut p.value).desired_width(f32::INFINITY)).request_focus();
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
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
            for a in op.cmds.iter_mut().flatten() {
                *a = a.replace("{}", v);
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

/// A failed op's one-click fixes (pull when diverged...).
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
