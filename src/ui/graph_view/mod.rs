//! The commit graph table: refs | graph | message | author | date. Only visible rows are laid out
//! and painted, so 100k+ commits scroll smoothly. Siblings: `search` (⌘F bar), `minimap`, `menus`,
//! `dnd`, `hover`, `empty` (the first-commit guide).

pub mod dnd;
mod empty;
pub mod hover;
pub mod menus;
mod minimap;
mod search;

use crate::git::{Ref, RefKind};
use crate::graph::NONE;
use crate::repo::{Action, RepoData, Search, Sel, View, branch_checkout};
use crate::theme::{self, icon};
use dnd::BranchDrag;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use crate::ui::format::{initials, relative_time};
use hover::{hover_ref, hovered_ref};
use menus::{checkout_action, commit_menu, multi_menu, wip_menu};

/// Closed outline of a rounded rect (for dashed borders).
fn rounded(b: Rect, r: f32) -> Vec<Pos2> {
    let (l, t, rt, bt) = (b.left(), b.top(), b.right(), b.bottom());
    let mut p = vec![pos2(l + r, t), pos2(rt - r, t)];
    corner(&mut p, pos2(rt - r, t), pos2(rt, t), pos2(rt, t + r));
    p.push(pos2(rt, bt - r));
    corner(&mut p, pos2(rt, bt - r), pos2(rt, bt), pos2(rt - r, bt));
    p.push(pos2(l + r, bt));
    corner(&mut p, pos2(l + r, bt), pos2(l, bt), pos2(l, bt - r));
    p.push(pos2(l, t + r));
    corner(&mut p, pos2(l, t + r), pos2(l, t), pos2(l + r, t));
    p
}

pub const ROW: f32 = 28.0;
pub const LANE: f32 = 20.0;
const DATE_W: f32 = 116.0;
const RAIL_W: f32 = 10.0;
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
        RefKind::Local if r.worktree.is_some() => format!("{} {}", icon::TREE_STRUCTURE, r.name),
        RefKind::Local => format!("{} {}", icon::GIT_BRANCH, r.name),
        RefKind::Remote => format!("{} {}", icon::CLOUD, r.name),
        RefKind::Tag => format!("{} {}", icon::TAG, r.name),
    }
}

