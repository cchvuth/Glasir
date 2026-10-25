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

pub use glass::{bar, bar_h, canvas, canvas_in, card, context_menu, description, empty_state, glass_shapes, group, keyed, photo, pill, primary, progress, rest, set_transparency, slider, small};

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Stroke, TextStyle, Ui, Visuals, vec2};
use std::sync::Arc;

/// Phosphor icons, mixed into ordinary text (`format!("{}  Push", icon::ARROW_LINE_UP)`).
pub use egui_phosphor::regular as icon;

/// Lane colours, one set for every flavour: all at the same luminance, the window where a line
/// stands out (3:1) on the lightest dark backdrop and white text stays readable (4.5:1) on the fill.
const LANES: [Color32; crate::graph::COLORS as usize] = [
    Color32::from_rgb(0x18, 0x76, 0xda), // blue
    Color32::from_rgb(0xe3, 0x00, 0x8c), // pink
    Color32::from_rgb(0x93, 0x72, 0x08), // yellow
    Color32::from_rgb(0x24, 0x88, 0x35), // green
    Color32::from_rgb(0x8b, 0x4e, 0xff), // violet
    Color32::from_rgb(0xee, 0x00, 0x17), // red
    Color32::from_rgb(0x14, 0x84, 0x7e), // teal
    Color32::from_rgb(0x6b, 0x77, 0x8d), // grey
    Color32::from_rgb(0xc3, 0x56, 0x00), // orange
    Color32::from_rgb(0x9f, 0x6b, 0x3c), // brown
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
// Flavours: ten dark, ten light.

/// Saved settings name the variant; a removed one (Ocean) loads as Midnight,
/// the `serde(other)` fallback, which serde wants last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Flavor {
    Black,
    Graphite,
    Abyss,
    Nord,
    Forest,
    Plum,
    Ember,
    Mocha,
    Rose,
    Daylight,
    Paper,
    Blossom,
    Sand,
    Mint,
    Lavender,
    Sky,
    Peach,
    Stone,
    Sage,
    #[default]
    #[serde(other)]
    Midnight,
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
    pub const DARK: [Flavor; 10] = [
        Flavor::Midnight,
        Flavor::Black,
        Flavor::Graphite,
        Flavor::Nord,
        Flavor::Abyss,
        Flavor::Forest,
        Flavor::Mocha,
        Flavor::Ember,
        Flavor::Rose,
        Flavor::Plum,
    ];
    pub const LIGHT: [Flavor; 10] = [
        Flavor::Daylight,
        Flavor::Stone,
        Flavor::Paper,
        Flavor::Sand,
        Flavor::Peach,
        Flavor::Blossom,
        Flavor::Lavender,
        Flavor::Sky,
        Flavor::Mint,
        Flavor::Sage,
    ];

    pub fn dark(self) -> bool {
        Self::DARK.contains(&self)
    }

    pub fn name(self) -> &'static str {
        match self {
            Flavor::Midnight => "Midnight",
            Flavor::Black => "Black",
            Flavor::Graphite => "Graphite",
            Flavor::Abyss => "Abyss",
            Flavor::Daylight => "Daylight",
            Flavor::Paper => "Paper",
            Flavor::Blossom => "Blossom",
            Flavor::Nord => "Nord",
            Flavor::Forest => "Forest",
            Flavor::Plum => "Plum",
            Flavor::Ember => "Ember",
            Flavor::Mocha => "Mocha",
            Flavor::Rose => "Rose",
            Flavor::Sand => "Sand",
            Flavor::Mint => "Mint",
            Flavor::Lavender => "Lavender",
            Flavor::Sky => "Sky",
            Flavor::Peach => "Peach",
            Flavor::Stone => "Stone",
            Flavor::Sage => "Sage",
        }
    }

    /// The palette as drawn: the tints pulled toward grey and the glow toned down, so the
    /// backdrop stays behind the content.
    /// The palette as drawn: dark tints pulled well toward grey with a faint glow, so the
    /// backdrop stays behind the content; light ones washed with their accent, so they tell apart.
    pub fn palette(self) -> Palette {
        let mut p = self.raw_palette();
        let (toward_grey, glow, wash) = match self {
            Flavor::Forest | Flavor::Mocha | Flavor::Ember | Flavor::Rose | Flavor::Plum => (0.88, 0.15, 0.0),
            _ if self.dark() => (0.75, 0.25, 0.0),
            Flavor::Blossom | Flavor::Lavender | Flavor::Sky | Flavor::Mint | Flavor::Sage => (0.55, 0.4, 0.05),
            _ => (0.3, 0.6, 0.07),
        };
        let a = [p.accent.r(), p.accent.g(), p.accent.b()];
        let tone = |[r, g, b]: [u8; 3], wash: f32| -> [u8; 3] {
            let l = r as f32 * 0.3 + g as f32 * 0.59 + b as f32 * 0.11;
            let c = [r, g, b].map(|c| c as f32 + (l - c as f32) * toward_grey);
            [0, 1, 2].map(|i| (c[i] + (a[i] as f32 - c[i]) * wash).round() as u8)
        };
        let [r, g, b] = tone([p.base.r(), p.base.g(), p.base.b()], wash * 1.5);
        p.base = Color32::from_rgb(r, g, b);
        p.glass = tone(p.glass, wash);
        for blob in &mut p.blobs {
            blob.2[3] = (blob.2[3] as f32 * glow) as u8;
        }
        p
    }

    fn raw_palette(self) -> Palette {
        let rgb = |r, g, b| Color32::from_rgb(r, g, b);
        let (a, b, c) = ((0.08, 0.05), (0.95, 0.15), (0.55, 1.05));
        match self {
            Flavor::Midnight => Palette {
                base: rgb(0x0b, 0x0d, 0x13),
                blobs: [(a, 0.75, [0x1d, 0x4e, 0xd8, 95]), (b, 0.6, [0x7c, 0x3a, 0xed, 70]), (c, 0.75, [0x0e, 0x9f, 0x9a, 60])],
                glass: [0x1c, 0x20, 0x2a],
                accent: rgb(0x40, 0x9c, 0xff),
            },
            Flavor::Black => Palette {
                base: rgb(0x00, 0x00, 0x00),
                blobs: [(a, 0.75, [0, 0, 0, 0]), (b, 0.6, [0, 0, 0, 0]), (c, 0.75, [0, 0, 0, 0])],
                glass: [0x0e, 0x0e, 0x10],
                accent: rgb(0x8e, 0x86, 0xff),
            },
            Flavor::Graphite => Palette {
                base: rgb(0x1c, 0x1d, 0x20),
                blobs: [(a, 0.75, [0x6a, 0x6e, 0x76, 90]), (b, 0.6, [0x50, 0x53, 0x5a, 80]), (c, 0.75, [0x78, 0x7c, 0x84, 50])],
                glass: [0x2d, 0x2e, 0x32],
                accent: rgb(0xf0, 0xa8, 0x2a),
            },
            Flavor::Nord => Palette {
                base: rgb(0x1f, 0x25, 0x30),
                blobs: [(a, 0.75, [0x5e, 0x81, 0xac, 110]), (b, 0.6, [0x88, 0xc0, 0xd0, 60]), (c, 0.75, [0x81, 0xa1, 0xc1, 60])],
                glass: [0x28, 0x2d, 0x39],
                accent: rgb(0x88, 0xc0, 0xd0),
            },
            Flavor::Abyss => Palette {
                base: rgb(0x02, 0x19, 0x1c),
                blobs: [(a, 0.75, [0x0b, 0x8a, 0x86, 120]), (b, 0.6, [0x0e, 0x9a, 0x6a, 90]), (c, 0.75, [0x0a, 0x5e, 0x8a, 80])],
                glass: [0x0a, 0x27, 0x2a],
                accent: rgb(0x14, 0xc8, 0xb0),
            },
            Flavor::Forest => Palette {
                base: rgb(0x07, 0x17, 0x0c),
                blobs: [(a, 0.75, [0x1f, 0x8a, 0x45, 110]), (b, 0.6, [0x5a, 0x8c, 0x12, 80]), (c, 0.75, [0x10, 0x7a, 0x50, 70])],
                glass: [0x12, 0x28, 0x18],
                accent: rgb(0x5c, 0xc9, 0x5c),
            },
            Flavor::Mocha => Palette {
                base: rgb(0x23, 0x19, 0x12),
                blobs: [(a, 0.75, [0x8a, 0x5e, 0x3c, 110]), (b, 0.6, [0x6f, 0x4e, 0x37, 90]), (c, 0.75, [0xa0, 0x70, 0x48, 60])],
                glass: [0x38, 0x2a, 0x21],
                accent: rgb(0xd9, 0xa3, 0x6a),
            },
            Flavor::Ember => Palette {
                base: rgb(0x1c, 0x09, 0x04),
                blobs: [(a, 0.75, [0xe0, 0x4a, 0x0c, 110]), (b, 0.6, [0xb0, 0x1b, 0x1b, 90]), (c, 0.75, [0xd0, 0x70, 0x09, 70])],
                glass: [0x32, 0x16, 0x0e],
                accent: rgb(0xff, 0x6a, 0x3d),
            },
            Flavor::Rose => Palette {
                base: rgb(0x20, 0x0a, 0x16),
                blobs: [(a, 0.75, [0xe1, 0x1d, 0x6a, 100]), (b, 0.6, [0xf4, 0x72, 0xb6, 70]), (c, 0.75, [0xbe, 0x18, 0x5d, 80])],
                glass: [0x36, 0x17, 0x28],
                accent: rgb(0xff, 0x7e, 0xb0),
            },
            Flavor::Plum => Palette {
                base: rgb(0x16, 0x0b, 0x24),
                blobs: [(a, 0.75, [0x7c, 0x2a, 0xd8, 110]), (b, 0.6, [0xa8, 0x1f, 0xb8, 80]), (c, 0.75, [0x5b, 0x21, 0xb6, 70])],
                glass: [0x26, 0x18, 0x3c],
                accent: rgb(0xc0, 0x84, 0xfc),
            },
            Flavor::Daylight => Palette {
                base: rgb(0xec, 0xef, 0xf5),
                blobs: [(a, 0.75, [0xa8, 0xc8, 0xff, 170]), (b, 0.6, [0xe0, 0xc8, 0xff, 150]), (c, 0.75, [0xb8, 0xf0, 0xe4, 140])],
                glass: [0xff, 0xff, 0xff],
                accent: rgb(0x00, 0x5b, 0xc8),
            },
            Flavor::Stone => Palette {
                base: rgb(0xde, 0xdf, 0xe2),
                blobs: [(a, 0.75, [0xb0, 0xb4, 0xbc, 170]), (b, 0.6, [0xc4, 0xc7, 0xce, 160]), (c, 0.75, [0xa4, 0xa8, 0xb0, 140])],
                glass: [0xf7, 0xf7, 0xf8],
                accent: rgb(0x8f, 0x35, 0x81),
            },
            Flavor::Paper => Palette {
                base: rgb(0xf4, 0xee, 0xdc),
                blobs: [(a, 0.75, [0xec, 0xdc, 0xb0, 170]), (b, 0.6, [0xf2, 0xe6, 0xc4, 150]), (c, 0.75, [0xe2, 0xd6, 0xb8, 130])],
                glass: [0xff, 0xfb, 0xee],
                accent: rgb(0x2c, 0x4a, 0x8f),
            },
            Flavor::Sand => Palette {
                base: rgb(0xe9, 0xdb, 0xbd),
                blobs: [(a, 0.75, [0xe0, 0xbc, 0x80, 180]), (b, 0.6, [0xea, 0xcc, 0x96, 160]), (c, 0.75, [0xd4, 0xbc, 0x90, 140])],
                glass: [0xfa, 0xf1, 0xde],
                accent: rgb(0x81, 0x4d, 0x08),
            },
            Flavor::Peach => Palette {
                base: rgb(0xfd, 0xe1, 0xcf),
                blobs: [(a, 0.75, [0xff, 0xb0, 0x84, 180]), (b, 0.6, [0xff, 0xc6, 0xa4, 160]), (c, 0.75, [0xff, 0xa4, 0x94, 140])],
                glass: [0xff, 0xf2, 0xea],
                accent: rgb(0x99, 0x3b, 0x22),
            },
            Flavor::Blossom => Palette {
                base: rgb(0xfb, 0xdf, 0xe8),
                blobs: [(a, 0.75, [0xff, 0xa6, 0xc4, 180]), (b, 0.6, [0xff, 0xc2, 0xd6, 160]), (c, 0.75, [0xf0, 0xb4, 0xee, 130])],
                glass: [0xff, 0xf1, 0xf5],
                accent: rgb(0xa0, 0x2c, 0x55),
            },
            Flavor::Lavender => Palette {
                base: rgb(0xe8, 0xe0, 0xfb),
                blobs: [(a, 0.75, [0xc2, 0xaa, 0xff, 180]), (b, 0.6, [0xda, 0xb4, 0xf8, 160]), (c, 0.75, [0xb2, 0xc2, 0xff, 140])],
                glass: [0xf7, 0xf3, 0xff],
                accent: rgb(0x5f, 0x40, 0xc2),
            },
            Flavor::Sky => Palette {
                base: rgb(0xd9, 0xeb, 0xfb),
                blobs: [(a, 0.75, [0x84, 0xc6, 0xff, 180]), (b, 0.6, [0xa6, 0xda, 0xff, 160]), (c, 0.75, [0x98, 0xe6, 0xf4, 140])],
                glass: [0xef, 0xf7, 0xff],
                accent: rgb(0x00, 0x61, 0x7c),
            },
            Flavor::Mint => Palette {
                base: rgb(0xd9, 0xf3, 0xe6),
                blobs: [(a, 0.75, [0x94, 0xe6, 0xbe, 180]), (b, 0.6, [0xb0, 0xee, 0xd6, 160]), (c, 0.75, [0xc4, 0xee, 0xac, 140])],
                glass: [0xef, 0xfc, 0xf5],
                accent: rgb(0x0a, 0x66, 0x43),
            },
            Flavor::Sage => Palette {
                base: rgb(0xe2, 0xe8, 0xd4),
                blobs: [(a, 0.75, [0xbc, 0xce, 0x98, 175]), (b, 0.6, [0xd0, 0xda, 0xb0, 160]), (c, 0.75, [0xae, 0xc6, 0xa4, 140])],
                glass: [0xf6, 0xf9, 0xee],
                accent: rgb(0x46, 0x61, 0x2d),
            },
        }
    }
}

