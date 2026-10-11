//! Diff viewer: unified or side-by-side, hunk and line staging for working-directory diffs,
//! revert / cherry-pick from commits, whitespace / context options, change navigation (F7 / ⇧F7)
//! and a change minimap. Only visible lines are painted.

use crate::ui::format::{keys, plural};
use super::format::{tops, visible, wrapped};
use crate::git::{DiffOpts, FileDiff, Page, partial_patch, selects_all};
use crate::repo::history::Mode;
use crate::repo::{Action, DiffSrc, DiffState, Op};
use crate::theme::{self, icon};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use std::collections::BTreeSet;

const LINE_H: f32 = 18.0;
const NUM_W: f32 = 44.0;
const RAIL_W: f32 = 8.0;
const TITLE_MIN_W: f32 = 220.0;
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
            rows.extend((0..h.lines.len()).map(|l| Row::Line(hi, l)));
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
                // "No newline at end of file" belongs to the side of the line before it.
                b'\\' if h.lines[li - 1].kind == b'-' => dels.push(li),
                b'\\' if h.lines[li - 1].kind == b'+' => adds.push(li),
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

/// What a diff's rows need every frame, built once per loaded file and layout.
struct Layout {
    key: (u64, bool),
    rows: Vec<Row>,
    kinds: Vec<u8>,
    starts: Vec<usize>,
    pairs: Vec<Vec<Option<usize>>>,
    max_chars: usize,
}

