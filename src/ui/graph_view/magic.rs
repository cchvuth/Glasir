//! The default branch's line glimmering (Settings → Main line): a soft light along it that wavers in
//! place, and sparkles twinkling beside it, all in the line's own colour. Stateless: a function of the
//! place in the history and the clock, painted as one mesh under the avatars.

use egui::{Color32, Event, Id, Mesh, Painter, Pos2, Ui, Vec2, pos2, vec2};
use std::f32::consts::{PI, TAU};

/// The glow strip has a row every `STEP` px of history, the glimmer a knot every `KNOT` px and the
/// line a sparkle slot every `SLOT` px.
const STEP: f32 = 9.0;
const KNOT: f32 = 36.0;
const SLOT: f32 = 7.0;
/// Everything repeats every `PERIOD` px of history (a multiple of STEP, KNOT and SLOT): measured from
/// row 0 moved down by whole periods, positions stay small for f32.
const PERIOD: f32 = 81_900.0;
/// Animation stops this long (s) after the last input.
const IDLE: f64 = 60.0;
/// Frames a second while it animates: its motion is slow, and every frame repaints the whole window.
const FPS: f64 = 20.0;
/// The glimmer never dims below this.
const LOW: f32 = 0.15;
/// The share of slots that sparkle.
const DENSITY: f32 = 0.36;
/// The glow's cross-section: offset from the line, and its share of the glow there. Full from under the
/// main line's anti-aliased rim (2.5 px wide, its rim fading over a pixel: no seam opens between them;
/// past it along a dimmed line, see `FAINT`), clear of its middle, falling off as (1 - u)^1.5 to
/// nothing 10 px out, flat there: its outer edge doesn't show.
const XS: [(f32, f32); 9] = [(0.0, 0.0), (0.6, 0.0), (0.7, 1.0), (2.25, 0.761), (3.8, 0.544), (5.35, 0.354), (6.9, 0.192), (8.45, 0.068), (10.0, 0.0)];

/// How far out (px) the glow's start moves along a line that is dimmed to nothing: the start of a dimmed
/// line's glow lies past its rim (1.5 px off its middle at the most, on a retina screen).
const FAINT: f32 = 1.3;

/// Main's line on screen at `x`, drawn in `color`: its spans (top, bottom, alpha). `top` is row 0's y:
/// the effect is anchored to the history and scrolls with it.
pub struct Line {
    pub x: f32,
    pub top: f32,
    pub spans: Vec<(f32, f32, f32)>,
    pub color: Color32,
}

/// The glow, the sparkles and their cores: always the line's colour, the sparkles mixed toward white
/// on dark backdrops (they glint) and toward black on light ones (white would vanish there).
struct Pal {
    glow: Color32,
    spark: Color32,
    core: Color32,
}

fn palette(line: Color32, dark: bool) -> Pal {
    let mix = |to, by| line.lerp_to_gamma(to, by);
    if dark {
        Pal { glow: line, spark: mix(Color32::WHITE, 0.4).gamma_multiply(0.9), core: mix(Color32::WHITE, 0.7) }
    } else {
        let deep = mix(Color32::BLACK, 0.25);
        Pal { glow: line, spark: deep.gamma_multiply(0.9), core: deep }
    }
}

/// Notes input anywhere in the window, whichever view shows: the effect animates for a minute after
/// it. The first frame counts as input (a fresh window animates for a while), and so does coming back
/// to the window.
pub fn note_input(ui: &Ui) {
    let (now, used) = ui.input(|i| {
        let used = |e: &Event| matches!(e, Event::PointerMoved(_) | Event::PointerButton { .. } | Event::MouseWheel { .. } | Event::Key { .. } | Event::WindowFocused(true));
        (i.time, i.events.iter().any(used))
    });
    ui.data_mut(|m| {
        let last = m.get_temp_mut_or(Id::new("magic_input"), now);
        if used {
            *last = now;
        }
    });
}

