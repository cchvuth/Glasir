//! The interactive rebase editor window (drawing + keyboard). The todo model
//! and git side live in `crate::repo::rebase`.

use crate::repo::rebase::{Act, Editor};
use crate::repo::Action;
use crate::theme::{self, icon};
use egui::{Key, RichText, TextEdit, Ui};
use std::path::Path;

const ACTS: [(Act, &str, Key, &str); 5] = [
    (Act::Pick, "Pick", Key::P, "keep the commit"),
    (Act::Reword, "Reword", Key::R, "keep, edit the message"),
    (Act::Squash, "Squash", Key::S, "meld into the commit below, combine messages"),
    (Act::Fixup, "Fixup", Key::F, "meld into the commit below, discard this message"),
    (Act::Drop, "Drop", Key::D, "remove the commit"),
];

/// Returns true when the editor should close.
pub fn show(ui: &mut Ui, ed: &mut Editor, git_dir: &Path, acts: &mut Vec<Action>) -> bool {
    let mut close = false;
    let n = ed.items.len();
    // Keyboard: ↑/↓ select, ⌥↑/⌥↓ move, P R S F D set the action.
    if !ui.ctx().memory(|m| m.focused().is_some()) {
        ui.input(|i| {
            let alt = i.modifiers.alt;
            if i.key_pressed(Key::ArrowUp) && ed.sel > 0 {
                if alt {
                    ed.items.swap(ed.sel, ed.sel - 1);
                }
                ed.sel -= 1;
            }
            if i.key_pressed(Key::ArrowDown) && ed.sel + 1 < n {
                if alt {
                    ed.items.swap(ed.sel, ed.sel + 1);
                }
                ed.sel += 1;
            }
            for (a, _, k, _) in ACTS {
                if i.key_pressed(k) && !i.modifiers.command {
                    ed.items[ed.sel].act = a;
                }
            }
            if i.key_pressed(Key::Escape) {
                close = true;
            }
        });
    }

    ui.horizontal(|ui| {
        let title = if ed.cherry { format!("{} Interactive cherry-pick", icon::CHERRIES) } else { format!("{} Interactive rebase", icon::GIT_COMMIT) };
        ui.label(RichText::new(title).heading().strong());
        ui.label(RichText::new(format!("{n} commit{} onto {}", if n == 1 { "" } else { "s" }, ed.label)).color(theme::dim(ui)));
    });
    ui.label(
        RichText::new("Newest at top. Keys: P pick · R reword · S squash · F fixup · D drop · ⌥↑/⌥↓ move")
            .small()
            .color(theme::dim(ui)),
    );
    ui.add_space(6.0);

    let footer = 56.0;
    egui::ScrollArea::vertical().max_height((ui.available_height() - footer).max(80.0)).auto_shrink([false, true]).show(ui, |ui| {
        let mut swap = None;
        for i in 0..n {
            let selected = i == ed.sel;
            let it = &mut ed.items[i];
            let frame = egui::Frame::new()
                .fill(if selected { theme::selection(ui) } else { theme::faint(ui) })
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(10, 6));
            let r = frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(i > 0, |ui| {
                        if theme::pill(ui, icon::CARET_UP).on_hover_text("Move up (⌥↑)").clicked() {
                            swap = Some((i, i - 1));
                        }
                    });
                    ui.add_enabled_ui(i + 1 < n, |ui| {
                        if theme::pill(ui, icon::CARET_DOWN).on_hover_text("Move down (⌥↓)").clicked() {
                            swap = Some((i, i + 1));
                        }
                    });
                    for (a, label, _, hint) in ACTS {
                        let color = match a {
                            Act::Drop => theme::DEL,
                            Act::Squash | Act::Fixup => theme::MOD,
                            Act::Reword | Act::Edit => theme::REN,
                            Act::Pick => theme::ADD,
                        };
                        let text = if it.act == a { RichText::new(label).strong().color(color) } else { RichText::new(label).color(theme::dim(ui)) };
                        if ui.add(egui::Button::selectable(it.act == a, text).corner_radius(255)).on_hover_text(hint).clicked() {
                            it.act = a;
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(&it.id[..it.id.len().min(7)]).monospace().color(theme::dim(ui)));
                    let summary = it.message.lines().next().unwrap_or("").to_owned();
                    let mut text = RichText::new(summary);
                    if it.act == Act::Drop {
                        text = text.strikethrough().color(theme::dim(ui));
                    }
                    ui.label(text);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&it.author).small().color(theme::dim(ui)));
                    });
                });
                if it.act == Act::Reword {
                    ui.add(TextEdit::multiline(&mut it.message).desired_rows(3).desired_width(f32::INFINITY));
                }
            });
            if r.response.interact(egui::Sense::click()).clicked() {
                ed.sel = i;
            }
            ui.add_space(4.0);
        }
        if let Some((a, b)) = swap {
            ed.items.swap(a, b);
            ed.sel = b;
        }
    });

    ui.add_space(6.0);
    let problem = ed.problem();
    if let Some(p) = problem {
        ui.colored_label(theme::DEL, p);
    }
    ui.horizontal(|ui| {
        let start = if ed.cherry { "Start cherry-pick" } else { "Start rebase" };
        if theme::primary(ui, problem.is_none(), &format!("{} {start}", icon::PLAY), 170.0).clicked() {
            match ed.op(git_dir) {
                Ok(op) => {
                    acts.push(Action::Git(op));
                    close = true;
                }
                Err(e) => acts.push(Action::Error(e)),
            }
        }
        if ui.button("Cancel").clicked() {
            close = true;
        }
    });
    close
}
