//! Glass widgets: the lit canvas behind everything, glass panels and cards,
//! capsule groups and pills, the primary button, the transparency slider and
//! the progress caption, empty states.

use super::{Flavor, Palette, accent, dark, dim, flavor};
use egui::epaint::{Mesh, Shadow};
use egui::{Color32, CornerRadius, Painter, Pos2, Rect, RichText, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

/// A round picture (avatar) of radius `r` at `c`, faded by `alpha`.
pub fn photo(p: &Painter, c: Pos2, r: f32, tex: &egui::TextureHandle, alpha: f32) {
    let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
    p.add(egui::epaint::RectShape::filled(Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r)), r, Color32::WHITE.gamma_multiply(alpha)).with_texture(tex.id(), uv));
}

/// A radial glow from `col` at `c` to transparent at radius `r`. With `round`
/// = (rect, corner radius) the glow is trimmed to that rounded rectangle, its
/// fade kept (painter clipping is rectangular only).
fn radial(mesh: &mut Mesh, mut c: Pos2, r: f32, col: Color32, round: Option<(Rect, f32)>) {
    if let Some((b, rad)) = round {
        // Trimming walks rays from the centre, so the centre must be inside:
        // a glow anchored past the edge moves onto it.
        c = pos2(c.x.min(b.max.x - 0.5).max(b.min.x + 0.5), c.y.min(b.max.y - 0.5).max(b.min.y + 0.5)); // not clamp: b may be tiny
        while !inside_rounded(b, rad, c) {
            c = c.lerp(b.center(), 0.1);
        }
    }
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(c, col);
    let n = 64;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let rim = c + vec2(a.cos(), a.sin()) * r;
        match round.filter(|&(b, rad)| !inside_rounded(b, rad, rim)) {
            // Walk back along the ray to the edge; the colour there is the
            // gradient's value at that distance.
            Some((b, rad)) => {
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..20 {
                    let m = (lo + hi) / 2.0;
                    if inside_rounded(b, rad, c + (rim - c) * m) { lo = m } else { hi = m }
                }
                mesh.colored_vertex(c + (rim - c) * lo, col.gamma_multiply(1.0 - lo));
            }
            None => mesh.colored_vertex(rim, Color32::TRANSPARENT),
        }
    }
    for i in 0..n {
        mesh.add_triangle(base, base + 1 + i, base + 2 + i);
    }
}

/// The lit backdrop the glass floats on. Over a native glass backdrop only a
/// translucent tint is painted so the desktop shows through.
pub fn canvas(p: &Painter, rect: Rect, backdrop: bool) {
    canvas_in(p, rect, 0.0, backdrop, flavor());
}

fn inside_rounded(b: Rect, rad: f32, p: Pos2) -> bool {
    let q = ((p - b.center()).abs() - (b.size() / 2.0 - vec2(rad, rad))).max(vec2(0.0, 0.0));
    b.expand(0.01).contains(p) && q.length() <= rad
}

/// A flavour's backdrop, with rounded corners when `radius` > 0 (the Settings swatches).
pub fn canvas_in(p: &Painter, rect: Rect, radius: f32, backdrop: bool, f: Flavor) {
    let Palette { base, blobs, .. } = f.palette();
    // Over the native glass backdrop the canvas itself becomes see-through too.
    let canvas_alpha = if backdrop { 0.55 * (1.0 - transparency()) } else { 1.0 };
    p.rect_filled(rect, radius, base.gamma_multiply(canvas_alpha));
    let mut mesh = Mesh::default();
    let s = rect.width().max(rect.height());
    let round = (radius > 0.0).then_some((rect, radius));
    for ((x, y), r, [cr, cg, cb, ca]) in blobs {
        radial(&mut mesh, rect.min + vec2(x * rect.width(), y * rect.height()), r * s, Color32::from_rgba_unmultiplied(cr, cg, cb, ca), round);
    }
    p.add(mesh);
}

/// User-chosen glass transparency, 0.0 (solid) ..= 1.0 (clearest), as f32 bits.
static TRANSPARENCY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_transparency(t: f32) {
    TRANSPARENCY.store(t.clamp(0.0, 1.0).to_bits(), std::sync::atomic::Ordering::Relaxed);
}