pub fn show(
    ui: &mut Ui,
    d: &RepoData,
    sel: &Option<Sel>,
    multi: &[String],
    scroll_to: &mut Option<usize>,
    search: &mut Option<Search>,
    acts: &mut Vec<Action>,
) {
    let sel_row = match sel {
        Some(Sel::Wip) if d.wip => Some(0),
        Some(Sel::Commit(id)) => d.row_of.get(id).copied(),
        _ => None,
    };
    if let Some(s) = search
        && search::search_bar(ui, s, d, scroll_to, acts)
    {
        *search = None;
    }
    let search = search.as_ref();
    keyboard_nav(ui, d, sel_row, scroll_to, acts);
    if d.state == crate::git::State::Bisecting {
        crate::ui::bisect::banner(ui, d, acts);
    }
    let v = &d.view;
    if v.filtered() || v.first_parent {
        ui.horizontal(|ui| {
            let shown = d.refs.iter().filter(|r| v.shows(&r.full)).count();
            let mut what = vec![];
            if v.filtered() {
                what.push(format!("Viewing {shown} of {} refs", d.refs.len()));
            }
            if v.first_parent {
                what.push("first-parent only".to_owned());
            }
            ui.label(egui::RichText::new(format!("{}  {}", icon::FUNNEL, what.join("  ·  "))).small().color(theme::MOD));
            if ui.link(egui::RichText::new("Show all").small()).clicked() {
                acts.push(Action::View(View::default()));
            }
        });
    }
    // Hovering a ref fades what it can't reach; ⌘F fades non-matches.
    let reach = hovered_ref(ui).and_then(|t| d.row_of.get(&t).copied()).map(|r| d.reachable(r));
    let dims = |r: usize| search.is_some_and(|s| s.dims(r)) || reach.as_ref().is_some_and(|v| !v[r]);

    // Column widths: [refs, graph]; graph 0 = fit lanes (up to 12). Drag the header to resize.
    let cols_id = egui::Id::new("graph_cols");
    let [refs_w, graph_w] = ui.data_mut(|m| *m.get_persisted_mut_or(cols_id, [160.0f32, 0.0]));
    let mut full = ui.available_rect_before_wrap();
    full.max.x -= RAIL_W + 2.0;
    let author_w = if full.width() > 1000.0 { 130.0 } else { 0.0 };
    let want_graph = if graph_w > 0.0 { graph_w } else { d.graph.width.clamp(1, 10) as f32 * LANE + 12.0 };
    let Cols { graph: x_graph, msg: x_msg, author: x_author, date: x_date } = columns(full.left(), full.width(), refs_w, want_graph, author_w);
    let (refs_w, graph_w) = (x_graph - full.left(), x_msg - x_graph);

    // Lanes wider than the graph column scroll sideways (trackpad / ⇧ wheel over
    // the graph, or the bar under it) instead of being cut off.
    let lanes_w = d.graph.width as f32 * LANE + 16.0;
    let max_gx = (lanes_w - graph_w).max(0.0);
    let gx_id = egui::Id::new(("graph_x", &d.git_dir));
    let mut gx = ui.data(|m| m.get_temp::<f32>(gx_id)).unwrap_or(0.0);
    if let Some(r) = *scroll_to {
        // Jumping to a commit brings its lane into view too.
        let cx = 8.0 + d.graph.nodes.get(r).map_or(0.0, |n| n.col as f32) * LANE + LANE / 2.0;
        let hi = (cx - LANE).max(0.0);
        gx = gx.clamp((cx - graph_w + LANE).min(hi), hi); // bounds never cross (f32::clamp panics)
    }

    let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 24.0), Sense::hover());
    let p = ui.painter();
    let hf = FontId::proportional(11.0);
    let dim = theme::dim(ui);
    let heads = [(full.left(), x_graph, "BRANCH / TAG"), (x_graph, x_msg - 30.0, "GRAPH"), (x_msg, x_author, "COMMIT MESSAGE"), (x_author, x_date, "AUTHOR"), (x_date, full.right(), "DATE")];
    for (x, end, t) in heads {
        // Each title stays inside its column, however narrow.
        if end > x + 8.0 {
            let clip = Rect::from_x_y_ranges(x..=end - 4.0, hdr.y_range());
            p.with_clip_rect(clip).text(pos2(x + 8.0, hdr.center().y), Align2::LEFT_CENTER, t, hf.clone(), dim);
        }
    }
    let fp = Rect::from_center_size(pos2(x_msg - 16.0, hdr.center().y), vec2(22.0, 20.0));
    let fp_resp = ui.interact(fp, cols_id.with("first_parent"), Sense::click()).on_hover_text(if v.first_parent {
        "First-parent only (on): click to show merged-in commits again"
    } else {
        "First-parent only: follow each branch's mainline and hide merged-in commits"
    });
    if v.first_parent || fp_resp.hovered() {
        p.rect_filled(fp, 10.0, if v.first_parent { theme::accent().gamma_multiply(0.45) } else { theme::hover(ui) });
    }
    p.text(fp.center(), Align2::CENTER_CENTER, icon::GIT_COMMIT, FontId::proportional(13.0), if v.first_parent { theme::text(ui) } else { dim });
    if fp_resp.clicked() {
        acts.push(Action::View(View { first_parent: !v.first_parent, ..v.clone() }));
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
    let graph_col = Rect::from_x_y_ranges(x_graph..=x_msg, body.y_range());
    if max_gx > 0.0 && ui.rect_contains_pointer(graph_col) {
        gx -= ui.input(|i| i.smooth_scroll_delta.x);
    }
    gx = gx.clamp(0.0, max_gx);
    let mut visible = 0..0;
    ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().id_salt(("graph", &d.git_dir)).auto_shrink(false).show_rows(ui, ROW, n, |ui, range| {
            let top = ui.max_rect().top() - range.start as f32 * ROW;
            let y = |r: usize| top + r as f32 * ROW + ROW / 2.0;
            let x = |c: u16| x_graph + 8.0 + c as f32 * LANE + LANE / 2.0 - gx;
            let left = full.left();
            visible = range.clone();

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
                let in_multi = commit.is_some_and(|c| multi.contains(&c.id));
                if Some(r) == sel_row || in_multi {
                    ui.painter().rect_filled(pill, 9.0, theme::selection(ui));
                } else if resp.hovered() {
                    hovered = Some(r);
                    ui.painter().rect_filled(pill, 9.0, theme::hover(ui));
                }
                if resp.clicked() {
                    let (cmd, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
                    match commit {
                        Some(c) if cmd || shift => acts.push(Action::Multi(multi_select(d, sel_row, multi, r, &c.id, shift))),
                        _ => acts.push(Action::Select(commit.map_or(Sel::Wip, |c| Sel::Commit(c.id.clone())))),
                    }
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
                    Some(_) if in_multi && multi.len() > 1 => multi_menu(ui, d, multi, acts),
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
                let col = theme::lane(e.color);
                let stroke = Stroke::new(2.0, if dims(e.from as usize) { col.gamma_multiply(0.3) } else { col });
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
                let faded = dims(r);
                let col = if faded { theme::lane(node.color).gamma_multiply(0.3) } else { theme::lane(node.color) };
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
                        let a = if faded { 0.3 } else { 1.0 };
                        gp.circle_filled(c, 9.0, theme::avatar(&cm.email).gamma_multiply(a));
                        gp.circle_stroke(c, 9.0, Stroke::new(1.5, col));
                        gp.text(c, Align2::CENTER_CENTER, initials(&cm.author), FontId::proportional(9.0), Color32::WHITE.gamma_multiply(a));
                    }
                }

                let mut stash_x = left + 6.0;
                let ids = commit.and_then(|cm| d.refs_at.get(&cm.id));
                if let Some(ids) = ids.filter(|v| !v.is_empty()) {
                    let rf = &d.refs[ids[0]];
                    let more = ids.len() - 1;
                    let label = ref_label(rf);
                    let font = FontId::proportional(12.0);
                    let label_col = if faded { Color32::WHITE.gamma_multiply(0.6) } else { Color32::WHITE };
                    let g = painter.layout_no_wrap(label, font.clone(), label_col);
                    let extra = if more > 0 { 26.0 } else { 0.0 };
                    let w = (g.size().x + 14.0).min(refs_w - 16.0 - extra).max(10.0);
                    let pill = Rect::from_min_size(pos2(left + 6.0, y(r) - 10.0), vec2(w, 20.0));
                    let to = (c.x - 9.0).clamp(pill.right(), x_msg - 2.0); // node may be scrolled out of view
                    painter.line_segment([pos2(pill.right(), y(r)), pos2(to, y(r))], Stroke::new(1.5, col.gamma_multiply(0.7)));
                    let fill = match rf.kind {
                        RefKind::Local => col.gamma_multiply(0.85),
                        RefKind::Remote => col.gamma_multiply(0.4),
                        RefKind::Tag if faded => theme::tag(ui).gamma_multiply(0.4),
                        RefKind::Tag => theme::tag(ui),
                    };
                    painter.rect_filled(pill, 10.0, fill);
                    painter.rect_stroke(pill, 10.0, Stroke::new(1.0, Color32::from_white_alpha(45)), egui::StrokeKind::Inside);
                    painter.with_clip_rect(pill.shrink(2.0)).galley(pos2(pill.left() + 7.0, y(r) - g.size().y / 2.0), g, label_col);
                    if more > 0 {
                        let b = Rect::from_min_size(pos2(pill.right() + 4.0, y(r) - 9.0), vec2(22.0, 18.0));
                        painter.rect_filled(b, 9.0, theme::faint(ui));
                        painter.text(b.center(), Align2::CENTER_CENTER, format!("+{more}"), FontId::proportional(10.5), text);
                    }
                    let tip_rect = Rect::from_min_max(pill.min, pos2(pill.right() + extra, pill.bottom()));
                    // Annotated tags show their message.
                    let names: Vec<String> = ids
                        .iter()
                        .map(|&i| match &d.refs[i] {
                            t if !t.message.is_empty() => format!("{}\n{}", ref_label(t), t.message),
                            t => ref_label(t),
                        })
                        .collect();
                    stash_x = tip_rect.right() + 4.0;
                    let pill_resp = ui.interact(tip_rect, ui.id().with(("refs", r)), Sense::click_and_drag()).on_hover_text(names.join("\n"));
                    hover_ref(ui, &pill_resp, &rf.target);
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

                // Stashes made on this commit: dashed capsules after the pills.
                for &i in commit.and_then(|cm| d.stash_at.get(&cm.id)).map_or(&[][..], |v| v) {
                    let st = &d.stashes[i];
                    let b = Rect::from_min_size(pos2(stash_x, y(r) - 9.0), vec2(26.0, 18.0));
                    if b.right() > x_graph - 4.0 {
                        break;
                    }
                    if stash_x == left + 6.0 {
                        let dash = [pos2(b.right(), y(r)), pos2((c.x - 9.0).clamp(b.right(), x_msg - 2.0), y(r))];
                        painter.extend(Shape::dashed_line(&dash, Stroke::new(1.2, col.gamma_multiply(0.6)), 3.0, 3.0));
                    }
                    stash_x = b.right() + 4.0;
                    let on = matches!(sel, Some(Sel::Commit(id)) if *id == st.id);
                    painter.rect_filled(b, 9.0, if on { theme::accent().gamma_multiply(0.45) } else { theme::bg(ui) });
                    painter.extend(Shape::dashed_line(&rounded(b, 9.0), Stroke::new(1.2, if faded { theme::dim(ui).gamma_multiply(0.4) } else { theme::dim(ui) }), 3.0, 2.0));
                    painter.text(b.center(), Align2::CENTER_CENTER, icon::ARCHIVE, FontId::proportional(12.0), if faded { theme::dim(ui) } else { text });
                    let resp = ui.interact(b, ui.id().with(("stash", i)), Sense::click()).on_hover_text(format!("{}  ·  stash on this commit\n{}", st.name, st.message));
                    if resp.clicked() {
                        acts.push(Action::Select(Sel::Commit(st.id.clone())));
                    }
                    resp.context_menu(|ui| crate::ui::sidebar::stashes::stash_menu(ui, st, acts));
                }

                let ty = y(r);
                let msg_clip = Rect::from_x_y_ranges(x_msg + 6.0..=(x_author - 8.0).max(x_msg + 6.0), ty - ROW..=ty + ROW);
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
                        let text = if faded { text.gamma_multiply(0.35) } else { text };
                        // Bisect: good / bad / skip marks, and the commit being tested.
                        let mut x_text = x_msg + 8.0;
                        if let Some(b) = &d.bisect
                            && let Some(m) = b.mark(&cm.id).or_else(|| (head_id == Some(cm.id.as_str()) && b.found().is_none()).then_some("testing"))
                        {
                            let color = crate::ui::bisect::mark_color(m);
                            let g = painter.layout_no_wrap(m.to_owned(), FontId::proportional(10.5), color);
                            let tag = Rect::from_min_size(pos2(x_text, ty - 9.0), vec2(g.size().x + 12.0, 18.0));
                            let mp = painter.with_clip_rect(msg_clip);
                            mp.rect_filled(tag, 9.0, color.gamma_multiply(0.2));
                            mp.galley(tag.center() - g.size() / 2.0, g, color);
                            x_text = tag.right() + 6.0;
                        }
                        painter.with_clip_rect(msg_clip).text(pos2(x_text, ty), Align2::LEFT_CENTER, &cm.summary, body.clone(), text);
                        let small = FontId::proportional(12.0);
                        painter.with_clip_rect(Rect::from_x_y_ranges(x_author..=(x_date - 8.0).max(x_author), msg_clip.y_range())).text(
                            pos2(x_author + 8.0, ty),
                            Align2::LEFT_CENTER,
                            &cm.author,
                            small.clone(),
                            theme::dim(ui),
                        );
                        let date_clip = Rect::from_x_y_ranges(x_date..=full.right(), msg_clip.y_range());
                        painter.with_clip_rect(date_clip).text(pos2(x_date + 8.0, ty), Align2::LEFT_CENTER, relative_time(cm.time, &cm.date), small, theme::dim(ui));
                    }
                }
            }
        });
    });
    if max_gx > 0.0 && graph_w > 48.0 {
        let track = Rect::from_x_y_ranges(x_graph + 6.0..=x_msg - 6.0, body.bottom() - 8.0..=body.bottom() - 2.0);
        let thumb_w = (track.width() * graph_w / lanes_w).max(24.0).min(track.width());
        let span = (track.width() - thumb_w).max(1.0);
        let thumb = Rect::from_min_size(pos2(track.left() + span * gx / max_gx, track.top()), vec2(thumb_w, track.height()));
        let resp = ui.interact(track.expand2(vec2(0.0, 3.0)), gx_id.with("bar"), Sense::click_and_drag()).on_hover_text("Scroll the graph sideways (or ⇧ + wheel over it)");
        if resp.dragged() {
            gx = (gx + resp.drag_delta().x * max_gx / span).clamp(0.0, max_gx);
        } else if resp.clicked()
            && let Some(px) = resp.interact_pointer_pos()
        {
            gx = ((px.x - track.left() - thumb_w / 2.0) / span * max_gx).clamp(0.0, max_gx);
        }
        let on = resp.hovered() || resp.dragged();
        ui.painter().rect_filled(track.expand(2.0), 5.0, theme::bg(ui).gamma_multiply(0.85)); // lanes don't run through it
        ui.painter().rect_filled(thumb, 3.0, if on { theme::text(ui).gamma_multiply(0.7) } else { theme::dim(ui) });
    }
    ui.data_mut(|m| m.insert_temp(gx_id, gx));
    // No commits yet: the guide fills the space under the WIP row.
    if d.log.is_empty() {
        let r = egui::Rect::from_min_max(pos2(body.left(), body.top() + ROW * (n as f32) + 40.0), body.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| empty::first_commit_guide(ui, d, acts));
    }
    if d.truncated && visible.end >= n {
        let btn = Rect::from_center_size(pos2(body.center().x, body.bottom() - 26.0), vec2(230.0, 32.0));
        let label = format!("{}  Load {} more commits", theme::icon::ARROW_DOWN, crate::repo::PAGE);
        if ui.put(btn, egui::Button::new(label).corner_radius(255).fill(theme::accent().gamma_multiply(0.85))).clicked() {
            acts.push(Action::LoadMore);
        }
    }
    let rail = Rect::from_x_y_ranges(full.right() + 2.0..=full.right() + 2.0 + RAIL_W, body.top() + 2.0..=body.bottom() - 2.0);
    minimap::minimap(ui, rail, d, sel_row, search, visible, scroll_to, acts);
}

