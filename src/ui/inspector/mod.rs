//! Right-hand inspector: what the graph selection shows. `commit` (commit or
//! compare details and files), `wip` (staging lists + commit box), `multi`
//! (several selected commits). File rows and their shared menu entries live here.

mod commit;
mod multi;
pub mod tree;
mod wip;

use crate::git::{Change, Page};
use crate::repo::history::Mode;
use crate::repo::{Action, Op, RepoData, RepoTab, Sel, tools};
use crate::theme::{self, icon};
use egui::{Sense, Ui};

impl RepoTab {
    pub fn details(&mut self, ui: &mut Ui, tree: &mut bool, acts: &mut Vec<Action>) {
        self.split_banner(ui, acts);
        if self.multi.len() > 1 {
            return self.multi_panel(ui, acts);
        }
        match self.sel.clone() {
            Some(Sel::Wip) => self.wip_panel(ui, tree, acts),
            Some(Sel::Commit(_) | Sel::Compare { .. }) => self.commit_panel(ui, tree, acts),
            // A clean working tree (e.g. right after committing everything): a calm note.
            None => {
                let clean = self.data.as_ref().is_some_and(|d| !d.wip);
                ui.add_space(ui.available_height() / 3.0);
                ui.vertical_centered(|ui| {
                    if clean {
                        theme::empty_state(ui, icon::CHECK_CIRCLE, "Nothing to commit", "Your working tree is clean. Select a commit to see its details.");
                    } else {
                        theme::empty_state(ui, icon::CURSOR_CLICK, "Select a commit", "Its details, files and diffs show here.");
                    }
                });
            }
        }
    }
}

/// History/Blame entries shared by every file context menu; `rev` blames that
/// revision (`None` = the working tree). `at`: the revision the history starts from (`None` = HEAD). `history`: the path the history is looked up by
/// (an uncommitted rename's old one: the new path has none yet).
pub fn file_items(ui: &mut Ui, path: &str, history: &str, at: Option<&str>, rev: Option<&str>, acts: &mut Vec<Action>) {
    let at = at.filter(|r| *r != crate::git::WORKTREE);
    if ui.button(format!("{}  File history", icon::CLOCK_COUNTER_CLOCKWISE)).clicked() {
        acts.push(Action::FileView(history.into(), Mode::History(at.map(Into::into))));
    }
    if let Some((dir, _)) = history.rsplit_once('/')
        && ui.button(format!("{}  Folder history", icon::FOLDER_SIMPLE)).on_hover_text(format!("Commits that touched {dir}/")).clicked()
    {
        acts.push(Action::FileView(format!("{dir}/"), Mode::History(at.map(Into::into))));
    }
    // A working-tree compare's "revision" is the file on disk: plain blame.
    let rev = rev.filter(|r| *r != crate::git::WORKTREE);
    let blame = match rev {
        Some(r) => format!("{}  Blame at {}", icon::USER_LIST, &r[..r.len().min(7)]),
        None => format!("{}  Blame", icon::USER_LIST),
    };
    if ui.button(blame).clicked() {
        acts.push(Action::FileView(path.into(), Mode::Blame(rev.map(Into::into))));
    }
}

/// "Open in editor" (unless the file is deleted there) and the custom commands that take a {path}.
pub fn open_items(ui: &mut Ui, d: &RepoData, sha: &str, path: &str, deleted: bool, acts: &mut Vec<Action>) {
    if !deleted && ui.button(format!("{}  Open in editor", icon::CODE)).on_hover_text("The working-tree file, in the editor from Settings → Tools").clicked() {
        acts.push(Action::Editor(path.into()));
    }
    let vars = tools::Vars { branch: d.status.branch.clone().unwrap_or_default(), sha: sha.into(), path: path.into() };
    custom_items(ui, tools::commands(Some("path")), &vars, acts);
}

/// Custom commands (Settings → Tools) as menu entries, run with `vars`.
pub fn custom_items(ui: &mut Ui, cmds: Vec<tools::Custom>, vars: &tools::Vars, acts: &mut Vec<Action>) {
    for c in cmds {
        if ui.button(format!("{}  {}", icon::TERMINAL, c.name)).on_hover_text(&c.command).clicked() {
            acts.push(Action::Custom(c, vars.clone()));
        }
    }
}

