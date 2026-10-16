//! File, folder and line-range history and blame views (blame with an age
//! heatmap), with back / forward. The data and git queries live in `crate::repo::history`.

use crate::ui::format::keys;
use super::format::{ago, initials, relative_time, text_job, tops, visible, wrapped};
use crate::repo::history::{Blame, Data, FileView, Mode};
use crate::repo::{Action, DiffSrc};
use crate::theme::{self, icon};
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

/// Cool blue (old) → warm orange (new).
fn heat_color(t: f32) -> Color32 {
    let (a, b) = ([0x3d as f32, 0x6e as f32, 0xff as f32], [0xff as f32, 0x7a as f32, 0x45 as f32]);
    let l = |i: usize| (a[i] + (b[i] - a[i]) * t) as u8;
    Color32::from_rgb(l(0), l(1), l(2))
}

/// Draws the file view; returns true when it should close.
pub fn show(ui: &mut Ui, fv: &mut FileView, wrap: &mut bool, acts: &mut Vec<Action>) -> bool {
    fv.poll();
    let mut close = super::format::esc_pressed(ui);
    let mut nav = None;
    if !super::format::modal_open(ui.ctx()) {
        nav = ui.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            (i.consume_key(cmd, egui::Key::OpenBracket).then_some(false)).or(i.consume_key(cmd, egui::Key::CloseBracket).then_some(true))
        });
    }
    let folder = fv.path.ends_with('/');
    ui.horizontal(|ui| {
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        if !fv.back.is_empty() && theme::pill(ui, icon::ARROW_LEFT).on_hover_text(keys("Back (⌘[)")).clicked() {
            nav = Some(false);
        }
        if !fv.fwd.is_empty() && theme::pill(ui, icon::ARROW_RIGHT).on_hover_text(keys("Forward (⌘])")).clicked() {
            nav = Some(true);
        }
        let title = match &fv.mode {
            Mode::History if folder => format!("{}  Folder history", icon::FOLDER_SIMPLE),
            Mode::History => format!("{}  History", icon::CLOCK_COUNTER_CLOCKWISE),
            Mode::Blame(None) => format!("{}  Blame", icon::USER_LIST),
            Mode::Blame(Some(r)) => format!("{}  Blame at {}", icon::USER_LIST, &r[..r.len().min(7)]),
            Mode::Lines { start, end, .. } if start == end => format!("{}  History of line {start}", icon::CLOCK_COUNTER_CLOCKWISE),
            Mode::Lines { start, end, .. } => format!("{}  History of lines {start}–{end}", icon::CLOCK_COUNTER_CLOCKWISE),
        };
        ui.label(RichText::new(title).strong());
        if matches!(fv.mode, Mode::Blame(_)) && ui.add(egui::Button::selectable(*wrap, format!("{}  Wrap", icon::ARROW_ELBOW_DOWN_LEFT))).on_hover_text("Wrap long lines instead of scrolling sideways").clicked() {
            *wrap = !*wrap;
        }
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut end = 0;
        for seg in fv.path.split_inclusive('/') {
            end += seg.len();
            let text = RichText::new(seg).monospace().color(theme::dim(ui));
            // Folders above the shown file or folder open their history.
            if seg.ends_with('/') && end < fv.path.len() {
                if ui.add(egui::Button::new(text).frame(false)).on_hover_text(format!("History of {}", &fv.path[..end])).clicked() {
                    acts.push(Action::FileView(fv.path[..end].into(), Mode::History));
                }
            } else {
                ui.add(egui::Label::new(text).truncate());
            }
        }
    });
    if let Some(forward) = nav
        && fv.step(ui.ctx(), forward)
    {
        acts.push(Action::CloseDiff);
    }
    ui.add_space(6.0);
    let data = match &fv.data {
        None => {
            ui.centered_and_justified(|ui| ui.spinner());
            return close;
        }
        Some(Err(e)) => {
            ui.colored_label(theme::del(), e);
            return close;
        }
        Some(Ok(d)) => d,
    };
    match data {
        Data::History(entries) => {
            const ROW: f32 = 46.0;
            // Rows as tall as their entry (a line-range entry carries its diff).
            let (id, key) = (egui::Id::new("history_tops"), fv.data_gen);
            let tops = match ui.data(|d| d.get_temp::<std::sync::Arc<(u64, Vec<f32>)>>(id)).filter(|c| c.0 == key) {
                Some(c) => c,
                None => {
                    let c = std::sync::Arc::new((key, tops(1.0, entries.iter().map(|e| hunk_height(&e.hunks) as usize + ROW as usize))));
                    ui.data_mut(|d| d.insert_temp(id, c.clone()));
                    c
                }
            };
            let tops = &tops.1;
            let more = matches!(fv.mode, Mode::History) && entries.len() >= fv.limit;
            let mut load_more = false;
            egui::ScrollArea::vertical().id_salt("file_history").auto_shrink(false).show_viewport(ui, |ui, view| {
                ui.set_min_height(tops[entries.len()] + if more { 40.0 } else { 0.0 });
                let (left, top, width) = (ui.max_rect().left(), ui.max_rect().top(), ui.available_width());
                for i in visible(tops, view.min.y, view.max.y) {
                    let e = &entries[i];
                    let at = top + tops[i];
                    let rect = Rect::from_min_size(pos2(left, at), vec2(width, ROW));
                    let r = ui.interact(rect, ui.id().with(("entry", &e.id)), Sense::click());
                    if fv.sel == Some(i) {
                        ui.painter().rect_filled(rect, 10.0, theme::selection(ui));
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, 10.0, theme::hover(ui));
                    }
                    let p = ui.painter().with_clip_rect(rect);
                    let c = pos2(rect.left() + 20.0, rect.center().y);
                    p.circle_filled(c, 12.0, theme::avatar(&e.email));
                    p.text(c, Align2::CENTER_CENTER, initials(&e.author), FontId::proportional(10.0), Color32::WHITE);
                    p.text(pos2(rect.left() + 42.0, rect.top() + 14.0), Align2::LEFT_CENTER, &e.summary, FontId::proportional(13.5), theme::text(ui));
                    let meta = format!("{}  ·  {}  ·  {}", &e.id[..e.id.len().min(7)], e.author, relative_time(e.time, &e.date));
                    p.text(pos2(rect.left() + 42.0, rect.top() + 33.0), Align2::LEFT_CENTER, meta, FontId::proportional(11.5), theme::dim(ui));
                    if !folder && e.change.old_path.is_some() {
                        let word = if e.change.code == 'C' { "copied" } else { "renamed" };
                        p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, format!("{}  {word}", icon::ARROW_RIGHT), FontId::proportional(11.0), theme::change_color('R'));
                    }
                    if r.clicked() {
                        fv.sel = Some(i);
                        if !folder {
                            acts.push(Action::Diff(DiffSrc::Commit { id: e.id.clone(), parent: e.parent.clone(), change: e.change.clone() }));
                        }
                        acts.push(Action::Jump(e.id.clone()));
                    }
                    if !folder && e.change.code != 'D' {
                        r.context_menu(|ui| {
                            if ui.button(format!("{}  Blame at this commit", icon::USER_LIST)).clicked() {
                                acts.push(Action::FileView(e.change.path.clone(), Mode::Blame(Some(e.id.clone()))));
                            }
                        });
                    }
                    hunks(ui, &e.hunks, Rect::from_min_size(pos2(left, at + ROW), vec2(width, tops[i + 1] - tops[i] - ROW)));
                }
                if entries.is_empty() {
                    ui.label(RichText::new("No history (not committed yet).").color(theme::dim(ui)));
                } else if more {
                    let at = pos2(left + 12.0, top + tops[entries.len()] + 6.0);
                    load_more = ui.put(Rect::from_min_size(at, vec2(160.0, 28.0)), egui::Button::new(format!("Load {} more commits", fv.limit * 3))).clicked();
                }
            });
            if load_more {
                fv.more(ui.ctx());
            }
        }
        Data::Blame(b) => blame_view(ui, b, fv.data_gen, *wrap, acts),
    }
    close
}

