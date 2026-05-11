#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Glasir entry point: login-env import, askpass dispatch, eframe launch.
//! Layers (lowest first): `git`, `graph`/`highlight`, `ai`, `repo`, `theme`, `ui`.

mod ai;
mod askpass;
mod git;
mod graph;
mod highlight;
mod repo;
mod theme;
mod ui;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let arg = std::env::args().nth(1);
    // git and ssh run us as GIT_ASKPASS / SSH_ASKPASS with the prompt as argument.
    if std::env::var_os("GLASIR_ASKPASS").is_some() {
        return askpass::run(arg.unwrap_or_default());
    }
    // Settings, open tabs and the fallback token lived under the former name.
    if let (Some(new), Some(old)) = (eframe::storage_dir("Glasir"), eframe::storage_dir("Cuttle"))
        && !new.exists()
        && old.exists()
    {
        let _ = std::fs::rename(old, new);
    }
    #[cfg(unix)]
    git::import_login_env();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 880.0])
            .with_min_inner_size([900.0, 520.0])
            .with_title("Glasir")
            .with_active(std::env::var_os("GLASIR_SCREENSHOT").is_none() && std::env::var_os("GLASIR_BACKGROUND").is_none())
            // macOS: unified title bar, content (and glass) runs under the traffic lights.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native("Glasir", opts, Box::new(move |cc| Ok(Box::new(ui::App::new(cc, arg.map(PathBuf::from))))))
}
