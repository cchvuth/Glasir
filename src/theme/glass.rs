//! Glass widgets: the lit canvas behind everything, glass panels and cards,
//! capsule groups and pills, the primary button and the progress caption.
//! Reusable widgets live here.

use super::{Palette, accent, dark, dim, palette};
use egui::epaint::{Mesh, Shadow};
use egui::{Color32, CornerRadius, Painter, Pos2, Rect, RichText, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

fn radial(mesh: &mut Mesh, c: Pos2, r: f32, col: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(c, col);
    let n = 64;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(c + vec2(a.cos(), a.sin()) * r, Color32::TRANSPARENT);
    }
    for i in 0..n {
        mesh.add_triangle(base, base + 1 + i, base + 2 + i);
    }
}

/// The lit backdrop the glass floats on.
pub fn canvas(p: &Painter, rect: Rect) {
    let Palette { base, blobs, .. } = palette();
    p.rect_filled(rect, 0.0, base);
    let mut mesh = Mesh::default();
    let s = rect.width().max(rect.height());
    for ((x, y), r, [cr, cg, cb, ca]) in blobs {
        radial(&mut mesh, rect.min + vec2(x * rect.width(), y * rect.height()), r * s, Color32::from_rgba_unmultiplied(cr, cg, cb, ca));
    }
    p.add(mesh);
}

/// Glass panel: soft shadow, translucent body, specular sheen, lit rim.
/// `opacity` 0..=255 controls how much of the canvas shows through.
pub fn glass_shapes(rect: Rect, radius: f32, dark: bool, opacity: u8) -> Vec<Shape> {
    let cr = CornerRadius::same(radius.min(255.0) as u8);
    let mut v = vec![];
    v.push(
        Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 34 }) }
            .as_shape(rect, cr)
            .into(),
    );
    let [r, g, b] = palette().glass;
    let fill = Color32::from_rgba_unmultiplied(r, g, b, opacity);
    v.push(Shape::rect_filled(rect, cr, fill));
    // Specular sheen: brightest at the top centre, fading down and to the sides.
    let h = (rect.height() * 0.45).min(56.0);
    let inset = radius * 0.7;
    if rect.width() > 2.0 * inset + 4.0 && h > 4.0 {
        let a = if dark { 20 } else { 110 };
        let (l, m, r) = (rect.left() + inset, rect.center().x, rect.right() - inset);
        let (t, b) = (rect.top() + 1.0, rect.top() + h);
        let mut mesh = Mesh::default();
        for (x, alpha) in [(l, a / 3), (m, a), (r, a / 3)] {
            mesh.colored_vertex(pos2(x, t), Color32::from_white_alpha(alpha));
            mesh.colored_vertex(pos2(x, b), Color32::TRANSPARENT);
        }
        for i in [0u32, 2] {
            mesh.add_triangle(i, i + 1, i + 3);
            mesh.add_triangle(i, i + 3, i + 2);
        }
        v.push(mesh.into());
    }
    if !dark {
        v.push(Shape::rect_stroke(rect, cr, Stroke::new(1.0, Color32::from_black_alpha(14)), StrokeKind::Outside));
    }
    v.push(Shape::rect_stroke(rect, cr, Stroke::new(1.0, Color32::from_white_alpha(if dark { 30 } else { 200 })), StrokeKind::Inside));
    // Edge light along the top.
    if rect.width() > 2.0 * radius {
        v.push(Shape::line_segment(
            [pos2(rect.left() + radius, rect.top() + 0.5), pos2(rect.right() - radius, rect.top() + 0.5)],
            Stroke::new(1.0, Color32::from_white_alpha(if dark { 60 } else { 255 })),
        ));
    }
    v
}

pub fn glass(ui: &Ui, rect: Rect, radius: f32, opacity: u8) {
    ui.painter().add(Shape::Vec(glass_shapes(rect, radius, dark(ui), opacity)));
}

/// A glass capsule wrapped around a horizontal group of controls.
pub fn group<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = ui.painter().add(Shape::Noop);
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(4.0);
        let r = add(ui);
        ui.add_space(4.0);
        r
    });
    let rect = inner.response.rect.expand2(vec2(0.0, 4.0));
    ui.painter().set(slot, Shape::Vec(glass_shapes(rect, rect.height() / 2.0, dark(ui), 150)));
    inner.inner
}

/// Frameless capsule button for use inside a `group`.
pub fn pill(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(text).frame_when_inactive(false).corner_radius(255))
}

/// Accent-tinted primary action.
pub fn primary(ui: &mut Ui, enabled: bool, text: &str, width: f32) -> egui::Response {
    let b = egui::Button::new(egui::RichText::new(text).strong().color(Color32::WHITE))
        .fill(if enabled { accent() } else { accent().gamma_multiply(0.35) })
        .corner_radius(255)
        .min_size(vec2(width, 32.0));
    ui.add_enabled(enabled, b)
}

/// Glass card filling the remaining space, with content inset by its radius.
pub fn card<R>(ui: &mut Ui, opacity: u8, add: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect();
    glass(ui, rect, 18.0, opacity);
    let r = ui
        .scope_builder(UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 10.0))), |ui| {
            ui.set_clip_rect(rect.shrink(1.0));
            add(ui)
        })
        .inner;
    ui.expand_to_include_rect(rect); // the panel keeps its full size, not the content's
    r
}

/// Spinner caption, plus git's `--progress` as a slim bar once it reports a percentage.
pub fn progress(ui: &mut Ui, label: &str, line: Option<String>) {
    ui.label(RichText::new(label).color(dim(ui)));
    let Some(line) = line else { return };
    let (text, pct) = crate::git::progress_label(&line);
    if let Some(p) = pct {
        ui.add(egui::ProgressBar::new(p).desired_width(120.0).desired_height(6.0).corner_radius(255));
    }
    ui.add(egui::Label::new(RichText::new(text).small().color(dim(ui))).truncate());
}
