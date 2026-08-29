//! "Open on the remote" / "Copy link" menu entries for pages of the hosting site.

use crate::git::Page;
use crate::repo::{Action, RepoData};
use crate::theme::icon;
use egui::Ui;

/// Open / copy entries for each `(what, page)`: flat for one page on one remote, else a submenu
/// with the entries of every remote. `shas` are the commits the pages need on the remote: when one
/// is not pushed there, Open is disabled and Copy warns (the link would 404).
pub fn items(ui: &mut Ui, d: &RepoData, pages: &[(&str, Page)], shas: &[&str], acts: &mut Vec<Action>) {
    let webs = d.webs();
    let draw = |ui: &mut Ui, acts: &mut Vec<Action>| {
        for (remote, web) in &webs {
            let at = if webs.len() == 1 { crate::git::host(web).unwrap_or_default() } else { remote.to_string() };
            let missing = shas.iter().any(|s| !d.on_remote(ui.ctx(), s, remote));
            let warn = format!("Not on {remote} yet (no remote-tracking branch contains it): the link would 404. Push first.");
            for (what, page) in pages {
                let url = page.url(web);
                let open = ui.add_enabled(!missing, egui::Button::new(format!("{} Open {what} on {at}", icon::GLOBE)));
                if open.on_hover_text(&url).on_disabled_hover_text(&warn).clicked() {
                    acts.push(Action::Url(url.clone()));
                }
                let copy = ui.button(format!("{} Copy link to {what}", icon::LINK));
                if (if missing { copy.on_hover_text(&warn) } else { copy.on_hover_text(&url) }).clicked() {
                    acts.push(Action::Copy(url));
                }
            }
        }
    };
    if webs.len() == 1 && pages.len() == 1 {
        draw(ui, acts);
    } else if !webs.is_empty() {
        ui.menu_button(format!("{} On remote", icon::GLOBE), |ui| draw(ui, acts));
    }
}