/// The file at commit `sha` (full SHA, so the links are permalinks), its blame and history on the remote.
pub fn permalink_items(ui: &mut Ui, d: &RepoData, sha: &str, path: &str, acts: &mut Vec<Action>) {
    let pages = [
        ("file", Page::File { rev: sha, path, lines: None }),
        ("blame", Page::Blame { rev: sha, path }),
        ("file history", Page::History { rev: sha, path }),
    ];
    crate::ui::links::items(ui, d, &pages, &[sha], acts);
}

/// "Track *.ext with LFS": adds the pattern to .gitattributes and stages it.
pub fn lfs_track_item(ui: &mut Ui, d: &RepoData, path: &str, acts: &mut Vec<Action>) {
    let Some(ext) = std::path::Path::new(path).extension().map(|e| e.to_string_lossy()) else { return };
    if d.lfs.files.contains(path) {
        return;
    }
    let pat = format!("*.{ext}");
    let b = ui.add_enabled(d.lfs.installed, egui::Button::new(format!("{}  Track {pat} with LFS", icon::DATABASE)));
    if b.on_hover_text("Adds the pattern to .gitattributes and stages it; matching files you add from now on are stored in LFS")
        .on_disabled_hover_text(crate::repo::LFS_MISSING)
        .clicked()
    {
        acts.push(Action::Git(Op::new(format!("Track {pat} with LFS"), &["lfs", "track", &pat]).then(&["add", ".gitattributes"])));
    }
}

pub const ROW_H: f32 = 24.0;

/// Pointer over `rect`; the screenshot harness can pin a pointer position (`row-hover:x,y`).
fn hovering(ui: &Ui, rect: egui::Rect) -> bool {
    ui.rect_contains_pointer(rect) || ui.data(|d| d.get_temp::<egui::Pos2>(egui::Id::new("shot_pointer"))).is_some_and(|p| rect.contains(p))
}

/// The hover button at a file row's right edge ("Stage" / "Unstage"): drawn only while the pointer
/// is over the row, true when clicked. Takes the click from the row beneath. Not the second or third
/// click of a double/triple-click: the first one's op may have slid the next file under the pointer.
pub fn row_button(ui: &Ui, row: &egui::Response, glyph: &str, label: &str) -> bool {
    if !hovering(ui, row.rect) {
        return false;
    }
    if ui.input(|i| i.is_scrolling()) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        return false;
    }
    let g = ui.painter().layout_no_wrap(format!("{glyph} {label}"), egui::FontId::proportional(11.5), egui::Color32::PLACEHOLDER);
    let rect = egui::Rect::from_center_size(egui::pos2(row.rect.right() - 6.0 - (g.size().x + 14.0) / 2.0, row.rect.center().y), g.size() + egui::vec2(14.0, 6.0));
    let resp = ui.interact(rect, row.id.with("quick"), Sense::click());
    let fill = theme::accent().gamma_multiply(if resp.hovered() { 1.0 } else { 0.8 });
    ui.painter().rect_filled(rect, 255.0, fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, egui::Color32::WHITE);
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    resp.clicked() && !resp.double_clicked() && !resp.triple_clicked()
}

