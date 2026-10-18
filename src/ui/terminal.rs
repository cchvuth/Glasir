//! The embedded terminal panel (⌃`): shell tabs, the cell grid with ANSI
//! colours, a cursor and mouse selection, keyboard / clipboard / wheel input.
//! The shells themselves live in `repo::terminal`.

use crate::ui::format::keys;
use crate::repo::terminal::{Session, Terminals};
use crate::theme::{self, icon};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui::{Color32, Event, EventFilter, FontId, ImeEvent, Key, Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use std::borrow::Cow;
use std::path::Path;

/// The grid's widget id: the app skips its Ctrl shortcuts while it has focus (off macOS).
pub(super) fn id() -> egui::Id {
    egui::Id::new("glasir-terminal")
}

/// `new`: Settings → Terminal is on (else the panel only finishes running commands).
pub(super) fn show(ui: &mut Ui, ts: &mut Terminals, dir: &Path, new: bool) {
    let ctx = ui.ctx().clone();
    let mut close = None;
    theme::bar(ui, |ui| {
        if theme::pill(ui, icon::CARET_DOWN).on_hover_text(keys("Hide (⌃`)")).clicked() {
            ts.open = false;
        }
        if new && theme::pill(ui, icon::PLUS).on_hover_text("New terminal").clicked() {
            ts.add(&ctx, dir);
        }
        theme::rest(ui, |ui| {
            ui.label(RichText::new(icon::TERMINAL).color(theme::dim(ui)));
            // Tabs share the width (titles cut with "…"): long shell titles never push them out of the panel.
            let spare = ui.available_width() - if ts.confirm.is_some() { 130.0 } else { 0.0 };
            let w = (spare / ts.tabs.len().max(1) as f32 - 50.0).clamp(0.0, 220.0);
            for (i, s) in ts.tabs.iter().enumerate() {
                let name = if s.title.is_empty() { format!("Shell {}", i + 1) } else { s.title.clone() };
                let tab = ui.scope(|ui| {
                    ui.set_max_width(w);
                    ui.add(egui::Button::selectable(i == ts.active, &name))
                });
                if tab.inner.on_hover_text(&name).clicked() {
                    ts.active = i;
                    ts.focus = true;
                }
                if ts.confirm == Some(i) {
                    if theme::pill(ui, RichText::new("Stop & close").small().color(theme::del())).clicked() {
                        close = Some(i);
                    }
                    if theme::pill(ui, RichText::new("Keep").small()).clicked() {
                        ts.confirm = None;
                    }
                } else if theme::pill(ui, RichText::new(icon::X).small()).on_hover_text("Close this shell").clicked() {
                    // A running command (a rebase in an editor, a push...) would be killed.
                    if s.busy() { ts.confirm = Some(i) } else { close = Some(i) }
                }
            }
        });
    });
    if let Some(i) = close {
        ts.close(i);
    }
    if let Some(e) = &ts.error {
        ui.colored_label(theme::del(), e);
    }
    let focus = std::mem::take(&mut ts.focus);
    if let Some(s) = ts.tabs.get_mut(ts.active) {
        ui.add_space(4.0);
        grid(ui, s, focus);
    }
}

/// The terminal's cells; sizes the PTY to the space left.
fn grid(ui: &mut Ui, s: &mut Session, focus: bool) {
    s.drawn.store(ui.ctx().cumulative_pass_nr(), std::sync::atomic::Ordering::Relaxed);
    let font = FontId::monospace(12.5);
    let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    let (cols, lines) = ((rect.width() / cw).max(2.0) as usize, (rect.height() / ch).max(1.0) as usize);
    s.resize(cols as u16, lines as u16, (cw.round() as u16, ch.round() as u16));
    // egui takes the focus from widgets under a modal (Settings, the shortcut sheet): get it back after.
    let (behind_modal, had_focus) = ui.ctx().memory(|m| (m.top_modal_layer().is_some_and(|l| l != ui.layer_id()), m.has_focus(id())));
    let restore = id().with("restore");
    if behind_modal && had_focus {
        ui.data_mut(|d| d.insert_temp(restore, true));
    }
    let r = ui.interact(rect, id(), Sense::click_and_drag());
    ui.expand_to_include_rect(rect);
    if focus || r.clicked() || r.drag_started() || (!behind_modal && ui.data_mut(|d| d.remove_temp::<bool>(restore)).is_some()) {
        r.request_focus();
    }
    let focused = r.has_focus();
    // A dialog on top (Settings, the shortcut sheet) gets the keys, though egui leaves us the focus.
    if focused && ui.ctx().memory(|m| m.top_modal_layer()).is_none_or(|l| l == ui.layer_id()) {
        ui.memory_mut(|m| m.set_focus_lock_filter(id(), EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        input(ui, s);
    }
    let acc_id = id().with("scroll");
    let mut n = 0;
    if r.hovered() {
        let acc = ui.data(|d| d.get_temp::<f32>(acc_id)).unwrap_or(0.0) + ui.input(|i| i.smooth_scroll_delta.y);
        n = (acc / ch).trunc() as i32;
        ui.data_mut(|d| d.insert_temp(acc_id, acc - n as f32 * ch));
    }
    // A command flooding output holds the lock for most of the time: rather than wait for it every
    // frame, paint the last frame's shapes again (for up to 0.1 s) unless the mouse acts on the grid.
    let snap_id = id().with(std::sync::Arc::as_ptr(&s.term) as usize);
    let now = ui.input(|i| i.time);
    let snap = ui.data(|d| d.get_temp::<Snapshot>(snap_id));
    let urgent = n != 0 || r.interact_pointer_pos().is_some();
    let mut term = match s.term.try_lock_unfair() {
        Some(t) => t,
        None if !urgent && let Some(snap) = snap.filter(|p| now - p.at < 0.1) => {
            ui.painter_at(rect).extend(snap.shapes.iter().cloned());
            if focused && let Some(ime) = snap.ime {
                ui.output_mut(|o| o.ime = Some(ime));
            }
            ui.ctx().request_repaint();
            return;
        }
        None => s.term.lock(),
    };
    let off = term.grid().display_offset();
    let cell_at = |p: Pos2| {
        let (x, y) = ((p.x - rect.left()) / cw, (p.y - rect.top()) / ch);
        let col = (x.max(0.0) as usize).min(cols - 1);
        let side = if x - col as f32 > 0.5 { Side::Right } else { Side::Left };
        (Point::new(Line((y.max(0.0) as usize).min(lines - 1) as i32 - off as i32), Column(col)), side)
    };
    if let Some(p) = r.interact_pointer_pos() {
        let (pt, side) = cell_at(p);
        if r.double_clicked() {
            term.selection = Some(Selection::new(SelectionType::Semantic, pt, side));
        } else if r.drag_started() {
            let (pt, side) = cell_at(ui.input(|i| i.pointer.press_origin()).unwrap_or(p));
            term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
        } else if r.dragged() {
            if let Some(sel) = &mut term.selection {
                sel.update(pt, side);
            }
        } else if r.clicked() {
            term.selection = None;
        }
    }
    if n != 0 {
        let alt = term.mode().contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL);
        if alt {
            // Pagers and editors on the alternate screen scroll with arrow keys.
            s.write((if n > 0 { "\x1b[A" } else { "\x1b[B" }).repeat(n.unsigned_abs() as usize).into_bytes());
        } else {
            term.scroll_display(Scroll::Delta(n));
        }
    }

    let dark = ui.visuals().dark_mode;
    let fg0 = theme::text(ui);
    let painter = ui.painter_at(rect);
    let mut shapes: Vec<Shape> = vec![];
    let mut ime = None;
    let at = |row: usize, col: usize| pos2(rect.left() + col as f32 * cw, rect.top() + row as f32 * ch);
    let content = term.renderable_content();
    let (colors, sel) = (content.colors, content.selection);
    // Runs of same-coloured text (one shape each), painted after the backgrounds and cursor.
    let mut texts: Vec<(Pos2, String, Color32)> = vec![];
    let mut run: Option<(usize, usize, usize, Color32)> = None; // row, first col, cols, colour
    for c in content.display_iter {
        let flags = c.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let (row, col) = ((c.point.line.0 + off as i32) as usize, c.point.column.0);
        let mut fg = rgb(c.fg, colors, dark, fg0);
        let mut bg = (c.bg != Color::Named(NamedColor::Background)).then(|| rgb(c.bg, colors, dark, fg0));
        if flags.contains(Flags::INVERSE) {
            (fg, bg) = (bg.unwrap_or(theme::bg(ui)), Some(fg));
        }
        if flags.intersects(Flags::DIM) {
            fg = fg.gamma_multiply(0.65);
        }
        if sel.is_some_and(|s| s.contains(c.point)) {
            bg = Some(theme::selection(ui));
        }
        let w = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        let cell = Rect::from_min_size(at(row, col), vec2(cw * w as f32, ch));
        if let Some(bg) = bg {
            shapes.push(Shape::rect_filled(cell, 0.0, bg));
        }
        if flags.intersects(Flags::ALL_UNDERLINES) {
            shapes.push(Shape::hline(cell.x_range(), cell.bottom() - 1.5, Stroke::new(1.0, fg)));
        }
        if flags.contains(Flags::STRIKEOUT) {
            shapes.push(Shape::hline(cell.x_range(), cell.center().y, Stroke::new(1.0, fg)));
        }
        let blank = c.c == ' ' || flags.contains(Flags::HIDDEN);
        match &mut run {
            Some((r, c0, n, color)) if *r == row && *c0 + *n == col && (*color == fg || blank) => {
                texts.last_mut().unwrap().1.push(if blank { ' ' } else { c.c });
                *n += w;
            }
            _ if blank => {
                run = None;
                continue;
            }
            _ => {
                texts.push((at(row, col), c.c.to_string(), fg));
                run = Some((row, col, w, fg));
            }
        }
        texts.last_mut().unwrap().1.extend(c.zerowidth().into_iter().flatten());
        if w == 2 {
            run = None; // a wide glyph's width isn't two cells in every font: restart the run after it
        }
    }
    let cur = content.cursor;
    let crow = cur.point.line.0 + off as i32;
    if cur.shape != CursorShape::Hidden && (0..lines as i32).contains(&crow) {
        let cell = Rect::from_min_size(at(crow as usize, cur.point.column.0), vec2(cw, ch));
        if focused {
            // Turns on the OS input method (CJK...) while the shell has focus; commits arrive as `Event::Ime`.
            let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
            let i = egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: to_global * rect,
                cursor_rect: to_global * cell,
                should_interrupt_composition: false,
            };
            ui.output_mut(|o| o.ime = Some(i.clone()));
            ime = Some(i);
        }
        let color = theme::accent();
        shapes.push(match cur.shape {
            _ if !focused => Shape::rect_stroke(cell.shrink(0.5), 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside),
            CursorShape::Beam => Shape::rect_filled(cell.with_max_x(cell.left() + 2.0), 0.0, color),
            CursorShape::Underline => Shape::rect_filled(cell.with_min_y(cell.bottom() - 2.0), 0.0, color),
            _ => Shape::rect_filled(cell, 1.0, color.gamma_multiply(0.6)),
        });
    }
    drop(term);
    for (p, t, color) in texts {
        shapes.push(Shape::galley(p, painter.layout_no_wrap(t, font.clone(), color), color));
    }
    painter.extend(shapes.iter().cloned());
    ui.data_mut(|d| d.insert_temp(snap_id, Snapshot { shapes: shapes.into(), ime, at: now }));
}

/// The grid as last painted, for frames when the shell holds the terminal lock.
#[derive(Clone)]
struct Snapshot {
    shapes: std::sync::Arc<[Shape]>,
    ime: Option<egui::output::IMEOutput>,
    at: f64,
}

/// Sends the frame's keyboard and clipboard events to the shell, and takes them out
/// of the queue so nothing else (Esc closing a diff, the graph's arrow keys) acts on them.
fn input(ui: &Ui, s: &Session) {
    let mac = cfg!(target_os = "macos");
    let shift = ui.input(|i| i.modifiers.shift);
    let mut events: Vec<Event> = ui.input_mut(|i| {
        let (mine, rest) = std::mem::take(&mut i.events).into_iter().partition(|e| match e {
            Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut | Event::Ime(_) => true,
            Event::Key { modifiers, .. } => !modifiers.mac_cmd, // ⌘ shortcuts stay the app's
            _ => false,
        });
        i.events = rest;
        mine
    });
    if events.is_empty() {
        return; // no lock to wait for
    }
    if !mac {
        alt_as_meta(&mut events);
    }
    let mut term = s.term.lock();
    let mode = *term.mode();
    for e in events {
        let bytes: Option<Cow<'static, [u8]>> = match e {
            Event::Text(t) | Event::Ime(ImeEvent::Commit(t)) if !t.is_empty() => Some(t.into_bytes().into()),
            Event::Paste(_) if !mac && !shift => Some(Cow::Borrowed(b"\x16")),
            Event::Paste(t) => Some(paste(&t, mode.contains(TermMode::BRACKETED_PASTE)).into_bytes().into()),
            // Off macOS, Ctrl+C / Ctrl+X arrive as Copy / Cut: copy a selection, else interrupt.
            Event::Copy => match term.selection_to_string().filter(|t| !t.is_empty()) {
                Some(t) => {
                    ui.ctx().copy_text(t);
                    term.selection = None;
                    None
                }
                None => (!mac).then_some(Cow::Borrowed(b"\x03")),
            },
            Event::Cut => (!mac).then_some(Cow::Borrowed(b"\x18")),
            Event::Key { key, pressed: true, modifiers, .. } => key_bytes(key, modifiers, mode.contains(TermMode::APP_CURSOR)),
            _ => None,
        };
        if let Some(b) = bytes {
            term.selection = None;
            term.scroll_display(Scroll::Bottom);
            s.write(b);
        }
    }
}

