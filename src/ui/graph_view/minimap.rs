//! Thin rail beside the graph table marking HEAD, the selection, WIP, branch tips
//! and search hits across the loaded history.

use crate::repo::{Action, RepoData, Search, Sel};
use crate::theme;
use egui::{Color32, Rect, Sense, Ui, pos2, vec2};

const SEARCH_HIT: Color32 = Color32::from_rgb(0xff, 0xd6, 0x0a);

/// Thin rail beside the table: where HEAD, the selection, WIP, branch tips and
/// search hits sit in the loaded history. Click a marker to jump to it.
#[allow(clippy::too_many_arguments)]
pub(super) fn minimap(ui: &Ui, rail: Rect, d: &RepoData, sel_row: Option<usize>, search: Option<&Search>, visible: std::ops::Range<usize>, scroll_to: &mut Option<usize>, acts: &mut Vec<Action>) {
    let n = d.rows();
    if n == 0 || rail.height() < 20.0 {
        return;
    }
    let y = |r: usize| rail.top() + (r as f32 + 0.5) / n as f32 * rail.height();
    let p = ui.painter();
    p.rect_filled(rail, 5.0, theme::faint(ui));
    let (v0, v1) = (rail.top() + visible.start as f32 / n as f32 * rail.height(), rail.top() + visible.end as f32 / n as f32 * rail.height());
    p.rect_filled(Rect::from_x_y_ranges(rail.x_range(), v0..=v1.max(v0 + 6.0)), 5.0, theme::hover(ui).gamma_multiply(2.5));

    // (row, color, label, thickness): later entries draw on top.
    // Tips are sorted by row; one mark per ~pixel keeps thousands of branches cheap to paint.
    let mut marks: Vec<(usize, Color32, &str, f32)> = Vec::new();
    let mut last = f32::NEG_INFINITY;
    for &(row, i) in &d.tips {
        if y(row) - last >= 1.5 {
            last = y(row);
            marks.push((row, theme::lane(d.graph.nodes[row].color), d.refs[i].name.as_str(), 1.5));
        }
    }
    if d.wip {
        marks.push((0, theme::MOD, "Uncommitted changes", 3.0));
    }
    if let Some(&row) = d.status.head.as_ref().and_then(|h| d.row_of.get(h)) {
        marks.push((row, theme::accent(), "HEAD", 3.0));
    }
    if let Some(row) = sel_row {
        marks.push((row, theme::text(ui), "Selected", 2.0));
    }
    let hits = search.map_or(&[][..], |s| &s.hits[..]);
    let mut last = f32::NEG_INFINITY;
    for &h in hits {
        if y(h) - last >= 2.0 {
            last = y(h);
            p.rect_filled(Rect::from_center_size(pos2(rail.center().x, last), vec2(rail.width() - 2.0, 2.0)), 0.0, SEARCH_HIT);
        }
    }
    for &(row, color, _, t) in &marks {
        p.rect_filled(Rect::from_center_size(pos2(rail.center().x, y(row)), vec2(rail.width(), t)), 1.0, color);
    }

    let resp = ui.interact(rail, ui.id().with("minimap"), Sense::click_and_drag());
    let Some(pos) = resp.hover_pos().or(resp.interact_pointer_pos()) else { return };
    let at = (((pos.y - rail.top()) / rail.height() * n as f32) as usize).min(n - 1);
    // Nearest marker within a few pixels (search hits included).
    let i = hits.partition_point(|&h| h < at);
    let near_hits = [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|i| hits.get(i)).map(|&h| (h, "Search match"));
    let near = marks.iter().map(|m| (m.0, m.2)).chain(near_hits).filter(|m| (y(m.0) - pos.y).abs() <= 5.0).min_by(|a, b| (y(a.0) - pos.y).abs().total_cmp(&(y(b.0) - pos.y).abs()));
    if resp.dragged() {
        *scroll_to = Some(at);
    } else if resp.clicked() {
        match near {
            Some((row, _)) => {
                *scroll_to = Some(row);
                acts.push(Action::Select(d.commit(row).map_or(Sel::Wip, |c| Sel::Commit(c.id.clone()))));
            }
            None => *scroll_to = Some(at),
        }
    }
    let tip = match (near, d.commit(near.map_or(at, |m| m.0))) {
        (Some((_, label)), Some(c)) => format!("{label}\n{}", c.summary),
        (Some((_, label)), None) => label.to_owned(),
        (None, c) => c.map_or("Jump here".to_owned(), |c| format!("Jump to {}", c.date)),
    };
    resp.on_hover_text(tip);
}