/// Paints the effect; asks for `FPS` frames a second while the window is focused and was used in the
/// last minute. At rest its clock stops: repaints for anything else don't move it.
pub fn paint(ui: &Ui, p: &Painter, line: &Line) {
    let clip = p.clip_rect();
    if (line.x - clip.center().x).abs() > clip.width() / 2.0 + 18.0 || !line.spans.iter().any(|s| s.0 < clip.bottom() && s.1 > clip.top()) {
        return;
    }
    let (now, focused, dt) = ui.input(|i| (i.time, i.viewport().focused.unwrap_or(true), i.predicted_dt));
    let live = focused && now - ui.data(|m| m.get_temp::<f64>(Id::new("magic_input"))).unwrap_or(now) < IDLE;
    if live {
        // egui takes a predicted frame (`predicted_dt`, 1/60 s: eframe never measures it) off any
        // delay: added back, FPS rather than 60.
        let frame = std::time::Duration::try_from_secs_f32(dt).unwrap_or_default();
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(1.0 / FPS) + frame);
    }
    // The clock runs by the frames it animates (a slow one counts 0.1 s at most): after a rest it
    // goes on from where it stopped, without a jump.
    let t = ui.data_mut(|m| {
        let (t, last) = m.get_temp_mut_or(Id::new("magic_t"), (now, now));
        if live {
            *t += (now - *last).clamp(0.0, 0.1);
        }
        *last = now;
        *t
    });
    // The screenshot harness may run the effect on a clock of its own.
    let t = ui.data(|m| m.get_temp(Id::new("magic_clock"))).unwrap_or(t);
    p.add(mesh(line, t, ui.visuals().dark_mode));
}

/// The effect on `line` at time `t` (s).
fn mesh(line: &Line, t: f64, dark: bool) -> Mesh {
    let pal = palette(line.color, dark);
    let mut m = Mesh::default();
    // Row 0 moved down by whole periods to near the view.
    let top = line.top + (-line.top / PERIOD).floor().max(0.0) * PERIOD;
    // The line's ends: everything fades in over 12 px there (the tip's avatar hides only its middle).
    let (y0, y1) = line.spans.iter().fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(s.0), b.max(s.1)));
    let ends = |y: f32| smooth(((y - y0).min(y1 - y) / 12.0).clamp(0.0, 1.0));
    // Abutting spans of one alpha (a row each where commits follow each other) make one strip.
    let mut spans: Vec<(f32, f32, f32)> = vec![];
    for &s in &line.spans {
        match spans.last_mut() {
            Some(l) if l.1 == s.0 && l.2 == s.2 => l.1 = s.1,
            _ => spans.push(s),
        }
    }
    for (i, &(a, b, alpha)) in spans.iter().enumerate() {
        // Next to a span of another strength (a dimmed row by a lit one) the glow crosses over within
        // half a row each side: the avatar there hides the line's own step, but not the wider glow's.
        let prev = i.checked_sub(1).map(|i| spans[i]).filter(|p| p.1 == a).map_or(alpha, |p| p.2);
        let next = spans.get(i + 1).filter(|n| n.0 == b).map_or(alpha, |n| n.2);
        let half = super::ROW / 2.0;
        let strength = |y: f32| alpha + (prev - alpha) * 0.5 * (1.0 - (y - a) / half).max(0.0) + (next - alpha) * 0.5 * (1.0 - (b - y) / half).max(0.0);
        // Rows every STEP px of history, and at the span's ends.
        let mut ys = vec![a];
        let mut k = ((a - top) / STEP).floor() + 1.0;
        while top + k * STEP < b {
            ys.push(top + k * STEP);
            k += 1.0;
        }
        ys.push(b);
        for (j, &y) in ys.iter().enumerate() {
            // As strong as the line where they meet, so the line gets no edge of its own (a glow dimmer than
            // the line reads as a dark border): the glimmer swells and tightens the glow beyond, each share
            // to the power 1 / glimmer.
            let (lit, p) = (ends(y) * strength(y), 1.0 / glimmer(y - top, t));
            // A dimmed line is faint, and a glow full from under its rim would double it there: a hollow line
            // between two bright rails. The fainter the line, the further out the glow starts (up to FAINT px).
            let far = (1.0 - strength(y)) * FAINT;
            let (base, mid) = (m.vertices.len() as u32, XS.len() as i32 - 1);
            for k in -mid..=mid {
                let (dx, share) = XS[k.unsigned_abs() as usize];
                let dx = match k.unsigned_abs() {
                    1 => dx + far / 3.0,
                    2 => dx + far,
                    _ => dx,
                };
                m.colored_vertex(pos2(line.x + dx * k.signum() as f32, y), pal.glow.gamma_multiply(lit * share.powf(p)));
            }
            if j > 0 {
                let w = 2 * mid as u32 + 1;
                for k in 0..w - 1 {
                    let (p, q) = (base - w + k, base + k);
                    m.add_triangle(p, p + 1, q);
                    m.add_triangle(p + 1, q + 1, q);
                }
            }
        }
    }
    for &(a, b, alpha) in &spans {
        for sp in sparkles(a - top, b - top, t) {
            let at = pos2(line.x + sp.dx, top + sp.c);
            let k = sp.alpha * alpha * ends(at.y);
            if k < 0.02 {
                continue;
            }
            if sp.star {
                star(&mut m, at, sp.size, sp.rot, pal.spark.gamma_multiply(k));
            } else {
                glow(&mut m, at, sp.size, pal.spark.gamma_multiply(k));
            }
            glow(&mut m, at, sp.size * 0.4, pal.core.gamma_multiply(k));
        }
    }
    m
}

