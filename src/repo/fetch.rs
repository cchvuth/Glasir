//! Background auto-fetch (on a timer and on window focus).

use super::task::Task;
use super::RepoTab;
use crate::git;

#[cfg(all(test, unix))]
mod tests {
    use crate::git;
    use crate::repo::task::Task;
    use crate::repo::testutil::{identity, scratch, settle, sh};
    use crate::repo::{Action, RepoTab, View, fetch_op};

    /// A Fetch clicked during the background fetch would race it on ref locks: it stops that
    /// one and runs at once, after it has ended; the stopped fetch leaves no error behind.
    #[test]
    fn user_fetch_stops_the_background_one() {
        let dir = scratch("bgq");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        let (cancel, d) = (git::Cancel::default(), dir.clone());
        t.bg_cancel = cancel.clone();
        t.bg_fetch = Some(Task::spawn(&ctx, move || cancel.scope(|| git::run(&d, &["-c", "alias.slow=!sleep 5", "slow"]).map(|_| false))));
        let start = std::time::Instant::now();
        t.apply(&ctx, Action::Git(fetch_op()));
        assert!(t.running.is_some() && t.bg_fetch.is_none());
        let msgs = settle(&ctx, &mut t);
        assert!(start.elapsed() < std::time::Duration::from_secs(4), "the background fetch was stopped");
        assert!(t.fetch_failed.is_none() && msgs.iter().all(|m| !m.error), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
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
        let idle = self.bg_fetch.is_none() && self.running.is_none() && self.data.as_ref().is_some_and(|d| !d.remotes.is_empty());
        if minutes == 0 || !idle {
            return;
        }
        let (since, every) = (now - self.last_fetch, f64::from(minutes) * 60.0);
        if since >= every || (focus && since >= 120.0) {
            self.last_fetch = now;
            let (dir, cancel) = (self.dir.clone(), git::Cancel::default());
            self.bg_cancel = cancel.clone();
            self.bg_fetch = Some(Task::spawn(ctx, move || cancel.scope(|| {
                let refs = || git::run(&dir, &["for-each-ref", "--format=%(objectname) %(refname)", "refs/remotes"]);
                let before = refs()?;
                git::fetch_quiet(&dir)?;
                Ok(refs()? != before)
            })));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(every - since));
        }
    }
}