/// Column left edges of the table (graph, message, author, date).
#[derive(Debug, PartialEq)]
struct Cols {
    graph: f32,
    msg: f32,
    author: f32,
    date: f32,
}

/// Lays out refs | graph | message | author | date in `width` from `left`. The
/// message keeps `MSG_MIN`; when space runs out the graph gives way first, then
/// the refs column, each down to a minimum (then the message is squeezed, but
/// no column ever starts before the previous one).
fn columns(left: f32, width: f32, refs_w: f32, graph_w: f32, author_w: f32) -> Cols {
    const MIN_REFS: f32 = 60.0;
    const MIN_GRAPH: f32 = 2.0 * LANE + 12.0;
    let date = left + (width - DATE_W).max(0.0);
    let author = (date - author_w).max(left);
    let room = (author - left - MSG_MIN).max(MIN_REFS + MIN_GRAPH);
    let refs = refs_w.min(room - MIN_GRAPH).max(MIN_REFS);
    let graph = graph_w.min(room - refs).max(MIN_GRAPH);
    let msg = (left + refs + graph).min(author);
    Cols { graph: (left + refs).min(msg), msg, author, date }
}

/// New multi-selection after a ⌘-click (toggle) or ⇧-click (range from the current row).
fn multi_select(d: &RepoData, sel_row: Option<usize>, multi: &[String], row: usize, id: &str, shift: bool) -> Vec<String> {
    let mut set: Vec<String> = if multi.is_empty() { sel_row.and_then(|r| d.commit(r)).map(|c| c.id.clone()).into_iter().collect() } else { multi.to_vec() };
    if shift {
        let from = sel_row.unwrap_or(row);
        let (a, b) = (from.min(row), from.max(row));
        set = (a..=b).filter_map(|r| d.commit(r)).map(|c| c.id.clone()).collect();
    } else if let Some(i) = set.iter().position(|x| x == id) {
        set.remove(i);
    } else {
        set.push(id.to_owned());
    }
    set.sort_by_key(|x| d.row_of.get(x).copied().unwrap_or(usize::MAX)); // newest first
    set
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_give_way_in_order() {
        let c = columns(0.0, 1400.0, 160.0, 120.0, 130.0);
        assert_eq!(c, Cols { graph: 160.0, msg: 280.0, author: 1154.0, date: 1284.0 });
        // Tighter: the graph shrinks so the message keeps MSG_MIN.
        let c = columns(0.0, 700.0, 160.0, 300.0, 0.0);
        assert_eq!(c.msg, 700.0 - DATE_W - MSG_MIN);
        assert_eq!(c.graph, 160.0);
        // Tighter still: the graph is at its minimum, then the refs column gives way.
        let c = columns(0.0, 480.0, 160.0, 300.0, 0.0);
        assert_eq!(c.msg - c.graph, 2.0 * LANE + 12.0);
        assert!(c.graph < 160.0 && c.graph >= 60.0);
        // Tiny window: columns never overlap or run backwards.
        for w in [0.0, 50.0, 150.0, 300.0] {
            let c = columns(10.0, w, 500.0, 900.0, 130.0);
            assert!(10.0 <= c.graph && c.graph <= c.msg && c.msg <= c.author && c.author <= c.date, "{w}: {c:?}");
        }
    }
}
