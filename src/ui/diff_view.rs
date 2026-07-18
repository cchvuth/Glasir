//! Diff viewer: unified or side-by-side, with hunk and line staging for
//! working-directory diffs, whitespace / context options, change navigation
//! (F7 / ⇧F7) and a change minimap. Only visible lines are painted.

use crate::git::{DiffOpts, FileDiff, partial_patch, selects_all};
use crate::repo::{Action, DiffSrc, DiffState, Op};
use crate::theme::{self, icon};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};

const LINE_H: f32 = 18.0;
const NUM_W: f32 = 44.0;
const CHAR_W: f32 = 7.6;
const RAIL_W: f32 = 8.0;
/// Lines kept above a change jumped to.
const LEAD: usize = 3;

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

/// A row's change: `+`, `-`, `~` (split row with both) or 0.
fn row_kind(f: &FileDiff, r: &Row) -> u8 {
    let k = |hi: usize, li: Option<usize>| li.map_or(b' ', |li| f.hunks[hi].lines[li].kind);
    match *r {
        Row::Hunk(_) => 0,
        Row::Line(hi, li) => Some(k(hi, Some(li))).filter(|k| matches!(k, b'+' | b'-')).unwrap_or(0),
        Row::Pair(hi, a, b) => match (k(hi, a) == b'-', k(hi, b) == b'+') {
            (true, true) => b'~',
            (true, false) => b'-',
            (false, true) => b'+',
            _ => 0,
        },
    }
}

/// First row of each run of changed rows.
fn change_starts(kinds: &[u8]) -> Vec<usize> {
    (0..kinds.len()).filter(|&r| kinds[r] != 0 && (r == 0 || kinds[r - 1] == 0)).collect()
}