/// Height of a line-range history entry's change under its row (0 for none).
fn hunk_height(hunks: &[crate::git::Hunk]) -> f32 {
    let n = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind != b'\\').count();
    if n == 0 { 0.0 } else { n as f32 * HUNK_H + 14.0 }
}

const HUNK_H: f32 = 16.0;

/// A line-range history entry's change, in `rect` under its row.
fn hunks(ui: &mut Ui, hunks: &[crate::git::Hunk], rect: Rect) {
    const H: f32 = HUNK_H;
    if rect.height() == 0.0 {
        return;
    }
    let lines = || hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind != b'\\');
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() - 6.0));
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect.shrink2(vec2(8.0, 0.0)), 6.0, theme::faint(ui));
    // Only the lines in view: a big range is thousands of them per entry.
    let clip = ui.clip_rect();
    let first = ((clip.top() - rect.top() - 4.0) / H).floor().max(0.0) as usize;
    let shown = ((clip.bottom() - rect.top() - 4.0) / H).ceil().max(0.0) as usize;
    for (i, l) in lines().enumerate().skip(first).take(shown.saturating_sub(first)) {
        let row = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 4.0 + i as f32 * H), vec2(rect.width() - 16.0, H));
        let (bg, sign) = match l.kind {
            b'+' => (theme::added_bg(ui), "+"),
            b'-' => (theme::removed_bg(ui), "-"),
            _ => (Color32::TRANSPARENT, " "),
        };
        p.rect_filled(row, 0.0, bg);
        let no = if l.new > 0 { l.new } else { l.old };
        p.text(pos2(row.left() + 34.0, row.center().y), Align2::RIGHT_CENTER, no.to_string(), FontId::monospace(10.5), theme::dim(ui));
        let text = format!("{sign} {}", crate::highlight::display(&l.text));
        p.text(pos2(row.left() + 42.0, row.center().y), Align2::LEFT_CENTER, text, FontId::monospace(11.5), theme::text(ui));
    }
}