/// A file row: colored status letter, file name (`old → new` for a rename), an LFS
/// badge, dimmed directory and, when known, the +/- line counts (`Some(None)`: binary).
/// In the folder tree (`depth` is `Some`) it is indented and has no directory: the folder rows give it.
pub fn file_row(ui: &mut Ui, c: &Change, selected: bool, lfs: bool, stat: Option<Option<(u32, u32)>>, depth: Option<usize>) -> egui::Response {
    let (dir, file) = c.path.rsplit_once('/').map_or(("", c.path.as_str()), |(d, f)| (d, f));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 8.0, theme::selection(ui));
    } else if hovering(ui, rect) {
        ui.painter().rect_filled(rect, 8.0, theme::hover(ui));
    }
    let y = rect.center().y;
    let x = rect.left() + tree::indent(depth);
    let mut right = rect.right() - 6.0;
    let counts = match stat {
        Some(Some((a, d))) => [(a, '+', theme::add()), (d, '-', theme::del())].into_iter().filter(|c| c.0 > 0).map(|(n, s, col)| (format!("{s}{n}"), col)).collect(),
        Some(None) => vec![("bin".to_owned(), theme::dim(ui))],
        None => vec![],
    };
    // Right to left, so "+a -d" reads in order.
    for (t, col) in counts.into_iter().rev() {
        right = ui.painter().text(egui::pos2(right, y), egui::Align2::RIGHT_CENTER, t, egui::FontId::monospace(11.0), col).left() - 6.0;
    }
    let p = ui.painter().with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(right, rect.max.y)));
    let file = &match &c.old_path {
        // Same folder: just the names, otherwise the old full path.
        Some(o) if o.rsplit_once('/').map_or("", |d| d.0) == dir => format!("{} → {file}", o.rsplit('/').next().unwrap_or(o)),
        Some(o) => format!("{o} → {file}"),
        None => file.to_owned(),
    };
    let code = if c.code == '?' { 'A' } else { c.code };
    p.text(egui::pos2(x + 6.0, y), egui::Align2::LEFT_CENTER, code, egui::FontId::monospace(12.0), theme::change_color(c.code));
    let mut r = p.text(egui::pos2(x + 22.0, y), egui::Align2::LEFT_CENTER, file, egui::FontId::proportional(13.0), theme::text(ui));
    if lfs {
        let g = p.layout_no_wrap("LFS".into(), egui::FontId::proportional(9.5), theme::accent());
        let badge = egui::Rect::from_min_size(egui::pos2(r.right() + 6.0, y - 7.0), g.size() + egui::vec2(8.0, 14.0 - g.size().y));
        p.rect_filled(badge, 4.0, theme::accent().gamma_multiply(0.18));
        p.galley(badge.center() - g.size() / 2.0, g, theme::accent());
        r = badge;
    }
    if !dir.is_empty() && depth.is_none() {
        p.text(egui::pos2(r.right() + 6.0, y), egui::Align2::LEFT_CENTER, dir, egui::FontId::proportional(11.5), theme::dim(ui));
    }
    resp.on_hover_text(match &c.old_path {
        Some(o) => format!("{o} → {}", c.path),
        None if lfs => format!("{}\nStored with Git LFS", c.path),
        None => c.path.clone(),
    })
}

#[cfg(test)]
mod tests {
    use crate::git::{Change, CommitInfo};
    use crate::repo::{Action, DiffSrc, RepoTab, Sel};
    use crate::ui::testkit::{Sim, load, tab};
    use egui::{Event, PointerButton, RawInput, pos2, vec2};

    fn wait_details(t: &mut RepoTab, ctx: &egui::Context, id: Option<String>) {
        if let Some(id) = id {
            t.select(ctx, Sel::Commit(id));
        }
        crate::repo::testutil::wait_for("the commit details", || {
            t.poll(ctx, &mut vec![]);
            t.info.is_some()
        });
    }

    #[test]
    fn each_tab_shows_its_own_commit_tree() {
        let mut sim = Sim::new(420.0, 600.0);
        let mut tabs = ["a.txt", "b.txt"].map(|f| {
            let mut t = tab(&sim.ctx, &format!("tree-{f}"), &["x"]);
            wait_details(&mut t, &sim.ctx, None);
            std::fs::write(t.dir.join(f), f).unwrap();
            for args in [&["add", f][..], &["commit", "-q", "-m", f]] {
                crate::git::run(&t.dir, args).unwrap();
            }
            t.reload(&sim.ctx, true);
            load(&mut t, &sim.ctx);
            let head = t.data.as_ref().and_then(|d| d.status.head.clone());
            wait_details(&mut t, &sim.ctx, head);
            t
        });
        let (mut tree, mut acts) = (true, vec![]);
        for t in &mut tabs {
            sim.frame(vec![], |ui| t.details(ui, &mut tree, &mut acts));
        }
        assert!(sim.text(|s| s == "b.txt").is_some() && sim.text(|s| s == "a.txt").is_none(), "the second tab shows the first one's files");
    }