/// The next (or previous) change start after (before) `anchor`.
fn step(starts: &[usize], anchor: usize, forward: bool) -> Option<usize> {
    if forward { starts.iter().find(|&&s| s > anchor) } else { starts.iter().rev().find(|&&s| s < anchor) }.copied()
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
fn apply_op(label: &str, patch: Option<String>, args: &[&str], src: &DiffSrc, opts: DiffOpts, f: &FileDiff) -> Action {
    match patch {
        Some(patch) => Action::Git(crate::repo::patch_op(label, src, opts, f, patch, args)),
        None => Action::Error(format!(
            "{label}: this selection can't be applied on its own (the hunk touches the end of a file without a final newline, or the file isn't plain UTF-8 text). Select the whole hunk, or stage/discard the whole file."
        )),
    }
}

/// Stage the `selected` lines of an unstaged diff. All of an untracked file is a plain `add`:
/// a part goes through `patch_op`, which first records the file as intent-to-add.
fn stage(label: &str, f: &FileDiff, selected: impl Fn(usize, usize) -> bool, src: &DiffSrc, opts: DiffOpts) -> Action {
    let change = src.change();
    if change.code == '?' && selects_all(f, &selected) {
        return Action::Git(Op::new("Stage", &["--literal-pathspecs", "add", "--", &change.path]).light());
    }
    apply_op(label, partial_patch(f, selected, false), &["--cached"], src, opts, f)
}

/// Discard the `selected` lines of an unstaged diff. All of an intent-to-add file (or worktree
/// rename): `apply --reverse` would leave its index entry behind, so the file is discarded.
fn discard(label: &str, f: &FileDiff, selected: impl Fn(usize, usize) -> bool, src: &DiffSrc, opts: DiffOpts) -> Action {
    let change = src.change();
    if matches!(change.code, 'A' | 'R' | 'C') && selects_all(f, &selected) {
        return Action::Git(crate::repo::discard_op(change));
    }
    apply_op(label, partial_patch(f, selected, true), &["--reverse"], src, opts, f)
}

pub fn show(ui: &mut Ui, ds: &mut DiffState, split: &mut bool, acts: &mut Vec<Action>) {
    let (src, ds_opts) = (ds.src.clone(), ds.opts);
    let change = src.change().clone();
    let (worktree, staged) = match &ds.src {
        DiffSrc::Worktree { staged, .. } => (true, *staged),
        DiffSrc::Commit { .. } => (false, false),
    };
    let lossy = ds.file.as_ref().is_some_and(|f| f.as_ref().is_ok_and(|f| f.lossy));
    // Partial staging patches the shown diff, which must be a plain (no -w), current one.
    // An untracked file's diff is never filtered, and only staged from (intent-to-add first).
    let untracked = change.code == '?';
    let partial = worktree && !lossy && (untracked || !ds.opts.ignore_ws) && !ds.loading();
    let file = ds.file.as_ref().and_then(|f| f.as_ref().ok());
    let rows = file.map_or(vec![], |f| rows(f, *split));
    let kinds: Vec<u8> = file.map_or(vec![], |f| rows.iter().map(|r| row_kind(f, r)).collect());
    let starts = change_starts(&kinds);
    // Scroll offset and viewport height of the last frame.
    let mem = ui.id().with("diff_pos");
    let (off, view_h) = ui.data(|m| m.get_temp::<(f32, f32)>(mem)).unwrap_or_default();
    let anchor = (off / LINE_H).round() as usize + LEAD;
    let mut nav = ui.input(|i| i.key_pressed(egui::Key::F7).then_some(!i.modifiers.shift));

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        acts.push(Action::CloseDiff);
    }
    theme::bar(ui, |ui| {
        if worktree && change.code != 'D' && theme::pill(ui, icon::CODE).on_hover_text("Open in editor").clicked() {
            acts.push(Action::Editor(change.path.clone()));
        }
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
                    acts.push(Action::Git(crate::repo::unstage_op(&change)));
                }
            } else if ui.button("Stage file").clicked() {
                acts.push(Action::Git(Op::new("Stage", &["--literal-pathspecs", "add", "--", path]).light()));
            }
        }
        if worktree && change.code != '?' && ds.opts.ignore_ws {
            ui.label(RichText::new(format!("{} Line staging off", icon::INFO)).small().color(theme::MOD))
                .on_hover_text("Hunks and lines can't be staged from a whitespace-ignoring diff: turn off Ignore whitespace to stage parts of this file.");
        }
        if let (true, Some(f)) = (partial && !ds.picked.is_empty(), file) {
            let n = ds.picked.len();
            let pick = |h: usize, l: usize| ds.picked.contains(&(h, l));
            if staged {
                if ui.button(format!("Unstage {n} line(s)")).clicked() {
                    acts.push(apply_op("Unstage lines", partial_patch(f, pick, true), &["--cached", "--reverse"], &src, ds_opts, f));
                }
            } else {
                if !untracked && ui.button(format!("Discard {n} line(s)")).clicked() {
                    acts.push(discard("Discard lines", f, pick, &src, ds_opts));
                }
                if ui.button(format!("Stage {n} line(s)")).clicked() {
                    acts.push(stage("Stage lines", f, pick, &src, ds_opts));
                }
            }
        }
        theme::rest(ui, |ui| {
            if ui.button(icon::X).on_hover_text("Close (Esc)").clicked() {
                acts.push(Action::CloseDiff);
            }
            if let Some(o) = &change.old_path {
                ui.label(RichText::new(format!("{o} →")).monospace().color(theme::dim(ui)));
            }
            ui.label(RichText::new(&change.path).monospace().strong());
        });
    });
    if ds.image.is_none() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 30.0; // the glass groups draw beyond their row height
            view_controls(ui, ds, split, worktree && change.code != '?', &starts, anchor, &mut nav, acts)
        });
    }
    ui.add_space(6.0);

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
    if let Some((old, new)) = &f.lfs {
        return lfs_note(ui, old.as_ref(), new.as_ref());
    }
    if let Some(img) = &ds.image {
        return super::image_diff::show(ui, img);
    }
    if f.binary {
        ui.centered_and_justified(|ui| ui.label("Binary file"));
        return;
    }
    if f.hunks.is_empty() {
        let text = if ds.opts.ignore_ws { "Only whitespace changed" } else { "No changes" };
        ui.centered_and_justified(|ui| ui.label(RichText::new(text).color(theme::dim(ui))));
        return;
    }

    let area = ui.available_rect_before_wrap();
    let rail = Rect::from_x_y_ranges(area.right() - RAIL_W..=area.right(), area.top() + 2.0..=area.bottom() - 2.0);
    let body = Rect::from_x_y_ranges(area.left()..=rail.left() - 4.0, area.y_range());
    let per = (rail.height() / rows.len() as f32).min(LINE_H);
    let row_at = |y: f32| (((y - rail.top()) / per) as usize).min(rows.len() - 1);
    let rail_resp = ui.interact(rail, mem.with("rail"), Sense::click_and_drag()).on_hover_text("Changes in this file: click to jump");
    let mut target = rail_resp.interact_pointer_pos().filter(|_| rail_resp.clicked() || rail_resp.dragged()).map(|p| row_at(p.y) as f32 * LINE_H - view_h / 2.0);
    if let Some(s) = nav.and_then(|fwd| step(&starts, anchor, fwd)) {
        target = Some(s.saturating_sub(LEAD) as f32 * LINE_H);
    }
    let pairs: Vec<Vec<Option<usize>>> = f.hunks.iter().map(partners).collect();
    let span = |hi: usize, li: usize| {
        let other = pairs[hi][li]?;
        changed_span(&expand(&f.hunks[hi].lines[li].text), &expand(&f.hunks[hi].lines[other].text))
    };
    let cw = ui.fonts_mut(|fo| fo.glyph_width(&FontId::monospace(12.5), 'M'));
    let max_chars = f.hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.len()).max().unwrap_or(0);
    let mono = FontId::monospace(12.5);
    let out = ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let mut area = egui::ScrollArea::both().id_salt("diff").auto_shrink(false);
        if let Some(t) = target {
            area = area.vertical_scroll_offset(t.max(0.0));
        }
        area.show_rows(ui, LINE_H, rows.len(), |ui, range| {
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
                                let font = egui::TextStyle::Small.resolve(ui.style());
                                let w = ui.fonts_mut(|fo| fo.layout_no_wrap(text.into(), font, Color32::WHITE).size().x) + 2.0 * ui.spacing().button_padding.x;
                                x -= w + 6.0;
                                let b = egui::Button::new(RichText::new(text).small()).wrap_mode(egui::TextWrapMode::Extend);
                                ui.put(Rect::from_min_size(pos2(x, rect.top() + 1.0), vec2(w, LINE_H - 2.0)), b).clicked()
                            };
                            if staged {
                                if btn(ui, "Unstage hunk") {
                        acts.push(apply_op("Unstage hunk", partial_patch(f, all, true), &["--cached", "--reverse"], &src, ds_opts, f));
                                }
                            } else {
                                if btn(ui, "Stage hunk") {
                        acts.push(stage("Stage hunk", f, all, &src, ds_opts));
                                }
                                if !untracked && btn(ui, "Discard hunk") {
                        acts.push(discard("Discard hunk", f, all, &src, ds_opts));
                                }
                            }
                        }
                    }
                    Row::Line(hi, li) => {
                        let l = &f.hunks[hi].lines[li];
                        let picked = ds.picked.contains(&(hi, li));
                        let pick = partial && l.kind != b' ';
                        let resp = ui.interact(rect, ui.id().with(("dl", r)), if pick { Sense::click_and_drag() } else { Sense::click() });
                        if pick {
                            pick_lines(ui, mem, &mut ds.picked, &rows, f, &resp, r, None, (hi, li));
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
                            let pick = partial && l.kind != b' ';
                            let resp = ui.interact(side, ui.id().with(("sl", r, old)), if pick { Sense::click_and_drag() } else { Sense::click() });
                            if pick {
                                pick_lines(ui, mem, &mut ds.picked, &rows, f, &resp, r, Some(old), (hi, li));
                            }
                            let no = if old { l.old } else { l.new };
                            let colors = ds.hl.get(hi).and_then(|h| h.get(li));
                            let picked = ds.picked.contains(&(hi, li));
                            paint_line(ui, &painter.with_clip_rect(side), side, l.kind, no, 0, &l.text, picked, &mono, false, span(hi, li), cw, colors);
                        }
                    }
                }
            }
        })
    });
    let (off, view_h) = (out.inner.state.offset.y, out.inner.inner_rect.height());
    ui.data_mut(|m| m.insert_temp(mem, (off, view_h)));
    minimap(ui, rail, &kinds, off, view_h);
}

