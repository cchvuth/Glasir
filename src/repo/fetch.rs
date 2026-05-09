//! Background auto-fetch (on a timer and on window focus).

use super::task::Task;
use super::RepoTab;
use crate::git;

impl RepoTab {
    /// Background fetch every `minutes` (0 = off), and when the window regains
    /// `focus` (at most every 2 minutes). Never while an op runs; never prompts.
    pub fn auto_fetch(&mut self, ctx: &egui::Context, minutes: u32, focus: bool) {
        let now = ctx.input(|i| i.time);
        match Task::poll(&mut self.bg_fetch) {
            Some(Ok(_)) => {
                self.fetch_failed = None;
                self.reload(ctx, true);
            }
            Some(Err(e)) => self.fetch_failed = Some(e),
            None => {}
        }
        let idle = self.bg_fetch.is_none() && self.running.is_none() && self.data.as_ref().is_some_and(|d| !d.remotes.is_empty());
        if minutes == 0 || !idle {
            return;
        }
        let (since, every) = (now - self.last_fetch, f64::from(minutes) * 60.0);
        if since >= every || (focus && since >= 120.0) {
            self.last_fetch = now;
            let dir = self.dir.clone();
            self.bg_fetch = Some(Task::spawn(ctx, move || git::fetch_quiet(&dir)));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(every - since));
        }
    }
}
