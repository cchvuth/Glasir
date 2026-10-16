//! ⌘P / ⌘K fuzzy command palette: git commands, checkout/merge/rebase any
//! branch, open recent repositories, app commands.

use crate::ui::format::keys;
use crate::git::RefKind;
use crate::repo::{Action, Op, Prompt, PullMode, RepoTab, Undo, branch_checkout, op, pop_stash, pull_op, stash_prompt};
use crate::repo::merge_opts::{Merge, merge_op};
use crate::theme::{self, icon};
use egui::{Key, RichText, TextEdit};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Cmd {
    Tab(Action),
    Open(PathBuf),
    PickOpen,
    Clone,
    Settings,
    Theme,
    CommandLog,
    Shortcuts,
}

pub struct Item {
    pub icon: &'static str,
    pub label: String,
    pub detail: String,
    pub cmd: Cmd,
}

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
    /// Built once per opening: with thousands of refs, building is too slow for every frame.
    pub items: Vec<Item>,
    /// Indices of the best matches for the query they were ranked for (and the item count then):
    /// scoring thousands of items is only redone when the query changes.
    pub ranked: (String, usize, Vec<usize>),
    pub dir: Option<PathBuf>,
}

/// Subsequence match, case-insensitive. Higher is better; rewards consecutive
/// runs and word starts, penalizes gaps.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut pos = 0;
    let mut last: Option<usize> = None;
    let mut s = 0;
    for qc in query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()) {
        let i = pos + t[pos..].iter().position(|&c| c == qc)?;
        s += 10;
        if last == Some(i.wrapping_sub(1)) {
            s += 15;
        }
        if i == 0 || matches!(t[i - 1], ' ' | '/' | '-' | '_' | '.') {
            s += 20;
        }
        s -= (i - pos).min(10) as i32;
        last = Some(i);
        pos = i + 1;
    }
    Some(s - t.len() as i32 / 8)
}

fn item(icon: &'static str, label: impl Into<String>, detail: impl Into<String>, cmd: Cmd) -> Item {
    Item { icon, label: label.into(), detail: detail.into(), cmd }
}

fn git(label: impl Into<String>, a: Action) -> Item {
    item(icon::TERMINAL_WINDOW, label, "", Cmd::Tab(a))
}

