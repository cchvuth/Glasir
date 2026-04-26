//! The commit graph table: refs | graph | message | author | date.
//! Only visible rows are laid out and painted, so 100k+ commits scroll smoothly.
//!
//! `mod.rs` paints the table and handles row clicks/keys; `menus` the
//! commit/WIP context menus, `dnd` branch drag-and-drop, `format`
//! dates and initials.

pub mod dnd;
pub mod menus;

use crate::git::{Ref, RefKind};
use crate::graph::NONE;
use crate::repo::{Action, RepoData, Sel, branch_checkout};
use crate::theme::{self, icon};
use dnd::BranchDrag;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use crate::ui::format::{initials, relative_time};
use menus::{checkout_action, commit_menu, wip_menu};

pub const ROW: f32 = 28.0;
pub const LANE: f32 = 20.0;
const DATE_W: f32 = 116.0;
/// The message column never gets narrower than this; the graph gives way first.
const MSG_MIN: f32 = 220.0;

fn corner(pts: &mut Vec<Pos2>, a: Pos2, c: Pos2, b: Pos2) {
    for i in 1..=6 {
        let t = i as f32 / 6.0;
        let u = 1.0 - t;
        pts.push(pos2(u * u * a.x + 2.0 * u * t * c.x + t * t * b.x, u * u * a.y + 2.0 * u * t * c.y + t * t * b.y));
    }
}

fn ref_label(r: &Ref) -> String {
    match r.kind {
        RefKind::Local if r.head => format!("{} {}", icon::CHECK, r.name),
        RefKind::Local => format!("{} {}", icon::GIT_BRANCH, r.name),
        RefKind::Remote => format!("{} {}", icon::CLOUD, r.name),
        RefKind::Tag => format!("{} {}", icon::TAG, r.name),
    }
}

