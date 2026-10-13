//! The commit graph table: refs | graph | message | author | date. Only visible rows are laid out
//! and painted, so 100k+ commits scroll smoothly. Siblings: `search` (⌘F bar), `minimap`, `menus`,
//! `dnd`, `hover`, `empty` (the first-commit guide).

pub mod dnd;
mod empty;
pub mod hover;
pub mod menus;
mod minimap;
mod search;

use crate::ui::format::{keys, plural};
use crate::ui::sidebar::ref_menus;
use crate::git::{Ref, RefKind};
use crate::graph::NONE;
use crate::repo::{Action, RepoData, Search, Sel, View, branch_checkout};
use crate::theme::{self, icon};
use dnd::{BranchDrag, CommitDrag};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use crate::ui::format::{initials, relative_time};
use hover::{hover_ref, hovered_ref};
use menus::{checkout_action, commit_menu, multi_menu, on_branch, wip_menu};

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
        RefKind::Local if r.head => format!("{}  {}", icon::CHECK, r.name),
        RefKind::Local if r.worktree.is_some() => format!("{}  {}", icon::TREE_STRUCTURE, r.name),
        RefKind::Local => format!("{}  {}", icon::GIT_BRANCH, r.name),
        RefKind::Remote => format!("{}  {}", icon::CLOUD, r.name),
        RefKind::Tag => format!("{}  {}", icon::TAG, r.name),
    }
}

/// A ref's pill at `min`, at most `max_w` wide; `synced`: its upstream rides along as a ☁. HEAD's leads with
/// a "HEAD" capsule (the name gets cut, not the capsule); a pill too narrow for it gets the pin glyph instead.
fn ref_pill(ui: &Ui, min: Pos2, max_w: f32, rf: &Ref, synced: bool, col: Color32, faded: bool) -> Rect {
    let p = ui.painter();
    let head_pill = rf.head && rf.kind == RefKind::Local;
    let hc = if faded { theme::accent().gamma_multiply(0.3) } else { theme::accent() };
    let cap_w = if head_pill && max_w >= 80.0 { 40.0 } else { 0.0 };
    let label = if cap_w > 0.0 { rf.name.clone() } else { ref_label(rf) };
    let label = if synced { format!("{label}  {}", icon::CLOUD) } else { label };
    let fill = match rf.kind {
        _ if head_pill => hc,
        RefKind::Local => col.gamma_multiply(0.85),
        RefKind::Remote => col.gamma_multiply(0.4),
        RefKind::Tag if faded => theme::tag(ui).gamma_multiply(0.4),
        RefKind::Tag => theme::tag(ui),
    };
    let label_col = Color32::WHITE.gamma_multiply(if faded { 0.6 } else { 1.0 });
    let g = p.layout_no_wrap(label, FontId::proportional(12.0), label_col);
    let pill = Rect::from_min_size(min, vec2((g.size().x + 14.0 + cap_w).min(max_w).max(10.0), 20.0));
    p.rect_filled(pill, 10.0, fill);
    p.rect_stroke(pill, 10.0, Stroke::new(1.0, Color32::from_white_alpha(45)), egui::StrokeKind::Inside);
    if cap_w > 0.0 {
        let cap = Rect::from_min_size(pill.min + vec2(3.0, 3.0), vec2(cap_w - 6.0, 14.0));
        p.rect_filled(cap, 7.0, label_col.gamma_multiply(0.22));
        p.text(cap.center(), Align2::CENTER_CENTER, "HEAD", FontId::proportional(9.5), label_col);
    }
    p.with_clip_rect(pill.shrink(2.0)).galley(pos2(pill.left() + 7.0 + cap_w, pill.center().y - g.size().y / 2.0), g, label_col);
    pill
}

/// A pill's hover text: its ref (and the upstream sharing it); annotated tags add their message.
fn tip_now(r: &egui::Response, text: String) {
    if r.hovered() && !egui::Popup::is_any_open(&r.ctx) && !egui::DragAndDrop::has_any_payload(&r.ctx) {
        egui::Popup::from_response(r).id(r.id.with("tip")).kind(egui::PopupKind::Tooltip).align(egui::RectAlign::RIGHT).gap(8.0).show(|ui| ui.label(text));
    }
}

