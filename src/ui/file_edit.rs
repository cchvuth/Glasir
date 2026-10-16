//! In-place editor for a changed working-tree file, opened from its diff: line
//! numbers, syntax colors, wrap. State and the save op live in `repo::file_edit`.

use super::format::keys;
use crate::highlight;
use crate::repo::file_edit::{FileEdit, HIGHLIGHT_MAX, text_key};
use crate::repo::Action;
use crate::theme::{self, icon};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Key, Modifiers, RichText, TextEdit, TextStyle, Ui, vec2};
use std::path::Path;

pub fn show(ui: &mut Ui, ed: &mut FileEdit, dir: &Path, wrap: &mut bool, acts: &mut Vec<Action>) {
    ed.check(dir);
    // ⌘⇧S is the global stage-all; consume_key ignores the extra Shift.
    let save = ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::COMMAND, Key::S));
    let dirty = ed.dirty();
    let text_id = ui.id().with(("file_edit_text", dir, &ed.path));
    let mut leave = ed.ask.is_none() && super::format::esc_pressed_in(ui, text_id);
    if save && dirty && !ed.saving {
        acts.push(Action::EditSave);
    }
    theme::bar(ui, |ui| {
        let b = theme::primary(ui, dirty && !ed.saving, "Save", 80.0).on_hover_text(keys("⌘S"));
        if b.clicked() {
            acts.push(Action::EditSave);
        }
        if ui.button("Cancel").on_hover_text("Back to the diff (Esc)").clicked() {
            leave = true;
        }
        if ui.add(egui::Button::selectable(*wrap, format!("{}  Wrap", icon::ARROW_ELBOW_DOWN_LEFT))).on_hover_text("Wrap long lines instead of scrolling sideways").clicked() {
            *wrap = !*wrap;
        }
        if ed.crlf() {
            ui.label(RichText::new("CRLF").small().color(theme::dim(ui))).on_hover_text("Windows line endings are kept when saving");
        }
        theme::rest(ui, |ui| {
            ui.label(RichText::new(format!("{}  Edit", icon::PENCIL_SIMPLE)).strong());
            ui.label(RichText::new(if dirty { format!("{} •", ed.path) } else { ed.path.clone() }).monospace().color(theme::dim(ui)));
        });
    });
    if leave {
        if dirty {
            ed.ask = Some(None);
        } else {
            acts.push(Action::EditClose);
        }
    }
    if ed.conflict {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{}  {} changed on disk since you opened it.", icon::WARNING, ed.path)).color(theme::changed()));
            if ui.button("Reload").on_hover_text("Throw away your edits and load the file again").clicked() {
                match FileEdit::load(dir, &ed.path) {
                    Ok(n) => *ed = n,
                    Err(e) => acts.extend([Action::Error(e), Action::EditClose]),
                }
            }
            if ui.button("Overwrite").on_hover_text("Save your version over the file as it is now (⌘Z brings the other one back)").clicked() {
                ed.overwrite(dir);
                acts.push(Action::EditSave);
            }
            if ui.button("Keep editing").clicked() {
                ed.conflict = false;
            }
        });
    }
    ui.add_space(6.0);

    let mono = TextStyle::Monospace.resolve(ui.style());
    let char_w = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    let row_h = ui.fonts_mut(|f| f.row_height(&mono));
    let lines = ed.text.matches('\n').count() + 1;
    let gutter = (lines.to_string().len().max(3) as f32 + 2.0) * char_w;
    let (dark, plain, dim) = (ui.visuals().dark_mode, ui.visuals().text_color(), theme::dim(ui));
    let wrapped = *wrap;
    let rows = ((ui.available_height() / row_h) as usize).max(4);
    let path = ed.path.clone();
    let hl = &mut ed.hl;
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, width: f32| {
        let text = buf.as_str();
        // Colors are recomputed per edit, so only for files that stay quick.
        let spans = (text.len() <= HIGHLIGHT_MAX).then(|| {
            let key = (text_key(text), dark);
            if hl.as_ref().is_none_or(|h| h.0 != key) {
                *hl = Some((key, highlight::text(&path, text, dark)));
            }
            &hl.as_ref().unwrap().1
        });
        let fmt = |color| TextFormat { font_id: mono.clone(), color, ..Default::default() };
        let mut job = LayoutJob::default();
        match spans {
            Some(s) if !s.is_empty() => s.iter().for_each(|(c, r)| job.append(&text[r.clone()], 0.0, fmt(*c))),
            _ => job.append(text, 0.0, fmt(plain)),
        }
        job.wrap.max_width = if wrapped { width } else { f32::INFINITY };
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let area = if wrapped { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    area.id_salt("file_edit").auto_shrink(false).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (g, _) = ui.allocate_exact_size(vec2(gutter, 0.0), egui::Sense::hover());
            let out = TextEdit::multiline(&mut ed.text)
                .id(text_id)
                .font(TextStyle::Monospace)
                .frame(egui::Frame::NONE)
                .interactive(!ed.saving) // text typed while the save runs would be lost
                .lock_focus(true)
                .desired_rows(rows)
                .desired_width(if wrapped { ui.available_width() } else { f32::INFINITY })
                .layouter(&mut layouter)
                .show(ui);
            let clip = ui.clip_rect();
            let mut n = 0;
            let mut start = true;
            let p = ui.painter();
            for r in &out.galley.rows {
                if start {
                    n += 1;
                    let y = out.galley_pos.y + r.pos.y;
                    if y + row_h >= clip.top() && y <= clip.bottom() {
                        p.text(egui::pos2(g.right() - char_w, y), Align2::RIGHT_TOP, n, mono.clone(), dim);
                    }
                }
                start = r.ends_with_newline;
            }
            if ui.memory(|m| m.focused().is_none() && m.top_modal_layer().is_none()) && ed.ask.is_none() && !ed.conflict && !ui.ctx().input(|i| i.pointer.any_down()) {
                out.response.request_focus();
            }
        });
    });

    let mut done = None;
    if ed.ask.is_some() {
        egui::Modal::new(egui::Id::new("file_edit_ask")).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.heading("Unsaved changes");
            ui.label(format!("Save your edits to {} first?", ed.path));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if theme::primary(ui, !ed.saving, "Save", 80.0).clicked() {
                    done = Some(0);
                }
                if ui.add(egui::Button::new(RichText::new("Discard").color(theme::del()))).clicked() {
                    done = Some(1);
                }
                if super::format::dismissed(ui, "Keep editing") {
                    done = Some(2);
                }
            });
        });
    }
    match done {
        Some(0) => {
            ed.then = ed.ask.take().flatten();
            acts.push(Action::EditSave);
        }
        Some(1) => acts.push(Action::EditClose),
        Some(_) => ed.ask = None,
        None => {}
    }
}
