//! Remote-facing helpers: running an op with local changes auto-stashed.

use super::{Cancel, Res, run};
use std::path::Path;

/// Runs `f` with all uncommitted changes (untracked too) stashed, then reapplies them.
/// If they conflict with the new state, the conflict markers stay and so does the stash.
pub fn with_autostash(dir: &Path, msg: &str, f: impl FnOnce() -> Res<String>) -> Res<String> {
    let top = || run(dir, &["rev-parse", "-q", "--verify", "refs/stash"]).unwrap_or_default();
    let before = top();
    run(dir, &["stash", "push", "-u", "-m", msg])?;
    if top() == before {
        return f(); // nothing was stashed: never pop someone else's entry
    }
    let res = f();
    // Outside any cancel scope: a cancelled op must still give the changes back.
    let pop = Cancel::default().scope(|| {
        run(dir, &["stash", "pop", "--index"]).or_else(|e| {
            // Staged changes that don't fit the new HEAD come back unstaged.
            if e.contains("--index") { run(dir, &["stash", "pop"]) } else { Err(e) }
        })
    });
    match (res, pop) {
        (res, Ok(_)) => res,
        (_, Err(e)) => Err(format!(
            "Your changes conflict with the new state: resolve the conflicts. They are also kept in the stash \"{msg}\"; drop it when you're done.\n\n{e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::git::CANCELLED;

    /// A cancelled autostash op still gives the stashed changes back.
    #[cfg(unix)]
    #[test]
    fn cancelled_autostash_reapplies() {
        let dir = std::env::temp_dir().join(format!("glasir-cancel-autostash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("f"), "one\n").unwrap();
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("f"), "mine\n").unwrap();
        let cancel = Cancel::default();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            c2.cancel();
        });
        // pull runs fetch, which runs upload-pack: cancel must stop git's children too.
        let late = dir.join("late");
        let pack = format!("remote.origin.uploadpack=sleep 1; touch '{}'; :", late.display());
        g(&["remote", "add", "origin", &dir.to_string_lossy()]);
        let res = cancel.scope(|| with_autostash(&dir, "auto", || run(&dir, &["-c", &pack, "pull", "origin", "main"])));
        assert_eq!(res, Err(CANCELLED.to_owned()));
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "mine\n");
        assert_eq!(g(&["stash", "list"]), "");
        std::thread::sleep(std::time::Duration::from_millis(1000));
        assert!(!late.exists(), "git's children kept running");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
