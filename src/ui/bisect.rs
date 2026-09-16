//! Visual bisect: the banner above the graph during `git bisect`, and the bisect menu entries.

use crate::git::{Commit, State};
use crate::repo::{Action, Op, RepoData, op};
use crate::theme::{self, icon};
use egui::{RichText, Ui};

pub fn mark_color(mark: &str) -> egui::Color32 {
    match mark {
        "bad" => theme::DEL,
        "good" => theme::ADD,
        "testing" => theme::accent(),
        _ => theme::MOD,
    }
}

pub fn banner(ui: &mut Ui, d: &RepoData, acts: &mut Vec<Action>) {
    let b = d.bisect.clone().unwrap_or_default();
    let short = |id: &str| id[..id.len().min(7)].to_owned();
    let summary = |id: &str| d.row_of.get(id).and_then(|&r| d.commit(r)).map_or(String::new(), |c| c.summary.clone());
    egui::Frame::new().fill(theme::MOD.gamma_multiply(0.18)).inner_margin(egui::Margin::symmetric(10, 6)).corner_radius(12.0).show(ui, |ui| {
        let found = b.found().map(str::to_owned);
        let text = match (&found, &b.bad) {
            (Some(id), _) => format!("First bad commit: {} {}", short(id), summary(id)),
            (None, None) => "Mark a bad commit: right-click a row → Bisect".into(),
            (None, Some(_)) if b.good.is_empty() => "Mark a good commit: right-click a row → Bisect".into(),
            _ => {
                let left = b.left.saturating_sub(1);
                format!("{left} commit{} left to test  ·  about {} step{} to go", if left == 1 { "" } else { "s" }, b.steps + 1, if b.steps == 0 { "" } else { "s" })
            }
        };
        // Controls first: a long "first bad commit" summary must not push Reset off-screen.
        theme::bar(ui, |ui| {
            theme::group(ui, |ui| {
                if theme::pill(ui, format!("{}  Reset", icon::ARROW_U_UP_LEFT)).on_hover_text("End the bisect and return to where you started (git bisect reset)").clicked() {
                    acts.push(op("Reset bisect", &["bisect", "reset"]));
                }
            });
            if let Some(id) = &found {
                if theme::group(ui, |ui| theme::pill(ui, format!("{}  Show", icon::CROSSHAIR))).clicked() {
                    acts.push(Action::Jump(id.clone()));
                }
            } else {
                let head = d.status.head.as_deref().map(short).unwrap_or_default();
                theme::group(ui, |ui| {
                    for (what, ic, hint) in [
                        ("skip", icon::SKIP_FORWARD, "Can't test this one: pick another commit nearby"),
                        ("bad", icon::THUMBS_DOWN, "The checked-out commit has the problem"),
                        ("good", icon::THUMBS_UP, "The checked-out commit works"),
                    ] {
                        let label = RichText::new(format!("{ic} {}", if what == "skip" { "Skip" } else if what == "bad" { "Bad" } else { "Good" })).color(mark_color(what));
                        if theme::pill(ui, label).on_hover_text(format!("{hint} ({head})")).clicked() {
                            acts.push(op(format!("Bisect: {what} {head}"), &["bisect", what]));
                        }
                    }
                });
            }
            theme::rest(ui, |ui| {
                ui.label(RichText::new(format!("{}  Bisect", icon::BUG)).strong());
                ui.label(RichText::new(text).color(if found.is_some() { theme::DEL } else { theme::text(ui) }));
            });
        });
    });
    ui.add_space(4.0);
}

pub fn commit_items(ui: &mut Ui, d: &RepoData, c: &Commit, acts: &mut Vec<Action>) {
    let short = &c.id[..c.id.len().min(7)];
    let is_head = d.status.head.as_deref() == Some(&c.id);
    match d.state {
        State::Clean => {
            ui.menu_button(format!("{}  Bisect", icon::BUG), |ui| {
                if ui.button("Start bisect: this commit is bad").clicked() {
                    acts.push(op(format!("Start bisect at {short}"), &["bisect", "start", &c.id]));
                }
                if !is_head && ui.button("Start bisect: this commit is good (HEAD is bad)").clicked() {
                    acts.push(op(format!("Start bisect from {short}"), &["bisect", "start", "HEAD", &c.id]));
                }
            });
        }
        State::Bisecting => {
            ui.menu_button(format!("{}  Bisect", icon::BUG), |ui| {
                for what in ["good", "bad", "skip"] {
                    if ui.button(RichText::new(format!("Mark {short} as {what}")).color(mark_color(what))).clicked() {
                        acts.push(Action::Git(Op::new(format!("Bisect: {what} {short}"), &["bisect", what, &c.id])));
                    }
                }
            });
        }
        _ => {}
    }
}

/// Two selected commits: the newer one is bad, the older one good.
pub fn multi_item(ui: &mut Ui, d: &RepoData, multi: &[String], acts: &mut Vec<Action>) {
    if d.state == State::Clean && multi.len() == 2 && ui.button(format!("{}  Start bisect between these (newer is bad)", icon::BUG)).clicked() {
        acts.push(op("Start bisect", &["bisect", "start", &multi[0], &multi[1]]));
    }
}
