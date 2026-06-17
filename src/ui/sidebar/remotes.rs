//! A remote's right-click menu (fetch, prune, pull requests, doctor, edit, remove)
//! and the add / edit remote dialog.

use crate::git;
use crate::repo::remotes::{self, Form};
use crate::repo::{Action, RepoData, RepoTab, op, prompt};
use crate::theme::{self, icon};
use egui::{Id, Key, RichText, TextEdit};

pub fn remote_menu(ui: &mut egui::Ui, d: &RepoData, name: &str, acts: &mut Vec<Action>) {
    if ui.button(format!("{} Fetch {name}", icon::CLOUD_ARROW_DOWN)).clicked() {
        acts.push(op(format!("Fetch {name}"), &["fetch", "--progress", name]));
    }
    if ui.button(format!("{} Prune deleted branches", icon::BROOM)).on_hover_text(format!("Drop remote-tracking branches that no longer exist on {name}")).clicked() {
        acts.push(op(format!("Prune {name}"), &["remote", "prune", name]));
    }
    ui.separator();
    if ui.button(format!("{} Check out pull request #…", icon::GIT_PULL_REQUEST)).on_hover_text("Fetches refs/pull/<n>/head into pr/<n> and checks it out (github.com and GitHub Enterprise)").clicked() {
        acts.push(prompt(format!("Pull request number on {name}"), "", remotes::pr_checkout(name)));
    }
    let mut on = remotes::pr_refs_on(d, name);
    let clash = remotes::pr_refs_clash(d, name);
    let tip = clash.map_or("Fetch every pull request's head too (+refs/pull/*/head); turning it off deletes them again".into(), |b| format!("Not possible: the branch {b} would collide with them"));
    if ui.add_enabled(clash.is_none(), egui::Checkbox::new(&mut on, format!("Show pull requests as {name}/pr/<n>"))).on_hover_text(tip.as_str()).on_disabled_hover_text(tip.as_str()).clicked() {
        acts.push(Action::Git(remotes::pr_refs_op(d, name)));
    }
    ui.separator();
    if ui.button(format!("{} Check connection…", icon::STETHOSCOPE)).on_hover_text("Test sign-in, SSO, SSH, proxy and certificates for this remote").clicked() {
        acts.push(Action::Doctor(name.into()));
    }
    if ui.button(format!("{} Edit remote…", icon::PENCIL_SIMPLE)).on_hover_text("Rename it or change its fetch / push URL").clicked() {
        acts.push(Action::RemoteForm(Form::edit(d, name)));
    }
    if ui.button(format!("{} Remove remote…", icon::TRASH)).clicked() {
        acts.push(Action::Git(remotes::remove(d, name)));
    }
    ui.separator();
    let url = d.remotes.iter().find(|r| r.0 == name).map_or("", |r| r.1.as_str());
    if let (Some(web), Some(host)) = (git::web_url(url), git::host(url))
        && ui.button(format!("{} Open on {host}", icon::GLOBE)).clicked()
    {
        acts.push(Action::Url(web));
    }
    if ui.button(format!("{} Copy URL", icon::COPY)).clicked() {
        acts.push(Action::Copy(url.into()));
    }
}

/// Add / edit remote: name, fetch URL and an optional separate push URL.
pub(in crate::ui) fn form_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let (Some(f), Some(d)) = (&mut t.remote_form, &t.data) else { return };
    let mut done = None;
    egui::Modal::new(Id::new("remote_form")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading(match &f.old {
            Some((old, ..)) => format!("Edit remote {old}"),
            None => "Add remote".into(),
        });
        ui.add_space(6.0);
        egui::Grid::new("remote_form_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            let name = ui.add(TextEdit::singleline(&mut f.name).margin(crate::theme::INPUT_MARGIN).hint_text("upstream").desired_width(f32::INFINITY));
            if f.old.is_none() && ui.memory(|m| m.focused().is_none()) {
                name.request_focus();
            }
            ui.end_row();
            ui.label("URL");
            ui.add(TextEdit::singleline(&mut f.url).margin(crate::theme::INPUT_MARGIN).hint_text("https://ghe.corp.example/org/repo.git or git@…").desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Push URL");
            ui.add(TextEdit::singleline(&mut f.push).margin(crate::theme::INPUT_MARGIN).hint_text("same as URL").desired_width(f32::INFINITY));
            ui.end_row();
        });
        if d.remotes.is_empty() && f.old.is_none() {
            ui.label(RichText::new("Publishing? Create an empty repository on GitHub or GitHub Enterprise first (no README), then paste its URL. Push sends your branch there.").small().color(theme::dim(ui)));
        }
        let problem = f.problem(d);
        let note = problem.map(String::from).or_else(|| git::web_url(f.url.trim()).map(|w| format!("{} {w}", icon::GLOBE)));
        if let Some(n) = note {
            ui.label(RichText::new(n).small().color(theme::dim(ui)));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if f.old.is_some() { "Save" } else { "Add and fetch" };
            if theme::primary(ui, problem.is_none(), label, 140.0).clicked() || (problem.is_none() && ui.input(|i| i.key_pressed(Key::Enter))) {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                done = Some(false);
            }
        });
    });
    if let Some(ok) = done {
        let f = t.remote_form.take().unwrap();
        if let Some(o) = f.op().filter(|_| ok) {
            acts.push(Action::Git(o));
        }
    }
}
