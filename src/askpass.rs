//! The askpass helper: git and ssh run Glasir as `GIT_ASKPASS` / `SSH_ASKPASS`
//! with the prompt as argument; this tiny window answers it.

/// Tiny credential window: prints the answer on stdout, exit code 1 on cancel.
pub fn run(prompt: String) -> eframe::Result {
    let lower = prompt.to_lowercase();
    let secret = ["password", "passphrase", "token", "pin"].iter().any(|k| lower.contains(k));
    let mut answer = String::new();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([440.0, 150.0]).with_title("Glasir — Git credentials"),
        ..Default::default()
    };
    let res = eframe::run_ui_native("Glasir askpass", opts, move |ui, _| {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(prompt.trim());
            ui.add(egui::TextEdit::singleline(&mut answer).password(secret).desired_width(f32::INFINITY)).request_focus();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    println!("{answer}");
                    std::process::exit(0);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    std::process::exit(1);
                }
            });
        });
    });
    res?;
    std::process::exit(1)
}