    #[test]
    fn enter_in_a_summary_moves_on_to_the_description() {
        let mut sim = Sim::new(420.0, 700.0);
        let mut t = tab(&sim.ctx, "summary-enter", &["x"]);
        wait_details(&mut t, &sim.ctx, None);
        let enter = Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        let (mut tree, mut acts) = (false, vec![]);
        let mut type_in = |sim: &mut Sim, t: &mut RepoTab| {
            for events in [vec![], vec![enter.clone()], vec![], vec![Event::Text("b".into())]] {
                sim.frame(events, |ui| t.details(ui, &mut tree, &mut acts));
            }
        };
        let id = t.info.as_ref().and_then(|i| i.as_ref().ok()).map(|i| i.id.clone()).unwrap();
        t.edit_msg = Some(crate::repo::MsgEdit::new(&id, "x", (None, vec![])));
        type_in(&mut sim, &mut t);
        let e = t.edit_msg.as_ref().unwrap();
        assert_eq!((e.summary.as_str(), e.body.as_str()), ("x", "b"), "the message editor");
        std::fs::write(t.dir.join("f"), "changed").unwrap();
        t.reload(&sim.ctx, false);
        load(&mut t, &sim.ctx);
        (t.sel, t.focus_summary, t.summary) = (Some(Sel::Wip), true, "x".into());
        type_in(&mut sim, &mut t);
        assert_eq!((t.summary.as_str(), t.body.as_str()), ("x", "b"), "the commit box");
    }

    #[test]
    fn a_stash_row_diffs_against_the_list_it_is_in() {
        let mut sim = Sim::new(420.0, 600.0);
        let mut t = tab(&sim.ctx, "stash-both", &["x"]);
        std::fs::write(t.dir.join(".env"), "k").unwrap();
        for args in [&["add", ".env"][..], &["commit", "-q", "-m", "env"], &["rm", "-q", "--cached", ".env"]] {
            crate::git::run(&t.dir, args).unwrap();
        }
        std::fs::write(t.dir.join(".env"), "k2").unwrap();
        crate::git::run(&t.dir, &["stash", "-u"]).unwrap();
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let stash = t.data.as_ref().unwrap().stashes[0].id.clone();
        wait_details(&mut t, &sim.ctx, Some(stash.clone()));
        let (mut tree, mut acts) = (false, vec![]);
        sim.frame(vec![], |ui| t.details(ui, &mut tree, &mut acts));
        let at = sim.text(|s| s == ".env").expect("the tracked row");
        sim.click(at, PointerButton::Primary, |ui| t.details(ui, &mut tree, &mut acts));
        let diffs: Vec<_> = acts.iter().filter_map(|a| if let Action::Diff(DiffSrc::Commit { id, change, .. }) = a { Some((id.clone(), change.code)) } else { None }).collect();
        assert_eq!(diffs, [(stash, 'M')]);
    }

    #[test]
    fn a_menu_stays_with_its_commit_file_when_the_list_changes() {
        let mut sim = Sim::new(420.0, 600.0);
        let mut t = tab(&sim.ctx, "commit-menu", &["x"]);
        let id = "1".repeat(40);
        let info = |names: &[&str]| Some(Ok(CommitInfo { id: id.clone(), author: "T".into(), files: names.iter().map(|n| Change { path: n.to_string(), old_path: None, code: 'M' }).collect(), ..Default::default() }));
        t.sel = Some(Sel::Commit(id.clone()));
        for as_tree in [false, true] {
            let (mut tree, mut acts) = (as_tree, vec![]);
            sim.menu_survives("c.txt", "Copy path", |ui, changed| {
                t.info = info(if changed { &["a.txt", "b.txt", "c.txt", "d.txt"] } else { &["b.txt", "c.txt", "d.txt"] });
                t.details(ui, &mut tree, &mut acts);
            });
            let copied: Vec<_> = acts.iter().filter_map(|a| if let Action::Copy(p) = a { Some(p.as_str()) } else { None }).collect();
            assert_eq!(copied, ["c.txt"], "as a tree: {as_tree}");
        }
    }

