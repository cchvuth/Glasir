//! File history and blame views (blame with an age heatmap). The data and git
//! queries live in `crate::repo::history`.

use super::format::{ago, initials, relative_time};
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
pub fn show(ui: &mut Ui, fv: &mut FileView, acts: &mut Vec<Action>) -> bool {
    fv.poll();
    let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));
    ui.horizontal(|ui| {
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        let title = match fv.mode {
            Mode::History => format!("{} History", icon::CLOCK_COUNTER_CLOCKWISE),
            Mode::Blame => format!("{} Blame", icon::USER_LIST),
        };
        ui.label(RichText::new(title).strong());
        ui.add(egui::Label::new(RichText::new(&fv.path).monospace().color(theme::dim(ui))).truncate());
    });
    ui.add_space(6.0);
    let data = match &fv.data {
        None => {
            ui.centered_and_justified(|ui| ui.spinner());
            return close;
        }
        Some(Err(e)) => {
            ui.colored_label(theme::DEL, e);
            return close;
        }
        Some(Ok(d)) => d,
    };
    match data {
        Data::History(entries) => {
            egui::ScrollArea::vertical().id_salt("file_history").auto_shrink(false).show(ui, |ui| {
                for (i, e) in entries.iter().enumerate() {
                    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
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
                    if e.change.old_path.is_some() {
                        p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, format!("{} renamed", icon::ARROW_RIGHT), FontId::proportional(11.0), theme::REN);
                    }
                    if r.clicked() {
                        fv.sel = Some(i);
                        acts.push(Action::Diff(DiffSrc::Commit { id: e.id.clone(), parent: e.parent.clone(), change: e.change.clone() }));
                        acts.push(Action::Jump(e.id.clone()));
                    }
                }
                if entries.is_empty() {
                    ui.label(RichText::new("No history (file not committed yet).").color(theme::dim(ui)));
                }
            });
        }
        Data::Blame(b) => blame_view(ui, b, acts),
    }
    close
}

fn blame_view(ui: &mut Ui, b: &Blame, acts: &mut Vec<Action>) {
    const H: f32 = 18.0;
    let mono = FontId::monospace(12.5);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::both().id_salt("blame").auto_shrink(false).show_rows(ui, H, b.lines.len(), |ui, range| {
            let max = b.lines.iter().map(|l| l.1.len()).max().unwrap_or(0) as f32 * 7.6 + 420.0;
            ui.set_min_width(max.max(ui.available_width()));
            let left = ui.max_rect().left();
            let top = ui.max_rect().top() - range.start as f32 * H;
            let p = ui.painter().clone();
            for r in range {
                let (ci, text) = &b.lines[r];
                let c = &b.commits[*ci];
                let rect = Rect::from_min_size(pos2(left, top + r as f32 * H), vec2(max, H));
                let y = rect.center().y;
                let heat = heat_color(b.heat[*ci]);
                let first = r == 0 || b.lines[r - 1].0 != *ci;
                p.rect_filled(Rect::from_min_size(rect.min, vec2(4.0, H)), 0.0, heat);
                if first {
                    p.line_segment([pos2(left, rect.top()), pos2(left + max, rect.top())], egui::Stroke::new(1.0, theme::faint(ui)));
                    let uncommitted = c.id.bytes().all(|x| x == b'0');
                    let who = if uncommitted { "Uncommitted".to_owned() } else { format!("{}  {}", &c.id[..7], c.author) };
                    let clip = p.with_clip_rect(Rect::from_min_size(rect.min, vec2(250.0, H)));
                    clip.text(pos2(left + 10.0, y), Align2::LEFT_CENTER, who, FontId::proportional(11.5), heat);
                    let when = if c.time > 0 { ago(c.time) } else { String::new() };
                    p.text(pos2(left + 330.0, y), Align2::RIGHT_CENTER, when, FontId::proportional(11.0), theme::dim(ui));
                }
                let resp = ui.interact(Rect::from_min_size(rect.min, vec2(340.0, H)), ui.id().with(("blame", r)), Sense::click());
                if resp.clicked() && !c.id.bytes().all(|x| x == b'0') {
                    acts.push(Action::Jump(c.id.clone()));
                }
                resp.on_hover_text(format!("{}\n{} — {}", c.summary, c.author, ago(c.time)));
                p.text(pos2(left + 380.0, y), Align2::RIGHT_CENTER, (r + 1).to_string(), FontId::monospace(11.0), theme::dim(ui));
                p.text(pos2(left + 392.0, y), Align2::LEFT_CENTER, text.replace('\t', "    "), mono.clone(), theme::text(ui));
            }
        });
    });
}
