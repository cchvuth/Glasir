//! Diff viewer: unified or side-by-side, with hunk and line staging for
//! working-directory diffs. Only visible lines are painted.

use crate::git::{FileDiff, partial_patch};
use crate::repo::{Action, Backup, DiffSrc, DiffState, Op};
use crate::theme::{self, icon};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

const LINE_H: f32 = 18.0;
const NUM_W: f32 = 44.0;
const CHAR_W: f32 = 7.6;

enum Row {
    Hunk(usize),
    /// Unified: one line.
    Line(usize, usize),
    /// Split: old line | new line.
    Pair(usize, Option<usize>, Option<usize>),
}

fn rows(f: &FileDiff, split: bool) -> Vec<Row> {
    let mut rows = vec![];
    for (hi, h) in f.hunks.iter().enumerate() {
        rows.push(Row::Hunk(hi));
        if !split {
            rows.extend((0..h.lines.len()).filter(|&l| h.lines[l].kind != b'\\').map(|l| Row::Line(hi, l)));
            continue;
        }
        let (mut dels, mut adds) = (vec![], vec![]);
        let flush = |rows: &mut Vec<Row>, dels: &mut Vec<usize>, adds: &mut Vec<usize>| {
            for i in 0..dels.len().max(adds.len()) {
                rows.push(Row::Pair(hi, dels.get(i).copied(), adds.get(i).copied()));
            }
            dels.clear();
            adds.clear();
        };
        for (li, l) in h.lines.iter().enumerate() {
            match l.kind {
                b'-' => dels.push(li),
                b'+' => adds.push(li),
                b'\\' => {}
                _ => {
                    flush(&mut rows, &mut dels, &mut adds);
                    rows.push(Row::Pair(hi, Some(li), Some(li)));
                }
            }
        }
        flush(&mut rows, &mut dels, &mut adds);
    }
    rows
}

/// For each line of a hunk, the line it replaces/is replaced by: the i-th `-`
/// of a run pairs with the i-th `+` of the run that follows.
fn partners(h: &crate::git::Hunk) -> Vec<Option<usize>> {
    let mut out = vec![None; h.lines.len()];
    let mut i = 0;
    while i < h.lines.len() {
        if h.lines[i].kind != b'-' {
            i += 1;
            continue;
        }
        let dels: Vec<usize> = (i..h.lines.len()).take_while(|&j| h.lines[j].kind == b'-').collect();
        let end = i + dels.len();
        let adds: Vec<usize> = (end..h.lines.len()).filter(|&j| h.lines[j].kind != b'\\').take_while(|&j| h.lines[j].kind == b'+').collect();
        for (&d, &a) in dels.iter().zip(&adds) {
            out[d] = Some(a);
            out[a] = Some(d);
        }
        i = end + adds.len().max(1);
    }
    out
}

/// Char range of `a` that differs from `b` (common prefix/suffix removed);
/// `None` when equal or when nearly the whole line changed.
pub fn changed_span(a: &str, b: &str) -> Option<(usize, usize)> {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    let (s, e) = (pre, a.len() - suf);
    (e > s && (e - s) * 10 < a.len().max(1) * 8).then_some((s, e))
}

use crate::highlight::display as expand;

/// `git apply` of a partial patch; `None` (an unsafe partial selection) becomes an explanation.
fn apply_op(label: &str, patch: Option<String>, args: &[&str], path: &str) -> Action {
    let Some(patch) = patch else {
        return Action::Error(format!(
            "{label}: this selection can't be applied on its own (the hunk touches the end of a file without a final newline, or the file isn't plain UTF-8 text). Select the whole hunk, or stage/discard the whole file."
        ));
    };
    let mut a = vec!["apply", "--recount", "--whitespace=nowarn"];
    a.extend_from_slice(args);
    a.push("-");
    let op = Op::new(label, &a).input(patch).light();
    Action::Git(if label.starts_with("Discard") {
        op.backup(Backup::Paths(vec![path.to_owned()])).confirm("Discard the selected changes? (Undo with ⌘Z.)")
    } else {
        op
    })
}