pub fn items(tab: Option<&RepoTab>, recent: &[PathBuf], pull: PullMode, terminal: bool) -> Vec<Item> {
    let mut v = vec![];
    if let Some(t) = tab
        && let Some(d) = &t.data
    {
        v.push(item(icon::CLOUD_ARROW_DOWN, "Fetch all remotes", "git fetch --all --prune", Cmd::Tab(Action::Git(crate::repo::fetch_op(d)))));
        for m in [PullMode::Merge, PullMode::FfOnly, PullMode::Rebase] {
            let o = pull_op(m);
            let detail = if m == pull { "default" } else { "" };
            v.push(item(icon::ARROW_LINE_DOWN, o.label.clone(), detail, Cmd::Tab(Action::Git(o))));
        }
        if let Some(o) = t.push_op(false) {
            v.push(item(icon::ARROW_LINE_UP, "Push", "", Cmd::Tab(Action::Git(o))));
        }
        if let Some(o) = t.push_op(true) {
            v.push(item(icon::ARROW_LINE_UP, "Force push (with lease)", "", Cmd::Tab(Action::Git(o))));
        }
        v.push(item(
            icon::GIT_BRANCH,
            "Create branch…",
            "from HEAD",
            Cmd::Tab(Action::Prompt(Prompt {
                title: "New branch from HEAD".into(),
                value: String::new(),
                op: Op::new("Create branch", &["checkout", "-b", "{}"]).undo(Undo::Keep),
                second: None,
                sign: None,
            })),
        ));
        v.push(git("Stash all changes…", stash_prompt()));
        if let Some(s) = d.stashes.first() {
            v.push(git("Pop stash", Action::Git(pop_stash(s))));
        }
        if let Some(o) = crate::repo::stage_all_op(&d.status) {
            v.push(git("Stage all changes", Action::Git(o)));
        }
        v.push(git("Unstage all changes", Action::Git(crate::repo::unstage_all_op())));
        if !d.status.staged.is_empty() && d.state == crate::git::State::Clean {
            v.push(item(icon::ARROWS_IN, "Absorb staged changes…", "into the commits they belong to", Cmd::Tab(Action::Absorb)));
        }
        if crate::ai::enabled() {
            use crate::ai::Purpose;
            let detail = if t.amend { "amend: last commit + staged changes" } else { "from staged changes" };
            v.push(item(icon::SPARKLE, "Write commit message with AI", detail, Cmd::Tab(Action::Ai(Purpose::CommitMessage { amend: t.amend }))));
            v.push(item(icon::SPARKLE, "Stash with AI message", "", Cmd::Tab(Action::Ai(Purpose::StashMessage))));
            if d.wip && d.state == crate::git::State::Clean {
                v.push(item(icon::SPARKLE, "Compose commits with AI…", "split uncommitted changes into commits", Cmd::Tab(Action::Ai(Purpose::Compose))));
            }
            if d.wip {
                v.push(item(icon::SPARKLE, "Explain working changes with AI", "", Cmd::Tab(Action::Ai(Purpose::ExplainWip))));
            }
        }
        if let Some(l) = t.undo_label() {
            v.push(item(icon::ARROW_COUNTER_CLOCKWISE, format!("Undo {l}"), keys("⌘Z"), Cmd::Tab(Action::Undo)));
        }
        if let Some(l) = t.redo_label() {
            v.push(item(icon::ARROW_CLOCKWISE, format!("Redo {l}"), keys("⇧⌘Z"), Cmd::Tab(Action::Redo)));
        }
        v.push(item(icon::ARROWS_CLOCKWISE, "Refresh", "F5", Cmd::Tab(Action::Reload)));
        v.push(item(icon::CODE, "Open in editor", "the repository folder", Cmd::Tab(Action::Editor(String::new()))));
        if terminal {
            v.push(item(icon::TERMINAL, "Toggle terminal panel", keys("⌃`"), Cmd::Tab(Action::ToggleTerminal)));
        }
        v.push(item(icon::TERMINAL, "Open terminal here", "external terminal app", Cmd::Tab(Action::Terminal)));
        v.push(item(icon::FOLDER_OPEN, "Show in file manager", "", Cmd::Tab(Action::Reveal(String::new()))));
        let vars = crate::repo::tools::Vars {
            branch: d.status.branch.clone().unwrap_or_default(),
            sha: d.status.head.clone().unwrap_or_default(),
            path: String::new(),
        };
        for c in crate::repo::tools::commands(None) {
            v.push(item(icon::TERMINAL, c.name.clone(), "custom command", Cmd::Tab(Action::Custom(c, vars.clone()))));
        }
        v.push(item(icon::BROOM, "Clean up branches (merged, squashed, gone, stale)…", "", Cmd::Tab(Action::Cleanup)));
        v.push(item(icon::LIFEBUOY, "Reflog — restore lost commits", "after a bad reset or rebase", Cmd::Tab(Action::Reflog)));
        if let Some((label, a)) = crate::repo::spinoff::move_unpushed(d) {
            v.push(item(icon::GIT_FORK, label, "", Cmd::Tab(a)));
        }
        match d.state {
            crate::git::State::Clean => v.push(item(icon::BUG, "Start bisect (HEAD is bad)", "then mark a good commit", Cmd::Tab(op("Start bisect", &["bisect", "start", "HEAD"])))),
            crate::git::State::Bisecting => {
                for what in ["good", "bad", "skip", "reset"] {
                    v.push(item(icon::BUG, format!("Bisect: {what}"), if what == "reset" { "end the bisect" } else { "the checked-out commit" }, Cmd::Tab(op(format!("Bisect: {what}"), &["bisect", what]))));
                }
            }
            _ => {}
        }
        if d.state == crate::git::State::Clean {
            v.push(item(icon::WRENCH, "Perform repo maintenance", "pack loose objects, refresh the commit-graph", Cmd::Tab(Action::Git(crate::repo::maintenance::op()))));
            let on = d.health.scheduled;
            v.push(item(icon::CLOCK, format!("{} background maintenance", if on { "Disable" } else { "Enable" }), "git maintenance start", Cmd::Tab(Action::Git(crate::repo::maintenance::schedule_op(!on)))));
        }
        v.push(item(icon::BANDAIDS, "Apply patch…", "git am / git apply", Cmd::Tab(Action::ApplyPatch)));
        if let Some(remote) = d.default_remote() {
            v.push(item(icon::TAG, format!("Push all tags to {remote}"), "", Cmd::Tab(Action::Git(crate::repo::tags::push_all(remote)))));
        }
        v.push(item(icon::PLUS, "Add remote…", "name and URL, then fetch", Cmd::Tab(Action::RemoteForm(crate::repo::remotes::Form::add(d)))));
        for (r, url) in d.remotes.iter() {
            let from = if d.remotes.len() > 1 { format!(" from {r}") } else { String::new() };
            let pr = crate::repo::prompt(format!("Pull request number on {r}"), "", crate::repo::remotes::pr_checkout(r));
            v.push(item(icon::GIT_PULL_REQUEST, format!("Check out PR #…{from}"), "fetch pull/<n>/head into pr/<n>", Cmd::Tab(pr)));
            v.push(item(icon::STETHOSCOPE, format!("Check connection to {r}"), crate::git::redact(url), Cmd::Tab(Action::Doctor(r.clone()))));
        }
        if let Some((r, _)) = d.remotes.iter().find(|r| r.0 == "origin").or(d.remotes.first()) {
            v.push(item(icon::KEY, "SSH key for this repository…", "core.sshCommand", Cmd::Tab(Action::Doctor(r.clone()))));
        }
        if let Some(web) = d.web() {
            v.push(item(icon::GLOBE, "Open repository on remote", web.clone(), Cmd::Tab(Action::Url(web.clone()))));
            if let Some(b) = &d.status.branch {
                let url = format!("{web}/compare/{}...{}?expand=1", crate::git::url_path(&d.default_branch()), crate::git::url_path(b));
                v.push(item(icon::GIT_PULL_REQUEST, format!("Create pull request for {b}"), "opens compare page", Cmd::Tab(Action::Url(url))));
            }
        }
        let head = d.status.branch.clone().unwrap_or_default();
        for r in d.refs.iter() {
            let (kind, ic) = match r.kind {
                RefKind::Local => ("local branch", icon::GIT_BRANCH),
                RefKind::Remote => ("remote branch", icon::CLOUD),
                RefKind::Tag => ("tag", icon::TAG),
            };
            if let Some(a) = branch_checkout(d, r) {
                v.push(item(ic, format!("Checkout {}", r.name), kind, Cmd::Tab(a)));
            }
            if r.kind != RefKind::Tag && !head.is_empty() && r.name != head {
                for how in [None, Some(Merge::Squash), Some(Merge::NoFf), Some(Merge::FfOnly)] {
                    let how_label = how.map_or(String::new(), |h| format!(" ({})", h.name()));
                    let m = merge_op(&r.name, &r.name, how);
                    v.push(item(icon::GIT_MERGE, format!("Merge {} into {head}{how_label}", r.name), kind, Cmd::Tab(Action::Git(m))));
                }
                let rb = Op::new(format!("Rebase onto {}", r.name), &["rebase", &r.name]).undo(Undo::Keep);
                let rb = Action::RunRebase { op: rb, onto: r.name.clone(), branch: None, first: None };
                v.push(item(icon::GIT_COMMIT, format!("Rebase {head} onto {}", r.name), kind, Cmd::Tab(rb)));
            }
        }
    }
    v.push(item(icon::FOLDER_OPEN, "Open repository…", keys("⌘O"), Cmd::PickOpen));
    v.push(item(icon::DOWNLOAD_SIMPLE, "Clone repository…", "", Cmd::Clone));
    v.push(item(icon::GEAR_SIX, "Settings", keys("⌘,"), Cmd::Settings));
    v.push(item(icon::MOON, "Toggle light / dark theme", "", Cmd::Theme));
    v.push(item(icon::TERMINAL_WINDOW, "Show command log", keys("⇧⌘L"), Cmd::CommandLog));
    v.push(item(icon::KEYBOARD, "Keyboard shortcuts", "?", Cmd::Shortcuts));
    for p in recent {
        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        v.push(item(icon::FOLDER_SIMPLE, format!("Open {name}"), p.display().to_string(), Cmd::Open(p.clone())));
    }
    v
}