impl Layout {
    /// Cached in egui memory under `stamp` (changes whenever the file is reloaded) and `split`.
    fn get(ui: &Ui, stamp: u64, f: Option<&FileDiff>, split: bool) -> std::sync::Arc<Self> {
        let (id, key) = (ui.id().with("diff_layout"), (stamp, split));
        if let Some(c) = ui.data(|m| m.get_temp::<std::sync::Arc<Self>>(id)).filter(|c| c.key == key) {
            return c;
        }
        let rows = f.map_or(vec![], |f| rows(f, split));
        let kinds: Vec<u8> = f.map_or(vec![], |f| rows.iter().map(|r| row_kind(f, r)).collect());
        let starts = change_starts(&kinds);
        let pairs = f.map_or(vec![], |f| f.hunks.iter().map(partners).collect());
        let max_chars = f.map_or(0, |f| f.hunks.iter().flat_map(|h| &h.lines).map(|l| crate::highlight::display_len(&l.text)).max().unwrap_or(0));
        let c = std::sync::Arc::new(Self { key, rows, kinds, starts, pairs, max_chars });
        ui.data_mut(|m| m.insert_temp(id, c.clone()));
        c
    }
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

const REVERT_HINT: &str = "Undo these lines of the commit in your working tree (⌘Z restores the file)";
const PICK_HINT: &str = "Apply these lines of the commit to your working tree (⌘Z restores the file)";

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

/// Path and `git log -L` range for "History of these lines": the picked lines when `at` is one
/// of them, else `at`. A commit diff's new side (its old side if only removed lines are
/// picked); a worktree diff's old side (HEAD when staged, else the index).
fn line_range(src: &DiffSrc, f: &FileDiff, picked: &BTreeSet<(usize, usize)>, at: (usize, usize)) -> Option<(String, Mode)> {
    let one = [at];
    let sel = if picked.contains(&at) { picked.iter().collect::<Vec<_>>() } else { one.iter().collect() };
    let lines: Vec<_> = sel.into_iter().filter_map(|&(h, l)| f.hunks.get(h)?.lines.get(l)).collect();
    let span = |new: bool| {
        let n: Vec<u32> = lines.iter().map(|l| if new { l.new } else { l.old }).filter(|&n| n > 0).collect();
        Some((*n.iter().min()?, *n.iter().max()?))
    };
    let c = src.change();
    let old_path = c.old_path.clone().unwrap_or_else(|| c.path.clone());
    let (path, rev, (start, end)) = match src {
        // The working tree (a compare's new side) has no history yet: the base's lines.
        DiffSrc::Commit { id, parent, .. } => match span(true).filter(|_| id != crate::git::WORKTREE) {
            Some(s) => (c.path.clone(), Some(id.clone()), s),
            None => (old_path, Some(parent.clone()?), span(false)?),
        },
        DiffSrc::Worktree { staged, .. } => (old_path, staged.then(|| "HEAD".into()), span(false)?),
    };
    Some((path, Mode::Lines { rev, start, end }))
}

fn line_menu(ui: &mut Ui, src: &DiffSrc, f: &FileDiff, picked: &BTreeSet<(usize, usize)>, at: (usize, usize), acts: &mut Vec<Action>) {
    let range = line_range(src, f, picked, at);
    let text = format!("{}  History of these lines", icon::CLOCK_COUNTER_CLOCKWISE);
    let b = ui.add_enabled(range.is_some(), egui::Button::new(text)).on_disabled_hover_text("Lines added here have no earlier history");
    if let (true, Some((path, mode))) = (b.on_hover_text("Commits that changed these lines (git log -L), following renames").clicked(), range) {
        acts.push(Action::FileView(path, mode));
    }
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

pub fn show(ui: &mut Ui, ds: &mut DiffState, d: Option<&crate::repo::RepoData>, split: &mut bool, wrap: &mut bool, acts: &mut Vec<Action>) {
    let (src, ds_opts) = (ds.src.clone(), ds.opts);
    let change = src.change().clone();
    let (worktree, staged) = match &ds.src {
        DiffSrc::Worktree { staged, .. } => (true, *staged),
        DiffSrc::Commit { .. } => (false, false),
    };
    let lossy = ds.file.as_ref().is_some_and(|f| f.as_ref().is_ok_and(|f| f.lossy || f.whole_only));
    // Partial staging patches the shown diff, which must be a plain (no -w), current one.
    // An untracked file's diff is never filtered, and only staged from (intent-to-add first).
    // A commit's hunks and lines revert / cherry-pick into the working tree (not a working-tree compare's).
    let untracked = change.code == '?';
    let wt_compare = matches!(&ds.src, DiffSrc::Commit { id, .. } if id == crate::git::WORKTREE);
    let partial = !lossy && !ds.loading() && if worktree { untracked || !ds.opts.ignore_ws } else { !ds.opts.ignore_ws && !wt_compare };
    let file = ds.file.as_ref().and_then(|f| f.as_ref().ok());
    let lay = Layout::get(ui, ds.stamp, file, *split);
    let Layout { rows, kinds, starts, pairs, max_chars, .. } = &*lay;
    // Scroll offset and viewport height of the last frame.
    let mem = ui.id().with("diff_pos");
    let (first, view_h) = ui.data(|m| m.get_temp::<(usize, f32)>(mem)).unwrap_or_default();
    let anchor = first + LEAD;
    let mut nav = ui.input(|i| i.key_pressed(egui::Key::F7).then_some(!i.modifiers.shift));

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        acts.push(Action::CloseDiff);
    }
    let narrow = ui.available_width() < 600.0;
    let used_id = ui.id().with("diff_actions_w");
    if ds.picked.is_empty() {
        ui.data_mut(|m| m.remove::<f32>(used_id));
    }
    let used = ui.data(|m| m.get_temp::<f32>(used_id));
    let short = narrow || used.is_some_and(|u| u > ui.available_width());
    let stacked = narrow || used.is_some_and(|u| u + TITLE_MIN_W > ui.available_width());
    let title = |ui: &mut Ui, acts: &mut Vec<Action>| {
        if ui.button(icon::X).on_hover_text("Close (Esc)").clicked() {
            acts.push(Action::CloseDiff);
        }
        if let Some(o) = &change.old_path {
            ui.label(RichText::new(format!("{o} →")).monospace().color(theme::dim(ui)));
        }
        ui.label(RichText::new(&change.path).monospace().strong());
    };
    if stacked {
        theme::bar(ui, |ui| theme::rest(ui, |ui| title(ui, acts)));
    }
    let actions = |ui: &mut Ui| {
        if worktree && change.code != 'D' && theme::pill(ui, icon::CODE).on_hover_text("Open in editor").clicked() {
            acts.push(Action::Editor(change.path.clone()));
        }
        if worktree && !matches!(change.code, 'D' | 'U') && theme::pill(ui, if narrow { icon::PENCIL_SIMPLE.to_string() } else { format!("{}  Edit", icon::PENCIL_SIMPLE) }).on_hover_text(format!("Edit this file here ({} saves, ⌘Z undoes)", keys("⌘S"))).clicked() {
            acts.push(Action::EditFile(change.path.clone()));
        }
        if let Some(op) = crate::repo::tools::difftool_op(&src) {
            let tool = d.map_or(Err(String::new()), |d| crate::repo::tools::tool(d, true));
            let b = ui.add_enabled(tool.is_ok(), egui::Button::new(icon::GIT_DIFF).frame_when_inactive(false).corner_radius(255));
            let b = b.on_hover_text(format!("Open in external diff tool ({}); other actions keep working while it is open", tool.as_deref().unwrap_or_default()));
            if b.on_disabled_hover_text(tool.err().unwrap_or_default()).clicked() {
                acts.push(Action::Git(op));
            }
        }
        if !worktree || (change.code != '?' && change.code != 'A') {
            // A commit's file is blamed at that commit (a working-tree compare's in the working
            // tree); a deleted one isn't there to blame.
            let rev = match &src {
                DiffSrc::Commit { id, .. } => (change.code != 'D').then(|| (!wt_compare).then(|| id.clone())),
                DiffSrc::Worktree { .. } => Some(None),
            };
            if let Some(rev) = rev
                && theme::pill(ui, icon::USER_LIST).on_hover_text(if rev.is_some() { "Blame at this commit" } else { "Blame" }).clicked()
            {
                acts.push(Action::FileView(change.path.clone(), Mode::Blame(rev)));
            }
            if theme::pill(ui, icon::CLOCK_COUNTER_CLOCKWISE).on_hover_text("File history").clicked() {
                acts.push(Action::FileView(change.path.clone(), Mode::History));
            }
        }
        if worktree {
            if staged {
                if ui.button("Unstage file").clicked() {
                    acts.push(Action::Git(crate::repo::unstage_op(&change)));
                }
            } else if ui.button("Stage file").clicked() {
                acts.extend(crate::repo::stage_files_op("Stage", std::slice::from_ref(&change)).map(Action::Git));
            }
        }
        if worktree && change.code != '?' && ds.opts.ignore_ws {
            ui.label(RichText::new(if narrow { icon::INFO.to_string() } else { format!("{}  Line staging off", icon::INFO) }).small().color(theme::MOD))
                .on_hover_text("Hunks and lines can't be staged from a whitespace-ignoring diff: turn off Ignore whitespace to stage parts of this file.");
        } else if let (true, Some(f)) = (worktree, file.filter(|f| f.whole_only || f.lossy)) {
            let (note, why) = match f.whole_only {
                true => ("git converts it on add", "git add converts this file (CRLF line endings, an LFS or other filter), so the lines shown aren't the ones staged: stage the whole file."),
                false => ("not UTF-8", "This file isn't valid UTF-8, so its lines are shown approximately and can't be staged one by one: stage the whole file."),
            };
            ui.label(RichText::new(if narrow { icon::INFO.to_string() } else { format!("{}  Line staging off: {note}", icon::INFO) }).small().color(theme::MOD)).on_hover_text(why);
        } else if !worktree && !wt_compare && ds.opts.ignore_ws {
            ui.label(RichText::new(if narrow { icon::INFO.to_string() } else { format!("{}  Hunk actions off", icon::INFO) }).small().color(theme::MOD))
                .on_hover_text("Hunks and lines can't be reverted or cherry-picked from a whitespace-ignoring diff: turn off Ignore whitespace.");
        }
        if let (false, Some(f), Some(&first)) = (worktree, file, ds.picked.first())
            && let Some((path, mode)) = line_range(&src, f, &ds.picked, first)
            && ui.button(if short { format!("{} {}", icon::CLOCK_COUNTER_CLOCKWISE, ds.picked.len()) } else { format!("{}  History of {} line(s)", icon::CLOCK_COUNTER_CLOCKWISE, ds.picked.len()) }).on_hover_text("Commits that changed the picked lines (git log -L)").clicked()
        {
            acts.push(Action::FileView(path, mode));
        }
        // Change lines only: a commit diff also picks context lines (for line history).
        let n = file.map_or(0, |f| ds.picked.iter().filter(|&&(h, l)| f.hunks.get(h).and_then(|h| h.lines.get(l)).is_some_and(|l| matches!(l.kind, b'+' | b'-'))).count());
        if let (true, Some(f)) = (partial && n > 0, file) {
            let pick = |h: usize, l: usize| ds.picked.contains(&(h, l));
            if !worktree {
                if ui.button(if short { format!("Revert {n}") } else { format!("Revert {n} line(s)") }).on_hover_text(keys(REVERT_HINT)).clicked() {
                    acts.push(apply_op("Revert lines", partial_patch(f, pick, true), &["--reverse"], &src, ds_opts, f));
                }
                if ui.button(if short { format!("Cherry-pick {n}") } else { format!("Cherry-pick {n} line(s)") }).on_hover_text(keys(PICK_HINT)).clicked() {
                    acts.push(apply_op("Cherry-pick lines", partial_patch(f, pick, false), &[], &src, ds_opts, f));
                }
            } else if staged {
                if ui.button(if short { format!("Unstage {n}") } else { format!("Unstage {n} line(s)") }).clicked() {
                    acts.push(apply_op("Unstage lines", partial_patch(f, pick, true), &["--cached", "--reverse"], &src, ds_opts, f));
                }
            } else {
                if !untracked && ui.button(if short { format!("Discard {n}") } else { format!("Discard {n} line(s)") }).clicked() {
                    acts.push(discard("Discard lines", f, pick, &src, ds_opts));
                }
                if ui.button(if short { format!("Stage {n}") } else { format!("Stage {n} line(s)") }).clicked() {
                    acts.push(stage("Stage lines", f, pick, &src, ds_opts));
                }
            }
        }
        // Links to the picked lines' new side (removed lines have none); only for a plain commit, not
        // a working-tree compare or a stash (`<sha>^3` for its untracked files), never on the remote.
        if let (DiffSrc::Commit { id, .. }, Some(f), Some(d)) = (&src, file, d)
            && id.bytes().all(|b| b.is_ascii_hexdigit())
            && !d.stashes.iter().any(|s| s.id == *id)
        {
            let nums = ds.picked.iter().map(|&(h, l)| f.hunks[h].lines[l].new).filter(|&n| n > 0);
            if let (Some(a), Some(b)) = (nums.clone().min(), nums.max()) {
                let page = Page::File { rev: id, path: &change.path, lines: Some((a, b)) };
                crate::ui::links::items(ui, d, &[(if a == b { "line" } else { "lines" }, page)], &[id], acts);
            }
        }
        if !short {
            ui.data_mut(|m| m.insert_temp(used_id, ui.min_rect().width()));
        }
        if !stacked {
            theme::rest(ui, |ui| title(ui, acts));
        }
    };
    if narrow {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min).with_main_wrap(true), actions);
    } else {
        theme::bar(ui, actions);
    }
    if ds.image.is_none() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 30.0; // the glass groups draw beyond their row height
            view_controls(ui, ds, split, wrap, worktree && change.code != '?', starts, anchor, &mut nav, acts)
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
    if let Some((lines, size)) = f.large {
        return large_note(ui, lines, size, ds.pending, acts);
    }
    if let Some(sm) = &f.submodule {
        return submodule_note(ui, &change.path, sm, acts);
    }
    if let Some((old, new)) = &f.lfs {
        return lfs_note(ui, old.as_ref(), new.as_ref());
    }
    if let Some(img) = &ds.image {
        return super::image_diff::show(ui, img);
    }
    let mode = f.mode_change().map(|(a, b)| format!("Mode changed {a} → {b}"));
    if f.binary {
        let size = |n| crate::ui::format::bytes(n);
        let what = match f.sizes {
            Some((Some(o), Some(n))) => format!("Binary file changed ({} → {})", size(o), size(n)),
            Some((None, Some(n))) => format!("Binary file added ({})", size(n)),
            Some((Some(o), None)) => format!("Binary file deleted (was {})", size(o)),
            _ => "Binary file".into(),
        };
        let text = match mode {
            Some(m) => format!("{what}\n{m}"),
            None => what,
        };
        ui.centered_and_justified(|ui| ui.label(text));
        return;
    }
    if f.hunks.is_empty() {
        let text = match (mode, ds.opts.ignore_ws) {
            (Some(m), _) => m,
            (None, true) => "Only whitespace changed".into(),
            (None, false) => "No changes".into(),
        };
        ui.centered_and_justified(|ui| ui.label(RichText::new(text).color(theme::dim(ui))));
        return;
    }
    if let Some(m) = mode {
        ui.label(RichText::new(format!("{}  {m}", icon::INFO)).small().color(theme::MOD));
    }

    let area = ui.available_rect_before_wrap();
    let rail = Rect::from_x_y_ranges(area.right() - RAIL_W..=area.right(), area.top() + 2.0..=area.bottom() - 2.0);
    let body = Rect::from_x_y_ranges(area.left()..=rail.left() - 4.0, area.y_range());
    let cw = ui.fonts_mut(|fo| fo.glyph_width(&FontId::monospace(12.5), 'M'));
    // Wrapped: columns of text per line (the scrollbar's width is left out); rows get as tall as their lines.
    let cols = |text_w: f32| if *wrap { ((text_w / cw) as usize).max(1) } else { usize::MAX };
    let (cols_u, cols_s) = (cols(body.width() - 14.0 - NUM_W * 2.0 - 16.0), cols(body.width() / 2.0 - 7.0 - NUM_W - 16.0));
    let n_lines = |hi: usize, li: usize, cols| wrapped(|| crate::highlight::display_len(&f.hunks[hi].lines[li].text), cols);
    let tops_id = ui.id().with("diff_tops");
    let tops_key = (ds.stamp, *split, cols_u, cols_s);
    let tops = match ui.data(|m| m.get_temp::<std::sync::Arc<((u64, bool, usize, usize), Vec<f32>)>>(tops_id)).filter(|t| t.0 == tops_key) {
        Some(t) => t,
        None => {
            let t = std::sync::Arc::new((
                tops_key,
                tops(
                    LINE_H,
                    rows.iter().map(|r| match *r {
                        Row::Hunk(_) => 1,
                        Row::Line(hi, li) => n_lines(hi, li, cols_u),
                        Row::Pair(hi, a, b) => [a, b].into_iter().flatten().map(|li| n_lines(hi, li, cols_s)).max().unwrap_or(1),
                    }),
                ),
            ));
            ui.data_mut(|m| m.insert_temp(tops_id, t.clone()));
            t
        }
    };
    let tops = &tops.1;
    let last = rows.len() - 1;
    // Fractional row at a scroll offset, for the minimap.
    let row_f = |y: f32| {
        let i = tops.partition_point(|&t| t <= y).saturating_sub(1).min(last);
        i as f32 + (y - tops[i]) / (tops[i + 1] - tops[i])
    };
    let per = (rail.height() / rows.len() as f32).min(LINE_H);
    let row_at = |y: f32| (((y - rail.top()) / per) as usize).min(last);
    let rail_resp = ui.interact(rail, mem.with("rail"), Sense::click_and_drag()).on_hover_text("Changes in this file: click to jump");
    let mut target = rail_resp.interact_pointer_pos().filter(|_| rail_resp.clicked() || rail_resp.dragged()).map(|p| tops[row_at(p.y)] - view_h / 2.0);
    if let Some(s) = nav.and_then(|fwd| step(starts, anchor, fwd)) {
        target = Some(tops[s.saturating_sub(LEAD)]);
    }
    let span = |hi: usize, li: usize| {
        let other = pairs[hi][li]?;
        changed_span(&expand(&f.hunks[hi].lines[li].text), &expand(&f.hunks[hi].lines[other].text))
    };
    let mono = FontId::monospace(12.5);
    let out = ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let mut area = egui::ScrollArea::both().id_salt("diff").auto_shrink(false);
        if let Some(t) = target {
            area = area.vertical_scroll_offset(t.max(0.0));
        }
        area.show_viewport(ui, |ui, view| {
            let range = visible(tops, view.min.y, view.max.y);
            let (view_w, pin) = (view.width(), view.min.x);
            let text_w = if *split { view_w / 2.0 - NUM_W - 16.0 } else { view_w - NUM_W * 2.0 - 20.0 };
            let overflow = if *wrap { 0.0 } else { (*max_chars as f32 * cw - text_w).max(0.0) };
            ui.set_min_size(vec2(view_w + overflow, tops[last + 1]));
            let left = ui.max_rect().left() + pin;
            let top = ui.max_rect().top();
            let painter = ui.painter().clone();
            for r in range {
                let rect = Rect::from_min_size(pos2(left, top + tops[r]), vec2(view_w, tops[r + 1] - tops[r]));
                let y = rect.top() + LINE_H / 2.0;
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
                            if !worktree {
                                if btn(ui, "Cherry-pick hunk") {
                        acts.push(apply_op("Cherry-pick hunk", partial_patch(f, all, false), &[], &src, ds_opts, f));
                                }
                                if btn(ui, "Revert hunk") {
                        acts.push(apply_op("Revert hunk", partial_patch(f, all, true), &["--reverse"], &src, ds_opts, f));
                                }
                            } else if staged {
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
                        let pick = l.kind != b'\\' && ((partial && l.kind != b' ') || !worktree);
                        let resp = ui.interact(rect, ui.id().with(("dl", r)), if pick { Sense::click_and_drag() } else { Sense::click() });
                        // Commit diffs pick any line, for "History of these lines".
                        if pick {
                            pick_lines(ui, mem, &mut ds.picked, rows, tops, f, &resp, r, None, (hi, li));
                        }
                        resp.context_menu(|ui| line_menu(ui, &src, f, &ds.picked, (hi, li), acts));
                        let colors = ds.hl.get(hi).and_then(|h| h.get(li));
                        paint_line(ui, &painter, rect, l.kind, l.old, l.new, &l.text, picked, &mono, true, span(hi, li), (cw, cols_u, pin), colors);
                    }
                    Row::Pair(hi, a, b) => {
                        let h = &f.hunks[hi];
                        let half = rect.width() / 2.0;
                        let lr = Rect::from_min_size(rect.min, vec2(half, rect.height()));
                        let rr = Rect::from_min_size(pos2(rect.left() + half, rect.top()), vec2(half, rect.height()));
                        for (side, idx, old) in [(lr, a, true), (rr, b, false)] {
                            let Some(li) = idx else {
                                painter.rect_filled(side, 0.0, theme::faint(ui).gamma_multiply(0.5));
                                continue;
                            };
                            let l = &h.lines[li];
                            let pick = l.kind != b'\\' && ((partial && l.kind != b' ') || !worktree);
                            let resp = ui.interact(side, ui.id().with(("sl", r, old)), if pick { Sense::click_and_drag() } else { Sense::click() });
                            if pick {
                                pick_lines(ui, mem, &mut ds.picked, rows, tops, f, &resp, r, Some(old), (hi, li));
                            }
                            resp.context_menu(|ui| line_menu(ui, &src, f, &ds.picked, (hi, li), acts));
                            let no = if old { l.old } else { l.new };
                            let colors = ds.hl.get(hi).and_then(|h| h.get(li));
                            let picked = ds.picked.contains(&(hi, li));
                            paint_line(ui, &painter.with_clip_rect(side), side, l.kind, no, 0, &l.text, picked, &mono, false, span(hi, li), (cw, cols_s, pin), colors);
                        }
                    }
                }
            }
        })
    });
    let (off, view_h) = (out.inner.state.offset.y, out.inner.inner_rect.height());
    ui.data_mut(|m| m.insert_temp(mem, (row_f(off) as usize, view_h)));
    minimap(ui, rail, kinds, row_f(off), row_f(off + view_h));
}