pub fn show(ui: &mut Ui, ds: &mut DiffState, split: &mut bool, acts: &mut Vec<Action>) {
    let change = ds.src.change().clone();
    let (worktree, staged) = match &ds.src {
        DiffSrc::Worktree { staged, .. } => (true, *staged),
        DiffSrc::Commit { .. } => (false, false),
    };
    let lossy = ds.file.as_ref().is_some_and(|f| f.as_ref().is_ok_and(|f| f.lossy));
    let partial = worktree && change.code != '?' && !lossy;
    let file = ds.file.as_ref().and_then(|f| f.as_ref().ok());

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        acts.push(Action::CloseDiff);
    }
    ui.horizontal(|ui| {
        if ui.button(icon::X).on_hover_text("Close (Esc)").clicked() {
            acts.push(Action::CloseDiff);
        }
        if let Some(o) = &change.old_path {
            ui.label(RichText::new(format!("{o} →")).monospace().color(theme::dim(ui)));
        }
        ui.label(RichText::new(&change.path).monospace().strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            theme::group(ui, |ui| {
                for (v, text) in [(true, "Split"), (false, "Unified")] {
                    if ui.add(egui::Button::selectable(*split == v, text)).clicked() {
                        *split = v;
                    }
                }
            });
            if change.code != '?' && change.code != 'A' {
                if theme::pill(ui, icon::USER_LIST).on_hover_text("Blame").clicked() {
                    acts.push(Action::FileView(change.path.clone(), crate::repo::history::Mode::Blame));
                }
                if theme::pill(ui, icon::CLOCK_COUNTER_CLOCKWISE).on_hover_text("File history").clicked() {
                    acts.push(Action::FileView(change.path.clone(), crate::repo::history::Mode::History));
                }
            }
            if worktree {
                let path = change.path.as_str();
                if staged {
                    if ui.button("Unstage file").clicked() {
                        acts.push(Action::Git(Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--", path]).light()));
                    }
                } else if ui.button("Stage file").clicked() {
                    acts.push(Action::Git(Op::new("Stage", &["--literal-pathspecs", "add", "--", path]).light()));
                }
            }
            if let (true, Some(f)) = (partial && !ds.picked.is_empty(), file) {
                let n = ds.picked.len();
                let pick = |h: usize, l: usize| ds.picked.contains(&(h, l));
                if staged {
                    if ui.button(format!("Unstage {n} line(s)")).clicked() {
                        acts.push(apply_op("Unstage lines", partial_patch(f, pick, true), &["--cached", "--reverse"], &change.path));
                    }
                } else {
                    if ui.button(format!("Discard {n} line(s)")).clicked() {
                        acts.push(apply_op("Discard lines", partial_patch(f, pick, true), &["--reverse"], &change.path));
                    }
                    if ui.button(format!("Stage {n} line(s)")).clicked() {
                        acts.push(apply_op("Stage lines", partial_patch(f, pick, false), &["--cached"], &change.path));
                    }
                }
            }
        });
    });
    ui.separator();

    let f = match &ds.file {
        None => {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        }
        Some(Err(e)) => {
            ui.colored_label(theme::DEL, e);
            return;
        }
        Some(Ok(f)) => f,
    };
    if let Some(sm) = &f.submodule {
        return submodule_note(ui, &change.path, sm, acts);
    }
    if f.binary {
        ui.centered_and_justified(|ui| ui.label("Binary file"));
        return;
    }
    if f.hunks.is_empty() {
        ui.centered_and_justified(|ui| ui.label(RichText::new("No changes").color(theme::dim(ui))));
        return;
    }

    let rows = rows(f, *split);
    let pairs: Vec<Vec<Option<usize>>> = f.hunks.iter().map(partners).collect();
    let span = |hi: usize, li: usize| {
        let other = pairs[hi][li]?;
        changed_span(&expand(&f.hunks[hi].lines[li].text), &expand(&f.hunks[hi].lines[other].text))
    };
    let cw = ui.fonts_mut(|fo| fo.glyph_width(&FontId::monospace(12.5), 'M'));
    let max_chars = f.hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.len()).max().unwrap_or(0);
    let mono = FontId::monospace(12.5);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    egui::ScrollArea::both().id_salt("diff").auto_shrink(false).show_rows(ui, LINE_H, rows.len(), |ui, range| {
        let content_w = if *split { ui.available_width() } else { (NUM_W * 2.0 + 20.0 + max_chars as f32 * CHAR_W).max(ui.available_width()) };
        ui.set_min_width(content_w);
        let left = ui.max_rect().left();
        let top = ui.max_rect().top() - range.start as f32 * LINE_H;
        let painter = ui.painter().clone();
        for r in range {
            let rect = Rect::from_min_size(pos2(left, top + r as f32 * LINE_H), vec2(content_w, LINE_H));
            let y = rect.center().y;
            match rows[r] {
                Row::Hunk(hi) => {
                    let h = &f.hunks[hi];
                    painter.rect_filled(rect, 0.0, theme::faint(ui));
                    painter.text(pos2(left + 8.0, y), Align2::LEFT_CENTER, &h.header, mono.clone(), theme::dim(ui));
                    if partial {
                        let all = |hh: usize, _: usize| hh == hi;
                        let mut x = ui.clip_rect().right() - 8.0;
                        let mut btn = |ui: &mut Ui, text: &str| {
                            let w = text.len() as f32 * 6.6 + 14.0;
                            x -= w + 6.0;
                            ui.put(Rect::from_min_size(pos2(x, rect.top() + 1.0), vec2(w, LINE_H - 2.0)), egui::Button::new(RichText::new(text).small())).clicked()
                        };
                        if staged {
                            if btn(ui, "Unstage hunk") {
                    acts.push(apply_op("Unstage hunk", partial_patch(f, all, true), &["--cached", "--reverse"], &change.path));
                            }
                        } else {
                            if btn(ui, "Stage hunk") {
                    acts.push(apply_op("Stage hunk", partial_patch(f, all, false), &["--cached"], &change.path));
                            }
                            if btn(ui, "Discard hunk") {
                    acts.push(apply_op("Discard hunk", partial_patch(f, all, true), &["--reverse"], &change.path));
                            }
                        }
                    }
                }
                Row::Line(hi, li) => {
                    let l = &f.hunks[hi].lines[li];
                    let picked = ds.picked.contains(&(hi, li));
                    let resp = ui.interact(rect, ui.id().with(("dl", r)), Sense::click());
                    if partial && l.kind != b' ' && resp.clicked() && !ds.picked.remove(&(hi, li)) {
                        ds.picked.insert((hi, li));
                    }
                    let colors = ds.hl.get(hi).and_then(|h| h.get(li));
                    paint_line(ui, &painter, rect, l.kind, l.old, l.new, &l.text, picked, &mono, true, span(hi, li), cw, colors);
                }
                Row::Pair(hi, a, b) => {
                    let h = &f.hunks[hi];
                    let half = rect.width() / 2.0;
                    let lr = Rect::from_min_size(rect.min, vec2(half, LINE_H));
                    let rr = Rect::from_min_size(pos2(rect.left() + half, rect.top()), vec2(half, LINE_H));
                    for (side, idx, old) in [(lr, a, true), (rr, b, false)] {
                        let Some(li) = idx else {
                            painter.rect_filled(side, 0.0, theme::faint(ui).gamma_multiply(0.5));
                            continue;
                        };
                        let l = &h.lines[li];
                        let resp = ui.interact(side, ui.id().with(("sl", r, old)), Sense::click());
                        if partial && l.kind != b' ' && resp.clicked() && !ds.picked.remove(&(hi, li)) {
                            ds.picked.insert((hi, li));
                        }
                        let no = if old { l.old } else { l.new };
                        let colors = ds.hl.get(hi).and_then(|h| h.get(li));
                        let picked = ds.picked.contains(&(hi, li));
                        paint_line(ui, &painter.with_clip_rect(side), side, l.kind, no, 0, &l.text, picked, &mono, false, span(hi, li), cw, colors);
                    }
                }
            }
        }
    });
}

