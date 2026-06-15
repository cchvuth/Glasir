//! Liquid Glass design system: a softly lit canvas under translucent glass
//! cards with specular rims, capsule controls and concentric radii, in dark and
//! light. Uses the platform UI font (SF Pro on macOS, Segoe UI on Windows).
//!
//! `mod.rs`: colours, flavours (palettes), accent, fonts and `apply`.
//! `glass.rs`: the glass widgets (`canvas`, `glass`, `group`, `pill`, `primary`, `slider`).

mod glass;

/// Inner margin of every text field: room around the text, mostly sideways
/// (egui's default of 4 px puts the text against a capsule's edge).
pub const INPUT_MARGIN: egui::Margin = egui::Margin::symmetric(12, 6);

pub use glass::{bar, bar_h, canvas, canvas_in, card, empty_state, glass_shapes, group, pill, primary, progress, rest, set_transparency, slider, small};

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Stroke, TextStyle, Ui, Visuals, vec2};
use std::sync::Arc;

/// Phosphor icons, mixed into ordinary text (`format!("{} Push", icon::ARROW_LINE_UP)`).
pub use egui_phosphor::regular as icon;

const LANES: [Color32; 10] = [
    Color32::from_rgb(0x2b, 0xb8, 0xff), // sky
    Color32::from_rgb(0xff, 0x5c, 0xd6), // pink
    Color32::from_rgb(0xff, 0xc4, 0x3d), // amber
    Color32::from_rgb(0x4c, 0xe0, 0x82), // green
    Color32::from_rgb(0xa2, 0x7b, 0xff), // violet
    Color32::from_rgb(0xff, 0x7a, 0x5c), // coral
    Color32::from_rgb(0x3d, 0xe3, 0xd0), // teal
    Color32::from_rgb(0x5b, 0x8c, 0xff), // blue
    Color32::from_rgb(0xff, 0x9f, 0x40), // orange
    Color32::from_rgb(0xc8, 0xe6, 0x4a), // lime
];

pub fn lane(i: u16) -> Color32 {
    LANES[i as usize % LANES.len()]
}

/// Stable per-author avatar color.
pub fn avatar(email: &str) -> Color32 {
    let h = email.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let c = lane((h % 10) as u16);
    Color32::from_rgb(c.r() / 2 + 24, c.g() / 2 + 24, c.b() / 2 + 24)
}

// ---------------------------------------------------------------------------
// Flavours: three dark, three light.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Flavor {
    #[default]
    Midnight,
    Graphite,
    Abyss,
    Daylight,
    Paper,
    Blossom,
}

/// Canvas base, three glow blobs ((x, y), radius, rgba), glass tint, accent.
type Blob = ((f32, f32), f32, [u8; 4]);
pub struct Palette {
    pub base: Color32,
    pub blobs: [Blob; 3],
    pub glass: [u8; 3],
    pub accent: Color32,
}

impl Flavor {
    pub const DARK: [Flavor; 3] = [Flavor::Midnight, Flavor::Graphite, Flavor::Abyss];
    pub const LIGHT: [Flavor; 3] = [Flavor::Daylight, Flavor::Paper, Flavor::Blossom];

    pub fn dark(self) -> bool {
        Self::DARK.contains(&self)
    }