/// Off macOS, Alt is Meta (readline / zsh Alt+B, F, D, .): egui types Alt+B as a plain
/// "b" after the key press, so that text gets an ESC prefix. Ctrl+Alt is AltGr: left alone.
fn alt_as_meta(events: &mut [Event]) {
    let mut meta = false;
    for e in events {
        match e {
            Event::Key { key, pressed: true, modifiers: m, .. } => meta = m.alt && !m.ctrl && !m.mac_cmd && key_bytes(*key, *m, false).is_none(),
            Event::Text(t) if std::mem::take(&mut meta) => t.insert(0, '\x1b'),
            _ => {}
        }
    }
}

/// Pasted text: bracketed (so the shell doesn't run it line by line) when the app asked.
fn paste(t: &str, bracketed: bool) -> String {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", t.replace('\x1b', ""))
    } else {
        t.replace("\r\n", "\r").replace('\n', "\r")
    }
}

/// The bytes a terminal sends for a key that egui doesn't deliver as text.
fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Cow<'static, [u8]>> {
    if m.mac_cmd {
        return None;
    }
    // xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let p = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi = |c: char, ss3: bool| match () {
        _ if p > 1 => format!("\x1b[1;{p}{c}"),
        _ if ss3 => format!("\x1bO{c}"),
        _ => format!("\x1b[{c}"),
    };
    let tilde = |n: u8| if p > 1 { format!("\x1b[{n};{p}~") } else { format!("\x1b[{n}~") };
    let s = match key {
        Key::ArrowUp => csi('A', app_cursor),
        Key::ArrowDown => csi('B', app_cursor),
        Key::ArrowRight => csi('C', app_cursor),
        Key::ArrowLeft => csi('D', app_cursor),
        Key::Home => csi('H', app_cursor),
        Key::End => csi('F', app_cursor),
        Key::F1 => csi('P', true),
        Key::F2 => csi('Q', true),
        Key::F3 => csi('R', true),
        Key::F4 => csi('S', true),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Enter => return Some(Cow::Borrowed(if m.alt { b"\x1b\r" } else { b"\r" })),
        Key::Tab => return Some(Cow::Borrowed(if m.shift { b"\x1b[Z" } else { b"\t" })),
        Key::Escape => return Some(Cow::Borrowed(b"\x1b")),
        Key::Backspace => {
            return Some(Cow::Borrowed(match () {
                _ if m.ctrl => b"\x08",
                _ if m.alt => b"\x1b\x7f",
                _ => b"\x7f",
            }));
        }
        Key::Space | Key::Num2 if m.ctrl => return Some(Cow::Borrowed(b"\0")),
        Key::Slash | Key::Minus if m.ctrl => return Some(Cow::Borrowed(b"\x1f")), // ^_: undo
        _ if m.ctrl => {
            // Ctrl+A..Z, Ctrl+[ \ ] -> 0x01..0x1d.
            let name = key.symbol_or_name();
            let c = name.bytes().next().filter(|c| name.len() == 1 && (b'A'..=b']').contains(c))?;
            let b = c - b'@';
            return Some(if m.alt { vec![0x1b, b] } else { vec![b] }.into());
        }
        _ => return None,
    };
    Some(s.into_bytes().into())
}

