//! Reflog view: everywhere HEAD has been, newest first, with one-click recovery
//! (new branch, reset, checkout) for commits a reset or rebase left behind.
//! State lives in `crate::repo::reflog`; the inspector shows the selected commit.

use super::format::ago;
use crate::git::ReflogEntry;
use crate::repo::reflog::Reflog;
use crate::repo::{Action, Op, RepoData, Undo, prompt};
use crate::theme::{self, icon};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

fn action_color(action: &str) -> Color32 {
    match action.split([' ', '(']).next().unwrap_or("") {
        "commit" | "merge" | "cherry-pick" | "am" => theme::add(),
        "reset" => theme::del(),
        "rebase" | "pull" => theme::change_color('R'),
        "checkout" | "switch" => theme::accent(),
        _ => theme::changed(),
    }
}

/// Recovery actions for one entry: the header bar for the selection, and each row's menu.
fn entry_items(ui: &mut Ui, d: &RepoData, e: &ReflogEntry, acts: &mut Vec<Action>) {
    let short = &e.id[..e.id.len().min(7)];
    if theme::pill(ui, format!("{}  Create branch here…", icon::GIT_BRANCH)).on_hover_text("A new branch at this commit; your checkout stays as it is").clicked() {
        acts.push(prompt(format!("New branch at {short}"), "", Op::new("Create branch", &["branch", "{}", &e.id]).undo(Undo::Keep)));
    }
    if let Some(head) = d.status.branch.as_deref().filter(|_| d.status.head.as_deref() != Some(&e.id)) {
        // The branch name goes in the hover text: a long one would push the other buttons off-screen.
        let label = format!("{}  Reset branch here", icon::ARROW_U_UP_LEFT);
        let busy = crate::ui::graph_view::menus::reset_busy(d);
        ui.add_enabled_ui(busy.is_none(), |ui| ui.menu_button(label, |ui| crate::ui::graph_view::menus::reset_items(ui, d, &e.id, acts)))
            .inner
            .response
            .on_disabled_hover_text(busy.unwrap_or_default())
            .on_hover_text(format!("Reset {head} to {short}"));
    }
    if theme::pill(ui, format!("{}  Check out", icon::ARROW_BEND_UP_RIGHT)).on_hover_text("Check out this commit (detached HEAD)").clicked() {
        acts.push(Action::Git(Op::new(format!("Checkout {short}"), &["checkout", "--detach", &e.id]).undo(Undo::Keep)));
    }
}