static FLAVOR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn flavor() -> Flavor {
    Flavor::DARK.iter().chain(&Flavor::LIGHT).nth(FLAVOR.load(std::sync::atomic::Ordering::Relaxed) as usize).copied().unwrap_or(Flavor::Midnight)
}

/// The current flavour's accent colour.
pub fn accent() -> Color32 {
    flavor().raw_palette().accent
}
/// Added, deleted, modified (also warnings) and renamed: bright on dark glass, deep on light.
fn change_colors(dark: bool) -> [Color32; 4] {
    if dark {
        [Color32::from_rgb(0x34, 0xc7, 0x59), Color32::from_rgb(0xff, 0x45, 0x3a), Color32::from_rgb(0xff, 0x9f, 0x0a), Color32::from_rgb(0x64, 0xd2, 0xff)]
    } else {
        [Color32::from_rgb(0x1a, 0x7f, 0x37), Color32::from_rgb(0xc4, 0x1d, 0x29), Color32::from_rgb(0x9a, 0x5b, 0x00), Color32::from_rgb(0x09, 0x63, 0xc9)]
    }
}
pub fn add() -> Color32 {
    change_colors(flavor().dark())[0]
}
pub fn del() -> Color32 {
    change_colors(flavor().dark())[1]
}
pub fn changed() -> Color32 {
    change_colors(flavor().dark())[2]
}