/// Dark / light ANSI colours 0-15.
const ANSI: [[u32; 16]; 2] = [
    [
        0x3b3f4a, 0xff6b6b, 0x4ce082, 0xffc43d, 0x5b8cff, 0xff5cd6, 0x3de3d0, 0xd8dee9, // normal
        0x7a8090, 0xff8787, 0x7bf0a6, 0xffd76e, 0x8aaeff, 0xff8ae3, 0x72f0e2, 0xffffff, // bright
    ],
    [
        0x1f2328, 0xcf222e, 0x116329, 0x9a6700, 0x0550ae, 0x8250df, 0x1b7c83, 0x6e7781, // normal
        0x57606a, 0xa40e26, 0x1a7f37, 0x7d4e00, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f, // bright
    ],
];

/// A cell colour: the program's palette override (OSC 4), else ours / xterm's 256 colours.
fn rgb(c: Color, colors: &Colors, dark: bool, fg: Color32) -> Color32 {
    let i = match c {
        Color::Spec(v) => return Color32::from_rgb(v.r, v.g, v.b),
        Color::Named(n) => n as usize,
        Color::Indexed(i) => i as usize,
    };
    if let Some(v) = colors[i] {
        return Color32::from_rgb(v.r, v.g, v.b);
    }
    let hex = |h: u32| Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    let level = |v: usize| if v == 0 { 0 } else { 55 + 40 * v as u8 };
    match i {
        0..16 => hex(ANSI[!dark as usize][i]),
        16..232 => Color32::from_rgb(level((i - 16) / 36), level((i - 16) / 6 % 6), level((i - 16) % 6)),
        232..256 => Color32::from_gray(8 + 10 * (i - 232) as u8),
        // Dim black..white (DimBlack = 259).
        259..=266 => rgb(Color::Indexed((i - 259) as u8), colors, dark, fg).gamma_multiply(0.65),
        268 => fg.gamma_multiply(0.65), // dim foreground
        _ => fg,                        // foreground, background (only when inverted), cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_xterm_sequences() {
        let k = |key, m, app| key_bytes(key, m, app).map(|b| String::from_utf8(b.into_owned()).unwrap());
        assert_eq!(k(Key::ArrowUp, Modifiers::NONE, false).unwrap(), "\x1b[A");
        assert_eq!(k(Key::ArrowUp, Modifiers::NONE, true).unwrap(), "\x1bOA");
        assert_eq!(k(Key::ArrowLeft, Modifiers::ALT, true).unwrap(), "\x1b[1;3D");
        assert_eq!(k(Key::Delete, Modifiers::NONE, false).unwrap(), "\x1b[3~");
        assert_eq!(k(Key::F5, Modifiers::SHIFT, false).unwrap(), "\x1b[15;2~");
        assert_eq!(k(Key::C, Modifiers::CTRL, false).unwrap(), "\x03");
        assert_eq!(k(Key::OpenBracket, Modifiers::CTRL, false).unwrap(), "\x1b");
        assert_eq!(k(Key::Tab, Modifiers::SHIFT, false).unwrap(), "\x1b[Z");
        assert_eq!(k(Key::Enter, Modifiers::NONE, false).unwrap(), "\r");
        assert_eq!(k(Key::Slash, Modifiers::CTRL, false).unwrap(), "\x1f"); // readline undo
        assert_eq!(k(Key::Minus, Modifiers::CTRL, false).unwrap(), "\x1f");
        assert_eq!(k(Key::Num2, Modifiers::CTRL, false).unwrap(), "\0");
        assert_eq!(k(Key::A, Modifiers::NONE, false), None); // typed as text instead
        assert_eq!(k(Key::C, Modifiers::MAC_CMD, false), None); // ⌘C is the app's
        assert_eq!(paste("a\nb", false), "a\rb");
        assert_eq!(paste("a\r\nb", false), "a\rb");
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut ev = vec![key(Key::B, Modifiers::ALT), Event::Text("b".into()), key(Key::B, Modifiers::NONE), Event::Text("b".into())];
        ev.extend([key(Key::Q, Modifiers::CTRL | Modifiers::ALT), Event::Text("@".into()), key(Key::ArrowLeft, Modifiers::ALT), Event::Text("x".into())]);
        alt_as_meta(&mut ev);
        let texts: Vec<_> = ev.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(texts, ["\x1bb", "b", "@", "x"]); // Alt+B is Meta-b; AltGr and Alt+arrow text untouched
        assert_eq!(paste("x\x1b[201~y", true), "\x1b[200~x[201~y\x1b[201~");
    }

    #[test]
    fn colours_cover_the_256_palette_and_overrides() {
        let mut colors = Colors::default();
        let fg = Color32::WHITE;
        assert_eq!(rgb(Color::Indexed(196), &colors, true, fg), Color32::from_rgb(255, 0, 0));
        assert_eq!(rgb(Color::Indexed(16), &colors, true, fg), Color32::BLACK);
        assert_eq!(rgb(Color::Indexed(255), &colors, true, fg), Color32::from_gray(238));
        assert_eq!(rgb(Color::Named(NamedColor::Red), &colors, false, fg), Color32::from_rgb(0xcf, 0x22, 0x2e));
        assert_eq!(rgb(Color::Named(NamedColor::Foreground), &colors, true, fg), fg);
        colors[1] = Some(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(rgb(Color::Named(NamedColor::Red), &colors, true, fg), Color32::from_rgb(1, 2, 3));
    }
}