/// Change lines of a row; a split row gives only the `old` (left) or new side.
fn row_changes(f: &FileDiff, row: &Row, old: Option<bool>) -> Vec<(usize, usize)> {
    let at = |hi: usize, li: usize| matches!(f.hunks[hi].lines[li].kind, b'+' | b'-').then_some((hi, li));
    match *row {
        Row::Hunk(_) => vec![],
        Row::Line(hi, li) => at(hi, li).into_iter().collect(),
        Row::Pair(hi, a, b) => [(true, a), (false, b)].into_iter().filter(|(o, _)| old.is_none_or(|s| s == *o)).filter_map(|(_, li)| at(hi, li?)).collect(),
    }
}

/// Click toggles a change line; dragging from one selects (or, starting on a picked line,
/// deselects) every change line down to the pointer, relative to the picks at the drag's start.
#[allow(clippy::too_many_arguments)]
fn pick_lines(ui: &Ui, mem: egui::Id, picked: &mut std::collections::BTreeSet<(usize, usize)>, rows: &[Row], tops: &[f32], f: &FileDiff, resp: &egui::Response, r: usize, old: Option<bool>, at: (usize, usize)) {
    if resp.clicked() && !picked.remove(&at) {
        picked.insert(at);
    }
    let key = mem.with("drag");
    if resp.drag_started() {
        ui.data_mut(|m| m.insert_temp(key, (picked.clone(), !picked.contains(&at))));
    }
    if let (true, Some(p), Some((base, add))) = (resp.dragged(), resp.interact_pointer_pos(), ui.data(|m| m.get_temp::<(std::collections::BTreeSet<(usize, usize)>, bool)>(key))) {
        // Rows are as tall as their wrapped lines: find the pointer's row by the row tops.
        let y = tops[r] + p.y - resp.rect.top();
        let end = tops.partition_point(|&t| t <= y).saturating_sub(1).min(rows.len() - 1);
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
fn view_controls(ui: &mut Ui, ds: &DiffState, split: &mut bool, wrap: &mut bool, stageable: bool, starts: &[usize], anchor: usize, nav: &mut Option<bool>, acts: &mut Vec<Action>) {
    let o = ds.pending;
    wrap_group(ui, "layout", |ui| {
        for (v, text) in [(false, "Unified"), (true, "Split")] {
            if ui.add(egui::Button::selectable(*split == v, text)).clicked() {
                *split = v;
            }
        }
    });
    ui.add_space(6.0);
    wrap_group(ui, "wrap", |ui| {
        if ui.add(egui::Button::selectable(*wrap, format!("{}  Wrap", icon::ARROW_ELBOW_DOWN_LEFT))).on_hover_text("Wrap long lines instead of scrolling sideways").clicked() {
            *wrap = !*wrap;
        }
    });
    ui.add_space(6.0);
    wrap_group(ui, "ws", |ui| {
        let hint = if stageable { "Ignore whitespace changes (git diff -w).\nLine and hunk staging are off while this is on." } else { "Ignore whitespace changes (git diff -w)" };
        if ui.add(egui::Button::selectable(o.ignore_ws, format!("{}  Ignore whitespace", icon::PARAGRAPH))).on_hover_text(hint).clicked() {
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
                if prev.on_hover_text(keys("Previous change (⇧F7)")).clicked() {
                    *nav = Some(false);
                }
            });
            let n = starts.len();
            ui.label(RichText::new(format!("{n} change{}", plural(n))).small().color(theme::dim(ui)));
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
/// `v0..v1`: the viewport, in (fractional) rows.
fn minimap(ui: &Ui, rail: Rect, kinds: &[u8], v0: f32, v1: f32) {
    // Short diffs map 1:1 to their lines; long ones are squeezed into the rail.
    let per = (rail.height() / kinds.len().max(1) as f32).min(LINE_H);
    let p = ui.painter();
    p.rect_filled(rail, 4.0, theme::faint(ui));
    let y = |row: f32| rail.top() + row * per;
    let (v0, v1) = (y(v0), y(v1).min(rail.bottom()));
    p.rect_filled(Rect::from_x_y_ranges(rail.x_range(), v0..=v1.max(v0 + 6.0)), 4.0, theme::hover(ui).gamma_multiply(2.5));
    let h = per.max(2.0);
    let mut last = f32::NEG_INFINITY;
    for (r, &k) in kinds.iter().enumerate().filter(|(_, k)| **k != 0) {
        let top = y(r as f32);
        if top - last < 1.0 {
            continue;
        }
        last = top;
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
            ui.label(RichText::new(format!("{}  {title}", icon::PACKAGE)).strong());
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
            ui.label(RichText::new(format!("{}  Working copy {d}", icon::PENCIL_SIMPLE)).color(theme::MOD));
        }
        ui.add_space(6.0);
        if !zero(&sm.to) && ui.button(format!("{}  Open submodule in new tab", icon::ARROW_SQUARE_OUT)).clicked() {
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
        ui.label(RichText::new(format!("{}  Git LFS object, {size}", icon::DATABASE)).strong());
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

/// A large diff is withheld until asked for: nothing is parsed or highlighted before.
fn large_note(ui: &mut Ui, lines: usize, size: usize, o: DiffOpts, acts: &mut Vec<Action>) {
    note_frame(ui, |ui| {
        ui.label(RichText::new(format!("{}  Large diff — {lines} lines, {}", icon::WARNING, crate::ui::format::bytes(size as u64))).strong());
        ui.add_space(4.0);
        ui.label(RichText::new("Showing it can be slow.").small().color(theme::dim(ui)));
        ui.add_space(6.0);
        if ui.button("Load anyway").clicked() {
            acts.push(Action::DiffOpts(DiffOpts { all: true, ..o }));
        }
    });
}

/// `cw`: width of a character; `cols`: columns to wrap the text at (`usize::MAX`: don't).
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
    (cw, cols, pin): (f32, usize, f32),
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
    let y = rect.top() + LINE_H / 2.0;
    let num = |n: u32| if n == 0 { String::new() } else { n.to_string() };
    let dim = theme::dim(ui);
    let mut x = rect.left() + NUM_W - 6.0;
    p.text(pos2(x, y), Align2::RIGHT_CENTER, num(a), FontId::monospace(11.0), dim);
    if two_nums {
        x += NUM_W;
        p.text(pos2(x, y), Align2::RIGHT_CENTER, num(b), FontId::monospace(11.0), dim);
    }
    x += 8.0;
    let code = p.with_clip_rect(Rect::from_min_max(pos2(x + 12.0, rect.top()), rect.max).intersect(p.clip_rect()));
    let sign = match kind {
        b'+' => "+",
        b'-' => "-",
        b'\\' => "\\",
        _ => " ",
    };
    p.text(pos2(x, y), Align2::LEFT_CENTER, sign, font.clone(), sign_color);
    if let Some((s, e)) = span {
        // Word-level emphasis on the part of the line that actually changed, row by wrapped row.
        for vr in s / cols..=(e - 1) / cols {
            let (c0, c1) = (if vr == s / cols { s % cols } else { 0 }, if vr == (e - 1) / cols { (e - 1) % cols + 1 } else { cols });
            let top = rect.top() + vr as f32 * LINE_H + 1.0;
            let r = Rect::from_min_max(pos2(x + 14.0 - pin + c0 as f32 * cw, top), pos2(x + 14.0 - pin + c1 as f32 * cw, top + LINE_H - 2.0));
            code.rect_filled(r, 3.0, sign_color.gamma_multiply(0.35));
        }
    }
    // The "\ No newline at end of file" marker is a note, not code.
    let note = kind == b'\\';
    let shown = if note { text.trim_start().to_owned() } else { expand(text) };
    let mut job = match colors.filter(|c| !c.is_empty() && !note) {
        Some(spans) => {
            let mut job = egui::text::LayoutJob::default();
            for (c, r) in spans {
                if let Some(s) = shown.get(r.clone()) {
                    job.append(s, 0.0, egui::TextFormat { line_height: Some(LINE_H), ..egui::TextFormat::simple(font.clone(), *c) });
                }
            }
            job
        }
        None => egui::text::LayoutJob::single_section(shown, egui::TextFormat { line_height: Some(LINE_H), ..egui::TextFormat::simple(font.clone(), if note { dim } else { theme::text(ui) }) }),
    };
    super::format::wrap_job(&mut job, cols, cw);
    let g = code.layout_job(job);
    code.galley(pos2(x + 14.0 - pin, rect.top()), g, theme::text(ui));
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

    /// "No newline at end of file" is shown (under its side) instead of dropped.
    #[test]
    fn no_newline_marker_rows() {
        let d = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n";
        let f = &parse_diff(d)[0];
        assert_eq!(rows(f, false).len(), 1 + 5);
        // Split: each marker sits on its own side, in the row after its line.
        let split = rows(f, true);
        let kinds: Vec<u8> = split.iter().map(|r| row_kind(f, r)).collect();
        assert_eq!(split.len(), 1 + 1 + 2);
        assert_eq!(kinds, [0, 0, b'~', 0]);
        assert!(matches!(split[3], Row::Pair(0, Some(2), Some(4))));
    }

    #[test]
    fn line_history_range() {
        let d = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n";
        let f = &parse_diff(d)[0];
        let change = crate::git::Change { path: "f".into(), old_path: Some("e".into()), code: 'R' };
        let commit = DiffSrc::Commit { id: "c1".into(), parent: Some("p1".into()), change: change.clone() };
        let lines = |rev: &str, start, end| Mode::Lines { rev: Some(rev.into()), start, end };
        // Picked lines 0 (" a") and 4 (" d"): their new-side span at the commit.
        let picked: BTreeSet<_> = [(0, 0), (0, 4)].into();
        assert_eq!(line_range(&commit, f, &picked, (0, 4)), Some(("f".into(), lines("c1", 1, 4))));
        // Right-click outside the picks: just that line.
        assert_eq!(line_range(&commit, f, &picked, (0, 3)), Some(("f".into(), lines("c1", 3, 3))));
        // Only a removed line: the parent's version, under the old name.
        assert_eq!(line_range(&commit, f, &picked, (0, 1)), Some(("e".into(), lines("p1", 2, 2))));
        // Compared with the working tree: the base's side.
        let wt = DiffSrc::Commit { id: crate::git::WORKTREE.into(), parent: Some("p1".into()), change: change.clone() };
        assert_eq!(line_range(&wt, f, &picked, (0, 3)), Some(("e".into(), lines("p1", 3, 3))));
        let staged = DiffSrc::Worktree { change: change.clone(), staged: true };
        assert_eq!(line_range(&staged, f, &picked, (0, 4)), Some(("e".into(), lines("HEAD", 1, 4))));
        let unstaged = DiffSrc::Worktree { change, staged: false };
        assert_eq!(line_range(&unstaged, f, &picked, (0, 2)), None); // an added line
        assert_eq!(line_range(&unstaged, f, &picked, (0, 3)), Some(("e".into(), Mode::Lines { rev: None, start: 3, end: 3 })));
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