/// Change lines of a row; a split row gives only the `old` (left) or new side.
fn row_changes(f: &FileDiff, row: &Row, old: Option<bool>) -> Vec<(usize, usize)> {
    let at = |hi: usize, li: usize| (f.hunks[hi].lines[li].kind != b' ').then_some((hi, li));
    match *row {
        Row::Hunk(_) => vec![],
        Row::Line(hi, li) => at(hi, li).into_iter().collect(),
        Row::Pair(hi, a, b) => [(true, a), (false, b)].into_iter().filter(|(o, _)| old.is_none_or(|s| s == *o)).filter_map(|(_, li)| at(hi, li?)).collect(),
    }
}

/// Click toggles a change line; dragging from one selects (or, starting on a picked line,
/// deselects) every change line down to the pointer, relative to the picks at the drag's start.
#[allow(clippy::too_many_arguments)]
fn pick_lines(ui: &Ui, mem: egui::Id, picked: &mut std::collections::BTreeSet<(usize, usize)>, rows: &[Row], f: &FileDiff, resp: &egui::Response, r: usize, old: Option<bool>, at: (usize, usize)) {
    if resp.clicked() && !picked.remove(&at) {
        picked.insert(at);
    }
    let key = mem.with("drag");
    if resp.drag_started() {
        ui.data_mut(|m| m.insert_temp(key, (picked.clone(), !picked.contains(&at))));
    }
    if let (true, Some(p), Some((base, add))) = (resp.dragged(), resp.interact_pointer_pos(), ui.data(|m| m.get_temp::<(std::collections::BTreeSet<(usize, usize)>, bool)>(key))) {
        let end = (r as f32 + ((p.y - resp.rect.top()) / LINE_H).floor()).clamp(0.0, (rows.len() - 1) as f32) as usize;
        *picked = base;
        for row in &rows[r.min(end)..=r.max(end)] {
            for l in row_changes(f, row, old) {
                if add { picked.insert(l) } else { picked.remove(&l) };
            }
        }
    }
}