/// A submodule pointer change: where it moved and the commits in between.
fn submodule_note(ui: &mut Ui, path: &str, sm: &crate::git::SubmoduleDiff, acts: &mut Vec<Action>) {
    let zero = |id: &str| id.bytes().all(|b| b == b'0');
    let title = match (zero(&sm.from), zero(&sm.to)) {
        _ if sm.from.is_empty() => "Submodule content changed".to_owned(),
        (true, _) => format!("Submodule added at {}", sm.to),
        (_, true) => format!("Submodule removed (was at {})", sm.from),
        _ => format!("Submodule moved from {} to {}", sm.from, sm.to),
    };
    note_frame(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {title}", icon::PACKAGE)).strong());
            if !sm.note.is_empty() && !sm.note.contains("submodule") {
                ui.label(RichText::new(format!("({})", sm.note)).color(theme::dim(ui)));
            }
        });
        if !sm.log.is_empty() {
            ui.add_space(6.0);
        }
        for l in &sm.log {
            // `> subject` came in, `< subject` was dropped (rewind).
            let (sign, color, text) = match l.split_once(' ') {
                Some(("<", t)) => ("−", theme::DEL, t),
                Some((_, t)) => ("+", theme::ADD, t),
                None => ("+", theme::ADD, l.as_str()),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(sign).monospace().color(color));
                ui.label(text);
            });
        }
        for d in &sm.dirty {
            ui.label(RichText::new(format!("{} Working copy {d}", icon::PENCIL_SIMPLE)).color(theme::MOD));
        }
        ui.add_space(6.0);
        if !zero(&sm.to) && ui.button(format!("{} Open submodule in new tab", icon::ARROW_SQUARE_OUT)).clicked() {
            acts.push(Action::OpenTab(path.to_owned()));
        }
    });
}