/// Keeps the end of a long detail (a path or URL) so labels keep their room.
fn elide(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_owned() } else { format!("…{}", s.chars().skip(n + 1 - max).collect::<String>()) }
}

/// Draws the palette; returns the chosen command, or `Some(None)` to close.
pub fn show(ctx: &egui::Context, p: &mut Palette) -> Option<Option<Cmd>> {
    if p.ranked.0 != p.query || p.ranked.1 != p.items.len() {
        let mut r: Vec<(i32, usize)> = p
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                if p.query.trim().is_empty() {
                    return Some((0, i));
                }
                // Prefer commands and branches over the (often hundreds of) tags.
                let boost = match it.detail.as_str() {
                    "tag" => 0,
                    "remote branch" => 3,
                    "local branch" => 5,
                    _ => 6,
                };
                Some((score(&p.query, &it.label)? + boost, i))
            })
            .collect();
        r.sort_by_key(|(s, _)| -s);
        r.truncate(60);
        p.ranked = (p.query.clone(), p.items.len(), r.into_iter().map(|x| x.1).collect());
    }
    let ranked: Vec<&Item> = p.ranked.2.iter().map(|&i| &p.items[i]).collect();
    let n = ranked.len();
    let modal = egui::Modal::new(egui::Id::new("palette"))
        .area(egui::Modal::default_area(egui::Id::new("palette_area")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 90.0)));
    let top = ctx.memory(|m| m.top_modal_layer()) == Some(modal.area.layer());
    let (up, down, enter) = if top { ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Enter))) } else { Default::default() };
    if down && n > 0 {
        p.sel = (p.sel + 1) % n;
    }
    if up && n > 0 {
        p.sel = (p.sel + n - 1) % n;
    }
    p.sel = p.sel.min(n.saturating_sub(1));
    let mut out = None;
    if enter && n > 0 {
        out = Some(Some(ranked[p.sel].cmd.clone()));
    }
    let screen = ctx.content_rect();
    let width = 640.0f32.min(screen.width() - 40.0);
    let resp = modal.show(ctx, |ui| {
        ui.set_width(width);
        let te = ui.add(
            TextEdit::singleline(&mut p.query)
                .hint_text(format!("{}  Type a command, branch or repository…", icon::MAGNIFYING_GLASS))
                .font(egui::TextStyle::Heading)
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE.inner_margin(theme::INPUT_MARGIN)),
        );
        if top {
            te.request_focus();
        }
        if te.changed() {
            p.sel = 0;
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, it) in ranked.iter().copied().enumerate() {
                let selected = i == p.sel;
                let r = ui.add(
                    egui::Button::selectable(
                        selected,
                        RichText::new(format!("{}   {}", it.icon, it.label)).color(if selected { theme::text(ui) } else { ui.visuals().text_color() }),
                    )
                    .right_text(RichText::new(elide(&it.detail, 56)).small().color(if selected { theme::text(ui) } else { theme::dim(ui) }))
                    .min_size(egui::vec2(ui.available_width(), 30.0)),
                );
                if selected && (up || down) {
                    r.scroll_to_me(None);
                }
                if r.clicked() {
                    out = Some(Some(it.cmd.clone()));
                }
            }
            if n == 0 {
                ui.label(RichText::new("No matches").color(theme::dim(ui)));
            }
        });
    });
    if resp.should_close() && out.is_none() {
        out = Some(None);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{elide, score};

    #[test]
    fn long_details_keep_their_end() {
        assert_eq!(elide("short", 10), "short");
        assert_eq!(elide("/a/very/long/path/repo", 10), "…path/repo");
        assert_eq!(elide("/a/very/long/path/repo", 10).chars().count(), 10);
    }

    #[test]
    fn fuzzy_ranking() {
        assert!(score("chk main", "Checkout main").is_some());
        assert!(score("xyz", "Checkout main").is_none());
        // word starts and consecutive runs beat scattered matches
        assert!(score("fa", "Fetch all remotes").unwrap() > score("fa", "Unstage all changes").unwrap_or(i32::MIN));
        assert!(score("push", "Push").unwrap() > score("push", "Force push (with lease)").unwrap());
    }
}
