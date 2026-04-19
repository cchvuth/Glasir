//! Inspector for the WIP row: conflicted / unstaged / staged file lists and
//! the commit box.

use super::file_row;
use crate::repo::{Action, Op, RepoTab};
use crate::theme;
use egui::{Align, Layout, RichText, ScrollArea, TextEdit, Ui};

impl RepoTab {
    pub(super) fn wip_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let s = &d.status;
        let busy = self.busy().is_some();

        let mut commit = false;
        egui::Panel::bottom("commit_box").frame(egui::Frame::NONE.inner_margin(egui::Margin { top: 8, ..Default::default() })).resizable(false).show(ui, |ui| {
            ui.label(RichText::new("Commit").small().strong());
            ui.horizontal(|ui| {
                let n = self.summary.chars().count();
                ui.add(TextEdit::singleline(&mut self.summary).hint_text("Summary").desired_width(ui.available_width() - 30.0));
                ui.label(RichText::new(format!("{n}")).small().color(if n > 72 { theme::DEL } else { theme::dim(ui) }));
            });
            ui.add(TextEdit::multiline(&mut self.body).hint_text("Description").desired_rows(4).desired_width(f32::INFINITY));
            let was = self.amend;
            ui.checkbox(&mut self.amend, "Amend previous commit");
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
            if n == 0 && !s.unstaged.is_empty() && !self.amend {
                ui.label(RichText::new("Stage files to commit (double-click a file, or Stage all).").small().color(theme::dim(ui)));
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
                        let resp = file_row(ui, c);
                        resp.context_menu(|ui| {
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
                    }
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Unstaged Files  {}", s.unstaged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !s.unstaged.is_empty() && ui.small_button("Stage all").clicked() {
                            acts.push(Action::Git(Op::new("Stage all", &["add", "-A"]).light()));
                        }
                    });
                });
                for c in &s.unstaged {
                    let resp = file_row(ui, c);
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
                            acts.push(Action::Git(o.light().confirm(format!("Discard changes to {}?", c.path))));
                        }
                        if c.code == '?' && ui.button("Add to .gitignore").clicked() {
                            match crate::repo::files::append_gitignore(&self.dir, &c.path) {
                                Ok(()) => acts.push(Action::Reload),
                                Err(e) => acts.push(Action::Error(format!("Could not update .gitignore: {e}"))),
                            }
                        }
                        if ui.button("Show in file manager").clicked() {
                            acts.push(Action::Reveal(c.path.clone()));
                        }
                        if ui.button("Copy path").clicked() {
                            acts.push(Action::Copy(c.path.clone()));
                        }
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Staged Files  {}", s.staged.len())).small().strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !s.staged.is_empty() && ui.small_button("Unstage all").clicked() {
                            acts.push(Action::Git(Op::new("Unstage all", &["reset", "-q"]).light()));
                        }
                    });
                });
                for c in &s.staged {
                    let resp = file_row(ui, c);
                    if resp.double_clicked() {
                        acts.push(Action::Git(Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--", &c.path]).light()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Unstage").clicked() {
                            acts.push(Action::Git(Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--", &c.path]).light()));
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
