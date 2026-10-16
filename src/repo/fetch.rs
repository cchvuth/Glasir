//! Background auto-fetch (on a timer and on window focus), one fetch at a time per repository.

use super::task::Task;
use super::RepoTab;
use crate::git;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};

static FETCHES: Mutex<Vec<(PathBuf, Option<git::Cancel>)>> = Mutex::new(vec![]);
static ENDED: Condvar = Condvar::new();

pub(super) struct Fetching(PathBuf, bool);

impl Fetching {
    fn background(common: PathBuf, cancel: &git::Cancel) -> Option<Self> {
        let mut f = FETCHES.lock().unwrap();
        if f.iter().any(|e| e.0 == common) {
            return None;
        }
        f.push((common.clone(), Some(cancel.clone())));
        Some(Fetching(common, true))
    }

    pub(super) fn user(common: PathBuf) -> Self {
        let mut f = FETCHES.lock().unwrap();
        while let Some((_, Some(c))) = f.iter().find(|e| e.0 == common && e.1.is_some()) {
            c.cancel();
            f = ENDED.wait(f).unwrap();
        }
        f.push((common.clone(), None));
        Fetching(common, false)
    }
}

impl Drop for Fetching {
    fn drop(&mut self) {
        let mut f = FETCHES.lock().unwrap();
        if let Some(i) = f.iter().position(|e| e.0 == self.0 && e.1.is_some() == self.1) {
            f.remove(i);
        }
        ENDED.notify_all();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::git;
    use crate::repo::testutil::{commit, identity, scratch, settle, sh, wait_for};
    use crate::repo::{Action, Op, RepoTab, View, fetch_op};

    #[test]
    fn one_fetch_at_a_time_across_worktrees() {
        let (dir, remote, wt) = (scratch("bgq"), scratch("bgq-remote"), scratch("bgq-wt"));
        std::fs::remove_dir_all(&wt).unwrap();
        sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1");
        sh(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        sh(&dir, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
        let gate = dir.join(".git/gate");
        sh(&dir, &["config", "remote.origin.uploadpack", &format!("test -e '{0}' || {{ : > '{0}'; sleep 30; }}; git-upload-pack", gate.display())]);
        let ctx = egui::Context::default();
        let (mut a, mut b) = (RepoTab::new(&ctx, dir.clone(), 100, View::default()), RepoTab::new(&ctx, wt.clone(), 100, View::default()));
        settle(&ctx, &mut a);
        settle(&ctx, &mut b);
        let fetches = || git::LOG.lock().unwrap().iter().filter(|l| l.cwd == dir && l.args.iter().any(|x| x == "fetch")).count();
        for user in [fetch_op(), Op::new("Prune origin", &[crate::repo::tools::SHELL, "git remote prune origin"])] {
            let _ = std::fs::remove_file(&gate);
            b.last_fetch = f64::NEG_INFINITY;
            b.auto_fetch(&ctx, 10, false);
            wait_for("the background fetch", || gate.exists());
            let n = fetches();
            a.last_fetch = f64::NEG_INFINITY;
            a.auto_fetch(&ctx, 10, false);
            wait_for("a background fetch that skips", || {
                a.auto_fetch(&ctx, 0, false);
                a.bg_fetch.is_none()
            });
            assert_eq!(fetches(), n, "two fetches at once");
            let start = std::time::Instant::now();
            a.apply(&ctx, Action::Git(user));
            let msgs = settle(&ctx, &mut a);
            assert!(start.elapsed() < std::time::Duration::from_secs(20), "the background fetch was waited out, not stopped");
            assert!(msgs.iter().all(|m| !m.error), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
            let stopped = (0..250).any(|_| {
                b.auto_fetch(&ctx, 0, false);
                std::thread::sleep(std::time::Duration::from_millis(20));
                b.bg_fetch.is_none()
            });
            assert!(stopped && b.fetch_failed.is_none(), "the other tab's background fetch was left running");
        }
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}

impl RepoTab {
    /// Background fetch every `minutes` (0 = off), and when the window regains
    /// `focus` (at most every 2 minutes). Never while an op runs, nor while another fetch of the
    /// repository does (a tab on another of its worktrees); never prompts.
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
            Some(Err(e)) if e == git::CANCELLED => {}
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
            let (dir, common, cancel) = (self.dir.clone(), self.common.clone(), git::Cancel::default());
            self.bg_fetch = Some(Task::spawn(ctx, move || {
                let Some(_turn) = Fetching::background(common, &cancel) else { return Ok(false) };
                cancel.scope(|| {
                    let refs = || git::run(&dir, &["for-each-ref", "--format=%(objectname) %(refname)", "refs/remotes"]);
                    let before = refs()?;
                    git::fetch_quiet(&dir)?;
                    Ok(refs()? != before)
                })
            }));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(every - since));
        }
    }
}
