//! Branch drag-and-drop: drag a branch (sidebar row or graph pill) onto another
//! to get the merge / rebase / push menu.

use crate::repo::Action;
use crate::theme::{self, icon};
use egui::{Id, RichText, Sense, Stroke, Ui, vec2};

/// Drag-and-drop payload: a branch name (`main`, `origin/feature`).
pub struct BranchDrag(pub String);

/// Makes `resp` a drag source and drop target for branch `name`.
pub fn branch_dnd(ui: &Ui, resp: &egui::Response, name: &str, acts: &mut Vec<Action>) {
    let resp = resp.interact(Sense::drag());
    resp.dnd_set_drag_payload(BranchDrag(name.to_owned()));
    if resp.dnd_hover_payload::<BranchDrag>().is_some_and(|p| p.0 != name) {
        ui.painter().rect_stroke(resp.rect, 8.0, Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
    }
    if let Some(p) = resp.dnd_release_payload::<BranchDrag>()
        && p.0 != name
    {
        acts.push(Action::DropBranch { src: p.0.clone(), dst: name.to_owned() });
    }
}

pub fn preview(ctx: &egui::Context) {
    if let Some(p) = egui::DragAndDrop::payload::<BranchDrag>(ctx)
        && let Some(pos) = ctx.pointer_hover_pos()
    {
        egui::Area::new(Id::new("dnd_preview")).order(egui::Order::Tooltip).fixed_pos(pos + vec2(14.0, 10.0)).interactable(false).show(ctx, |ui| {
            theme::group(ui, |ui| ui.label(RichText::new(format!("{} {}", icon::GIT_BRANCH, p.0)).strong()));
        });
    }
}