fn pill_tip(rf: &Ref, up: Option<&Ref>) -> String {
    let up = up.map_or(String::new(), |u| format!("  ·  {}", u.name));
    let msg = if rf.message.is_empty() { String::new() } else { format!("\n{}", rf.message) };
    format!("{}{up}{msg}", ref_label(rf))
}

/// A small capsule of `fg` text on `bg` at `x`; returns where the next thing starts.
fn chip(p: &egui::Painter, x: f32, y: f32, text: &str, fg: Color32, bg: Color32) -> f32 {
    let g = p.layout_no_wrap(text.to_owned(), FontId::proportional(10.5), fg);
    let tag = Rect::from_min_size(pos2(x, y - 9.0), vec2(g.size().x + 12.0, 18.0));
    p.rect_filled(tag, 9.0, bg);
    p.galley(tag.center() - g.size() / 2.0, g, fg);
    tag.right() + 6.0
}

/// `all`: the loaded history; the graph shows the search hits instead while they filter it.
pub fn show(
    ui: &mut Ui,
    all: &RepoData,
    sel: &Option<Sel>,
    multi: &[String],
    scroll_to: &mut Option<usize>,
    search: &mut Option<Search>,
    avatars: &crate::repo::avatars::Avatars,
    acts: &mut Vec<Action>,
) {
    let filtered = search.as_ref().and_then(|s| s.filtered.clone());
    let d = filtered.as_deref().unwrap_or(all);
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
            ui.label(egui::RichText::new(format!("{}  {}", icon::FUNNEL, what.join("  ·  "))).small().color(theme::changed()));
            if ui.link(egui::RichText::new("Show all").small()).clicked() {
                acts.push(Action::View(View::default()));
            }
        });
    }
    // Hovering a ref fades what it can't reach; ⌘F fades non-matches.
    // Reachability, links and menus ask the full history: the filtered graph lacks the commits between hits.
    // A hit older than the loaded page isn't in `all`: a complete ancestry rules it out, else git is asked.
    let full_row = |r: usize| if filtered.is_none() { Some(r) } else { d.commit(r).and_then(|c| all.row_of.get(&c.id).copied()) };
    // HEAD: a larger, bold, accent-coloured title and a branch pill marked HEAD;
    // detached, a solid "detached HEAD" chip before the message instead of the pill.
    let head_row = d.status.head.as_deref().and_then(|h| d.row_of.get(h)).copied();
    let detached = d.status.branch.is_none();
    let hc = theme::accent();
    let tip = hovered_ref(ui);
    let reach = tip.as_ref().and_then(|t| all.row_of.get(t).copied()).map(|r| all.reachable(r));
    let complete = filtered.is_some() && reach.as_ref().is_some_and(|v| !all.graph.has_unloaded_parent(v));
    let ctx = ui.ctx().clone();
    let hover_dims = |r: usize| {
        let Some(t) = &tip else { return false };
        // The WIP row sits above HEAD: it belongs to the hovered ref only if that is HEAD's.
        let Some(c) = d.commit(r) else { return all.status.head.as_ref() != Some(t) };
        let known = Search::judge(reach.as_ref().map(|v| v.as_slice()), complete, full_row(r));
        !known.or_else(|| search.and_then(|s| s.is_ancestor(&ctx, &c.id, t))).unwrap_or(true)
    };
    let dims = |r: usize| search.is_some_and(|s| s.dims(r)) || hover_dims(r);

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
    let lanes_w = d.graph.width as f32 * LANE + 12.0; // same padding as the auto-fit column
    let max_gx = (lanes_w - graph_w).max(0.0);
    let gx_id = egui::Id::new(("graph_x", &d.git_dir));
    let mut gx = ui.data(|m| m.get_temp::<f32>(gx_id)).unwrap_or(0.0);
    let sel_id = gx_id.with("sel");
    let sel_moved = ui.data(|m| m.get_temp::<Option<usize>>(sel_id)) != Some(sel_row);
    ui.data_mut(|m| m.insert_temp(sel_id, sel_row));
    if let Some(r) = sel_row.filter(|_| sel_moved) {
        // Selecting a commit (keys, ⌘F, a branch, the minimap) brings its lane into view.
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
        // A title shows only when it fits its column whole (no "GRA" fragments).
        let g = p.layout_no_wrap(t.into(), hf.clone(), dim);
        if g.size().x <= end - x - 12.0 {
            p.galley(pos2(x + 8.0, hdr.center().y - g.size().y / 2.0), g, dim);
        }
    }
    let fp = Rect::from_center_size(pos2(x_msg - 16.0, hdr.center().y), vec2(22.0, 20.0));
    let fp_room = graph_w >= 32.0;
    let fp_resp = ui.interact(fp, cols_id.with("first_parent"), Sense::click()).on_hover_text(if v.first_parent {
        "First-parent only (on): click to show merged-in commits again"
    } else {
        "First-parent only: follow each branch's mainline and hide merged-in commits"
    });
    if fp_room && (v.first_parent || fp_resp.hovered()) {
        p.rect_filled(fp, 10.0, if v.first_parent { theme::accent().gamma_multiply(0.45) } else { theme::hover(ui) });
    }
    if fp_room {
        p.text(fp.center(), Align2::CENTER_CENTER, icon::GIT_COMMIT, FontId::proportional(13.0), if v.first_parent { theme::text(ui) } else { dim });
    }
    if fp_room && fp_resp.clicked() {
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
        // Only mostly-sideways gestures: a vertical swipe's small x part must not drift the lanes.
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.x.abs() > d.y.abs() {
            gx -= d.x;
        }
    }
    gx = gx.clamp(0.0, max_gx);
    let mut visible = 0..0;
    let mut row0 = 0.0; // screen y of row 0's top
    ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().id_salt(("graph", &d.git_dir)).auto_shrink(false).show_rows(ui, ROW, n, |ui, range| {
            let top = ui.max_rect().top() - range.start as f32 * ROW;
            let y = |r: usize| top + r as f32 * ROW + ROW / 2.0;
            let x = |c: u16| x_graph + 8.0 + c as f32 * LANE + LANE / 2.0 - gx;
            let left = full.left();
            visible = range.clone();
            row0 = top;

            if let Some(r) = scroll_to.take() {
                let rect = Rect::from_min_size(pos2(left, top + r as f32 * ROW), vec2(1.0, ROW));
                if !(range.start + 1..range.end.saturating_sub(1)).contains(&r) {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            }

            let mut hovered = None;
            for r in range.clone() {
                let rect = Rect::from_min_size(pos2(left, top + r as f32 * ROW), vec2(full.width(), ROW));
                let commit = d.commit(r);
                let resp = ui.interact(rect, ui.id().with(("row", r)), if commit.is_some() { Sense::click_and_drag() } else { Sense::click() });
                let pill = rect.shrink2(vec2(2.0, 1.5));
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
                    if resp.dnd_hover_payload::<BranchDrag>().is_some_and(|p| p.1 != dst.full) {
                        ui.painter().rect_stroke(rect.shrink2(vec2(2.0, 1.5)), 9.0, Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
                    }
                    if let Some(p) = resp.dnd_release_payload::<BranchDrag>()
                        && p.1 != dst.full
                    {
                        acts.push(Action::DropBranch { src: p.1.clone(), dst: dst.full.clone() });
                    }
                }
                // Dragging a commit onto another moves / squashes it, or cherry-picks it onto the row's branch.
                if let Some(c) = commit {
                    resp.dnd_set_drag_payload(CommitDrag(c.id.clone(), c.summary.clone()));
                    if resp.dnd_hover_payload::<CommitDrag>().is_some_and(|p| p.0 != c.id) {
                        ui.painter().rect_stroke(pill, 9.0, Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
                    }
                    if let Some(p) = resp.dnd_release_payload::<CommitDrag>()
                        && p.0 != c.id
                    {
                        let locals = d.refs_at.get(&c.id).map_or(&[][..], |v| v).iter().map(|&i| &d.refs[i]).filter(|r| r.kind == RefKind::Local);
                        let branch = locals.max_by_key(|r| r.head).map(|r| r.full.clone());
                        let squash = ui.input(|i| i.modifiers.alt);
                        acts.push(Action::DropCommit { src: p.0.clone(), dst: Some(c.id.clone()), branch, squash });
                    }
                }
                resp.context_menu(|ui| {
                    theme::menu_scroll(ui, |ui| match commit {
                        Some(_) if in_multi && multi.len() > 1 => multi_menu(ui, all, d, multi, acts),
                        Some(c) => commit_menu(ui, all, c, on_branch(ui, all, search, &c.id), acts),
                        None => wip_menu(ui, acts),
                    })
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
                    // The edge can span the whole history: dash only what is on screen (y is monotone).
                    // The top is snapped to the dash period so the pattern doesn't crawl when scrolling.
                    let clip = ui.clip_rect();
                    let y0 = pts[0].y;
                    let top = y0 + ((clip.top() - ROW - y0) / 7.0).floor().max(0.0) * 7.0;
                    for p in &mut pts {
                        p.y = p.y.clamp(top, clip.bottom() + ROW);
                    }
                    gp.extend(Shape::dashed_line(&pts, stroke, 4.0, 3.0));
                } else {
                    gp.add(Shape::line(pts, stroke));
                }
            }

            let painter = ui.painter();
            let body = FontId::proportional(13.0);
            let text = theme::text(ui);
            let head_id = d.status.head.as_deref();
            // The commit whose refs are spread out in a row (hovering its pill / +N).
            let open_id = gx_id.with("ref_row");
            let open = ui.data(|m| m.get_temp::<String>(open_id));
            let mut shown = None;
            for r in range {
                let node = d.graph.nodes[r];
                let c = pos2(x(node.col), y(r));
                let faded = dims(r);
                let col = if faded { theme::lane(node.color).gamma_multiply(0.3) } else { theme::lane(node.color) };
                let commit = d.commit(r);
                let is_head = Some(r) == head_row;
                let hc = if faded { hc.gamma_multiply(0.3) } else { hc };
                match commit {
                    None => {
                        gp.circle(c, 7.0, theme::bg(ui), Stroke::new(2.0, col));
                    }
                    Some(cm) if cm.parents.len() > 1 => {
                        gp.circle_filled(c, if is_head { 7.0 } else { 5.0 }, col);
                    }
                    Some(cm) => {
                        let rad = if is_head { 10.5 } else { 9.0 };
                        let a = if faded { 0.3 } else { 1.0 };
                        gp.circle_filled(c, rad, theme::avatar(&cm.email).gamma_multiply(a));
                        match avatars.get(&cm.email) {
                            Some(t) => theme::photo(&gp, c, rad, &t, a),
                            None => {
                                gp.text(c, Align2::CENTER_CENTER, initials(&cm.author), FontId::proportional(9.0), Color32::WHITE.gamma_multiply(a));
                            }
                        }
                        gp.circle_stroke(c, rad, Stroke::new(1.5, col));
                    }
                }

                let mut stash_x = left + 6.0;
                if let Some((cm, ids)) = commit.and_then(|cm| Some((cm, d.refs_at.get(&cm.id).filter(|v| !v.is_empty())?))) {
                    // A local branch level with its upstream shares one pill; the checked-out branch leads.
                    let at = || ids.iter().map(|&i| &d.refs[i]);
                    let tracked = |r: &Ref| r.kind == RefKind::Remote && at().any(|l| l.kind == RefKind::Local && l.upstream == r.name);
                    let mut pills: Vec<(&Ref, Option<&Ref>)> =
                        at().filter(|r| !tracked(r)).map(|r| (r, at().find(|u| r.kind == RefKind::Local && u.kind == RefKind::Remote && u.name == r.upstream))).collect();
                    pills.sort_by_key(|(r, _)| !(r.head && r.kind == RefKind::Local));
                    let (rf, up) = pills[0];
                    let more = pills.len() - 1;
                    let extra = if more > 0 { 26.0 } else { 0.0 };
                    let pill = ref_pill(ui, pos2(left + 6.0, y(r) - 10.0), refs_w - 16.0 - extra, rf, up.is_some(), col, faded);
                    // Node may be scrolled out of view; in a narrow column the pill can even
                    // end past the column's edge, so min/max rather than clamp (which panics).
                    let from = pill.right() + extra; // after the +N badge, not through it
                    let to = (c.x - 9.0).min(x_msg - 2.0).max(from);
                    let line = if rf.head && rf.kind == RefKind::Local { Stroke::new(2.5, hc) } else { Stroke::new(1.5, col.gamma_multiply(0.7)) };
                    painter.line_segment([pos2(from, y(r)), pos2(to, y(r))], line);
                    if more > 0 {
                        let b = Rect::from_min_size(pos2(pill.right() + 4.0, y(r) - 9.0), vec2(22.0, 18.0));
                        painter.rect_filled(b, 9.0, theme::faint(ui));
                        painter.text(b.center(), Align2::CENTER_CENTER, format!("+{more}"), FontId::proportional(10.5), text);
                    }
                    let tip_rect = Rect::from_min_max(pill.min, pos2(pill.right() + extra, pill.bottom()));
                    stash_x = tip_rect.right() + 4.0;
                    // Click selects the commit, double-click checks the ref out, right-click is its menu; branches drag.
                    let mut act = |ui: &Ui, resp: &egui::Response, (rf, up): (&Ref, Option<&Ref>)| {
                        hover_ref(ui, resp, &rf.target);
                        if rf.kind != RefKind::Tag {
                            resp.dnd_set_drag_payload(BranchDrag(rf.name.clone(), rf.full.clone()));
                        }
                        if resp.clicked() {
                            acts.push(Action::Select(Sel::Commit(cm.id.clone())));
                        }
                        if resp.double_clicked() {
                            acts.extend(branch_checkout(d, rf));
                        }
                        resp.context_menu(|ui| match up {
                            Some(u) => ref_menus::synced_menu(ui, all, rf, u, acts),
                            None => ref_menus::ref_menu(ui, all, rf, acts),
                        })
                        .is_some()
                    };
                    let resp = ui.interact(tip_rect, ui.id().with(("refs", r)), Sense::click_and_drag());
                    if more == 0 {
                        tip_now(&resp, pill_tip(rf, up));
                        act(ui, &resp, pills[0]);
                    } else {
                        act(ui, &resp, pills[0]);
                        // Several refs: hovering shows them all, each its own pill, stacked over the table.
                        let mut keep = resp.hovered() && !egui::DragAndDrop::has_any_payload(ui.ctx());
                        if open.as_deref() == Some(cm.id.as_str()) {
                            let row = egui::Area::new(open_id).order(egui::Order::Foreground).fixed_pos(pill.min - vec2(4.0, 4.0)).show(ui.ctx(), |ui| {
                                let slot = ui.painter().add(Shape::Noop);
                                let (mut back, mut menu) = (pill, false);
                                for (k, &p) in pills.iter().enumerate() {
                                    let rect = ref_pill(ui, pos2(pill.left(), back.bottom() + if k == 0 { -pill.height() } else { 4.0 }), f32::INFINITY, p.0, p.1.is_some(), col, faded);
                                    let r = ui.interact(rect, ui.id().with(k), Sense::click_and_drag());
                                    tip_now(&r, pill_tip(p.0, p.1));
                                    menu |= act(ui, &r, p);
                                    back = back.union(rect);
                                }
                                let back = back.expand(4.0);
                                ui.painter().set(slot, Shape::Vec(theme::glass_shapes(back, 14.0, ui.visuals().dark_mode, 235)));
                                ui.allocate_rect(back, Sense::hover());
                                menu
                            });
                            // Open while the pointer is on it (or its menu is): the row covers the pill, so no gap to cross.
                            keep |= row.inner || ui.ctx().pointer_hover_pos().is_some_and(|p| row.response.rect.expand(4.0).contains(p));
                        }
                        if keep {
                            shown = Some(cm.id.clone());
                        }
                    }
                }

                // Stashes made on this commit: dashed capsules after the pills.
                let here = commit.and_then(|cm| d.stash_at.get(&cm.id)).map_or(&[][..], |v| v);
                for (k, &i) in here.iter().enumerate() {
                    let st = &d.stashes[i];
                    let b = Rect::from_min_size(pos2(stash_x, y(r) - 9.0), vec2(26.0, 18.0));
                    if b.right() > x_graph - 4.0 {
                        break;
                    }
                    if stash_x == left + 6.0 {
                        let dash = [pos2(b.right(), y(r)), pos2((c.x - 9.0).min(x_msg - 2.0).max(b.right()), y(r))];
                        painter.extend(Shape::dashed_line(&dash, Stroke::new(1.2, col.gamma_multiply(0.6)), 3.0, 3.0));
                    }
                    stash_x = b.right() + 4.0;
                    // The next one won't fit: this slot says how many are left ("+N"; hover lists them, the menu reaches each).
                    if here.len() - k > 1 && stash_x + 26.0 > x_graph - 4.0 {
                        let rest = &here[k..];
                        painter.rect_filled(b, 9.0, theme::bg(ui));
                        painter.extend(Shape::dashed_line(&rounded(b, 9.0), Stroke::new(1.2, if faded { theme::dim(ui).gamma_multiply(0.4) } else { theme::dim(ui) }), 3.0, 2.0));
                        painter.text(b.center(), Align2::CENTER_CENTER, format!("+{}", rest.len()), FontId::proportional(9.0), if faded { theme::dim(ui) } else { text });
                        let names: Vec<_> = rest.iter().map(|&j| format!("{}  {}", d.stashes[j].name, d.stashes[j].message)).collect();
                        let resp = ui.interact(b, ui.id().with(("stash-more", r)), Sense::click()).on_hover_text(names.join("\n"));
                        resp.context_menu(|ui| {
                            for &j in rest {
                                ui.menu_button(&d.stashes[j].message, |ui| crate::ui::sidebar::stashes::stash_menu(ui, d, &d.stashes[j], acts));
                            }
                        });
                        break;
                    }
                    let on = matches!(sel, Some(Sel::Commit(id)) if *id == st.id);
                    painter.rect_filled(b, 9.0, if on { theme::accent().gamma_multiply(0.45) } else { theme::bg(ui) });
                    painter.extend(Shape::dashed_line(&rounded(b, 9.0), Stroke::new(1.2, if faded { theme::dim(ui).gamma_multiply(0.4) } else { theme::dim(ui) }), 3.0, 2.0));
                    painter.text(b.center(), Align2::CENTER_CENTER, icon::ARCHIVE, FontId::proportional(12.0), if faded { theme::dim(ui) } else { text });
                    let resp = ui.interact(b, ui.id().with(("stash", i)), Sense::click()).on_hover_text(format!("{}  ·  stash on this commit\n{}", st.name, st.message));
                    if resp.clicked() {
                        acts.push(Action::Select(Sel::Commit(st.id.clone())));
                    }
                    resp.context_menu(|ui| crate::ui::sidebar::stashes::stash_menu(ui, d, st, acts));
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
                            format!("// WIP  ·  {n} changed file{}", plural(n)),
                            body.clone(),
                            theme::dim(ui),
                        );
                    }
                    Some(cm) => {
                        let text = if faded { text.gamma_multiply(0.35) } else { text };
                        let mp = painter.with_clip_rect(msg_clip);
                        let mut x_text = x_msg + 8.0;
                        // A detached HEAD has no branch pill to carry the HEAD mark: a chip before the message does.
                        if Some(r) == head_row && detached {
                            let t = if d.state == crate::git::State::Clean { "detached HEAD" } else { "HEAD" };
                            x_text = chip(&mp, x_text, ty, t, Color32::WHITE.gamma_multiply(if faded { 0.5 } else { 1.0 }), hc);
                        }
                        // Bisect: good / bad / skip marks, and the commit being tested.
                        if let Some(b) = &d.bisect
                            && let Some(m) = b.mark(&cm.id).or_else(|| (head_id == Some(cm.id.as_str()) && b.found().is_none()).then_some("testing"))
                        {
                            let c = crate::ui::bisect::mark_color(m);
                            x_text = chip(&mp, x_text, ty, m, c, c.gamma_multiply(0.2));
                        }
                        if Some(r) == head_row {
                            // HEAD's title: larger, in the accent colour (lifted toward white on dark themes, where the
                            // plain accent reads poorly), faux-bold (drawn twice, half a px apart).
                            let c = if ui.visuals().dark_mode { hc.lerp_to_gamma(Color32::WHITE, 0.4) } else { hc };
                            let (f, c) = (FontId::proportional(14.5), if faded { c.gamma_multiply(0.35) } else { c });
                            for dx in [0.0, 0.6] {
                                mp.text(pos2(x_text + dx, ty), Align2::LEFT_CENTER, &cm.summary, f.clone(), c);
                            }
                        } else {
                            mp.text(pos2(x_text, ty), Align2::LEFT_CENTER, &cm.summary, body.clone(), text);
                        }
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
            if shown != open {
                ui.ctx().request_repaint();
            }
            ui.data_mut(|m| match shown {
                Some(id) => _ = m.insert_temp(open_id, id),
                None => m.remove::<String>(open_id),
            });
        });
    });
    if max_gx > 2.0 && graph_w > 48.0 {
        let track = Rect::from_x_y_ranges(x_graph + 6.0..=x_msg - 6.0, body.bottom() - 8.0..=body.bottom() - 2.0);
        let thumb_w = (track.width() * graph_w / lanes_w).max(24.0).min(track.width());
        let span = (track.width() - thumb_w).max(1.0);
        let thumb = Rect::from_min_size(pos2(track.left() + span * gx / max_gx, track.top()), vec2(thumb_w, track.height()));
        let resp = ui.interact(track.expand2(vec2(0.0, 3.0)), gx_id.with("bar"), Sense::click_and_drag()).on_hover_text(keys("Scroll the graph sideways (or ⇧ + wheel over it)"));
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
    // (A filter that shows nothing, e.g. a soloed branch since deleted, is not an empty repo.)
    if d.log.is_empty() && d.status.head.is_none() {
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
    // HEAD scrolled out of view: a chip at the edge it lies beyond jumps back to it. It sits in
    // the first / last whole row's date end, so it hides one row's date, not two messages.
    let now = ui.input(|i| i.time);
    let sid = egui::Id::new(("head_chip_scroll", &d.git_dir));
    let (last, since) = ui.data(|m| m.get_temp::<(f32, f64)>(sid)).unwrap_or((row0, f64::NEG_INFINITY));
    let since = if (row0 - last).abs() > 0.5 { now } else { since };
    ui.data_mut(|m| m.insert_temp(sid, (row0, since)));
    let wait = 0.3 - (now - since);
    let away = head_row.filter(|h| !visible.is_empty() && !visible.contains(h));
    if away.is_some() && wait > 0.0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }
    if let Some(h) = away.filter(|_| wait <= 0.0) {
        let up = h < visible.start;
        let name = d.status.branch.as_deref().map_or("HEAD (detached)".to_owned(), |b| format!("{b} (HEAD)"));
        let text = format!("{}  {name}", if up { icon::ARROW_UP } else { icon::ARROW_DOWN });
        let fg = Color32::WHITE;
        let g = ui.painter().layout_no_wrap(text, FontId::proportional(12.5), fg);
        let row = if up { ((body.top() - row0) / ROW).ceil() } else { ((body.bottom() - row0) / ROW).floor() - 1.0 };
        let w = g.size().x + 28.0;
        let chip = Rect::from_min_size(pos2(full.right() - 8.0 - w, row0 + row * ROW + (ROW - 22.0) / 2.0), vec2(w, 22.0));
        let resp = ui.interact(chip, egui::Id::new(("head_chip", &d.git_dir)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Scroll to HEAD");
        let p = ui.painter();
        p.add(egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha(if ui.visuals().dark_mode { 90 } else { 40 }) }.as_shape(chip, 11.0));
        p.rect_filled(chip, 11.0, if resp.hovered() { hc.lerp_to_gamma(Color32::WHITE, 0.2) } else { hc });
        p.rect_stroke(chip, 11.0, Stroke::new(1.0, Color32::from_white_alpha(110)), egui::StrokeKind::Inside);
        p.galley(chip.center() - g.size() / 2.0, g, fg);
        if resp.clicked() {
            *scroll_to = Some(h);
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
    // A text field or a modal dialog (a confirm has no focused widget) owns the keys.
    if ui.ctx().memory(|m| m.focused().is_some() || m.top_modal_layer().is_some_and(|l| l != ui.layer_id())) {
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