fn transparency() -> f32 {
    f32::from_bits(TRANSPARENCY.load(std::sync::atomic::Ordering::Relaxed))
}

/// Scale a glass alpha by the transparency setting; never below 15% so text stays legible.
fn see_through(alpha: u8) -> u8 {
    (alpha as f32 * (1.0 - 0.85 * transparency())).round() as u8
}

/// Glass panel: soft shadow, translucent body, specular sheen, lit rim.
/// `opacity` 0..=255 controls how much of the canvas shows through.
pub fn glass_shapes(rect: Rect, radius: f32, dark: bool, opacity: u8) -> Vec<Shape> {
    let opacity = see_through(opacity);
    let cr = CornerRadius::same(radius.min(255.0) as u8);
    let mut v = vec![];
    v.push(
        Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 34 }) }
            .as_shape(rect, cr)
            .into(),
    );
    let [r, g, b] = flavor().palette().glass;
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
        for (x, alpha) in [(l, 0), (m, a), (r, 0)] {
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
    ui.add(egui::Button::new(text).frame_when_inactive(false).corner_radius(255).wrap_mode(egui::TextWrapMode::Extend))
}

/// 0..=1 slider with a solid macOS-style knob (egui's knob shares the
/// translucent widget fill with the rail).
pub fn slider(ui: &mut Ui, value: &mut f32) -> egui::Response {
    let r = ui.add(egui::Slider::new(value, 0.0..=1.0).show_value(false).trailing_fill(true));
    let rect = r.rect;
    let hr = rect.height() / 2.5;
    let x = egui::lerp((rect.left() + hr)..=(rect.right() - hr), value.clamp(0.0, 1.0));
    let c = pos2(x, rect.center().y);
    let p = ui.painter();
    p.circle_filled(c + vec2(0.0, 1.0), hr + 1.0, Color32::from_black_alpha(60));
    p.circle(c, hr, Color32::WHITE, Stroke::new(0.5, Color32::from_black_alpha(40)));
    r
}

/// Accent-tinted primary action.
pub fn primary(ui: &mut Ui, enabled: bool, text: &str, width: f32) -> egui::Response {
    let b = egui::Button::new(egui::RichText::new(text).strong().color(super::on_color(accent())))
        .fill(if enabled { accent() } else { accent().gamma_multiply(0.35) })
        .corner_radius(255)
        .min_size(vec2(width, 32.0))
        .wrap_mode(egui::TextWrapMode::Extend);
    ui.add_enabled(enabled, b)
}

/// Glass card filling the remaining space, with content inset by its radius.
pub fn card<R>(ui: &mut Ui, opacity: u8, add: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect();
    glass(ui, rect, 18.0, opacity);
    let mut inner = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(16.0, 12.0))));
    inner.set_clip_rect(rect.shrink(1.0));
    let r = add(&mut inner);
    ui.allocate_rect(rect, egui::Sense::hover()); // content that doesn't fit is clipped: it never widens the panel
    r
}

/// A one-line row laid out from the right: add the trailing controls first
/// (rightmost first), then call [`rest`] for the leading part. Rows built left
/// to right let a long path or name push the controls off-screen or widen the
/// panel (a panel sizes itself to its content, so it can even grow every frame).
pub fn bar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    bar_h(ui, ui.spacing().interact_size.y + 6.0, add)
}

/// A [`bar`] of a given height (toolbars of glass groups are taller than a button).
pub fn bar_h<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), height));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let clip = child.clip_rect();
    child.set_clip_rect(clip.intersect(egui::Rect::from_x_y_ranges(rect.x_range(), clip.y_range())));
    child.set_min_height(height);
    add(&mut child)
}

/// The leading part of a [`bar`]: left to right in the width the controls left
/// free; its text is cut with "…" (labels show the full text on hover).
pub fn rest<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        add(ui)
    })
    .inner
}

