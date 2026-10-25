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
mod update;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1);
    // git and ssh run us as GIT_ASKPASS / SSH_ASKPASS with the prompt as the only argument.
    if std::env::var_os("GLASIR_ASKPASS").is_some()
        && let Some(prompt) = arg.as_ref().filter(|p| std::env::args_os().len() == 2 && !p.as_encoded_bytes().starts_with(b"-") && !std::path::Path::new(p).exists())
    {
        return askpass::run(prompt.to_string_lossy().into_owned());
    }
    // Settings, open tabs and the fallback token lived under the former name.
    if let (Some(new), Some(old)) = (eframe::storage_dir("Glasir"), eframe::storage_dir("Cuttle"))
        && !new.exists()
        && old.exists()
    {
        let _ = std::fs::rename(old, new);
    }
    // Linux: `Glasir --install` (the README's install command) registers the launcher entry and
    // icons without opening a window; every start refreshes them.
    #[cfg(target_os = "linux")]
    {
        let install = arg.as_deref() == Some("--install".as_ref());
        desktop_entry(install);
        if install {
            return Ok(());
        }
    }
    #[cfg(unix)]
    git::import_login_env();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([std::env::var("GLASIR_SCREENSHOT_WIDTH").ok().and_then(|w| w.parse().ok()).unwrap_or(1440.0), 880.0])
            .with_min_inner_size([900.0, 520.0])
            .with_title("Glasir")
            .with_icon(eframe::icon_data::from_png_bytes(ICON).expect("bundled icon is a valid PNG"))
            .with_app_id("Glasir")
            .with_active(std::env::var_os("GLASIR_SCREENSHOT").is_none() && std::env::var_os("GLASIR_BACKGROUND").is_none())
            // macOS: unified title bar, content (and glass) runs under the traffic lights.
            .with_fullsize_content_view(true)
            .with_transparent(cfg!(target_os = "macos"))
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native("Glasir", opts, Box::new(move |cc| Ok(Box::new(ui::App::new(cc, arg.map(PathBuf::from))))))
}

const ICON: &[u8] = include_bytes!("../assets/icon-512.png");

/// Linux: a desktop entry and icon in the user's data dir, so launchers and Wayland docks (which
/// ignore the window icon) show Glasir's; rewritten when the binary has moved. A Linux binary has
/// no icon of its own, so the file manager (GNOME Files) gets one as the file's custom icon.
/// Nothing for the .deb's /usr/bin/glasir: the package ships both. `wait`: until `gio` is done.
#[cfg(target_os = "linux")]
fn desktop_entry(wait: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    if exe.starts_with("/usr") {
        return;
    }
    let Some(data) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))) else { return };
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Glasir\nComment=Git client\nExec=\"{}\"\nIcon=glasir\nTerminal=false\nCategories=Development;RevisionControl;\nStartupWMClass=Glasir\n",
        exe.display()
    );
    let file = data.join("applications/Glasir.desktop");
    let icon = data.join("icons/hicolor/512x512/apps/glasir.png");
    if !std::fs::read_to_string(&file).is_ok_and(|s| s == entry) || !icon.exists() {
        for (path, bytes) in [(&icon, ICON), (&file, entry.as_bytes())] {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&data)).and_then(|_| std::fs::write(path, bytes));
        }
    }
    let uri = icon.to_string_lossy().bytes().fold(String::from("file://"), |mut u, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => u.push(b as char),
            _ => u.push_str(&format!("%{b:02X}")),
        }
        u
    });
    let gio = move || {
        let _ = std::process::Command::new("gio").arg("set").arg(&exe).arg("metadata::custom-icon").arg(uri).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    };
    match wait {
        true => gio(),
        false => drop(std::thread::spawn(gio)),
    }
}
