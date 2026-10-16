//! Commit Composer dialog: Claude's commit plan, editable (messages, moving
//! changes between commits, split / merge / reorder), then Create commits.

use crate::ui::format::{dismissed, keys, plural};
use crate::repo::{Action, RepoTab};
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, RichText, TextEdit};

enum Edit {
    Move(usize, Option<usize>),
    MergeUp(usize),
    Swap(usize),
}

pub(super) fn compose_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let busy = t.busy().is_some();
    let Some(p) = t.compose.as_mut().filter(|p| !p.commits.is_empty()) else { return };
    let (mut close, mut run, mut edit) = (false, false, None);
    egui::Modal::new(Id::new("compose")).show(ctx, |ui| {
        ui.set_width(680.0);
        let what = match (p.replaces, p.wip) {
            (0, _) => "your uncommitted changes".to_owned(),
            (n, wip) => format!("the changes of your last {n} commits{}", if wip { " and your uncommitted ones" } else { "" }),
        };
        ui.heading(format!("{}  {}", icon::SPARKLE, if p.replaces > 0 { "Recompose commits" } else { "Compose commits" }));
        ui.label(
            RichText::new(keys(&format!("{}'s plan for {what}, oldest commit first. Edit the messages, move changes between commits, then create them. Undo with ⌘Z.", crate::ai::name())))
                .small()
                .color(theme::dim(ui)),
        );
        if let Some(n) = &p.note {
            ui.label(RichText::new(format!("{}  {n}", icon::WARNING)).small().color(theme::changed()));
        }
        ui.add_space(6.0);
        let n = p.commits.len();
        // Short names for the move menus, before the messages are borrowed for editing.
        let names: Vec<String> = p.commits.iter().enumerate().map(|(k, c)| format!("{}. {}", k + 1, c.message.lines().next().unwrap_or(""))).collect();
        egui::ScrollArea::vertical().max_height(480.0).auto_shrink([false, true]).show(ui, |ui| {
            for (k, c) in p.commits.iter_mut().enumerate() {
                Frame::new().fill(theme::faint(ui)).corner_radius(12).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}  Commit {}", icon::GIT_COMMIT, k + 1)).strong());
                        ui.label(RichText::new(format!("{} change{}", c.units.len(), plural(c.units.len()))).small().color(theme::dim(ui)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if k > 0 && theme::pill(ui, icon::ARROWS_MERGE).on_hover_text("Merge into the commit above").clicked() {
                                edit = Some(Edit::MergeUp(k));
                            }
                            if k + 1 < n && theme::pill(ui, icon::ARROW_DOWN).on_hover_text("Move down").clicked() {
                                edit = Some(Edit::Swap(k));
                            }
                            if k > 0 && theme::pill(ui, icon::ARROW_UP).on_hover_text("Move up").clicked() {
                                edit = Some(Edit::Swap(k - 1));
                            }
                        });
                    });
                    ui.add(TextEdit::multiline(&mut c.message).id_salt((p.id, &c.units)).margin(crate::theme::INPUT_MARGIN).hint_text("Commit message").desired_rows(1).desired_width(f32::INFINITY));
                    // Fixed-height rows, so only the ones in view are built (a plan can hold thousands).
                    let rh = ui.spacing().interact_size.y;
                    let step = rh + ui.spacing().item_spacing.y;
                    let (clip, top) = (ui.clip_rect(), ui.cursor().top());
                    let len = c.units.len();
                    let lo = (((clip.top() - top) / step).floor().max(0.0) as usize).min(len);
                    let hi = (((clip.bottom() - top) / step).ceil().max(0.0) as usize).clamp(lo, len);
                    ui.add_space(lo as f32 * step);
                    for &u in &c.units[lo..hi] {
                        let unit = &p.units[u];
                        let row = egui::vec2(ui.available_width(), rh);
                        theme::keyed(ui, ("unit", u), |ui| ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.menu_button(RichText::new(icon::ARROWS_DOWN_UP).small(), |ui| {
                                for (j, name) in names.iter().enumerate().filter(|(j, _)| *j != k) {
                                    if ui.add(Button::new(format!("Move to {name}")).truncate()).clicked() {
                                        edit = Some(Edit::Move(u, Some(j)));
                                    }
                                }
                                if c.units.len() > 1 && ui.button(format!("{}  Split into a new commit", icon::ARROWS_SPLIT)).clicked() {
                                    edit = Some(Edit::Move(u, None));
                                }
                            })
                            .response
                            .on_hover_text("Move this change to another commit");
                            ui.label(RichText::new(&unit.path).strong());
                            ui.label(RichText::new(&unit.what).small().color(theme::dim(ui)));
                        }));
                    }
                    ui.add_space((len - hi) as f32 * step);
                });
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ready = p.commits.iter().all(|c| !c.message.trim().is_empty());
            if theme::primary(ui, ready && !busy, &format!("Create {n} commit{}", plural(n)), 170.0).on_disabled_hover_text("Every commit needs a message").clicked() {
                run = true;
            }
            if dismissed(ui, "Cancel") {
                close = true;
            }
        });
    });
    match edit {
        Some(Edit::Move(u, to)) => p.move_unit(u, to),
        Some(Edit::MergeUp(k)) => p.merge_up(k),
        Some(Edit::Swap(k)) => p.commits.swap(k, k + 1),
        None => {}
    }
    if run {
        acts.push(Action::CommitPlan);
    } else if close {
        t.compose = None;
    }
}

#[cfg(test)]
mod tests {
    use crate::repo::{Action, compose::Plan};
    use crate::ui::testkit::{Sim, load, tab};

    /// Create builds the plan's op off the UI thread (it reads every change again), then the
    /// commits are made as one ⌘Z step.
    #[test]
    fn create_builds_the_op_off_the_ui_thread() {
        let mut sim = Sim::new(900.0, 700.0);
        let mut t = tab(&sim.ctx, "compose-create", &["one"]);
        let git = |t: &crate::repo::RepoTab, args: &[&str]| crate::git::run(&t.dir, args).unwrap();
        std::fs::write(t.dir.join("f"), "two").unwrap();
        let (mut plan, _) = Plan::load(&t.dir, None).unwrap();
        plan.fill(r#"{"commits":[{"message":"Two","units":[1]}]}"#).unwrap();
        t.compose = Some(plan);
        let mut acts = vec![];
        let mut draw = |ui: &mut egui::Ui| super::compose_dialog(&mut t, ui.ctx(), &mut acts);
        sim.frame(vec![], &mut draw);
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s == "Create 1 commit").expect("the Create button");
        sim.click(at, egui::PointerButton::Primary, &mut draw);
        for a in acts {
            t.apply(&sim.ctx, a);
        }
        assert!(t.compose.is_none());
        assert_eq!(t.busy(), Some("Preparing the rewrite…"));
        load(&mut t, &sim.ctx);
        assert_eq!((git(&t, &["log", "--format=%s"]), git(&t, &["status", "--porcelain"])), ("Two\none\n".into(), String::new()));
        t.apply(&sim.ctx, Action::Undo);
        load(&mut t, &sim.ctx);
        assert_eq!((git(&t, &["log", "--format=%s"]), git(&t, &["status", "--porcelain"])), ("one\n".into(), "M  f\n".into()));
    }
}
