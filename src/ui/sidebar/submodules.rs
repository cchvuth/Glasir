//! The SUBMODULES section of the sidebar: one row per submodule (state badges,
//! recorded vs checked-out commit) with its menu, then update-all / sync / add.

use super::{matches, section};
use crate::git::Submodule;
use crate::repo::{Action, Op, RepoData, op, prompt};
use crate::theme::{self, icon};
use egui::{RichText, Ui};

pub(super) fn list(ui: &mut Ui, d: &RepoData, f: &str, acts: &mut Vec<Action>) {
    let subs: Vec<_> = d.submodules.iter().filter(|s| matches(f, &s.path)).collect();
    section(ui, "SUBMODULES", subs.len(), |ui| {
        for s in subs {
            row(ui, d, s, acts);
        }
        ui.horizontal_wrapped(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend); // whole buttons; the row wraps instead
            if !d.submodules.is_empty() {
                if theme::small(ui, true, format!("{} Update all", icon::ARROWS_CLOCKWISE)).on_hover_text("Initialize and check out the recorded commits, recursively").clicked() {
                    acts.push(op("Update submodules", &["submodule", "update", "--init", "--recursive"]));
                }
                if theme::small(ui, true, "Sync URLs").on_hover_text("Copy URLs from .gitmodules into the config").clicked() {
                    acts.push(op("Sync submodule URLs", &["submodule", "sync", "--recursive"]));
                }
            }
            if theme::small(ui, true, format!("{} Add submodule…", icon::PLUS)).on_hover_text("Add a submodule (its folder is named after the URL)").clicked() {
                acts.push(prompt("Add submodule: repository URL", "", Op::new("Add submodule", &["submodule", "add", "--", "{}"])));
            }
        });
    });
}

fn row(ui: &mut Ui, d: &RepoData, s: &Submodule, acts: &mut Vec<Action>) {
    // Porcelain v2 `S<c><m><u>` (top-level submodules with changes only).
    let st = d.status.submodules.get(&s.path).map_or("", String::as_str);
    let short = |id: &str| id[..id.len().min(7)].to_owned();
    let (init, moved) = (s.flag != '-', s.flag == '+' || st.contains('C'));
    let mut label = format!("{} {}", icon::PACKAGE, s.path);
    let mut tip = vec![format!("recorded {}", short(&s.recorded))];
    if !init {
        label += &format!("  {}", icon::CIRCLE_DASHED);
        tip.push("not initialized".into());
    } else if moved {
        label += &format!("  {}", icon::GIT_DIFF);
        tip.push(format!("checked out {} (differs from the recorded commit)", short(&s.head)));
    }
    if st.contains('M') {
        label += &format!("  {}", icon::PENCIL_SIMPLE);
        tip.push("modified content".into());
    }
    if st.contains('U') {
        label += &format!("  {}", icon::QUESTION);
        tip.push("untracked content".into());
    }
    if s.flag == 'U' {
        label += &format!("  {}", icon::WARNING);
        tip.push("merge conflict".into());
    }
    let text = match (init, moved) {
        (false, _) => RichText::new(label).color(theme::dim(ui)),
        (true, true) => RichText::new(label).color(theme::MOD),
        _ => RichText::new(label),
    };
    let resp = ui.add(egui::Button::selectable(false, text).frame_when_inactive(false)).on_hover_text(tip.join("\n"));
    if resp.double_clicked() && init {
        acts.push(Action::OpenTab(s.path.clone()));
    }
    // git submodule commands take top-level paths; nested ones are reached with --recursive.
    let nested = d.submodules.iter().any(|o| s.path.starts_with(&format!("{}/", o.path)));
    resp.context_menu(|ui| {
        if !nested {
            let (label, args): (&str, &[&str]) =
                if init { ("Update (recursive)", &["--literal-pathspecs", "submodule", "update", "--recursive", "--", &s.path]) } else { ("Initialize & update", &["--literal-pathspecs", "submodule", "update", "--init", "--recursive", "--", &s.path]) };
            if ui.button(label).on_hover_text("Check out the recorded commit").clicked() {
                acts.push(Action::Git(Op::new(format!("{label} {}", s.path), args)));
            }
        }
        if init && ui.button("Open in new tab").clicked() {
            acts.push(Action::OpenTab(s.path.clone()));
        }
        if ui.button("Show in file manager").clicked() {
            acts.push(Action::Reveal(s.path.clone()));
        }
        if ui.button("Copy path").clicked() {
            acts.push(Action::Copy(s.path.clone()));
        }
        if !nested {
            ui.separator();
            if ui.button("Remove submodule").clicked() {
                acts.push(Action::Git(remove(&s.path)));
            }
        }
    });
}

// Literal pathspecs: a glob-y path like `lib[2]` must not also deinit/rm `lib2`.
fn remove(path: &str) -> Op {
    Op::new(format!("Remove submodule {path}"), &["--literal-pathspecs", "submodule", "deinit", "-f", "--", path])
        .then(&["--literal-pathspecs", "rm", "-f", "--", path])
        .confirm(format!("Remove submodule {path}? Its folder (and any uncommitted work in it) is deleted; the removal is staged for the next commit."))
}

#[cfg(test)]
mod tests {
    /// Real git: removing submodule `lib[2]` leaves the tracked file `lib2` (and its edits) alone.
    #[test]
    fn remove_matches_path_literally() {
        use crate::git::run;
        let root = std::env::temp_dir().join(format!("glasir-subremove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (lib, app) = (root.join("lib"), root.join("app"));
        let g = |dir: &std::path::Path, args: &[&str]| {
            let mut a = vec!["-c", "protocol.file.allow=always", "-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false"];
            a.extend_from_slice(args);
            run(dir, &a).unwrap_or_else(|e| panic!("{args:?}: {e}"))
        };
        for d in [&lib, &app] {
            std::fs::create_dir_all(d).unwrap();
            g(d, &["init", "-q"]);
            std::fs::write(d.join("f"), "a\n").unwrap();
            g(d, &["add", "f"]);
            g(d, &["commit", "-q", "-m", "f"]);
        }
        g(&app, &["submodule", "add", "-q", lib.to_str().unwrap(), "lib[2]"]);
        std::fs::write(app.join("lib2"), "a\n").unwrap();
        g(&app, &["add", "lib2"]);
        g(&app, &["commit", "-q", "-m", "sub"]);
        std::fs::write(app.join("lib2"), "WORK\n").unwrap();
        for cmd in super::remove("lib[2]").cmds {
            g(&app, &cmd.iter().map(String::as_str).collect::<Vec<_>>());
        }
        assert_eq!(std::fs::read_to_string(app.join("lib2")).unwrap(), "WORK\n");
        assert_eq!(g(&app, &["status", "--porcelain", "--", "lib2"]), " M lib2\n");
        assert!(!app.join("lib[2]").join("f").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
