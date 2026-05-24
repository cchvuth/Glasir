//! Image diff: before / after side by side, or stacked with a swipe divider.
//! Added or deleted images show one preview.

use crate::git::Res;
use crate::repo::image_diff::{ImageDiff, Side};
use crate::theme::{self, icon};
use crate::ui::format::bytes;
use egui::{Color32, Painter, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

const UV: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

fn dims(s: &Side) -> String {
    format!("{}×{}", s.size[0], s.size[1])
}

/// `size` scaled to fit `area` (up to 2×), centred.
fn fit(size: [u32; 2], area: Rect) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let s = (area.width() / w).min(area.height() / h).min(2.0);
    Rect::from_center_size(area.center(), vec2(w * s, h * s))
}

fn checker(ui: &Ui, p: &Painter, r: Rect) {
    let (a, b) = if ui.visuals().dark_mode { (Color32::from_gray(52), Color32::from_gray(40)) } else { (Color32::from_gray(238), Color32::from_gray(214)) };
    p.rect_filled(r, 0.0, a);
    const S: f32 = 10.0;
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    for (j, y) in (0..).map(|j| (j, r.top() + j as f32 * S)).take_while(|(_, y)| *y < r.bottom()) {
        for x in (0..).map(|i| r.left() + i as f32 * S).take_while(|x| *x < r.right()).skip(j % 2).step_by(2) {
            p.rect_filled(Rect::from_min_size(pos2(x, y), vec2(S, S)), 0.0, b);
        }
    }
}

fn picture(ui: &Ui, s: &Side, area: Rect) -> Rect {
    let r = fit(s.size, area);
    checker(ui, ui.painter(), r);
    ui.painter().image(s.tex.id(), r, UV, Color32::WHITE);
    r
}

/// One side in its own column: caption, then the picture (or why there is none).
fn column(ui: &mut Ui, title: &str, side: &Res<Side>, rect: Rect) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        match side {
            Ok(s) => {
                ui.label(RichText::new(format!("{title}  ·  {}  ·  {}", dims(s), bytes(s.bytes))).small().color(theme::dim(ui)));
                let area = ui.available_rect_before_wrap();
                picture(ui, s, area.shrink(4.0));
            }
            Err(e) => {
                ui.label(RichText::new(title).small().color(theme::dim(ui)));
                ui.centered_and_justified(|ui| ui.label(RichText::new(e).color(theme::dim(ui))));
            }
        }
    });
}

pub fn show(ui: &mut Ui, img: &ImageDiff) {
    let id = ui.id().with("image_diff");
    let (mut swipe, mut at) = ui.data(|m| m.get_temp::<(bool, f32)>(id)).unwrap_or((false, 0.5));
    let pair = match (&img.old, &img.new) {
        (Some(Ok(o)), Some(Ok(n))) => Some((o, n)),
        _ => None,
    };
    ui.horizontal(|ui| {
        if pair.is_some() {
            theme::group(ui, |ui| {
                if ui.add(egui::Button::selectable(!swipe, format!("{} Side by side", icon::COLUMNS))).clicked() {
                    swipe = false;
                }
                if ui.add(egui::Button::selectable(swipe, format!("{} Swipe", icon::ARROWS_OUT_LINE_HORIZONTAL))).clicked() {
                    swipe = true;
                }
            });
            ui.add_space(8.0);
        }
        let text = match (&img.old, &img.new, pair) {
            (_, _, Some((o, n))) => {
                let delta = n.bytes as i64 - o.bytes as i64;
                let sign = if delta < 0 { "−" } else { "+" };
                let size = if dims(o) == dims(n) { dims(n) } else { format!("{} → {}", dims(o), dims(n)) };
                format!("{size}  ·  {} → {} ({sign}{})", bytes(o.bytes), bytes(n.bytes), bytes(delta.unsigned_abs()))
            }
            (None, Some(_), _) => "Added image".into(),
            (Some(_), None, _) => "Deleted image".into(),
            _ => String::new(),
        };
        ui.label(RichText::new(text).color(theme::dim(ui)));
    });
    ui.add_space(6.0);
    let area = ui.available_rect_before_wrap();
    match (&img.old, &img.new, pair) {
        (_, _, Some((o, n))) if swipe => {
            // One frame for both: the old image left of the divider, the new one right of it.
            let r = fit([o.size[0].max(n.size[0]), o.size[1].max(n.size[1])], area.shrink2(vec2(8.0, 24.0)));
            let resp = ui.interact(area, id.with("swipe"), Sense::click_and_drag());
            if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
                at = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
            }
            let x = r.left() + at * r.width();
            let p = ui.painter();
            checker(ui, p, r);
            for (s, clip) in [(o, Rect::from_x_y_ranges(r.left()..=x, r.y_range())), (n, Rect::from_x_y_ranges(x..=r.right(), r.y_range()))] {
                p.with_clip_rect(clip).image(s.tex.id(), fit(s.size, r), UV, Color32::WHITE);
            }
            p.vline(x, r.y_range(), Stroke::new(2.0, Color32::WHITE));
            p.circle(pos2(x, r.center().y), 9.0, theme::accent(), Stroke::new(2.0, Color32::WHITE));
            let cap = |t: &str, x: f32, a| p.text(pos2(x, r.top() - 12.0), a, t, egui::FontId::proportional(11.5), theme::dim(ui));
            cap("Before", r.left(), egui::Align2::LEFT_CENTER);
            cap("After", r.right(), egui::Align2::RIGHT_CENTER);
            resp.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        }
        (Some(o), Some(n), _) => {
            let half = (area.width() - 12.0) / 2.0;
            column(ui, "Before", o, Rect::from_min_size(area.min, vec2(half, area.height())));
            column(ui, "After", n, Rect::from_min_size(pos2(area.left() + half + 12.0, area.top()), vec2(half, area.height())));
        }
        (Some(s), None, _) => column(ui, "Before", s, area),
        (None, Some(s), _) => column(ui, "After", s, area),
        (None, None, _) => {}
    }
    ui.data_mut(|m| m.insert_temp(id, (swipe, at)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_scales_down_and_caps_upscaling() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        assert_eq!(fit([400, 100], area).size(), vec2(200.0, 50.0));
        assert_eq!(fit([10, 10], area).size(), vec2(20.0, 20.0));
        assert_eq!(fit([10, 10], area).center(), area.center());
    }
}
