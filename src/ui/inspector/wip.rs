//! Inspector for the WIP row: operation-in-progress banner, conflicted /
//! unstaged / staged file lists and the commit box.

use super::{file_items, file_row, lfs_track_item};
use crate::ai::Purpose;
use crate::git::State;
use crate::repo::{Action, DiffSrc, Op, RepoTab, op};
use crate::theme::{self, icon};
use egui::{Align, Layout, RichText, ScrollArea, TextEdit, Ui};

impl RepoTab {
    pub(super) fn wip_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let s = &d.status;
        let open = self.diff.as_ref().map(|d| d.src.clone());
        let busy = self.busy().is_some();

        if d.state != State::Clean {
            let (what, cmd) = match d.state {
                State::Merging => ("Merge", "merge"),
                State::Rebasing => ("Rebase", "rebase"),
                State::CherryPicking => ("Cherry-pick", "cherry-pick"),
                State::Reverting => ("Revert", "revert"),
                _ => ("Patch (git am)", "am"),
            };
            egui::Frame::new().fill(theme::MOD.gamma_multiply(0.2)).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.label(RichText::new(format!("{what} in progress")).strong());
                if !s.conflicted.is_empty() {
                    ui.label(format!("{} conflicted file(s) — resolve, then continue.", s.conflicted.len()));
                }
                ui.horizontal(|ui| {
                    if ui.button("Continue").clicked() {
                        acts.push(Action::Git(Op::new(format!("Continue {cmd}"), &["-c", "core.editor=true", cmd, "--continue"])));
                    }
                    if cmd == "am" && ui.button("Skip this patch").clicked() {
                        acts.push(op("Skip patch", &["am", "--skip"]));
                    }
                    if ui.button("Abort").clicked() {
                        acts.push(op(format!("Abort {cmd}"), &[cmd, "--abort"]));
                    }
                });
            });
            ui.add_space(6.0);
        }

        let generating = self.ai_busy() == Some(&Purpose::CommitMessage);
        let mut commit = false;
        egui::Panel::bottom("commit_box").frame(egui::Frame::NONE.inner_margin(egui::Margin { top: 8, ..Default::default() })).resizable(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Commit").small().strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if generating {
                        ui.spinner();
                        ui.label(RichText::new("Writing…").small().color(theme::dim(ui)));
                    } else if ui
                        .add_enabled(!s.staged.is_empty(), egui::Button::new(format!("{} AI message", icon::SPARKLE)).small())
                        .on_hover_text("Write the commit message from the staged changes with Claude")
                        .on_disabled_hover_text("Stage changes first")
                        .clicked()
                    {
                        acts.push(Action::Ai(Purpose::CommitMessage));
                    }
                });
            });
            ui.horizontal(|ui| {
                let n = self.summary.chars().count();
                ui.add(TextEdit::singleline(&mut self.summary).hint_text("Summary").desired_width(ui.available_width() - 30.0));
                ui.label(RichText::new(format!("{n}")).small().color(if n > 72 { theme::DEL } else { theme::dim(ui) }));
            });
            ui.add(TextEdit::multiline(&mut self.body).hint_text("Description").desired_rows(4).desired_width(f32::INFINITY));
            let was = self.amend;
            // Nothing to amend before the first commit.
            if s.head.is_some() {
                ui.checkbox(&mut self.amend, "Amend previous commit");
            }
            if self.amend && !was && self.summary.is_empty() {
                if let Ok(msg) = crate::git::run(&self.dir, &["log", "-1", "--format=%B"]) {
                    let (s, b) = msg.trim().split_once('\n').unwrap_or((msg.trim(), ""));
                    self.summary = s.into();
                    self.body = b.trim().into();
                }
            }
            let n = s.staged.len();
            let can = (n > 0 || self.amend) && !self.summary.trim().is_empty() && !busy;
            let label = if self.amend { "Amend commit".to_owned() } else { format!("Commit changes to {n} file{}", if n == 1 { "" } else { "s" }) };
            let btn = theme::primary(ui, can, &label, ui.available_width());
            let shortcut = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
            if can && (btn.clicked() || shortcut) {
                commit = true;
            }
        });
        if commit {
            acts.push(Action::Git(self.commit_op()));
        }
        {
            ScrollArea::vertical().id_salt("wip_files").auto_shrink(false).show(ui, |ui| {
                if !s.conflicted.is_empty() {
                    ui.label(RichText::new(format!("Conflicts  {}", s.conflicted.len())).small().strong().color(theme::DEL));
                    for c in &s.conflicted {
                        let resp = file_row(ui, c, self.merge.as_ref().is_some_and(|m| m.path == c.path), d.lfs.files.contains(&c.path));
                        resp.context_menu(|ui| {
                            if ui.button(format!("{} Resolve with AI…", icon::SPARKLE)).clicked() {
                                acts.push(Action::Ai(Purpose::Resolve(c.path.clone())));
                            }
                            if ui.button("Mark resolved").clicked() {
                                acts.push(Action::Git(Op::new("Mark resolved", &["--literal-pathspecs", "add", "--", &c.path]).light()));
                            }
                            if ui.button("Use ours").clicked() {
                                acts.push(Action::Git(Op::new("Use ours", &["--literal-pathspecs", "checkout", "--ours", "--", &c.path]).then(&["--literal-pathspecs", "add", "--", &c.path]).light()));
                            }
                            if ui.button("Use theirs").clicked() {
                                acts.push(Action::Git(Op::new("Use theirs", &["--literal-pathspecs", "checkout", "--theirs", "--", &c.path]).then(&["--literal-pathspecs", "add", "--", &c.path]).light()));
                            }
                            if ui.button("Show in file manager").clicked() {
                                acts.push(Action::Reveal(c.path.clone()));
                            }
                            if ui.button("Restore conflict markers").clicked() {
                                acts.push(Action::Git(Op::new("Restore conflict", &["--literal-pathspecs", "checkout", "-m", "--", &c.path]).light()));
                            }
                        });
                        if resp.clicked() {
                            acts.push(Action::MergeEditor(c.path.clone()));
                        }
                    }
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Unstaged files  {}", s.unstaged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !s.unstaged.is_empty() && ui.small_button("Stage all").clicked() {
                            acts.push(Action::Git(Op::new("Stage all", &["add", "-A"]).light()));
                        }
                    });
                });
                if s.unstaged.is_empty() {
                    ui.label(RichText::new("No unstaged changes").small().color(theme::dim(ui)));
                }
                for c in &s.unstaged {
                    let src = DiffSrc::Worktree { change: c.clone(), staged: false };
                    let resp = file_row(ui, c, open.as_ref() == Some(&src), d.lfs.files.contains(&c.path));
                    if resp.clicked() {
                        acts.push(Action::Diff(src));
                    }
                    if resp.double_clicked() {
                        acts.push(Action::Git(Op::new("Stage", &["--literal-pathspecs", "add", "--", &c.path]).light()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Stage").clicked() {
                            acts.push(Action::Git(Op::new("Stage", &["--literal-pathspecs", "add", "--", &c.path]).light()));
                        }
                        if ui.button("Discard changes").clicked() {
                            let o = if c.code == '?' {
                                Op::new("Discard", &["--literal-pathspecs", "clean", "-fq", "--", &c.path])
                            } else {
                                Op::new("Discard", &["--literal-pathspecs", "checkout", "--", &c.path])
                            };
                            acts.push(Action::Git(o.light().backup(crate::repo::Backup::Paths(vec![c.path.clone()])).confirm(format!("Discard changes to {}? (Undo with ⌘Z.)", c.path))));
                        }
                        lfs_track_item(ui, d, &c.path, acts);
                        if c.code == '?' && ui.button("Add to .gitignore").clicked() {
                            match crate::repo::files::append_gitignore(&self.dir, &c.path) {
                                Ok(()) => acts.push(Action::Reload),
                                Err(e) => acts.push(Action::Error(format!("Could not update .gitignore: {e}"))),
                            }
                        }
                        if ui.button("Show in file manager").clicked() {
                            acts.push(Action::Reveal(c.path.clone()));
                        }
                        if c.code != '?' {
                            file_items(ui, &c.path, acts);
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(c.path.clone()));
                        }
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Staged files  {}", s.staged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !s.staged.is_empty() && ui.small_button("Unstage all").clicked() {
                            acts.push(Action::Git(Op::new("Unstage all", &["reset", "-q"]).light()));
                        }
                    });
                });
                if s.staged.is_empty() {
                    ui.label(RichText::new("Nothing staged yet: double-click a file above, or Stage all").small().color(theme::dim(ui)));
                }
                for c in &s.staged {
                    let src = DiffSrc::Worktree { change: c.clone(), staged: true };
                    let resp = file_row(ui, c, open.as_ref() == Some(&src), d.lfs.files.contains(&c.path));
                    if resp.clicked() {
                        acts.push(Action::Diff(src));
                    }
                    if resp.double_clicked() {
                        acts.push(Action::Git(Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--", &c.path]).light()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Unstage").clicked() {
                            acts.push(Action::Git(Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--", &c.path]).light()));
                        }
                        lfs_track_item(ui, d, &c.path, acts);
                        if c.code != 'A' {
                            file_items(ui, &c.path, acts);
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(c.path.clone()));
                        }
                    });
                }
            });
        }
    }
}