fn blame_view(ui: &mut Ui, b: &Blame, data_gen: u64, wrap: bool, acts: &mut Vec<Action>) {
    const H: f32 = 18.0;
    let mono = FontId::monospace(12.5);
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono, 'M'));
    let g = if ui.available_width() >= 760.0 { 392.0 } else { 146.0 };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // Wrapped: rows as tall as their lines (the scrollbar's width is left out).
        let cols = if wrap { (((ui.available_width() - g - 14.0) / cw) as usize).max(1) } else { usize::MAX };
        // Measuring every line is O(file): once per file and column count, not per frame.
        let key = (data_gen, cols);
        let id = egui::Id::new("blame_tops");
        let cached = ui.data(|d| d.get_temp::<std::sync::Arc<((u64, usize), Vec<f32>, f32)>>(id)).filter(|c| c.0 == key);
        let cached = cached.unwrap_or_else(|| {
            let chars = |s: &str| s.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
            let tops = tops(H, b.lines.iter().map(|l| wrapped(|| chars(&l.2), cols)));
            let width = b.lines.iter().map(|l| chars(&l.2)).max().unwrap_or(0) as f32 * cw;
            let c = std::sync::Arc::new((key, tops, width));
            ui.data_mut(|d| d.insert_temp(id, c.clone()));
            c
        });
        let (tops, width) = (&cached.1, cached.2 + g + 28.0);
        egui::ScrollArea::both().id_salt("blame").auto_shrink(false).show_viewport(ui, |ui, view| {
            let max = if wrap { 0.0 } else { width };
            ui.set_min_size(vec2(max.max(ui.available_width()), tops[b.lines.len()]));
            let max = view.width();
            let content_left = ui.max_rect().left();
            let left = content_left + view.min.x;
            let top = ui.max_rect().top();
            let p = ui.painter().clone();
            for r in visible(tops, view.min.y, view.max.y) {
                let (ci, _, text) = &b.lines[r];
                let c = &b.commits[*ci];
                let rect = Rect::from_min_size(pos2(left, top + tops[r]), vec2(max, tops[r + 1] - tops[r]));
                let y = rect.top() + H / 2.0;
                let heat = heat_color(b.heat[*ci]);
                let first = r == 0 || b.lines[r - 1].0 != *ci;
                p.rect_filled(Rect::from_min_size(rect.min, vec2(4.0, rect.height())), 0.0, heat);
                if first {
                    p.line_segment([pos2(left, rect.top()), pos2(left + max, rect.top())], egui::Stroke::new(1.0, theme::faint(ui)));
                    let uncommitted = c.id.bytes().all(|x| x == b'0');
                    let who = if uncommitted { "Uncommitted".to_owned() } else { format!("{}  {}", &c.id[..7], c.author) };
                    let clip = p.with_clip_rect(Rect::from_min_size(rect.min, vec2(if g > 200.0 { 250.0 } else { 100.0 }, H)));
                    clip.text(pos2(left + 10.0, y), Align2::LEFT_CENTER, who, FontId::proportional(11.5), heat);
                    let when = if c.time > 0 && g > 200.0 { ago(c.time) } else { String::new() };
                    p.text(pos2(left + g - 62.0, y), Align2::RIGHT_CENTER, when, FontId::proportional(11.0), theme::dim(ui));
                }
                let resp = ui.interact(Rect::from_min_size(rect.min, vec2(g - 52.0, H)), ui.id().with(("blame", r, &c.id)), Sense::click());
                let uncommitted = c.id.bytes().all(|x| x == b'0');
                if resp.clicked() && !uncommitted {
                    acts.push(Action::Jump(c.id.clone()));
                }
                resp.context_menu(|ui| {
                    let parent = ui.add_enabled(c.previous.is_some(), egui::Button::new(format!("{}  Blame at parent revision", icon::ARROW_BEND_UP_RIGHT)));
                    let parent = parent.on_hover_text("Blame the file as it was before this commit, to see who wrote these lines earlier").on_disabled_hover_text("This commit added these lines");
                    if let (true, Some((id, path))) = (parent.clicked(), &c.previous) {
                        acts.push(Action::FileView(path.clone(), Mode::Blame(Some(id.clone()))));
                    }
                    if !uncommitted && ui.button(format!("{}  History of this block", icon::CLOCK_COUNTER_CLOCKWISE)).on_hover_text("Commits that changed these lines (git log -L)").clicked() {
                        // The run of lines from this commit around `r`, in that commit's numbering.
                        let same = |i: &usize| b.lines[*i].0 == *ci;
                        let (lo, hi) = ((0..=r).rev().take_while(same).last().unwrap_or(r), (r..b.lines.len()).take_while(same).last().unwrap_or(r));
                        let nums = (lo..=hi).map(|i| b.lines[i].1);
                        let (start, end) = (nums.clone().min().unwrap_or(1), nums.max().unwrap_or(1));
                        acts.push(Action::FileView(c.file.clone(), Mode::Lines { rev: Some(c.id.clone()), start, end }));
                    }
                });
                resp.on_hover_text(format!("{}\n{} — {}\nRight-click: blame at parent, history of this block", c.summary, c.author, ago(c.time)));
                p.text(pos2(left + g - 12.0, y), Align2::RIGHT_CENTER, (r + 1).to_string(), FontId::monospace(11.0), theme::dim(ui));
                let job = p.layout_job(text_job(&text.replace('\t', "    "), mono.clone(), theme::text(ui), H, cols, cw));
                p.with_clip_rect(Rect::from_min_max(pos2(left + g, rect.top()), rect.max).intersect(p.clip_rect())).galley(pos2(content_left + g, rect.top()), job, theme::text(ui));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Change;
    use crate::repo::history::{BlameCommit, Entry};
    use crate::ui::testkit::{Sim, tab};

    #[test]
    fn a_blame_menu_closes_when_its_line_now_belongs_to_another_commit() {
        let mut sim = Sim::new(520.0, 500.0);
        let t = tab(&sim.ctx, "blame-menu", &["x"]);
        let mut fv = FileView::open(&sim.ctx, &t.dir, "f", Mode::Blame(None));
        crate::repo::testutil::wait_for("the blame", || {
            fv.poll();
            fv.data.is_some()
        });
        let blame = |ids: [&str; 2]| {
            let commits = ids.map(|i| BlameCommit { id: i.repeat(40), author: format!("author{i}"), time: 0, summary: String::new(), file: "f".into(), previous: None }).into();
            Some(Ok(Data::Blame(Blame { commits, lines: vec![(0, 1, "a".into()), (0, 2, "b".into()), (1, 3, "c".into())], heat: vec![0.0, 1.0] })))
        };
        (fv.data, fv.data_gen) = (blame(["1", "2"]), 1);
        let (mut wrap, mut acts) = (false, vec![]);
        sim.open_menu("author2", |ui| _ = show(ui, &mut fv, &mut wrap, &mut acts));
        assert!(egui::Popup::is_any_open(&sim.ctx));
        (fv.data, fv.data_gen) = (blame(["1", "3"]), 2);
        for _ in 0..2 {
            sim.frame(vec![], |ui| _ = show(ui, &mut fv, &mut wrap, &mut acts));
        }
        assert!(!egui::Popup::is_any_open(&sim.ctx), "the menu doesn't turn into the other commit's");
    }

    #[test]
    fn back_or_forward_with_nowhere_to_go_keeps_the_diff() {
        let mut sim = Sim::new(520.0, 500.0);
        let t = tab(&sim.ctx, "history-nav", &["x"]);
        let mut fv = FileView::open(&sim.ctx, &t.dir, "f", Mode::History);
        let (mut wrap, mut acts) = (false, vec![]);
        for key in [egui::Key::OpenBracket, egui::Key::CloseBracket] {
            let press = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
            sim.frame(vec![press], |ui| _ = show(ui, &mut fv, &mut wrap, &mut acts));
        }
        assert!(!acts.iter().any(|a| matches!(a, Action::CloseDiff)));
    }

    #[test]
    fn a_menu_stays_with_its_entry_when_newer_ones_arrive() {
        let mut sim = Sim::new(520.0, 500.0);
        let t = tab(&sim.ctx, "history-menu", &["x"]);
        let mut fv = FileView::open(&sim.ctx, &t.dir, "f", Mode::History);
        crate::repo::testutil::wait_for("the history", || {
            fv.poll();
            fv.data.is_some()
        });
        let entries = |n: i64| {
            let entry = |i: i64| Entry { id: format!("{i:07x}{}", "0".repeat(33)), parent: None, author: "T".into(), email: "t@t".into(), time: i, date: String::new(), summary: format!("summary {i}"), change: Change { path: "f".into(), old_path: None, code: 'M' }, hunks: vec![] };
            Some(Ok(Data::History((0..n).rev().map(entry).collect())))
        };
        (fv.data, fv.data_gen) = (entries(10), 1);
        let (mut wrap, mut acts) = (false, vec![]);
        sim.open_menu("summary 4", |ui| _ = show(ui, &mut fv, &mut wrap, &mut acts));
        (fv.data, fv.data_gen) = (entries(11), 2);
        sim.pick(&format!("{}  Blame at this commit", icon::USER_LIST), |ui| _ = show(ui, &mut fv, &mut wrap, &mut acts));
        let blamed: Vec<_> = acts.iter().filter_map(|a| if let Action::FileView(_, Mode::Blame(Some(id))) = a { Some(id.as_str()) } else { None }).collect();
        assert_eq!(blamed, [format!("0000004{}", "0".repeat(33))]);
    }
}
