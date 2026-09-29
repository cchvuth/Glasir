//! Background auto-fetch (on a timer and on window focus).

use super::task::Task;
use super::RepoTab;
use crate::git;

#[cfg(test)]
mod tests {
    use crate::repo::task::Task;
    use crate::repo::testutil::{identity, scratch, sh};
    use crate::repo::{Action, RepoTab, View, fetch_op};

    /// A Fetch clicked during the background fetch would race it on ref locks: it waits.
    #[test]
    fn user_fetch_waits_for_the_background_one() {
        let dir = scratch("bgq");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir, 100, View::default());
        let (tx, rx) = std::sync::mpsc::channel();
        t.bg_fetch = Some(Task(rx));
        t.apply(&ctx, Action::Git(fetch_op()));
        assert!(t.after_bg_fetch.is_some() && t.running.is_none());
        tx.send(Ok(false)).unwrap();
        t.auto_fetch(&ctx, 0, false);
        assert!(t.after_bg_fetch.is_none() && t.running.is_some());
    }
}

impl RepoTab {
    /// Background fetch every `minutes` (0 = off), and when the window regains
    /// `focus` (at most every 2 minutes). Never while an op runs; never prompts.
    pub fn auto_fetch(&mut self, ctx: &egui::Context, minutes: u32, focus: bool) {
        let now = ctx.input(|i| i.time);
        match Task::poll(&mut self.bg_fetch) {
            Some(Ok(changed)) => {
                self.fetch_failed = None;
                // The file watcher sees the ref updates too; this covers it being unavailable.
                if changed {
                    self.reload(ctx, true);
                }
            }
            Some(Err(e)) => self.fetch_failed = Some(e),
            None => {}
        }
        if self.bg_fetch.is_none()
            && let Some(op) = self.after_bg_fetch.take()
        {
            self.run(ctx, op);
        }
        let idle = self.bg_fetch.is_none() && self.running.is_none() && self.data.as_ref().is_some_and(|d| !d.remotes.is_empty());
        if minutes == 0 || !idle {
            return;
        }
        let (since, every) = (now - self.last_fetch, f64::from(minutes) * 60.0);
        if since >= every || (focus && since >= 120.0) {
            self.last_fetch = now;
            let dir = self.dir.clone();
            self.bg_fetch = Some(Task::spawn(ctx, move || {
                let refs = || git::run(&dir, &["for-each-ref", "--format=%(objectname) %(refname)", "refs/remotes"]);
                let before = refs()?;
                git::fetch_quiet(&dir)?;
                Ok(refs()? != before)
            }));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(every - since));
        }
    }
}