    pub fn name(self) -> &'static str {
        match self {
            Flavor::Midnight => "Midnight",
            Flavor::Graphite => "Graphite",
            Flavor::Abyss => "Abyss",
            Flavor::Daylight => "Daylight",
            Flavor::Paper => "Paper",
            Flavor::Blossom => "Blossom",
        }
    }

    pub fn palette(self) -> Palette {
        let rgb = |r, g, b| Color32::from_rgb(r, g, b);
        let (a, b, c) = ((0.08, 0.05), (0.95, 0.15), (0.55, 1.05));
        match self {
            Flavor::Midnight => Palette {
                base: rgb(0x0b, 0x0d, 0x13),
                blobs: [(a, 0.75, [0x1d, 0x4e, 0xd8, 95]), (b, 0.6, [0x7c, 0x3a, 0xed, 70]), (c, 0.75, [0x0e, 0x9f, 0x9a, 60])],
                glass: [0x1c, 0x20, 0x2a],
                accent: rgb(0x0a, 0x84, 0xff),
            },
            Flavor::Graphite => Palette {
                base: rgb(0x10, 0x11, 0x13),
                blobs: [(a, 0.75, [0x48, 0x4e, 0x58, 90]), (b, 0.6, [0x35, 0x39, 0x40, 80]), (c, 0.75, [0x5a, 0x60, 0x6b, 45])],
                glass: [0x1f, 0x21, 0x25],
                accent: rgb(0x7a, 0xa2, 0xf7),
            },
            Flavor::Abyss => Palette {
                base: rgb(0x03, 0x10, 0x16),
                blobs: [(a, 0.75, [0x0b, 0x6e, 0x75, 110]), (b, 0.6, [0x0e, 0x8a, 0x5f, 80]), (c, 0.75, [0x1b, 0x3a, 0x8a, 70])],
                glass: [0x0e, 0x1d, 0x24],
                accent: rgb(0x14, 0xc8, 0xb0),
            },
            Flavor::Daylight => Palette {
                base: rgb(0xec, 0xef, 0xf5),
                blobs: [(a, 0.75, [0xa8, 0xc8, 0xff, 170]), (b, 0.6, [0xe0, 0xc8, 0xff, 150]), (c, 0.75, [0xb8, 0xf0, 0xe4, 140])],
                glass: [0xff, 0xff, 0xff],
                accent: rgb(0x00, 0x7a, 0xff),
            },
            Flavor::Paper => Palette {
                base: rgb(0xf3, 0xef, 0xe6),
                blobs: [(a, 0.75, [0xe6, 0xd9, 0xbf, 160]), (b, 0.6, [0xee, 0xe4, 0xd0, 150]), (c, 0.75, [0xdc, 0xd4, 0xc4, 120])],
                glass: [0xff, 0xfd, 0xf7],
                accent: rgb(0xb5, 0x55, 0x1d),
            },
            Flavor::Blossom => Palette {
                base: rgb(0xfb, 0xee, 0xf1),
                blobs: [(a, 0.75, [0xff, 0xc4, 0xd4, 170]), (b, 0.6, [0xff, 0xd6, 0xbc, 150]), (c, 0.75, [0xe4, 0xd0, 0xff, 125])],
                glass: [0xff, 0xf8, 0xfa],
                accent: rgb(0xe0, 0x3e, 0x76),
            },
        }
    }
}

static FLAVOR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn flavor() -> Flavor {
    let i = FLAVOR.load(std::sync::atomic::Ordering::Relaxed);
    *[Flavor::DARK, Flavor::LIGHT].concat().get(i as usize).unwrap_or(&Flavor::Midnight)
}

/// The current flavour's accent colour.
pub fn accent() -> Color32 {
    flavor().palette().accent
}
pub const ADD: Color32 = Color32::from_rgb(0x34, 0xc7, 0x59);
pub const DEL: Color32 = Color32::from_rgb(0xff, 0x45, 0x3a);
pub const MOD: Color32 = Color32::from_rgb(0xff, 0x9f, 0x0a);
pub const REN: Color32 = Color32::from_rgb(0x64, 0xd2, 0xff);

pub fn change_color(code: char) -> Color32 {
    match code {
        'A' | '?' => ADD,
        'D' | 'U' => DEL,
        'R' | 'C' => REN,
        _ => MOD,
    }
}

fn dark(ui: &Ui) -> bool {
    ui.visuals().dark_mode
}
/// Solid-ish background for small marks (WIP node center).
pub fn bg(ui: &Ui) -> Color32 {
    if dark(ui) { Color32::from_rgb(0x14, 0x17, 0x1e) } else { Color32::from_rgb(0xf4, 0xf6, 0xfa) }
}
pub fn faint(ui: &Ui) -> Color32 {
    ui.visuals().faint_bg_color
}
pub fn text(ui: &Ui) -> Color32 {
    ui.visuals().strong_text_color()
}
pub fn dim(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}
pub fn selection(ui: &Ui) -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), if dark(ui) { 70 } else { 45 })
}
pub fn hover(ui: &Ui) -> Color32 {
    if dark(ui) { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(9) }
}
pub fn tag(ui: &Ui) -> Color32 {
    if dark(ui) { Color32::from_rgb(0x5a, 0x63, 0x73) } else { Color32::from_rgb(0x8a, 0x93, 0xa3) }
}
pub fn added_bg(ui: &Ui) -> Color32 {
    if dark(ui) { Color32::from_rgba_unmultiplied(0x34, 0xc7, 0x59, 34) } else { Color32::from_rgba_unmultiplied(0x34, 0xc7, 0x59, 40) }
}
pub fn removed_bg(ui: &Ui) -> Color32 {
    if dark(ui) { Color32::from_rgba_unmultiplied(0xff, 0x45, 0x3a, 34) } else { Color32::from_rgba_unmultiplied(0xff, 0x45, 0x3a, 36) }
}