/// The second header row: layout, whitespace, context, change navigation.
#[allow(clippy::too_many_arguments)]
fn view_controls(ui: &mut Ui, ds: &DiffState, split: &mut bool, stageable: bool, starts: &[usize], anchor: usize, nav: &mut Option<bool>, acts: &mut Vec<Action>) {
    let o = ds.pending;
    wrap_group(ui, "layout", |ui| {
        for (v, text) in [(false, "Unified"), (true, "Split")] {
            if ui.add(egui::Button::selectable(*split == v, text)).clicked() {
                *split = v;
            }
        }
    });
    ui.add_space(6.0);
    wrap_group(ui, "ws", |ui| {
        let hint = if stageable { "Ignore whitespace changes (git diff -w).\nLine and hunk staging are off while this is on." } else { "Ignore whitespace changes (git diff -w)" };
        if ui.add(egui::Button::selectable(o.ignore_ws, format!("{} Ignore whitespace", icon::PARAGRAPH))).on_hover_text(hint).clicked() {
            acts.push(Action::DiffOpts(DiffOpts { ignore_ws: !o.ignore_ws, ..o }));
        }
    });
    ui.add_space(6.0);
    wrap_group(ui, "context", |ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("Context").small().color(theme::dim(ui)));
        ui.add_space(8.0);
        for (n, text, hint) in [(3, "3", "3 lines around each change"), (10, "10", "10 lines around each change"), (DiffOpts::FILE, "File", "The whole file")] {
            if ui.add(egui::Button::selectable(o.context == n, text)).on_hover_text(hint).clicked() && o.context != n {
                acts.push(Action::DiffOpts(DiffOpts { context: n, ..o }));
            }
        }
    });
    wrap_before(ui, "nav");
    let w = ui
        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            theme::group(ui, |ui| {
                let next = ui.add_enabled(step(starts, anchor, true).is_some(), egui::Button::new(icon::ARROW_DOWN).frame_when_inactive(false).corner_radius(255));
                if next.on_hover_text("Next change (F7)").clicked() {
                    *nav = Some(true);
                }
                let prev = ui.add_enabled(step(starts, anchor, false).is_some(), egui::Button::new(icon::ARROW_UP).frame_when_inactive(false).corner_radius(255));
                if prev.on_hover_text("Previous change (⇧F7)").clicked() {
                    *nav = Some(false);
                }
            });
            let n = starts.len();
            ui.label(RichText::new(format!("{n} change{}", if n == 1 { "" } else { "s" })).small().color(theme::dim(ui)));
            ui.min_rect().width() + 12.0
        })
        .inner;
    ui.data_mut(|d| d.insert_temp(ui.id().with("nav"), w));
}