/// Smoothstep of 0..=1.
fn smooth(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

/// How lit the glimmer is (`LOW`..=1) at history position `c` at time `t` (s). A knot every KNOT px
/// swells and fades on a slow cycle of its own (1.5–4 s, length and offset from a hash of the knot),
/// blended smoothly with the next: it varies along the line and over time, but nothing travels.
fn glimmer(c: f32, t: f64) -> f32 {
    let knot = |k: i64| {
        // Repeating every PERIOD too.
        let h = hash(k.rem_euclid((PERIOD / KNOT) as i64) as u32, 0x61a3);
        let cycle = t / (1.5 + 2.5 * unit(h, 0) as f64) + unit(h, 1) as f64;
        0.5 - 0.5 * (cycle.fract() as f32 * TAU).cos()
    };
    let (k, f) = ((c / KNOT).floor(), (c / KNOT).rem_euclid(1.0));
    let (a, b) = (knot(k as i64), knot(k as i64 + 1));
    LOW + (1.0 - LOW) * (a + (b - a) * smooth(f))
}

/// A sparkle at history position `c`, `dx` beside the line: a star (rays `size` long) or a mote (glow
/// radius `size`).
#[derive(Debug, PartialEq)]
struct Spark {
    c: f32,
    dx: f32,
    size: f32,
    alpha: f32,
    star: bool,
    rot: f32,
}

/// The sparkles of history positions c0..c1 at time `t`. A `DENSITY` share of the slots sparkle, each in
/// cycles of 1–3 s (length and offset from a hash of the slot), placed and sized by a hash of (slot,
/// cycle): it fades in and out at one spot, a new one only with the next cycle.
fn sparkles(c0: f32, c1: f32, t: f64) -> Vec<Spark> {
    let mut out = vec![];
    for slot in (c0 / SLOT).ceil() as i64..(c1 / SLOT).ceil() as i64 {
        // Repeating every PERIOD too.
        let id = slot.rem_euclid((PERIOD / SLOT) as i64) as u32;
        let h = hash(id, 0x5eed);
        if unit(h, 0) > DENSITY {
            continue;
        }
        let cycle = t / (1.0 + 2.0 * unit(h, 1) as f64) + unit(h, 2) as f64;
        let u = cycle.fract() as f32;
        let (h, h2) = (hash(id, cycle as u32), hash(id ^ 0xa5a5, cycle as u32));
        let c = (slot as f32 + unit(h, 0)) * SLOT;
        let side = if unit(h, 1) < 0.5 { -1.0 } else { 1.0 };
        let kind = unit(h2, 0);
        let env = (PI * u).sin().max(0.0);
        // A few large stars, more small ones, half of them motes. The column's edge is 18 px from the
        // line: a star's sideways rays are 0.6 of its length, a mote's glow its radius.
        let (star, size, dx) = match kind {
            k if k < 0.12 => (true, 10.0 + 3.0 * unit(h2, 1), 5.0 + 3.0 * unit(h, 2)),
            k if k < 0.5 => (true, 6.0 + 3.0 * unit(h2, 1), 5.0 + 6.5 * unit(h, 2)),
            _ => (false, 3.3 + 1.1 * unit(h2, 1), 4.0 + 8.0 * unit(h, 2)),
        };
        let size = size * if star { env.sqrt() } else { 1.0 };
        let alpha = env.powi(if star { 3 } else { 2 });
        out.push(Spark { c, dx: side * dx, size, alpha, star, rot: (u - 0.5) * 0.6 });
    }
    out
}

/// lowbias32 of a slot and a cycle.
fn hash(a: u32, b: u32) -> u32 {
    let mut h = a ^ b.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// Byte `i` of `h` as 0..=1.
fn unit(h: u32, i: u32) -> f32 {
    ((h >> (8 * i)) & 0xff) as f32 / 255.0
}

/// A soft glow at `c`: `col` in the middle, a bell-shaped fade to nothing at radius `r`. Bigger ones get
/// more corners, so they don't show as polygons.
fn glow(m: &mut Mesh, c: Pos2, r: f32, col: Color32) {
    let n = match r {
        r if r > 5.0 => 14,
        r if r > 3.0 => 10,
        _ => 6,
    };
    let mut dirs = [Vec2::ZERO; 14];
    for (i, d) in dirs[..n].iter_mut().enumerate() {
        *d = Vec2::angled(i as f32 / n as f32 * TAU) * r;
    }
    let base = m.vertices.len() as u32;
    m.colored_vertex(c, col);
    // Two rings: (fraction of the radius, share of `col`).
    for (f, a) in [(0.4, 0.38), (1.0, 0.0)] {
        let col = col.gamma_multiply(a);
        for &d in &dirs[..n] {
            m.colored_vertex(c + d * f, col);
        }
    }
    let n = n as u32;
    for i in 0..n {
        let (j, a, b) = ((i + 1) % n, base + 1, base + 1 + n);
        m.add_triangle(base, a + i, a + j);
        m.add_triangle(a + i, a + j, b + i);
        m.add_triangle(a + j, b + j, b + i);
    }
}

/// A four-point star: rays `len` long (the sideways ones shorter) fading to their tips and edges,
/// turned by `rot`, over a small glow.
fn star(m: &mut Mesh, c: Pos2, len: f32, rot: f32, col: Color32) {
    glow(m, c, len * 0.45, col.gamma_multiply(0.6));
    let w = (len * 0.12).max(0.8);
    let base = m.vertices.len() as u32;
    m.colored_vertex(c, col);
    for k in 0..4 {
        let a = rot + k as f32 * PI / 2.0;
        let dir = vec2(a.sin(), -a.cos()) * if k % 2 == 0 { len } else { len * 0.6 };
        let side = vec2(a.cos(), a.sin()) * w;
        let v = m.vertices.len() as u32;
        m.colored_vertex(c + side, Color32::TRANSPARENT);
        m.colored_vertex(c + dir, Color32::TRANSPARENT);
        m.colored_vertex(c - side, Color32::TRANSPARENT);
        m.add_triangle(base, v, v + 1);
        m.add_triangle(base, v + 1, v + 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{RawInput, ViewportId};
    use std::time::Duration;

    /// Main's line at x = 100 in `color`, row 0 at -40: its tip at 30, the rows below 58 dimmed.
    fn line(color: Color32) -> Line {
        Line { x: 100.0, top: -40.0, spans: vec![(30.0, 58.0, 1.0), (58.0, 900.0, 0.3)], color }
    }

    #[test]
    fn sparkles_are_stable_and_stay_beside_the_line() {
        for t in [0.0, 1.7, 3.2, 86_400.5] {
            let s = sparkles(100.0, 1500.0, t);
            assert_eq!(s, sparkles(100.0, 1500.0, t));
            assert!(s.iter().any(|sp| sp.dx < 0.0) && s.iter().any(|sp| sp.dx > 0.0), "{t}: on both sides of the line");
            for sp in &s {
                assert!((4.0..=13.0).contains(&sp.dx.abs()) && (0.0..=1.0).contains(&sp.alpha) && sp.size >= 0.0, "{sp:?}");
                assert!(sp.c >= 100.0 && sp.c < 1500.0 + SLOT, "{sp:?}");
                // Stars reach out at most 17 px (the sideways rays are shorter): the column's edge is 18 px away.
                assert!(sp.dx.abs() + if sp.star { sp.size * 0.6 } else { sp.size } <= 17.0, "{sp:?}");
            }
        }
        // They twinkle: a moment later the sparkles differ.
        assert_ne!(sparkles(100.0, 1500.0, 1.0), sparkles(100.0, 1500.0, 1.3));
    }

    #[test]
    fn a_sparkle_stays_where_it_is() {
        // Every slot over 40 s: it takes a new spot only with its next cycle (1–3 s), never drifts within one,
        // and fades in from nothing and out again.
        let (mut seen, mut lo, mut hi) = (0, 1.0f32, 0.0f32);
        for slot in 1000..1100 {
            let (c0, c1) = (slot as f32 * SLOT, (slot + 1) as f32 * SLOT);
            let mut spots = vec![];
            for i in 0..800 {
                for sp in sparkles(c0, c1, i as f64 * 0.05) {
                    (seen, lo, hi) = (seen + 1, lo.min(sp.alpha), hi.max(sp.alpha));
                    if spots.last() != Some(&(sp.c, sp.dx)) {
                        spots.push((sp.c, sp.dx));
                    }
                }
            }
            assert!(spots.len() <= 41, "slot {slot}: {} spots in 40 s", spots.len());
        }
        assert!(seen > 10_000 && lo < 0.01 && hi > 0.99, "{seen} {lo} {hi}");
    }

    #[test]
    fn intensities_stay_in_range() {
        for i in 0..=1000 {
            assert!((0.0..=1.0).contains(&smooth(i as f32 / 1000.0)));
        }
        // The glimmer is never off nor above full, anywhere in a long history and at any time.
        for c in (-500..90_000).step_by(11) {
            for t in [0.0, 0.9, 5.3, 123.4, 1e6] {
                let g = glimmer(c as f32, t);
                assert!((0.1..=1.0 + 1e-4).contains(&g), "{c} {t}: {g}");
            }
        }
        // It is alive: at a knot it swings between dim and full within a cycle.
        for c in [0.0, KNOT, 100.0 * KNOT] {
            let (lo, hi) = (0..400).map(|i| glimmer(c, i as f64 * 0.05)).fold((1.0f32, 0.0f32), |(lo, hi), g| (lo.min(g), hi.max(g)));
            assert!(lo < LOW + 0.05 && hi > 0.95, "{c}: {lo}..{hi}");
        }
    }

    #[test]
    fn the_glimmer_stays_in_place() {
        // Between two knots the glimmer is a monotone blend of their levels: wherever it peaks or dips along
        // the line, that is at a knot. Nothing runs up or down it.
        for t in [0.0, 0.7, 1.9, 3.1, 10.3, 99.9] {
            let g: Vec<f32> = (0..=1800).map(|i| glimmer(i as f32 * 0.5, t)).collect();
            let mut turns = 0;
            for i in 1..g.len() - 1 {
                let (before, after) = (g[i] - g[i - 1], g[i + 1] - g[i]);
                if before * after < 0.0 && before.abs().min(after.abs()) > 1e-6 {
                    let off = (i as f32 * 0.5 / KNOT).rem_euclid(1.0);
                    assert!(off.min(1.0 - off) * KNOT < 1.0, "{t}: it turns at {} px, between knots", i as f32 * 0.5);
                    turns += 1;
                }
            }
            assert!(turns > 3, "{t}: the glimmer varies along the line");
        }
    }

    #[test]
    fn neighbouring_knots_swell_independently() {
        // Nor does a wave run along the knots: a knot's swelling isn't its neighbour's a moment earlier or later.
        let series = |k: i64| (0..1200).map(|i| glimmer(k as f32 * KNOT, i as f64 * 0.05) as f64).collect::<Vec<_>>();
        let corr = |a: &[f64], b: &[f64]| {
            let mean = |x: &[f64]| x.iter().sum::<f64>() / x.len() as f64;
            let (ma, mb) = (mean(a), mean(b));
            let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
            cov / (a.iter().map(|x| (x - ma).powi(2)).sum::<f64>() * b.iter().map(|y| (y - mb).powi(2)).sum::<f64>()).sqrt()
        };
        let mut best: Vec<f64> = (100..140)
            .map(|k| {
                let (a, b) = (series(k), series(k + 1));
                (0..=40).flat_map(|lag| [corr(&a[..1200 - lag], &b[lag..]), corr(&b[..1200 - lag], &a[lag..])]).fold(f64::MIN, f64::max)
            })
            .collect();
        best.sort_by(f64::total_cmp);
        assert!(best[best.len() / 2] < 0.6, "the median pair's best match within ±2 s: {best:?}");
    }

    #[test]
    fn the_mesh_stays_in_the_column_and_blends_normally() {
        // A pure red and a pure blue line: mixed toward white or black they keep their hue, so red keeps green
        // equal to blue, and blue red equal to green.
        for t in (0..60).map(|i| i as f64 * 0.37) {
            for dark in [true, false] {
                for col in [Color32::RED, Color32::BLUE] {
                    let m = mesh(&line(col), t, dark);
                    assert!(m.is_valid() && !m.vertices.is_empty());
                    for v in &m.vertices {
                        // The graph column's edge is 18 px from column 0's line; the line runs from 30 to 900.
                        assert!((v.pos.x - 100.0).abs() <= 18.0 && v.pos.y > 30.0 - 18.0 && v.pos.y < 900.0 + 18.0, "{:?}", v.pos);
                        // Premultiplied with no additive part.
                        let [r, g, b, a] = v.color.to_array();
                        assert!(r.max(g).max(b) <= a, "{:?}", v.color);
                        let (p, q) = if col == Color32::RED { (g, b) } else { (r, g) };
                        assert_eq!(p, q, "{dark}: {:?} is not the line's hue", v.color);
                    }
                }
            }
        }
        // Nor a grey or white: on a dark backdrop the line's colour, lightened (never to white), on a light one
        // only darkened.
        for dark in [true, false] {
            let m = mesh(&Line { spans: vec![(30.0, 900.0, 1.0)], ..line(Color32::RED) }, 2.2, dark);
            let strong: Vec<_> = m.vertices.iter().filter(|v| v.color.a() >= 64).collect();
            assert!(strong.len() > 50);
            for v in strong {
                let [r, g, b, _] = v.color.to_array();
                assert!(g == b && if dark { r > g && g as f32 <= 0.75 * r as f32 } else { g == 0 }, "{dark}: {:?}", v.color);
            }
        }
    }

    #[test]
    fn the_mesh_puts_the_glimmer_and_each_sparkle_where_they_belong() {
        // Row 0 at -40, the line lit from 30 and dimmed below 300. The glow's rows sit every STEP px of history,
        // reaching 10 px out on both sides, never over the line's middle. Where it meets the line it is as strong as
        // the line (no dark border): the line's dimming, crossing over within half a row at 300 and fading in at
        // the line's ends. It starts under a lit line's rim, but past a dimmed line's (see `FAINT`): there it
        // would double the faint line into a hollow one between two bright rails. Further out the glimmer swells
        // and tightens it. Each sparkle is where `sparkles` puts it, as bright as it says, times the dimming.
        let spans = vec![(30.0, 300.0, 1.0), (300.0, 900.0, 0.3)];
        let l = Line { spans: spans.clone(), ..line(Color32::BLUE) };
        let (edge, (out, half)) = (XS[2].0, XS[5]);
        // Full from under the main line's rim (2.5 px wide, it fades out from 0.75 px off the middle on a 1x screen),
        // fainter at every step out, next to nothing before its end: its outer edge doesn't show.
        assert!(edge < 0.75 && XS[2].1 == 1.0 && XS[2..].windows(2).all(|w| w[1].1 < w[0].1) && XS[XS.len() - 2].1 < 0.1 && XS[XS.len() - 1].1 == 0.0, "{XS:?}");
        // Pushed out all the way, the start stays short of the next stop.
        assert!(XS[2].0 + FAINT < XS[3].0, "{XS:?}");
        for t in [0.0, 1.3, 7.7, 123.4] {
            let m = mesh(&l, t, true);
            let at = |x: f32, y: f32| m.vertices.iter().filter(move |v| v.pos == pos2(x, y));
            // The glow's rows, by their stop `out` px off the line (it never moves), and where each starts: its first
            // vertex off the line that shows.
            let rows: Vec<_> = m.vertices.iter().filter(|v| v.pos.x == 100.0 + out).collect();
            let start = |y: f32| m.vertices.iter().filter(|v| v.pos.y == y && v.pos.x > 100.0 && v.color.a() > 0).min_by(|p, q| p.pos.x.total_cmp(&q.pos.x));
            assert!(rows.len() > 90);
            for v in &rows {
                let (y, f) = (v.pos.y, ((v.pos.y + 40.0) / STEP).fract());
                assert!(f.min(1.0 - f) < 1e-3 || [30.0, 300.0, 900.0].contains(&y), "{v:?} is off the rows");
                assert!(at(90.0, y).next().is_some() && at(110.0, y).next().is_some(), "{v:?}: the glow reaches 10 px out on both sides");
                let Some(s) = start(y) else { continue };
                let off = s.pos.x - 100.0;
                assert!(m.vertices.iter().filter(|w| w.pos.y == y && (w.pos.x - 100.0).abs() < off - 1e-3).all(|w| w.color.a() == 0), "{t} {y}: over the line");
                if (y - 300.0).abs() > 14.0 && y > 42.0 && y < 888.0 {
                    let dim = if y < 300.0 { 1.0 } else { 0.3 };
                    assert!((s.color.a() as f32 - 255.0 * dim).abs() <= 1.0, "{t}: {s:?} as strong as the line, {dim}");
                    // Under a lit line's rim (0.75 px off its middle at the least), past a dimmed line's (1.5 px at the most).
                    assert!(if dim == 1.0 { off < 0.75 } else { off > 1.5 }, "{t}: {s:?} starts {off} px off a line dimmed to {dim}");
                    let (w, want) = (at(100.0 + out, y).next().unwrap(), 255.0 * dim * half.powf(1.0 / glimmer(y + 40.0, t)));
                    assert!((w.color.a() as f32 - want).abs() <= 1.0, "{t}: {w:?} want {want}");
                }
            }
            // At 300 both sides meet half way: no seam where the avatar there doesn't cover the glow.
            let seam: Vec<u8> = start(300.0).map_or(vec![], |s| m.vertices.iter().filter(|v| v.pos == s.pos).map(|v| v.color.a()).collect());
            let want = 255.0 * 0.65;
            assert!(seam.len() == 2 && seam.iter().all(|&a| (a as f32 - want).abs() <= 1.0), "{t}: {seam:?} want {want}");
            assert!(m.vertices.iter().filter(|v| v.pos.y == 30.0 || v.pos.y == 900.0).all(|v| v.color.a() == 0), "{t}: the ends don't fade");
            let mut lit = 0;
            for &(a, b, dim) in &spans {
                for sp in sparkles(a + 40.0, b + 40.0, t) {
                    let (at, k) = (pos2(100.0 + sp.dx, sp.c - 40.0), sp.alpha * dim);
                    if k < 0.05 || !(45.0..885.0).contains(&at.y) {
                        continue;
                    }
                    lit += 1;
                    let a = m.vertices.iter().filter(|v| v.pos == at).map(|v| v.color.a()).max();
                    assert!(a.is_some_and(|a| (a as f32 - 255.0 * k).abs() <= 1.0), "{t}: {sp:?} at {at:?}: {a:?}, want {}", 255.0 * k);
                }
            }
            assert!(lit > 10, "{lit}");
        }
    }

    #[test]
    fn the_effect_scrolls_with_the_history() {
        // Row 0 and the line moved by `d` (a scroll): every vertex moves with them, as bright as it was.
        let l = line(Color32::BLUE);
        let base = mesh(&l, 2.2, true);
        for d in [13.0, 28.0, 123.5, 1000.0, -57.0] {
            let moved = Line { top: l.top + d, spans: l.spans.iter().map(|&(a, b, k)| (a + d, b + d, k)).collect(), ..l };
            let m = mesh(&moved, 2.2, true);
            assert_eq!(m.vertices.len(), base.vertices.len(), "{d}");
            for (v, w) in base.vertices.iter().zip(&m.vertices) {
                assert!(w.pos.x == v.pos.x && (w.pos.y - v.pos.y - d).abs() < 0.02, "{d}: {v:?} {w:?}");
                assert!(w.color.to_array().iter().zip(v.color.to_array()).all(|(&p, q)| p.abs_diff(q) <= 1), "{d}: {v:?} {w:?}");
            }
        }
    }

    #[test]
    fn a_glow_fades_to_nothing_at_its_rim() {
        let (c, col) = (pos2(50.0, 60.0), Color32::from_rgb(40, 80, 120));
        let mut m = Mesh::default();
        glow(&mut m, c, 5.0, col);
        assert!(m.is_valid());
        for v in &m.vertices {
            let d = (v.pos - c).length();
            assert!(d <= 5.001 && (d < 4.99 || v.color == Color32::TRANSPARENT) && (d > 0.01 || v.color == col), "{v:?}");
        }
    }

    #[test]
    fn deep_in_a_long_history_it_looks_the_same() {
        assert!([STEP, KNOT, SLOT].iter().all(|p| PERIOD % p == 0.0));
        // Row 0 two million rows up, where f32 rounds history positions to 4 px.
        let deep = |top| Line { x: 100.0, top, spans: vec![(0.0, 600.0, 1.0)], color: Color32::BLUE };
        for t in [1.234, 77.7] {
            assert_eq!(mesh(&deep(-100.0 - 683.0 * PERIOD), t, true).vertices, mesh(&deep(-100.0), t, true).vertices);
        }
        // The sparkles and the glimmer too are the same a period on: moving row 0 down by one doesn't show.
        let (a, b) = (sparkles(100.0, 1500.0, 7.7), sparkles(100.0 + PERIOD, 1500.0 + PERIOD, 7.7));
        assert!(a.len() == b.len() && a.iter().zip(&b).all(|(p, q)| (p.c + PERIOD - q.c).abs() < 0.05 && p.dx == q.dx && (p.alpha - q.alpha).abs() < 0.01));
        for c in [0.0, 17.0, 400.0, 1234.5] {
            assert!((glimmer(c, 3.3) - glimmer(c + PERIOD, 3.3)).abs() < 1e-3, "{c}");
        }
    }

    #[test]
    fn animates_only_while_focused_and_in_use() {
        let ctx = egui::Context::default();
        let line = Line { x: 100.0, top: 0.0, spans: vec![(20.0, 500.0, 1.0)], color: Color32::BLUE };
        // A moment: its input, then two frames without (egui itself repaints at once after input), the
        // graph shown or another view. The delay then asked for, and the effect painted.
        let moment = |time: f64, focused: bool, events: Vec<Event>, line: &Line, graph: bool| {
            let mut last = (Duration::ZERO, vec![]);
            for (k, events) in [events, vec![], vec![]].into_iter().enumerate() {
                let mut raw = RawInput { time: Some(time + k as f64 * 0.001), events, screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
                raw.viewports.entry(ViewportId::ROOT).or_default().focused = Some(focused);
                let mut out = ctx.run_ui(raw, |ui| {
                    note_input(ui);
                    if graph {
                        paint(ui, ui.painter(), line);
                    }
                });
                out.textures_delta.clear();
                let mesh = out.shapes.into_iter().find_map(|s| if let egui::Shape::Mesh(m) = s.shape { Some(m.vertices.clone()) } else { None });
                last = (out.viewport_output[&ViewportId::ROOT].repaint_delay, mesh.unwrap_or_default());
            }
            last
        };
        // The glow's alpha down the line, one column of it: a third of the way out, where the glimmer shows.
        let column = |m: &[egui::epaint::Vertex]| m.iter().filter(|v| v.pos.x == 100.0 + XS[4].0).map(|v| v.color.a()).collect::<Vec<_>>();
        let moved = || vec![Event::PointerMoved(pos2(300.0, 300.0))];
        let key = || vec![Event::Key { key: egui::Key::J, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }];
        let wheel = || vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, -1.0), phase: egui::TouchPhase::Move, modifiers: Default::default() }];
        let click = || vec![Event::PointerButton { pos: pos2(300.0, 300.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }];
        // 20 frames a second (every frame repaints the whole window), though egui takes a predicted frame off the delay asked for.
        // A literal, not derived from FPS: a changed rate has to fail here.
        let live = Duration::from_millis(49)..=Duration::from_millis(52);
        assert!(live.contains(&moment(1.0, true, vec![], &line, true).0), "a fresh window animates");
        let still = moment(1.2, false, vec![], &line, true);
        assert!(still.0 > Duration::from_secs(1), "unfocused");
        assert_eq!(moment(1.7, false, moved(), &line, true).1, still.1, "at rest it stands still, whatever repaints");
        let back = moment(30.0, true, vec![], &line, true);
        assert!(live.contains(&back.0));
        // A frame's worth of glimmer at most (it swings about 0.18 of full in 0.1 s); a jump by the rest's 28 s swings
        // the knots anywhere.
        let (a, b) = (column(&still.1), column(&back.1));
        assert!(a.len() > 40 && a.len() == b.len() && a.iter().zip(&b).all(|(p, q)| p.abs_diff(*q) <= 60), "it goes on from where it rested: {a:?} {b:?}");
        assert!(live.contains(&moment(55.0, true, vec![], &line, true).0), "a minute is a minute");
        assert!(moment(62.0, true, vec![], &line, true).0 > Duration::from_secs(1), "idle for a minute");
        moment(62.5, true, key(), &line, false);
        assert!(live.contains(&moment(63.0, true, vec![], &line, true).0), "a key counts as use, in any view");
        for (k, input) in [wheel(), click(), moved(), vec![Event::WindowFocused(true)]].into_iter().enumerate() {
            let at = 200.0 + k as f64 * 100.0;
            assert!(moment(at, true, vec![], &line, true).0 > Duration::from_secs(1), "idle");
            moment(at + 1.0, true, input, &line, false);
            moment(at + 2.0, true, vec![], &line, false); // egui's own smooth scrolling after a wheel turn settles
            assert!(live.contains(&moment(at + 3.0, true, vec![], &line, true).0), "the wheel, a click, the pointer and coming back count as use: #{k}");
        }
        // Main's line scrolled off the view, its line off (no spans: Settings → Main line off), or sideways.
        for (k, spans) in [vec![], vec![(2000.0, 2500.0, 1.0)], vec![(-500.0, -20.0, 1.0)]].into_iter().enumerate() {
            let off = Line { spans, ..line };
            assert!(moment(600.0 + k as f64 * 10.0, true, moved(), &off, true).0 > Duration::from_secs(1), "off the view, or off: #{k}");
        }
        let away = Line { x: 900.0, ..line };
        assert!(moment(700.0, true, moved(), &away, true).0 > Duration::from_secs(1), "main's line is off screen");
    }
}