/// Compact button (header actions such as "AI message", "Stage all", "+ Add
/// remote…"): lower than a normal button, with roomy side padding. egui's small
/// buttons drop vertical padding, so the height comes from a minimum size. The
/// label stays on one line; a wrapping row moves the whole button down instead.
pub fn small<'a>(ui: &mut Ui, enabled: bool, text: impl egui::IntoAtoms<'a>) -> egui::Response {
    let old = ui.spacing().button_padding.x;
    ui.spacing_mut().button_padding.x = 18.0;
    let r = ui.add_enabled(enabled, egui::Button::new(text).small().min_size(egui::vec2(0.0, 24.0)).wrap_mode(egui::TextWrapMode::Extend));
    ui.spacing_mut().button_padding.x = old;
    r
}

/// A calm, centered empty state: a large accent glyph, a title and one line of help.
/// Callers add their buttons below it.
/// A commit description field: four rows tall, longer text scrolls inside it.
pub fn description(ui: &mut Ui, text: &mut String, id: egui::Id) {
    let v = ui.visuals();
    let stroke = if ui.memory(|m| m.has_focus(id)) { v.selection.stroke } else { Stroke::new(v.selection.stroke.width, Color32::TRANSPARENT) };
    let frame = egui::Frame::NONE.fill(v.text_edit_bg_color()).stroke(stroke).corner_radius(v.widgets.inactive.corner_radius).inner_margin(super::INPUT_MARGIN);
    frame.show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).max_height(ui.text_style_height(&egui::TextStyle::Body) * 4.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(text).id(id).frame(egui::Frame::NONE).hint_text("Description").desired_rows(4).desired_width(f32::INFINITY));
        });
    });
}

/// A right-click menu. One taller than the room above or below the click starts at the top
/// of the window instead, uses its full height and scrolls the rest, with an arrow at the
/// edge that has more.
pub fn context_menu<R>(resp: &egui::Response, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let ctx = resp.ctx.clone();
    let (at_id, h_id) = (resp.id.with("menu_at"), resp.id.with("menu_h"));
    if resp.secondary_clicked()
        && let Some(p) = ctx.pointer_interact_pos()
    {
        ctx.data_mut(|d| d.insert_temp(at_id, p));
    }
    let screen = ctx.content_rect().shrink(6.0);
    let tall = ctx.data(|d| d.get_temp::<f32>(h_id)).unwrap_or(0.0);
    let mut popup = egui::Popup::context_menu(resp);
    if let Some(p) = ctx.data(|d| d.get_temp::<Pos2>(at_id)).filter(|p| tall > (screen.bottom() - p.y).max(p.y - screen.top()))
    {
        popup = popup.anchor(pos2(p.x, screen.top())).align(egui::RectAlign::BOTTOM_START).gap(0.0);
    }
    popup
        .show(|ui| {
            let margin = ui.spacing().menu_margin.sum().y;
            ui.set_max_height(screen.height() - margin);
            let o = egui::ScrollArea::vertical().max_height(screen.height() - margin).show(ui, add);
            ctx.data_mut(|d| d.insert_temp(h_id, o.content_size.y + margin));
            let (view, off) = (o.inner_rect, o.state.offset.y);
            for (more, y, glyph) in [(off > 1.0, view.top(), super::icon::CARET_UP), (off + view.height() < o.content_size.y - 1.0, view.bottom(), super::icon::CARET_DOWN)] {
                if more {
                    let band = Rect::from_center_size(pos2(view.center().x, y), vec2(view.width(), 16.0)).intersect(view.expand2(vec2(0.0, 2.0)));
                    ui.painter().rect_filled(band, 4.0, ui.visuals().window_fill.to_opaque());
                    ui.painter().text(band.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(12.0), super::dim(ui));
                }
            }
            o.inner
        })
        .map(|r| r.inner)
}

pub fn empty_state(ui: &mut Ui, glyph: &str, title: &str, help: &str) {
    ui.label(RichText::new(glyph).size(34.0).color(accent().gamma_multiply(0.85)));
    ui.add_space(2.0);
    ui.label(RichText::new(title).size(16.5).strong());
    ui.add(egui::Label::new(RichText::new(help).color(dim(ui))).wrap());
    ui.add_space(6.0);
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