/// In a wrapping row: starts a new row when `key`'s width from the last frame no
/// longer fits on this one (egui only learns a group's width after drawing it).
fn wrap_before(ui: &mut Ui, key: &str) {
    let need = ui.data(|d| d.get_temp::<f32>(ui.id().with(key))).unwrap_or(0.0);
    if need > ui.available_size_before_wrap().x && ui.cursor().min.x > ui.max_rect().left() + 1.0 {
        ui.end_row();
    }
}

fn wrap_group(ui: &mut Ui, key: &str, add: impl FnOnce(&mut Ui)) {
    wrap_before(ui, key);
    let w = ui.scope(|ui| theme::group(ui, add)).response.rect.width();
    ui.data_mut(|d| d.insert_temp(ui.id().with(key), w));
}

/// Rail of added / removed / changed rows across the whole diff, with the viewport.
fn minimap(ui: &Ui, rail: Rect, kinds: &[u8], off: f32, view_h: f32) {
    // Short diffs map 1:1 to their lines; long ones are squeezed into the rail.
    let per = (rail.height() / kinds.len().max(1) as f32).min(LINE_H);
    let p = ui.painter();
    p.rect_filled(rail, 4.0, theme::faint(ui));
    let y = |row: f32| rail.top() + row * per;
    let (v0, v1) = (y(off / LINE_H), y((off + view_h) / LINE_H).min(rail.bottom()));
    p.rect_filled(Rect::from_x_y_ranges(rail.x_range(), v0..=v1.max(v0 + 6.0)), 4.0, theme::hover(ui).gamma_multiply(2.5));
    let h = per.max(2.0);
    let mut last = (f32::NEG_INFINITY, 0u8);
    for (r, &k) in kinds.iter().enumerate().filter(|(_, k)| **k != 0) {
        let top = y(r as f32);
        if top - last.0 < 1.0 && k == last.1 {
            continue;
        }
        last = (top, k);
        let color = match k {
            b'+' => theme::ADD,
            b'-' => theme::DEL,
            _ => theme::MOD,
        };
        p.rect_filled(Rect::from_x_y_ranges(rail.x_range(), top..=top + h), 1.0, color);
    }
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

/// An LFS pointer file: the object's size and id instead of the pointer text.
fn lfs_note(ui: &mut Ui, old: Option<&crate::git::LfsPointer>, new: Option<&crate::git::LfsPointer>) {
    use crate::ui::format::bytes;
    note_frame(ui, |ui| {
        let size = match (old, new) {
            (Some(o), Some(n)) => format!("size {} (was {})", bytes(n.size), bytes(o.size)),
            (None, Some(n)) => format!("size {} (new)", bytes(n.size)),
            (Some(o), None) => format!("size {} (removed)", bytes(o.size)),
            (None, None) => String::new(),
        };
        ui.label(RichText::new(format!("{} Git LFS object, {size}", icon::DATABASE)).strong());
        ui.add_space(4.0);
        for (what, p) in [("old", old), ("new", new)] {
            if let Some(p) = p {
                ui.label(RichText::new(format!("{what}  {}", p.oid)).monospace().small().color(theme::dim(ui)));
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("The repository stores a pointer; the content lives in LFS storage.").small().color(theme::dim(ui)));
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
    fn change_navigation() {
        let d = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n a\n-b\n+B\n c\n d\n-e\n+E\n f\n";
        let f = &parse_diff(d)[0];
        let unified: Vec<u8> = rows(f, false).iter().map(|r| row_kind(f, r)).collect();
        assert_eq!(unified, [0, 0, b'-', b'+', 0, 0, b'-', b'+', 0]);
        let split: Vec<u8> = rows(f, true).iter().map(|r| row_kind(f, r)).collect();
        assert_eq!(split, [0, 0, b'~', 0, 0, b'~', 0]);
        let starts = change_starts(&unified);
        assert_eq!(starts, [2, 6]);
        assert_eq!((step(&starts, 0, true), step(&starts, 2, true), step(&starts, 6, true)), (Some(2), Some(6), None));
        assert_eq!((step(&starts, 6, false), step(&starts, 2, false)), (Some(2), None));
    }

    #[test]
    fn partner_lines() {
        let d = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n a\n-b\n-c\n+B\n d\n";
        let h = &parse_diff(d)[0].hunks[0];
        assert_eq!(partners(h), [None, Some(3), None, Some(1), None]);
    }

    /// Discarding all of a worktree rename's diff runs the file discard: `apply --reverse` would
    /// keep the intent-to-add entry (` D new.txt`). Part of it stays a patch.
    #[test]
    fn whole_discard_of_intent_to_add_rename() {
        let dir = std::env::temp_dir().join(format!("glasir-dv-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        for kv in [["user.email", "t@t"], ["user.name", "T"], ["commit.gpgsign", "false"]] {
            g(&["config", kv[0], kv[1]]);
        }
        let text = "1\n2\n3\n4\n5\n6\n7\n8\n";
        std::fs::write(dir.join("old.txt"), text).unwrap();
        g(&["add", "old.txt"]);
        g(&["commit", "-q", "-m", "i"]);
        std::fs::remove_file(dir.join("old.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), format!("{text}edit\nmore\n")).unwrap();
        g(&["add", "-N", "new.txt"]);
        let c = crate::git::status(&dir).unwrap().unstaged.remove(0);
        assert_eq!(c.code, 'R');
        let f = crate::git::worktree_diff(&dir, &c, false, DiffOpts::default()).unwrap();
        let src = DiffSrc::Worktree { change: c.clone(), staged: false };
        let Action::Git(op) = discard("Discard hunk", &f, |h, _| h == 0, &src, DiffOpts::default()) else { panic!() };
        for cmd in &op.cmds {
            crate::git::run_input(&dir, &cmd.iter().map(String::as_str).collect::<Vec<_>>(), op.input.as_deref()).unwrap();
        }
        assert_eq!(g(&["status", "--porcelain"]), "");
        assert_eq!(std::fs::read_to_string(dir.join("old.txt")).unwrap(), text);
        let part = discard("Discard lines", &f, |_, l| l == 3, &src, DiffOpts::default());
        assert!(matches!(part, Action::Git(o) if o.cmds.iter().any(|c| c[0] == "apply")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
