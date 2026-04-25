//! Dev/CI harness: `GLASIR_SCREENSHOT=out.bmp` saves a screenshot once the repo
//! is loaded, then exits. `GLASIR_SCREENSHOT_VIEW` opens a view first (settings,
//! dialogs, diffs...); extend the switch when a new view needs
//! visual verification.

use super::app::App;
use crate::git;
use crate::repo::{Op, PullMode, pull_op};

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
                // Network dialogs and badges: `pull-ff`, `push-rejected`, `force-push`.
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
                Ok(q) if q.starts_with("diff:") => {
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        let change = git::Change { path: q[5..].into(), old_path: None, code: 'M' };
                        t.apply(&ctx.clone(), crate::repo::Action::Diff(crate::repo::DiffSrc::Worktree { change, staged: false }));
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
