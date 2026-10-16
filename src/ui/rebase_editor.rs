//! The interactive rebase editor window (drawing + keyboard). The todo model
//! and git side live in `crate::repo::rebase`.

use crate::ui::format::{keys, plural};
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

fn field(commit: &str) -> egui::Id {
    egui::Id::new(("rebase_msg", commit))
}

/// Returns true when the editor should close.
pub fn show(ui: &mut Ui, ed: &mut Editor, git_dir: &Path, acts: &mut Vec<Action>) -> bool {
    let mut close = false;
    let n = ed.items.len();
    // Keyboard: ↑/↓ select, ⌥↑/⌥↓ move, P R S F D set the action.
    if crate::ui::format::nothing_was_focused(ui.ctx()) && !ui.ctx().memory(|m| m.focused().is_some() || m.top_modal_layer().is_some() || ed.items.iter().any(|it| it.act == Act::Reword && m.had_focus_last_frame(field(&it.id)))) {
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
        let title = if ed.cherry { format!("{}  Interactive cherry-pick", icon::CHERRIES) } else { format!("{}  Interactive rebase", icon::GIT_COMMIT) };
        ui.label(RichText::new(title).heading().strong());
        ui.label(RichText::new(format!("{n} commit{} onto {}", plural(n), ed.label)).color(theme::dim(ui)));
    });
    ui.label(
        RichText::new(keys(&format!("Newest at top. Keys: {}", super::shortcuts::REBASE)))
            .small()
            .color(theme::dim(ui)),
    );
    ui.add_space(6.0);

    let footer = 56.0;
    egui::ScrollArea::vertical().max_height((ui.available_height() - footer).max(80.0)).auto_shrink([false, true]).show(ui, |ui| {
        let mut swap = None;
        // Rows off screen are skipped as blank space of the height of a plain row (reword rows are taller).
        let row_id = egui::Id::new("rebase_row_h");
        let row_h: Option<f32> = ui.data(|d| d.get_temp(row_id));
        for i in 0..n {
            if let Some(h) = row_h.filter(|_| ed.items[i].act != Act::Reword) {
                let top = ui.cursor().min.y;
                let clip = ui.clip_rect();
                if top + h < clip.min.y || top > clip.max.y {
                    ui.add_space(h + 4.0 + ui.spacing().item_spacing.y);
                    continue;
                }
            }
            let selected = i == ed.sel;
            let it = &mut ed.items[i];
            let frame = egui::Frame::new()
                .fill(if selected { theme::selection(ui) } else { theme::faint(ui) })
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(10, 6));
            let r = frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(i > 0, |ui| {
                        if theme::pill(ui, icon::CARET_UP).on_hover_text(keys("Move up (⌥↑)")).clicked() {
                            swap = Some((i, i - 1));
                        }
                    });
                    ui.add_enabled_ui(i + 1 < n, |ui| {
                        if theme::pill(ui, icon::CARET_DOWN).on_hover_text(keys("Move down (⌥↓)")).clicked() {
                            swap = Some((i, i + 1));
                        }
                    });
                    for (a, label, _, hint) in ACTS {
                        let color = match a {
                            Act::Drop => theme::del(),
                            Act::Squash | Act::Fixup => theme::changed(),
                            Act::Reword | Act::Edit => theme::change_color('R'),
                            Act::Pick => theme::add(),
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
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&it.author).small().color(theme::dim(ui)));
                        theme::rest(ui, |ui| ui.label(text));
                    });
                });
                if it.act == Act::Reword {
                    ui.add(TextEdit::multiline(&mut it.message).id(field(&it.id)).margin(crate::theme::INPUT_MARGIN).desired_rows(3).desired_width(f32::INFINITY));
                }
            });
            if ed.items[i].act != Act::Reword {
                ui.data_mut(|d| d.insert_temp(row_id, r.response.rect.height()));
            }
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
        ui.colored_label(theme::del(), p);
    }
    ui.horizontal(|ui| {
        let start = if ed.cherry { "Start cherry-pick" } else { "Start rebase" };
        if theme::primary(ui, problem.is_none(), &format!("{}  {start}", icon::PLAY), 170.0).clicked() {
            match ed.start(git_dir) {
                Ok(a) => {
                    close = !matches!(a, Action::RunRebase { .. });
                    acts.push(a);
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

#[cfg(test)]
mod tests {
    use crate::repo::rebase::{Act, Editor};
    use crate::ui::testkit::Sim;
    use egui::{Event, Key, Modifiers};
    use std::path::{Path, PathBuf};

    fn editor(name: &str) -> (PathBuf, Editor) {
        let dir = std::env::temp_dir().join(format!("glasir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        for m in ["one", "two"] {
            crate::git::run(&dir, &["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-q", "-m", m]).unwrap();
        }
        let ed = Editor::load(&dir, "HEAD~1", "one").unwrap();
        (dir, ed)
    }

    fn frame(sim: &mut Sim, ed: &mut Editor, dir: &Path, keys: &[Key], dialog: bool) -> bool {
        let events = keys.iter().map(|&key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }).collect();
        let mut close = false;
        sim.frame(events, |ui| {
            close |= super::show(ui, ed, &dir.join(".git"), &mut vec![]);
            if dialog {
                egui::Modal::new(egui::Id::new("sheet")).show(ui.ctx(), |ui| ui.label("Keyboard shortcuts"));
            }
        });
        close
    }

    #[test]
    fn a_dialog_over_the_editor_keeps_the_keys_and_the_plan() {
        let (dir, mut ed) = editor("rebase-keys");
        let mut sim = Sim::new(800.0, 600.0);
        frame(&mut sim, &mut ed, &dir, &[], true);
        frame(&mut sim, &mut ed, &dir, &[], true);
        assert!(!frame(&mut sim, &mut ed, &dir, &[Key::D, Key::Escape], true), "Esc closes the dialog, not the editor");
        assert_eq!(ed.items[0].act, Act::Pick, "D belongs to the dialog too");
        frame(&mut sim, &mut ed, &dir, &[], false);
        frame(&mut sim, &mut ed, &dir, &[], false);
        assert!(frame(&mut sim, &mut ed, &dir, &[Key::D, Key::Escape], false));
        assert_eq!(ed.items[0].act, Act::Drop);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn start(sim: &mut Sim, t: &mut crate::repo::RepoTab, plan: impl FnOnce(&mut Editor)) -> Option<String> {
        let mut ed = Editor::load(&t.dir, "HEAD~3", "one").unwrap();
        plan(&mut ed);
        let (git_dir, mut acts) = (t.dir.join(".git"), vec![]);
        let mut draw = |ui: &mut egui::Ui| {
            super::show(ui, &mut ed, &git_dir, &mut acts);
        };
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s.ends_with("Start rebase")).expect("the Start button");
        sim.click(at, egui::PointerButton::Primary, &mut draw);
        for a in acts {
            t.apply(&sim.ctx, a);
        }
        crate::ui::testkit::load(t, &sim.ctx);
        t.confirm.take().and_then(|o| o.confirm)
    }

    #[test]
    fn cancelling_the_pushed_confirm_brings_the_plan_back() {
        use crate::repo::{Action, Op, RepoTab};
        let mut sim = Sim::new(800.0, 600.0);
        let mut t = crate::ui::testkit::tab(&sim.ctx, "rebase-park", &["one"]);
        let git = |t: &RepoTab, args: &[&str]| crate::git::run(&t.dir, args).unwrap();
        for f in ["two", "three", "four"] {
            std::fs::write(t.dir.join(f), f).unwrap();
            git(&t, &["add", f]);
            git(&t, &["commit", "-q", "-m", f]);
        }
        git(&t, &["update-ref", "refs/remotes/origin/main", "HEAD~2"]);
        let mut ed = Editor::load(&t.dir, "HEAD~3", "one").unwrap();
        ed.items[1].act = Act::Squash;
        t.rebase = Some(ed);
        let git_dir = t.dir.join(".git");
        let click = |sim: &mut Sim, t: &mut RepoTab, label: &str| {
            let mut acts = vec![];
            let mut draw = |ui: &mut egui::Ui| {
                if let Some(ed) = &mut t.rebase {
                    super::show(ui, ed, &git_dir, &mut acts);
                }
                crate::ui::dialogs::confirm_dialog(t, ui.ctx(), &mut acts);
            };
            sim.frame(vec![], &mut draw);
            sim.frame(vec![], &mut draw);
            let at = sim.text(|s| s.ends_with(label)).unwrap_or_else(|| panic!("no {label}"));
            sim.click(at, egui::PointerButton::Primary, &mut draw);
            for a in acts {
                t.apply(&sim.ctx, a);
            }
            crate::ui::testkit::load(t, &sim.ctx);
        };
        click(&mut sim, &mut t, "Start rebase");
        assert!(t.confirm.is_some() && t.rebase.is_none(), "the plan waits behind the confirm");
        click(&mut sim, &mut t, "Cancel");
        assert!(t.confirm.is_none() && t.rebase.as_ref().is_some_and(|ed| ed.items[1].act == Act::Squash), "Cancel brings the plan back");
        assert_eq!(git(&t, &["log", "--format=%s"]), "four\nthree\ntwo\none\n");
        click(&mut sim, &mut t, "Start rebase");
        let go = t.confirm.take().expect("it asks again");
        t.apply(&sim.ctx, Action::Git(Op { confirm: None, ..go }));
        crate::ui::testkit::load(&mut t, &sim.ctx);
        assert!(t.rebase.is_none());
        assert_eq!(git(&t, &["log", "--format=%s"]).lines().count(), 3, "the squash ran");
    }

    #[test]
    fn start_asks_only_when_the_plan_rewrites_pushed_commits() {
        let mut sim = Sim::new(800.0, 600.0);
        let mut t = crate::ui::testkit::tab(&sim.ctx, "rebase-start-pushed", &["one"]);
        let git = |t: &crate::repo::RepoTab, args: &[&str]| crate::git::run(&t.dir, args).unwrap();
        for f in ["two", "three", "four"] {
            std::fs::write(t.dir.join(f), f).unwrap();
            git(&t, &["add", f]);
            git(&t, &["commit", "-q", "-m", f]);
        }
        git(&t, &["update-ref", "refs/remotes/origin/main", "HEAD~2"]);
        let two = git(&t, &["rev-parse", "HEAD~2"]);
        let ask = start(&mut sim, &mut t, |ed| ed.items[1].act = Act::Squash);
        assert!(ask.as_deref().is_some_and(|c| c.contains("1 commit already on a remote branch")), "{ask:?}");
        assert_eq!(git(&t, &["log", "--format=%s"]), "four\nthree\ntwo\none\n");
        assert_eq!(start(&mut sim, &mut t, |ed| ed.items.swap(0, 1)), None);
        assert_eq!((git(&t, &["log", "--format=%s"]), git(&t, &["rev-parse", "HEAD~2"])), ("three\nfour\ntwo\none\n".into(), two));
    }

    #[test]
    fn escape_from_a_message_field_only_leaves_the_field() {
        let (dir, mut ed) = editor("rebase-field");
        ed.items[0].act = Act::Reword;
        let field = super::field(&ed.items[0].id);
        let mut sim = Sim::new(800.0, 600.0);
        frame(&mut sim, &mut ed, &dir, &[], false);
        sim.ctx.memory_mut(|m| m.request_focus(field));
        frame(&mut sim, &mut ed, &dir, &[], false);
        assert!(sim.ctx.memory(|m| m.has_focus(field)), "the message field has the keyboard");
        assert!(!frame(&mut sim, &mut ed, &dir, &[Key::D, Key::Escape], false), "Esc from the field only leaves it");
        assert!(sim.ctx.memory(|m| m.focused().is_none()));
        assert_eq!(ed.items[0].act, Act::Reword, "the plan keeps its reword");
        assert!(!frame(&mut sim, &mut ed, &dir, &[], false));
        assert!(frame(&mut sim, &mut ed, &dir, &[Key::Escape], false), "the next Esc closes the editor");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