pub fn show(
    ui: &mut Ui,
    d: &RepoData,
    sel: &Option<Sel>,
    scroll_to: &mut Option<usize>,
    acts: &mut Vec<Action>,
) {
    let sel_row = match sel {
        Some(Sel::Wip) if d.wip => Some(0),
        Some(Sel::Commit(id)) => d.row_of.get(id).copied(),
        _ => None,
    };
    keyboard_nav(ui, d, sel_row, scroll_to, acts);

    // Column widths: [refs, graph]; graph 0 = fit lanes (up to 12). Drag the header to resize.
    let cols_id = egui::Id::new("graph_cols");
    let [refs_w, graph_w] = ui.data_mut(|m| *m.get_persisted_mut_or(cols_id, [160.0f32, 0.0]));
    let full = ui.available_rect_before_wrap();
    let author_w = if full.width() > 1000.0 { 130.0 } else { 0.0 };
    let graph_w = if graph_w > 0.0 { graph_w } else { d.graph.width.clamp(1, 10) as f32 * LANE + 12.0 };
    let graph_w = graph_w.min(full.width() - refs_w - author_w - DATE_W - MSG_MIN).max(2.0 * LANE + 12.0);
    let x_graph = full.left() + refs_w;
    let x_msg = x_graph + graph_w;
    let x_date = full.right() - DATE_W;
    let x_author = x_date - author_w;

    let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 24.0), Sense::hover());
    let p = ui.painter();
    let hf = FontId::proportional(11.0);
    let dim = theme::dim(ui);
    for (x, t) in [(full.left(), "BRANCH / TAG"), (x_graph, "GRAPH"), (x_msg, "COMMIT MESSAGE"), (x_author, "AUTHOR"), (x_date, "DATE")] {
        if (t != "AUTHOR" || author_w > 0.0) && (t != "GRAPH" || graph_w > 84.0) {
            p.text(pos2(x + 8.0, hdr.center().y), Align2::LEFT_CENTER, t, hf.clone(), dim);
        }
    }
    for (i, x) in [x_graph, x_msg].into_iter().enumerate() {
        let handle = Rect::from_center_size(pos2(x, hdr.center().y), vec2(8.0, hdr.height()));
        let r = ui.interact(handle, cols_id.with(i), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        ui.painter().vline(x, hdr.shrink2(vec2(0.0, 5.0)).y_range(), Stroke::new(1.0, theme::faint(ui)));
        if r.dragged() {
            let dx = r.drag_delta().x;
            ui.data_mut(|m| {
                let w = m.get_persisted_mut_or(cols_id, [190.0f32, 0.0]);
                if i == 0 {
                    w[0] = (refs_w + dx).clamp(60.0, 500.0);
                } else {
                    w[1] = (graph_w + dx).clamp(LANE + 12.0, 1200.0);
                }
            });
        }
    }

    let n = d.rows();
    let body = Rect::from_min_max(ui.cursor().min, pos2(full.right(), full.bottom()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().id_salt(("graph", &d.git_dir)).auto_shrink(false).show_rows(ui, ROW, n, |ui, range| {
            let top = ui.max_rect().top() - range.start as f32 * ROW;
            let y = |r: usize| top + r as f32 * ROW + ROW / 2.0;
            let x = |c: u16| x_graph + 8.0 + c as f32 * LANE + LANE / 2.0;
            let left = full.left();

            if let Some(r) = scroll_to.take() {
                let rect = Rect::from_min_size(pos2(left, top + r as f32 * ROW), vec2(1.0, ROW));
                if !(range.start + 1..range.end.saturating_sub(1)).contains(&r) {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            }

            let mut hovered = None;
            for r in range.clone() {
                let rect = Rect::from_min_size(pos2(left, top + r as f32 * ROW), vec2(full.width(), ROW));
                let resp = ui.interact(rect, ui.id().with(("row", r)), Sense::click());
                let pill = rect.shrink2(vec2(2.0, 1.5));
                let commit = d.commit(r);
                if Some(r) == sel_row {
                    ui.painter().rect_filled(pill, 9.0, theme::selection(ui));
                } else if resp.hovered() {
                    hovered = Some(r);
                    ui.painter().rect_filled(pill, 9.0, theme::hover(ui));
                }
                if resp.clicked() {
                    acts.push(Action::Select(commit.map_or(Sel::Wip, |c| Sel::Commit(c.id.clone()))));
                }
                if resp.double_clicked()
                    && let Some(c) = commit
                {
                    acts.extend(checkout_action(d, c));
                }
                // Dropping a branch onto a row targets that row's branch.
                if let Some(dst) = commit.and_then(|c| d.refs_at.get(&c.id)).and_then(|v| v.iter().map(|&i| &d.refs[i]).find(|r| r.kind != RefKind::Tag)) {
                    if resp.dnd_hover_payload::<BranchDrag>().is_some_and(|p| p.0 != dst.name) {
                        ui.painter().rect_stroke(rect.shrink2(vec2(2.0, 1.5)), 9.0, Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
                    }
                    if let Some(p) = resp.dnd_release_payload::<BranchDrag>()
                        && p.0 != dst.name
                    {
                        acts.push(Action::DropBranch { src: p.0.clone(), dst: dst.name.clone() });
                    }
                }
                resp.context_menu(|ui| match commit {
                    Some(c) => commit_menu(ui, d, c, acts),
                    None => wip_menu(ui, acts),
                });
            }
            let _ = hovered;

            let gp = ui.painter().with_clip_rect(Rect::from_x_y_ranges(x_graph..=x_msg - 2.0, ui.clip_rect().y_range()));
            let r = LANE.min(ROW) * 0.5;
            let bottom = y(n) + ROW;
            for e in d.graph.edges_in(range.clone()) {
                let (x1, xl, x2) = (x(e.from_col), x(e.lane), x(e.to_col));
                let (y1, y2) = (y(e.from as usize), if e.to == NONE { bottom } else { y(e.to as usize) });
                let mut pts = vec![pos2(x1, y1)];
                if xl != x1 {
                    let s = (xl - x1).signum();
                    pts.push(pos2(xl - s * r, y1));
                    corner(&mut pts, pos2(xl - s * r, y1), pos2(xl, y1), pos2(xl, y1 + r));
                }
                if xl != x2 && e.to != NONE {
                    let s = (x2 - xl).signum();
                    pts.push(pos2(xl, y2 - r));
                    corner(&mut pts, pos2(xl, y2 - r), pos2(xl, y2), pos2(xl + s * r, y2));
                    pts.push(pos2(x2, y2));
                } else {
                    pts.push(pos2(xl, y2));
                }
                let stroke = Stroke::new(2.0, theme::lane(e.color));
                if d.wip && e.from == 0 {
                    gp.extend(Shape::dashed_line(&pts, stroke, 4.0, 3.0));
                } else {
                    gp.add(Shape::line(pts, stroke));
                }
            }

            let painter = ui.painter();
            let body = FontId::proportional(13.0);
            let text = theme::text(ui);
            let head_id = d.status.head.as_deref();
            for r in range {
                let node = d.graph.nodes[r];
                let c = pos2(x(node.col), y(r));
                let col = theme::lane(node.color);
                let commit = d.commit(r);
                match commit {
                    None => {
                        gp.circle(c, 7.0, theme::bg(ui), Stroke::new(2.0, col));
                    }
                    Some(cm) if cm.parents.len() > 1 => {
                        gp.circle_filled(c, 5.0, col);
                    }
                    Some(cm) => {
                        if head_id == Some(cm.id.as_str()) {
                            gp.circle_stroke(c, 11.5, Stroke::new(2.0, col));
                        }
                        gp.circle_filled(c, 9.0, theme::avatar(&cm.email));
                        gp.circle_stroke(c, 9.0, Stroke::new(1.5, col));
                        gp.text(c, Align2::CENTER_CENTER, initials(&cm.author), FontId::proportional(9.0), Color32::WHITE);
                    }
                }

                let ids = commit.and_then(|cm| d.refs_at.get(&cm.id));
                if let Some(ids) = ids.filter(|v| !v.is_empty()) {
                    let rf = &d.refs[ids[0]];
                    let more = ids.len() - 1;
                    let label = ref_label(rf);
                    let font = FontId::proportional(12.0);
                    let g = painter.layout_no_wrap(label, font.clone(), Color32::WHITE);
                    let extra = if more > 0 { 26.0 } else { 0.0 };
                    let w = (g.size().x + 14.0).min(refs_w - 16.0 - extra).max(10.0);
                    let pill = Rect::from_min_size(pos2(left + 6.0, y(r) - 10.0), vec2(w, 20.0));
                    painter.line_segment([pos2(pill.right(), y(r)), pos2(c.x - 9.0, y(r))], Stroke::new(1.5, col.gamma_multiply(0.7)));
                    let fill = match rf.kind {
                        RefKind::Local => col.gamma_multiply(0.85),
                        RefKind::Remote => col.gamma_multiply(0.4),
                        RefKind::Tag => theme::tag(ui),
                    };
                    painter.rect_filled(pill, 10.0, fill);
                    painter.rect_stroke(pill, 10.0, Stroke::new(1.0, Color32::from_white_alpha(45)), egui::StrokeKind::Inside);
                    painter.with_clip_rect(pill.shrink(2.0)).galley(pos2(pill.left() + 7.0, y(r) - g.size().y / 2.0), g, Color32::WHITE);
                    if more > 0 {
                        let b = Rect::from_min_size(pos2(pill.right() + 4.0, y(r) - 9.0), vec2(22.0, 18.0));
                        painter.rect_filled(b, 9.0, theme::faint(ui));
                        painter.text(b.center(), Align2::CENTER_CENTER, format!("+{more}"), FontId::proportional(10.5), text);
                    }
                    let tip_rect = Rect::from_min_max(pill.min, pos2(pill.right() + extra, pill.bottom()));
                    let names: Vec<String> = ids.iter().map(|&i| ref_label(&d.refs[i])).collect();
                    let pill_resp = ui.interact(tip_rect, ui.id().with(("refs", r)), Sense::click_and_drag()).on_hover_text(names.join("\n"));
                    if rf.kind != RefKind::Tag {
                        pill_resp.dnd_set_drag_payload(BranchDrag(rf.name.clone()));
                    }
                    if pill_resp.clicked() {
                        acts.extend(commit.map(|c| Action::Select(Sel::Commit(c.id.clone()))));
                    }
                    if pill_resp.double_clicked() {
                        acts.extend(branch_checkout(d, rf));
                    }
                    // Right-clicking a pill gives that ref's own menu (all refs here when several).
                    pill_resp.context_menu(|ui| {
                        if ids.len() == 1 {
                            crate::ui::sidebar::ref_menus::ref_menu(ui, d, rf, acts);
                        } else {
                            for &i in ids {
                                ui.menu_button(ref_label(&d.refs[i]), |ui| crate::ui::sidebar::ref_menus::ref_menu(ui, d, &d.refs[i], acts));
                            }
                        }
                    });
                }

                let ty = y(r);
                let msg_clip = Rect::from_x_y_ranges(x_msg + 6.0..=x_author - 8.0, ty - ROW..=ty + ROW);
                match commit {
                    None => {
                        let s = &d.status;
                        let n = s.staged.len() + s.unstaged.len() + s.conflicted.len();
                        painter.with_clip_rect(msg_clip).text(
                            pos2(x_msg + 8.0, ty),
                            Align2::LEFT_CENTER,
                            format!("// WIP  ·  {n} changed file{}", if n == 1 { "" } else { "s" }),
                            body.clone(),
                            theme::dim(ui),
                        );
                    }
                    Some(cm) => {
                        painter.with_clip_rect(msg_clip).text(pos2(x_msg + 8.0, ty), Align2::LEFT_CENTER, &cm.summary, body.clone(), text);
                        let small = FontId::proportional(12.0);
                        painter.with_clip_rect(Rect::from_x_y_ranges(x_author..=(x_date - 8.0).max(x_author), msg_clip.y_range())).text(
                            pos2(x_author + 8.0, ty),
                            Align2::LEFT_CENTER,
                            &cm.author,
                            small.clone(),
                            theme::dim(ui),
                        );
                        painter.text(pos2(x_date + 8.0, ty), Align2::LEFT_CENTER, relative_time(cm.time, &cm.date), small, theme::dim(ui));
                    }
                }
            }
        });
    });
}

fn keyboard_nav(ui: &Ui, d: &RepoData, sel_row: Option<usize>, scroll_to: &mut Option<usize>, acts: &mut Vec<Action>) {
    if ui.ctx().memory(|m| m.focused().is_some()) {
        return;
    }
    let (up, down) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::K),
            i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::J),
        )
    });
    let n = d.rows();
    let next = match (sel_row, up, down) {
        (Some(r), true, _) => r.saturating_sub(1),
        (Some(r), _, true) => (r + 1).min(n.saturating_sub(1)),
        (None, true, _) | (None, _, true) if n > 0 => 0,
        _ => return,
    };
    *scroll_to = Some(next);
    acts.push(Action::Select(d.commit(next).map_or(Sel::Wip, |c| Sel::Commit(c.id.clone()))));
}