    #[test]
    fn a_co_author_menu_closes_when_another_commit_is_selected() {
        let mut sim = Sim::new(420.0, 600.0);
        let mut t = tab(&sim.ctx, "coauthor-menu", &["x"]);
        let info = |id: &str, who: [&str; 2]| {
            let message = format!("m\n\nCo-authored-by: {0} <{0}@x>\nCo-authored-by: {1} <{1}@x>", who[0], who[1]);
            Some(Ok(CommitInfo { id: id.repeat(40), author: "T".into(), message, coauthors: who.map(|w| (w.to_owned(), format!("{w}@x"))).to_vec(), ..Default::default() }))
        };
        (t.sel, t.info) = (Some(Sel::Commit("1".repeat(40))), info("1", ["Bo", "Cy"]));
        let (mut tree, mut acts) = (false, vec![]);
        sim.open_menu("co-authored by Cy", |ui| t.details(ui, &mut tree, &mut acts));
        assert!(egui::Popup::is_any_open(&sim.ctx));
        (t.sel, t.info) = (Some(Sel::Commit("2".repeat(40))), info("2", ["Di", "Ed"]));
        for _ in 0..2 {
            sim.frame(vec![], |ui| t.details(ui, &mut tree, &mut acts));
        }
        assert!(!egui::Popup::is_any_open(&sim.ctx), "the menu doesn't move to the other commit's co-author");
    }

    #[test]
    fn a_status_reload_keeps_a_menu_on_its_file_or_folder() {
        for as_tree in [false, true] {
            let mut sim = Sim::new(420.0, 700.0);
            let mut t = tab(&sim.ctx, "wip-menu", &["x"]);
            std::fs::create_dir(t.dir.join("src")).unwrap();
            for name in ["b.txt", "c.txt", "d.txt"] {
                std::fs::write(t.dir.join("src").join(name), name).unwrap();
            }
            t.reload(&sim.ctx, false);
            load(&mut t, &sim.ctx);
            t.sel = Some(Sel::Wip);
            let (mut tree, mut acts) = (as_tree, vec![]);
            sim.open_menu("c.txt", |ui| t.details(ui, &mut tree, &mut acts));
            std::fs::write(t.dir.join("src").join("a.txt"), "a").unwrap();
            t.reload(&sim.ctx, false);
            load(&mut t, &sim.ctx);
            sim.pick("Discard changes", |ui| t.details(ui, &mut tree, &mut acts));
            let discarded: Vec<_> = acts.iter().filter_map(|a| if let Action::Git(op) = a { op.cmds.first().and_then(|c| c.last()) } else { None }).collect();
            assert_eq!(discarded, ["src/c.txt"], "as a tree: {as_tree}");
            if as_tree {
                acts.clear();
                sim.open_menu("src", |ui| t.details(ui, &mut tree, &mut acts));
                std::fs::create_dir(t.dir.join("lib")).unwrap();
                std::fs::write(t.dir.join("lib").join("x.txt"), "x").unwrap();
                t.reload(&sim.ctx, false);
                load(&mut t, &sim.ctx);
                sim.pick("Discard changes in folder", |ui| t.details(ui, &mut tree, &mut acts));
                let discarded: Vec<_> = acts.iter().filter_map(|a| if let Action::Git(op) = a { Some(op.cmds.iter().flatten().filter(|p| p.contains(".txt")).cloned().collect::<Vec<_>>()) } else { None }).collect();
                assert_eq!(discarded, [["src/a.txt", "src/b.txt", "src/c.txt", "src/d.txt"]], "the folder, not the one that took its row");
            }
        }
    }

    /// Double- or triple-clicking the Stage button stages once: the second click may land on the next file.
    #[test]
    fn row_button_skips_double_click() {
        let ctx = egui::Context::default();
        let at = pos2(180.0, 12.0);
        let mut hits = 0;
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let frames = [vec![Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![press(true)], vec![press(false)], vec![press(true)], vec![press(false)], vec![]];
        for (i, events) in frames.into_iter().enumerate() {
            let raw = RawInput { time: Some(i as f64 * 0.05), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let (_, row) = ui.allocate_exact_size(vec2(200.0, 24.0), egui::Sense::click());
                hits += super::row_button(ui, &row, "+", "Stage") as usize;
            });
            out.textures_delta.clear();
        }
        assert_eq!(hits, 1);
    }
}
