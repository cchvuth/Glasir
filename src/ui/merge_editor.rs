//! Built-in 3-way merge conflict editor: current and incoming sides next to each other, an editable
//! output below. It works on the working file's conflict markers, so hand edits to the output stay
//! the source of truth. State and parsing live in `crate::repo::merge_editor`.

use crate::ai;
use crate::repo::merge_editor::{Chunk, Editor, Row, Take, conflicts, parse, parts, rebuild, save, stage, whole_ops};
use crate::repo::{Action, Task};
use crate::theme::{self, icon};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText, Sense, TextEdit, Ui, pos2, vec2};

const CUR: Color32 = Color32::from_rgb(0x0a, 0x84, 0xff);
const INC: Color32 = theme::MOD;

fn tint(c: Color32, strong: bool) -> Color32 {
    c.gamma_multiply(if strong { 0.32 } else { 0.14 })
}

/// Returns true when the editor should close.
pub fn show(ui: &mut Ui, ed: &mut Editor, ai: Option<&ai::Job>, acts: &mut Vec<Action>) -> bool {
    if let Some(d) = Task::poll(&mut ed.task) {
        ed.output = String::from_utf8_lossy(&d.original).into_owned();
        ed.data = Some(d);
        ed.reparse();
    }
    if let Some(r) = Task::poll(&mut ed.saving) {
        match r {
            Ok(()) => {
                acts.push(Action::Git(stage(&ed.path)));
                return true;
            }
            Err(e) => ed.error = Some(e),
        }
    }
    let mut close = false;
    let mut auto = false;
    let thinking = ai.is_some_and(|j| j.running() && j.purpose == ai::Purpose::Resolve(ed.path.clone()));
    theme::bar(ui, |ui| {
        if ed.data.as_ref().is_some_and(|d| d.whole.is_none()) {
            if !ai::enabled() {
            } else if thinking {
                ui.label(RichText::new("Claude is resolving…").small().color(theme::dim(ui)));
                ui.spinner();
            } else if theme::pill(ui, format!("{} Resolve with AI", icon::SPARKLE)).on_hover_text("Let Claude write the output; review it, then save").clicked() {
                acts.push(Action::Ai(ai::Purpose::Resolve(ed.path.clone())));
            }
            let n = conflicts(&ed.chunks).filter(|c| c.trivial()).count();
            let b = ui.add_enabled(n > 0, egui::Button::new(format!("{} Auto-resolve trivial ({n})", icon::MAGIC_WAND)).frame_when_inactive(false).corner_radius(255));
            if b.on_hover_text("Resolve conflicts whose sides are identical or differ only in whitespace (keeps current)").clicked() {
                auto = true;
            }
        }
        theme::rest(ui, |ui| {
            if theme::pill(ui, icon::X).on_hover_text("Close without saving (the conflict markers stay)").clicked() {
                close = true;
            }
            ui.label(RichText::new(format!("{} Resolve conflict", icon::GIT_MERGE)).strong());
            ui.label(RichText::new(&ed.path).monospace().color(theme::dim(ui)));
        });
    });
    ui.add_space(6.0);
    let Some(d) = &ed.data else {
        ui.centered_and_justified(|ui| ui.spinner());
        return close;
    };

    if let Some(why) = d.whole {
        ui.add_space(8.0);
        ui.label(format!("{why} Pick the version to keep:"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for ((op, label), c) in whole_ops(&ed.path, d).into_iter().zip(&d.labels).zip([CUR, INC]) {
                let b = egui::Button::new(RichText::new(format!("{} · {label}", op.label)).strong().color(c)).corner_radius(255);
                if ui.add(b).clicked() {
                    acts.push(Action::Git(op));
                    close = true;
                }
            }
        });
        return close;
    }

    let m = ed.orig_of.len();
    let sel_orig = ed.orig_of.get(ed.sel).copied().flatten();
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&mono));
    let char_w = ui.fonts_mut(|f| f.glyph_width(&mono, 'M'));
    let footer = 52.0;
    let bar = 36.0;
    let top_h = ((ui.available_height() - footer - bar - 40.0) * 0.45).max(80.0);
    let scroll = std::mem::take(&mut ed.scroll);
    let mut pick: Option<(usize, Take)> = None;
    let mut select = None;

    ui.columns(2, |cols| {
        for (s, ui) in cols.iter_mut().enumerate() {
            let (title, color) = if s == 0 { ("Current", CUR) } else { ("Incoming", INC) };
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} {title}", icon::CIRCLE)).strong().color(color));
                ui.add(egui::Label::new(RichText::new(&d.labels[s]).small().color(theme::dim(ui))).truncate());
            });
            let rows = &d.sides[s];
            let mut area = egui::ScrollArea::both().id_salt(("merge_side", s)).max_height(top_h).auto_shrink(false);
            if scroll && let Some(k) = sel_orig {
                let at = rows.iter().position(|r| matches!(r, Row::Head(h) if *h == k)).unwrap_or(0);
                area = area.vertical_scroll_offset((at as f32 - 2.0).max(0.0) * row_h);
            }
            egui::Frame::new().fill(theme::faint(ui)).corner_radius(10).inner_margin(6).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0; // contiguous conflict tints; rows exactly `row_h` apart
                area.show_rows(ui, row_h, rows.len(), |ui, range| {
                    let w = (d.width[s] as f32 * char_w + 24.0).max(ui.available_width());
                    for row in &rows[range] {
                        let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
                        let p = ui.painter();
                        match row {
                            Row::Head(k) => {
                                let i = ed.orig_of.iter().position(|o| *o == Some(*k));
                                p.rect_filled(rect, 4.0, tint(color, sel_orig == Some(*k)));
                                let what = if i.is_some() { format!("Conflict {}", k + 1) } else { format!("{} Conflict {} resolved", icon::CHECK, k + 1) };
                                p.text(pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, what, FontId::proportional(11.5), color);
                                if let Some(i) = i {
                                    let take = if s == 0 { Take::Current } else { Take::Incoming };
                                    // Painted, not a widget: a widget would push the next rows down.
                                    let br = egui::Rect::from_min_size(pos2(rect.left() + 96.0, rect.top() + 1.0), vec2(46.0, row_h - 2.0));
                                    let b = ui.interact(br, ui.id().with(("take", s, k)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    p.rect_filled(br, 255.0, color.gamma_multiply(if b.hovered() { 0.6 } else { 0.3 }));
                                    p.text(br.center(), egui::Align2::CENTER_CENTER, "Take", FontId::proportional(11.5), theme::text(ui));
                                    if b.on_hover_text(format!("Take {} for this conflict", title.to_lowercase())).clicked() {
                                        pick = Some((i, take));
                                    }
                                }
                            }
                            Row::Line(text, at) => {
                                if let Some(k) = at {
                                    p.rect_filled(rect, 0.0, tint(color, sel_orig == Some(*k)));
                                    if resp.clicked() {
                                        select = ed.orig_of.iter().position(|o| o == &Some(*k));
                                    }
                                }
                                p.text(pos2(rect.left() + 4.0, rect.center().y), egui::Align2::LEFT_CENTER, text, mono.clone(), theme::text(ui));
                            }
                        }
                    }
                });
            });
        }
    });

    ui.add_space(6.0);
    let cur = conflicts(&ed.chunks).nth(ed.sel).cloned();
    ui.horizontal_wrapped(|ui| {
        theme::group(ui, |ui| {
            if m == 0 {
                ui.label(RichText::new(format!("{} No conflicts left", icon::CHECK_CIRCLE)).color(theme::ADD));
                return;
            }
            if theme::pill(ui, icon::CARET_UP).on_hover_text("Previous conflict").clicked() {
                select = Some((ed.sel + m - 1) % m);
            }
            ui.label(RichText::new(format!("Conflict {} of {m}", ed.sel + 1)).strong());
            if theme::pill(ui, icon::CARET_DOWN).on_hover_text("Next conflict").clicked() {
                select = Some((ed.sel + 1) % m);
            }
        });
        if let Some(c) = &cur {
            ui.add_space(6.0);
            theme::group(ui, |ui| {
                let mut b = |ui: &mut Ui, text: RichText, t: Take, hint: &str| {
                    if theme::pill(ui, text).on_hover_text(hint).clicked() {
                        pick = Some((ed.sel, t));
                    }
                };
                b(ui, RichText::new("Take current").color(CUR), Take::Current, &d.labels[0]);
                b(ui, RichText::new("Take incoming").color(INC), Take::Incoming, &d.labels[1]);
                b(ui, RichText::new("Both (current first)"), Take::CurrentFirst, "Current lines, then incoming lines");
                b(ui, RichText::new("Both (incoming first)"), Take::IncomingFirst, "Incoming lines, then current lines");
                if c.base.is_some() {
                    b(ui, RichText::new("Take base").color(theme::dim(ui)), Take::Base, "The common ancestor's version");
                }
            });
        }
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label(RichText::new("Output").strong());
        ui.label(RichText::new("editable — this is what gets saved").small().color(theme::dim(ui)));
    });
    let mut area = egui::ScrollArea::both().id_salt("merge_out").max_height((ui.available_height() - footer).max(60.0)).auto_shrink(false);
    if scroll && m > 0 {
        area = area.vertical_scroll_offset((ed.output_line(ed.sel) as f32 - 2.0).max(0.0) * row_h);
    }
    let text_col = theme::text(ui);
    let dim = theme::dim(ui);
    let size = d.size;
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        // Re-parses per layout; fine for source files, cache by hash if huge files matter.
        let mut job = LayoutJob::default();
        let fmt = |color: Color32, background: Color32| TextFormat { font_id: mono.clone(), color, background, ..Default::default() };
        for c in parse(buf.as_str(), size) {
            match c {
                Chunk::Text(t) => job.append(&t, 0.0, fmt(text_col, Color32::TRANSPARENT)),
                Chunk::Conflict(c) => {
                    for (part, l) in parts(&c.raw, size) {
                        let format = match part {
                            0 => fmt(text_col, tint(CUR, false)),
                            1 => fmt(dim, Color32::TRANSPARENT),
                            2 => fmt(text_col, tint(INC, false)),
                            _ => fmt(theme::DEL, tint(theme::DEL, false)),
                        };
                        job.append(l, 0.0, format);
                    }
                }
            }
        }
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let changed = egui::Frame::new()
        .fill(theme::faint(ui))
        .corner_radius(10)
        .inner_margin(6)
        .show(ui, |ui| {
            area.show(ui, |ui| {
                let te = TextEdit::multiline(&mut ed.output).code_editor().frame(egui::Frame::NONE.inner_margin(crate::theme::INPUT_MARGIN)).min_size(ui.available_size()).layouter(&mut layouter);
                ui.add_enabled(!thinking, te).changed()
            })
            .inner
        })
        .inner;

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let saving = ed.saving.is_some();
        let label = if saving { "Saving…".to_owned() } else { format!("{} Save & mark resolved", icon::CHECK) };
        let r = theme::primary(ui, m == 0 && !saving && !thinking, &label, 200.0);
        let r = r.on_disabled_hover_text(format!("Resolve the {m} remaining conflict(s) first, or close without saving"));
        if r.clicked() {
            let (dir, path, original, out) = (ed.dir.clone(), ed.path.clone(), d.original.clone(), ed.output.clone());
            ed.error = None;
            ed.saving = Some(Task::spawn(ui.ctx(), move || save(&dir, &path, &original, &out)));
        }
        if ui.button("Close without saving").on_hover_text("The conflict markers stay in the file").clicked() {
            close = true;
        }
        if let Some(e) = &ed.error {
            ui.colored_label(theme::DEL, e);
        }
    });

    if changed {
        ed.reparse();
        ed.scroll = false; // don't jump while typing
    }
    if let Some((i, t)) = pick {
        ed.resolve(i, t);
    }
    if auto {
        ed.output = rebuild(&ed.chunks, |_, c| c.trivial().then_some(Take::Current));
        ed.reparse();
    }
    if let Some(i) = select {
        ed.sel = i;
        ed.scroll = true;
    }
    close
}