fn note_frame(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    egui::Frame::new().fill(theme::faint(ui)).inner_margin(12.0).corner_radius(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
}

#[allow(clippy::too_many_arguments)]
fn paint_line(
    ui: &Ui,
    p: &egui::Painter,
    rect: Rect,
    kind: u8,
    a: u32,
    b: u32,
    text: &str,
    picked: bool,
    font: &FontId,
    two_nums: bool,
    span: Option<(usize, usize)>,
    cw: f32,
    colors: Option<&crate::highlight::Spans>,
) {
    let (bg, sign_color) = match kind {
        b'+' => (theme::added_bg(ui), theme::ADD),
        b'-' => (theme::removed_bg(ui), theme::DEL),
        _ => (Color32::TRANSPARENT, theme::dim(ui)),
    };
    p.rect_filled(rect, 0.0, bg);
    if picked {
        p.rect_filled(rect, 0.0, theme::selection(ui));
        p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, ui.visuals().selection.bg_fill);
    }
    let y = rect.center().y;
    let num = |n: u32| if n == 0 { String::new() } else { n.to_string() };
    let dim = theme::dim(ui);
    let mut x = rect.left() + NUM_W - 6.0;
    p.text(pos2(x, y), Align2::RIGHT_CENTER, num(a), FontId::monospace(11.0), dim);
    if two_nums {
        x += NUM_W;
        p.text(pos2(x, y), Align2::RIGHT_CENTER, num(b), FontId::monospace(11.0), dim);
    }
    x += 8.0;
    let sign = match kind {
        b'+' => "+",
        b'-' => "-",
        _ => " ",
    };
    p.text(pos2(x, y), Align2::LEFT_CENTER, sign, font.clone(), sign_color);
    if let Some((s, e)) = span {
        // Word-level emphasis on the part of the line that actually changed.
        let r = Rect::from_min_max(pos2(x + 14.0 + s as f32 * cw, rect.top() + 1.0), pos2(x + 14.0 + e as f32 * cw, rect.bottom() - 1.0));
        p.rect_filled(r, 3.0, sign_color.gamma_multiply(0.35));
    }
    let shown = expand(text);
    match colors.filter(|c| !c.is_empty()) {
        Some(spans) => {
            let mut job = egui::text::LayoutJob::default();
            for (c, r) in spans {
                if let Some(s) = shown.get(r.clone()) {
                    job.append(s, 0.0, egui::TextFormat::simple(font.clone(), *c));
                }
            }
            let g = p.layout_job(job);
            p.galley(pos2(x + 14.0, y - g.size().y / 2.0), g, theme::text(ui));
        }
        None => {
            p.text(pos2(x + 14.0, y), Align2::LEFT_CENTER, shown, font.clone(), theme::text(ui));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_diff;

    #[test]
    fn word_spans() {
        assert_eq!(changed_span("let x = 1;", "let x = 2;"), Some((8, 9)));
        assert_eq!(changed_span("same", "same"), None);
        assert_eq!(changed_span("abc", "xyz"), None); // whole-line rewrite: no emphasis
        assert_eq!(changed_span("foo(a)", "foo(a, b)"), None); // pure insertion: nothing removed
        assert_eq!(changed_span("foo(a, b)", "foo(a)"), Some((5, 8))); // ", b"
    }

    #[test]
    fn partner_lines() {
        let d = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n a\n-b\n-c\n+B\n d\n";
        let h = &parse_diff(d)[0].hunks[0];
        assert_eq!(partners(h), [None, Some(3), None, Some(1), None]);
    }
}
