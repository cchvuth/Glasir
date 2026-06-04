//! The keyboard-shortcut cheat sheet (`?` or ⌘/). `SHORTCUTS` is the one table:
//! the README's shortcut table is checked against it by a test.

use crate::theme::{self, icon};
use egui::{Id, Key, RichText};

/// (keys, what they do), in macOS notation; `keys` converts for Linux / Windows.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘P / ⌘K", "Command palette"),
    ("? / ⌘/", "Keyboard shortcuts"),
    ("⌘F", "Search commits"),
    ("⌘Z / ⇧⌘Z", "Undo / redo"),
    ("⌘↩", "Commit"),
    ("⇧⌘S / ⇧⌘U", "Stage all / unstage all"),
    ("⌘L", "Fetch"),
    ("⇧⌘L", "Command log"),
    ("↑ ↓ / J K", "Move through commits"),
    ("⌘-click / ⇧-click", "Select several commits"),
    ("⌘O / ⌘T / ⌘W", "Open repo / new tab / close tab"),
    ("⌃Tab", "Next tab"),
    ("⌘,", "Settings"),
    ("F5 / ⌘R", "Refresh"),
    ("Esc", "Close diff / editor"),
    ("F7 / ⇧F7", "Next / previous change in a diff"),
];

/// Interactive rebase editor keys.
pub const REBASE: &str = "P pick · R reword · S squash · F fixup · D drop · ⌥↑ / ⌥↓ move";

/// Ctrl / Shift / Alt spelled out off macOS.
fn keys(k: &str) -> String {
    if cfg!(target_os = "macos") {
        return k.to_owned();
    }
    k.replace("⇧⌘", "Ctrl+Shift+").replace('⌘', "Ctrl+").replace('⇧', "Shift+").replace('⌥', "Alt+").replace('⌃', "Ctrl+")
}

/// Draws the cheat sheet; returns false once closed.
pub(super) fn show(ctx: &egui::Context) -> bool {
    let mut open = true;
    let r = egui::Modal::new(Id::new("shortcuts")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.horizontal(|ui| {
            ui.heading(format!("{} Keyboard shortcuts", icon::KEYBOARD));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
                    open = false;
                }
            });
        });
        ui.add_space(8.0);
        egui::Grid::new("shortcut_grid").num_columns(2).spacing([28.0, 7.0]).min_col_width(150.0).show(ui, |ui| {
            for (k, what) in SHORTCUTS {
                ui.label(RichText::new(keys(k)).strong().color(theme::accent()));
                ui.label(*what);
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new(format!("Interactive rebase: {}", keys(REBASE))).small().color(theme::dim(ui)));
        ui.label(RichText::new("Everything else is in the command palette (⌘P) and the right-click menus.").small().color(theme::dim(ui)));
    });
    open && !r.should_close() && !ctx.input(|i| i.key_pressed(Key::Escape))
}

#[cfg(test)]
mod tests {
    #[test]
    fn readme_lists_every_shortcut() {
        let readme = include_str!("../../README.md");
        for (k, what) in super::SHORTCUTS {
            assert!(readme.contains(&format!("| {} | {what} |", k)), "README is missing `| {k} | {what} |`");
        }
    }
}
