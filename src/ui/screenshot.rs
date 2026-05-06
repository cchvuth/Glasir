//! Dev/CI harness: `GLASIR_SCREENSHOT=out.bmp` saves a screenshot once the repo
//! is loaded, then exits. `GLASIR_SCREENSHOT_VIEW` opens a view first (settings,
//! dialogs, diffs, search...); extend the switch when a new view needs
//! visual verification.

use super::app::App;
use crate::git;
use crate::repo::history::Mode;
use crate::repo::{Action, Op, PullMode, Search, Undo, pull_op};
use egui::Id;

impl App {
    pub(super) fn screenshot(&mut self, ctx: &egui::Context) {
        let ready = self.tabs.get(self.active).is_none_or(|t| t.data.is_some() && !t.loading());
        let Some((_, frames)) = &mut self.shot else { return };
        ctx.request_repaint();
        if ready {
            *frames += 1;
        }
        if *frames == 2 {
            match std::env::var("GLASIR_SCREENSHOT_VIEW").as_deref() {
                Ok("settings") => self.open_settings(),
                Ok("bottom") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.scroll_to = t.data.as_ref().map(|d| d.rows().saturating_sub(1));
                    }
                }
                Ok("log") => self.show_log = true,
                // Network dialogs and badges: `checkout:<branch>`, `pull-ff`, `push-rejected`, `force-push`.
                Ok(q) if q.starts_with("checkout:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let b = &q["checkout:".len()..];
                        t.apply(&ctx.clone(), crate::repo::Action::Git(Op::new(format!("Checkout {b}"), &["checkout", b, "--"]).undo(Undo::Keep)));
                    }
                }
                Ok("pull-ff") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.apply(&ctx.clone(), crate::repo::Action::Git(pull_op(PullMode::FfOnly)));
                    }
                }
                Ok("push") => {
                    if let Some(t) = self.tabs.get_mut(self.active)
                        && let Some(o) = t.push_op(false)
                    {
                        t.apply(&ctx.clone(), crate::repo::Action::Git(o));
                    }
                }
                Ok("force-push") => {
                    if let Some(t) = self.tabs.get_mut(self.active)
                        && let Some(o) = t.push_op(true)
                    {
                        t.apply(&ctx.clone(), crate::repo::Action::Git(o));
                    }
                }
                // Run an op, e.g. `git:fetch --all` (to see the running/cancel state).
                Ok(q) if q.starts_with("git:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let args: Vec<&str> = q[4..].split(' ').collect();
                        t.apply(&ctx.clone(), crate::repo::Action::Git(Op::new(args[0], &args)));
                    }
                }
                Ok(q) if q.starts_with("multi:") => {
                    if let Some(t) = self.tabs.get_mut(self.active)
                        && let Some(d) = &t.data
                    {
                        let n: usize = q[6..].parse().unwrap_or(2);
                        let ids: Vec<String> = (0..d.rows()).filter_map(|r| d.commit(r)).take(n).map(|c| c.id.clone()).collect();
                        t.apply(&ctx.clone(), crate::repo::Action::Multi(ids));
                    }
                }
                Ok(q) if q.starts_with("diff:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let change = git::Change { path: q[5..].into(), old_path: None, code: 'M' };
                        t.apply(&ctx.clone(), crate::repo::Action::Diff(crate::repo::DiffSrc::Worktree { change, staged: false }));
                    }
                }
                Ok(q) if q.starts_with("merge:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.apply(&ctx.clone(), crate::repo::Action::MergeEditor(q[6..].into()));
                    }
                }
                Ok(q) if q.starts_with("search:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let mut s = Search::open();
                        s.query = q["search:".len()..].into();
                        t.search = Some(s);
                    }
                }
                Ok(q) if q.starts_with("blame:") || q.starts_with("history:") => {
                    let (mode, path) = q.split_once(':').unwrap();
                    let mode = if mode == "blame" { Mode::Blame } else { Mode::History };
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.apply(&ctx.clone(), crate::repo::Action::FileView(path.into(), mode));
                    }
                }
                Ok(q) if q.starts_with("drop:") => {
                    let mut it = q[5..].splitn(2, ':');
                    if let (Some(src), Some(dst), Some(t)) = (it.next(), it.next(), self.tabs.get_mut(self.active)) {
                        t.drop_menu = Some((src.into(), dst.into(), egui::pos2(600.0, 300.0)));
                    }
                }
                // Graph views: `hover:<ref name>`, `view:fp`, `view:hide:<full ref>`, `view:solo:<full ref>`, `stash`.
                Ok(q) if q.starts_with("hover:") => {
                    if let Some(r) = self.tabs.get(self.active).and_then(|t| t.data.as_ref()).and_then(|d| d.refs.iter().find(|r| r.name == q[6..])) {
                        // The state `graph_view::hover_ref` records, pinned as hovered long ago and still.
                        ctx.data_mut(|m| m.insert_temp(Id::new("ref_hover"), (r.target.clone(), f64::NEG_INFINITY, u64::MAX - 1)));
                    }
                }
                Ok(q) if q.starts_with("view:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let mut v = t.view.clone();
                        match q[5..].split_once(':') {
                            Some(("hide", r)) => v.hidden = vec![r.into()],
                            Some(("solo", r)) => v.solo = vec![r.into()],
                            _ => v.first_parent = true,
                        }
                        if v != t.view {
                            t.apply(&ctx.clone(), Action::View(v));
                        }
                    }
                }
                Ok("stash") => {
                    if let Some(t) = self.tabs.get_mut(self.active)
                        && let Some(id) = t.data.as_ref().and_then(|d| d.stashes.first()).map(|s| s.id.clone())
                    {
                        t.apply(&ctx.clone(), Action::Select(crate::repo::Sel::Commit(id)));
                    }
                }
                _ => {}
            }
        }
        let Some((_, frames)) = &mut self.shot else { return };
        let wait: u32 = std::env::var("GLASIR_SCREENSHOT_FRAMES").ok().and_then(|f| f.parse().ok()).unwrap_or(20);
        if *frames == wait {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let img = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            let [w, h] = img.size;
            let mut buf = Vec::with_capacity(54 + w * h * 4);
            buf.extend(b"BM");
            buf.extend(((54 + w * h * 4) as u32).to_le_bytes());
            buf.extend([0u8; 4]);
            buf.extend(54u32.to_le_bytes());
            buf.extend(40u32.to_le_bytes());
            buf.extend((w as i32).to_le_bytes());
            buf.extend((-(h as i32)).to_le_bytes());
            buf.extend(1u16.to_le_bytes());
            buf.extend(32u16.to_le_bytes());
            buf.extend([0u8; 24]);
            for p in &img.pixels {
                buf.extend([p.b(), p.g(), p.r(), 255]);
            }
            let _ = std::fs::write(&self.shot.as_ref().unwrap().0, buf);
            std::process::exit(0);
        }
    }
}