fn system_fonts() -> Vec<(&'static str, &'static [&'static str], FontFamily)> {
    vec![
        (
            "ui",
            &[
                "/System/Library/Fonts/SFNS.ttf",
                "C:\\Windows\\Fonts\\SegUIVar.ttf",
                "C:\\Windows\\Fonts\\segoeui.ttf",
                "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
                "/usr/share/fonts/inter/Inter-Regular.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            ],
            FontFamily::Proportional,
        ),
        (
            "mono",
            &[
                "/System/Library/Fonts/SFNSMono.ttf",
                "C:\\Windows\\Fonts\\CascadiaMono.ttf",
                "C:\\Windows\\Fonts\\consola.ttf",
                "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            ],
            FontFamily::Monospace,
        ),
    ]
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, paths, family) in system_fonts() {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            fonts.families.entry(family).or_default().insert(0, name.into());
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, f: Flavor) {
    FLAVOR.store([Flavor::DARK, Flavor::LIGHT].concat().iter().position(|x| *x == f).unwrap_or(0) as u8, std::sync::atomic::Ordering::Relaxed);
    let light = !f.dark();
    let accent = f.palette().accent;
    let mut v = if light { Visuals::light() } else { Visuals::dark() };
    let w = |a: u8| if light { Color32::from_black_alpha(a / 2 + 4) } else { Color32::from_white_alpha(a) };
    v.panel_fill = Color32::TRANSPARENT;
    v.faint_bg_color = w(10);
    v.extreme_bg_color = if light { Color32::from_white_alpha(190) } else { Color32::from_black_alpha(90) };
    v.code_bg_color = w(14);
    v.window_fill = if light { Color32::from_rgba_unmultiplied(0xf8, 0xf9, 0xfc, 245) } else { Color32::from_rgba_unmultiplied(0x22, 0x26, 0x30, 245) };
    v.window_stroke = Stroke::new(1.0, if light { Color32::from_black_alpha(20) } else { Color32::from_white_alpha(34) });
    v.window_corner_radius = CornerRadius::same(18);
    v.menu_corner_radius = CornerRadius::same(12);
    v.window_shadow = Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_black_alpha(if light { 50 } else { 140 }) };
    v.popup_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if light { 40 } else { 110 }) };
    v.selection.bg_fill = accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = if light { Color32::from_rgb(0x00, 0x66, 0xd6) } else { Color32::from_rgb(0x4d, 0xa8, 0xff) };
    if !light {
        v.override_text_color = None;
        v.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0xd5, 0xda, 0xe3);
        v.widgets.inactive.fg_stroke.color = Color32::from_rgb(0xe6, 0xe9, 0xef);
    }
    for (wv, a) in [
        (&mut v.widgets.inactive, 16),
        (&mut v.widgets.hovered, 30),
        (&mut v.widgets.active, 44),
        (&mut v.widgets.open, 30),
    ] {
        wv.weak_bg_fill = w(a);
        wv.bg_fill = w(a);
        // No stroke: egui counts it in a button's size, so a hover border would grow frameless buttons.
        wv.bg_stroke = Stroke::NONE;
        wv.corner_radius = CornerRadius::same(9);
        wv.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, w(18));
    v.widgets.noninteractive.corner_radius = CornerRadius::same(10);
    // Glass edges already separate things; no indent rule under open sections.
    v.indent_has_left_vline = false;
    let theme = if light { egui::Theme::Light } else { egui::Theme::Dark };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, v);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(19.0)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        s.spacing.item_spacing = vec2(10.0, 6.0);
        s.spacing.button_padding = vec2(14.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        // Short widgets keep their own width: egui widens them to interact_size.x
        // and centres the content, which puts e.g. a checkbox labelled "x" out of line.
        s.spacing.interact_size.x = 24.0;
        // Checkboxes and radio buttons: a bigger box, the tick inset, room before the label.
        s.spacing.icon_width = 18.0;
        s.spacing.icon_width_inner = 10.0;
        s.spacing.icon_spacing = 8.0;
        s.spacing.menu_margin = egui::Margin::symmetric(12, 8);
        s.spacing.window_margin = egui::Margin::same(22);
    });
}