pub fn change_color(code: char) -> Color32 {
    let [add, del, changed, ren] = change_colors(flavor().dark());
    match code {
        'A' | '?' => add,
        'D' | 'U' => del,
        'R' | 'C' => ren,
        _ => changed,
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
/// Text colour that reads on a solid fill of `c`: white or dark ink, whichever contrasts more.
pub fn on_color(c: Color32) -> Color32 {
    let ink = Color32::from_rgb(0x10, 0x14, 0x1c);
    if contrast(Color32::WHITE, c) >= contrast(ink, c) { Color32::WHITE } else { ink }
}

/// WCAG 2 relative luminance of an opaque colour.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2 contrast ratio, 1.0 ..= 21.0.
fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub fn hover(ui: &Ui) -> Color32 {
    let a = accent();
    if dark(ui) { Color32::from_white_alpha(10) } else { Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 22) }
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
            "cjk",
            &[
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "C:\\Windows\\Fonts\\msyh.ttc",
                "C:\\Windows\\Fonts\\YuGothM.ttc",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
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
            if name == "cjk" {
                // A fallback only: after every other font, in both families.
                for f in [FontFamily::Proportional, FontFamily::Monospace] {
                    fonts.families.entry(f).or_default().push(name.into());
                }
            } else {
                fonts.families.entry(family).or_default().insert(0, name.into());
            }
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

fn visuals(f: Flavor) -> Visuals {
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
    v.hyperlink_color = accent;
    v.weak_text_alpha = if light { 0.8 } else { 0.7 };
    if !light {
        v.override_text_color = None;
        v.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0xd5, 0xda, 0xe3);
        v.widgets.inactive.fg_stroke.color = Color32::from_rgb(0xe6, 0xe9, 0xef);
    } else {
        // Darker than egui's grey: tinted glass needs it for dimmed text to stay readable.
        v.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0x34, 0x38, 0x40);
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
    v
}

pub fn apply(ctx: &egui::Context, f: Flavor) {
    FLAVOR.store([Flavor::DARK, Flavor::LIGHT].concat().iter().position(|x| *x == f).unwrap_or(0) as u8, std::sync::atomic::Ordering::Relaxed);
    let theme = if f.dark() { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals(f));
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
        s.spacing.icon_width_inner = 7.0;
        s.spacing.icon_spacing = 8.0;
        s.spacing.menu_margin = egui::Margin::symmetric(12, 8);
        s.spacing.window_margin = egui::Margin::same(22);
        s.interaction.tooltip_delay = 0.2;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `top` (premultiplied, as Color32 stores it) over an opaque `under`.
    fn over(top: Color32, under: Color32) -> Color32 {
        let k = 1.0 - top.a() as f32 / 255.0;
        let ch = |t: u8, u: u8| (t as f32 + u as f32 * k).round().min(255.0) as u8;
        Color32::from_rgb(ch(top.r(), under.r()), ch(top.g(), under.g()), ch(top.b(), under.b()))
    }

    /// Panel backgrounds a flavour's text can land on: its glass cards (sidebar 150,
    /// graph 205, at the default 25% transparency) over the bare base and over each
    /// glow blob's brightest point.
    fn panels(f: Flavor) -> Vec<Color32> {
        let p = f.palette();
        let backdrops = std::iter::once(p.base).chain(p.blobs.iter().map(|(_, _, [r, g, b, a])| over(Color32::from_rgba_unmultiplied(*r, *g, *b, *a), p.base)));
        let [r, g, b] = p.glass;
        backdrops
            .flat_map(|d| [150.0, 205.0].map(|a: f32| over(Color32::from_rgba_unmultiplied(r, g, b, (a * (1.0 - 0.85 * 0.25)).round() as u8), d)))
            .collect()
    }

    /// Lowest WCAG 2 contrast of each check on every flavour; fails below the minimum.
    /// `cargo test flavours -- --nocapture` prints the table.
    #[test]
    fn flavours_have_readable_contrast() {
        let mut failures = vec![];
        for f in [Flavor::DARK, Flavor::LIGHT].concat() {
            let v = visuals(f);
            let accent = f.palette().accent;
            let sel = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), if f.dark() { 70 } else { 45 }); // as `selection`
            let mut lowest: Vec<(&str, f32, f32)> = vec![];
            let mut check = |name, min, fg: Color32, bg: Color32| {
                let r = contrast(over(fg, bg), bg);
                match lowest.iter_mut().find(|(n, ..)| *n == name) {
                    Some(l) => l.2 = l.2.min(r),
                    None => lowest.push((name, min, r)),
                }
            };
            for bg in panels(f) {
                check("text", 4.5, v.text_color(), bg);
                check("strong", 4.5, v.strong_text_color(), bg);
                check("dim", 4.5, v.weak_text_color(), bg);
                check("accent", 4.5, accent, bg);
                check("link", 4.5, v.hyperlink_color, bg);
                check("on accent", 4.5, on_color(accent), accent);
                for (n, c) in ["add", "del", "mod", "ren"].into_iter().zip(change_colors(f.dark())) {
                    check(n, 3.0, c, bg);
                }
                check("selection", 4.5, v.text_color(), over(sel, bg));
                for c in LANES {
                    check("lane", 3.0, c, bg);
                    check("on lane", 4.5, Color32::WHITE, c);
                }
            }
            let worst = lowest.iter().min_by(|a, b| (a.2 / a.1).total_cmp(&(b.2 / b.1))).unwrap();
            eprintln!("{:<9} lowest {:<10} {:.2}   {}", f.name(), worst.0, worst.2, lowest.iter().map(|(n, _, r)| format!("{n} {r:.1}")).collect::<Vec<_>>().join(", "));
            failures.extend(lowest.iter().filter(|(_, min, r)| r < min).map(|(n, min, r)| format!("{} {n}: {r:.2} < {min}", f.name())));
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_accent_is_never_toned() {
        for f in [Flavor::DARK, Flavor::LIGHT].concat() {
            assert_eq!(f.raw_palette().accent, f.palette().accent, "{}", f.name());
        }
    }
}
