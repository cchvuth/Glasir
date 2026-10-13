//! The connection doctor window: whether git reaches the remote without
//! prompting, what is wrong and how to fix it, and the settings git uses.

use crate::repo::doctor::Doctor;
use crate::repo::sshkey::{self, Active};
use crate::repo::{Action, RepoTab};
use crate::theme::{self, icon};
use crate::ui::format::dismissed;
use egui::{Frame, Id, RichText};

pub(super) fn doctor_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(doc) = &t.doctor else { return };
    let mut close = false;
    egui::Modal::new(Id::new("doctor")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.heading(format!("{}  Connection to {}", icon::STETHOSCOPE, doc.remote));
        let Some(r) = &doc.report else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Running git ls-remote without prompting…").color(theme::dim(ui)));
            });
            close = dismissed(ui, "Close");
            return;
        };
        ui.label(RichText::new(&r.url).monospace().small().color(theme::dim(ui)));
        ui.add_space(6.0);
        let host = crate::git::host(&r.url).unwrap_or_else(|| "<host>".into());
        match r.problem {
            None => {
                ui.label(RichText::new(format!("{}  Connected: git can fetch from {} without asking for anything.", icon::CHECK_CIRCLE, doc.remote)).color(theme::add()).strong());
            }
            Some((what, hint)) => {
                ui.label(RichText::new(format!("{}  {what}", icon::WARNING)).color(theme::del()).strong());
                ui.label(hint.replace("<host>", &host));
            }
        }
        if !r.output.is_empty() && r.problem.is_some() {
            ui.add_space(4.0);
            Frame::new().fill(theme::faint(ui)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(110.0).auto_shrink([false, true]).show(ui, |ui| ui.label(RichText::new(&r.output).monospace().size(11.5)));
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("What git uses for this remote").small().strong());
        egui::Grid::new("doctor_settings").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
            for (k, v) in &r.settings {
                ui.label(RichText::new(k).monospace().small());
                ui.label(RichText::new(v).monospace().small());
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ssh_key_section(ui, t, doc, &r.url, acts);
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Check again", icon::ARROWS_CLOCKWISE)).clicked() {
                acts.push(Action::Doctor(doc.remote.clone()));
            }
            if ui.button(format!("{}  Copy report", icon::COPY)).clicked() {
                let mut s = format!("{}  {}\n{}\n\n{}\n", doc.remote, r.url, r.problem.map_or("Connected", |p| p.0), r.output);
                for (k, v) in &r.settings {
                    s += &format!("{k} = {v}\n");
                }
                acts.push(Action::Copy(s));
            }
            close = dismissed(ui, "Close");
        });
    });
    if close {
        t.doctor = None;
    }
}

fn ssh_key_section(ui: &mut egui::Ui, t: &RepoTab, doc: &Doctor, url: &str, acts: &mut Vec<Action>) {
    ui.label(RichText::new("SSH key for this repository").small().strong());
    if !sshkey::is_ssh(url) {
        ui.label(RichText::new(format!("{} uses HTTPS: the key setting only affects SSH remotes.", doc.remote)).small().color(theme::dim(ui)));
        ui.add_space(8.0);
        return;
    }
    let active = sshkey::parse(t.data.as_ref().and_then(|d| d.cfg("core.sshcommand")));
    let mut pick = |ui: &mut egui::Ui, on: bool, label: String, sub: &str, key: Option<String>| {
        let r = ui.add(egui::Button::selectable(on, label)).on_hover_text(sub);
        if !sub.is_empty() {
            ui.label(RichText::new(sub).monospace().small().color(theme::dim(ui)));
        }
        if r.clicked() && !on {
            acts.push(Action::SshKey(key));
        }
    };
    egui::ScrollArea::vertical().max_height(150.0).auto_shrink([false, true]).id_salt("ssh_keys").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| pick(ui, active == Active::Default, "Default (ssh-agent / ~/.ssh/config)".into(), "", None));
        for k in &doc.keys {
            let on = active == Active::Ours(k.path.clone());
            ui.horizontal_wrapped(|ui| pick(ui, on, format!("{}  {}", icon::KEY, k.name), &k.fingerprint, Some(k.path.clone())));
        }
        if let Active::Ours(p) = &active
            && !doc.keys.iter().any(|k| &k.path == p)
        {
            ui.label(RichText::new(format!("{}  {p}", icon::KEY)).monospace().small().color(theme::accent()));
        }
    });
    match &active {
        Active::Default => ui.label(RichText::new("Active: default (ssh-agent / ~/.ssh/config)").small().color(theme::dim(ui))),
        Active::Ours(p) => ui.label(RichText::new(format!("Active: {p}")).small().color(theme::accent())),
        Active::Custom(c) => ui.label(RichText::new(format!("Active: custom core.sshCommand = {c}")).small().color(theme::changed())),
    };
    ui.horizontal(|ui| {
        if ui.button(format!("{}  Browse…", icon::FOLDER_OPEN)).clicked()
            && let Some(p) = rfd::FileDialog::new().set_title("Choose a private key").set_directory(sshkey::home().unwrap_or_default().join(".ssh")).pick_file()
        {
            acts.push(Action::SshKey(Some(p.to_string_lossy().into_owned())));
        }
        if doc.testing.is_some() {
            ui.spinner();
        } else if ui.button(format!("{}  Test connection", icon::PLUGS_CONNECTED)).on_hover_text("ssh -T with this key, never prompting").clicked() {
            acts.push(Action::SshTest);
        }
    });
    if let Some((ok, text)) = &doc.tested {
        let (ic, col) = if *ok { (icon::CHECK_CIRCLE, theme::add()) } else { (icon::WARNING, theme::del()) };
        ui.label(RichText::new(format!("{ic}  {}", text.lines().next().unwrap_or("No output"))).small().color(col));
    }
    ui.add_space(8.0);
}