/// Draws the reflog; returns true when it should close.
pub fn show(ui: &mut Ui, rl: &mut Reflog, d: &RepoData, acts: &mut Vec<Action>) -> bool {
    rl.poll();
    let mut close = super::format::esc_pressed(ui);
    let entries = match &rl.entries {
        Some(Ok(v)) => Some(v),
        _ => None,
    };
    ui.horizontal(|ui| {
        ui.set_height(30.0);
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        ui.label(RichText::new(format!("{}  Reflog", icon::LIFEBUOY)).strong());
        if let Some(v) = entries {
            let lost = v.iter().filter(|e| e.lost).map(|e| &e.id).collect::<std::collections::HashSet<_>>().len();
            let mut text = format!("where HEAD has been · {} entries", v.len());
            if lost > 0 {
                text += &format!(" · {lost} commit{} on no branch", if lost == 1 { "" } else { "s" });
            }
            ui.label(RichText::new(text).color(theme::dim(ui)));
        }
    });
    // The selected entry's recovery actions (always there, so the list doesn't jump).
    ui.horizontal(|ui| {
        ui.set_height(34.0);
        match rl.sel.and_then(|i| entries?.get(i)) {
            Some(e) => {
                theme::group(ui, |ui| entry_items(ui, d, e, acts));
            }
            None => _ = ui.label(RichText::new("Select an entry to see its commit and restore it; right-click for more.").color(theme::dim(ui))),
        }
    });
    ui.add_space(6.0);
    let v = match &rl.entries {
        None => {
            ui.centered_and_justified(|ui| ui.spinner());
            return close;
        }
        Some(Err(e)) => {
            ui.colored_label(theme::del(), e);
            return close;
        }
        Some(Ok(v)) => v,
    };
    egui::ScrollArea::vertical().id_salt("reflog").auto_shrink(false).show_rows(ui, 46.0, v.len(), |ui, range| {
        for i in range {
            let e = &v[i];
            let (rect, r) = theme::keyed(ui, ("reflog", &e.id, v.len() - 1 - i), |ui| ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click()));
            if rl.sel == Some(i) {
                ui.painter().rect_filled(rect, 10.0, theme::selection(ui));
            } else if r.hovered() {
                ui.painter().rect_filled(rect, 10.0, theme::hover(ui));
            }
            let p = ui.painter().with_clip_rect(rect);
            // Line 1: what moved HEAD (capsule), then the commit's summary.
            let col = action_color(&e.action);
            let g = p.layout_no_wrap(e.action.clone(), FontId::proportional(11.0), col);
            let cap = Rect::from_min_size(pos2(rect.left() + 10.0, rect.top() + 5.0), vec2(g.size().x + 14.0, 18.0));
            p.rect_filled(cap, 9.0, col.gamma_multiply(0.18));
            p.galley(pos2(cap.left() + 7.0, cap.center().y - g.size().y / 2.0), g, col);
            p.text(pos2(cap.right() + 8.0, cap.center().y), Align2::LEFT_CENTER, &e.summary, FontId::proportional(13.5), theme::text(ui));
            // Line 2: sha · when · the reflog message, and a badge for commits no ref reaches.
            let mut meta = format!("{}  ·  {}", &e.id[..e.id.len().min(7)], ago(e.time));
            if !e.detail.is_empty() && e.detail != e.summary {
                meta += &format!("  ·  {}", e.detail);
            }
            let m = p.text(pos2(rect.left() + 12.0, rect.top() + 34.0), Align2::LEFT_CENTER, meta, FontId::proportional(11.5), theme::dim(ui));
            if e.lost {
                let g = p.layout_no_wrap("on no branch".into(), FontId::proportional(10.5), theme::changed());
                let b = Rect::from_min_size(pos2(m.right() + 10.0, m.center().y - 8.0), vec2(g.size().x + 12.0, 16.0));
                p.rect_stroke(b, 8.0, egui::Stroke::new(1.0, theme::changed().gamma_multiply(0.7)), egui::StrokeKind::Inside);
                p.galley(pos2(b.left() + 6.0, b.center().y - g.size().y / 2.0), g, theme::changed());
            }
            let r = if e.lost { r.on_hover_text("Only the reflog remembers this commit: create a branch here to keep it.") } else { r };
            if r.clicked() {
                rl.sel = Some(i);
                acts.push(Action::Jump(e.id.clone()));
            }
            r.context_menu(|ui| {
                entry_items(ui, d, e, acts);
                ui.separator();
                if ui.button("Copy commit SHA").clicked() {
                    acts.push(Action::Copy(e.id.clone()));
                }
            });
        }
        if v.is_empty() {
            ui.label(RichText::new("The reflog is empty.").color(theme::dim(ui)));
        }
    });
    close
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testkit::{Sim, tab};

    #[test]
    fn a_menu_stays_with_its_entry_when_newer_ones_arrive() {
        let mut sim = Sim::new(520.0, 500.0);
        let mut t = tab(&sim.ctx, "reflog-menu", &["x"]);
        let d = t.data.take().unwrap();
        let mut rl = Reflog::open(&sim.ctx, &t.dir);
        crate::repo::testutil::wait_for("the reflog", || {
            rl.poll();
            rl.entries.is_some()
        });
        let entries = |n: i64| Some(Ok((0..n).rev().map(|i| ReflogEntry { id: format!("{i:07x}{}", "0".repeat(33)), time: i, action: "commit".into(), detail: String::new(), summary: format!("summary {i}"), lost: false }).collect()));
        rl.entries = entries(10);
        let mut acts = vec![];
        sim.open_menu("summary 4", |ui| _ = show(ui, &mut rl, &d, &mut acts));
        rl.entries = entries(11);
        sim.pick(&format!("{}  Create branch here", icon::GIT_BRANCH), |ui| _ = show(ui, &mut rl, &d, &mut acts));
        let titles: Vec<_> = acts.iter().filter_map(|a| if let Action::Prompt(p) = a { Some(p.title.as_str()) } else { None }).collect();
        assert_eq!(titles, ["New branch at 0000004"]);
    }
}
