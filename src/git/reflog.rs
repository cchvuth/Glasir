//! HEAD's reflog: every commit HEAD has pointed at (commits, resets, rebases,
//! checkouts), newest first, flagging the ones no ref reaches any more.

use super::{Res, run, run_input};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct ReflogEntry {
    pub id: String,
    /// Unix time HEAD moved here.
    pub time: i64,
    /// What moved it: `commit (amend)`, `reset`, `rebase (finish)`, `checkout`…
    pub action: String,
    /// The rest of the reflog message (`moving to HEAD~2`, a commit summary…).
    pub detail: String,
    /// The commit's own summary.
    pub summary: String,
    /// No branch, tag, remote branch or HEAD reaches this commit: only the reflog remembers it.
    pub lost: bool,
}

/// Parses `git log -g -z --date=unix --format=%H%x1f%gd%x1f%gs%x1f%s`.
pub fn parse_reflog(out: &str) -> Vec<ReflogEntry> {
    out.split('\0')
        .filter_map(|rec| {
            let f: Vec<&str> = rec.trim_start_matches('\n').splitn(4, '\x1f').collect();
            let [id, gd, gs, summary] = f[..] else { return None };
            // %gd is `HEAD@{<unix time>}` with --date=unix.
            let time = gd.rsplit_once("@{")?.1.trim_end_matches('}').parse().ok()?;
            let (action, detail) = gs.split_once(": ").unwrap_or((gs, ""));
            Some(ReflogEntry { id: id.into(), time, action: action.into(), detail: detail.into(), summary: summary.into(), lost: false })
        })
        .collect()
}

pub fn reflog(dir: &Path) -> Res<Vec<ReflogEntry>> {
    let mut v = parse_reflog(&run(dir, &["log", "-g", "-z", "-n2000", "--date=unix", "--format=%H%x1f%gd%x1f%gs%x1f%s", "HEAD", "--"])?);
    // Commits reachable from the reflog's commits but from no ref: the lost ones.
    let ids: HashSet<&str> = v.iter().map(|e| e.id.as_str()).collect();
    let input = ids.into_iter().collect::<Vec<_>>().join("\n");
    let args = ["rev-list", "--ignore-missing", "--stdin", "--not", "--exclude=refs/glasir/*", "--all"];
    let lost: HashSet<String> = run_input(dir, &args, Some(input.as_bytes())).unwrap_or_default().lines().map(String::from).collect();
    for e in &mut v {
        e.lost = lost.contains(&e.id);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_in_another_worktree_is_not_lost() {
        let base = std::env::temp_dir().join(format!("glasir-reflog-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (dir, wt) = (base.join("main"), base.join("wt"));
        std::fs::create_dir_all(&dir).unwrap();
        let g = |d: &Path, a: &[&str]| run(d, a).unwrap();
        g(&dir, &["init", "-q"]);
        let commit = |m: &str| g(&dir, &["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", m]);
        commit("one");
        g(&dir, &["checkout", "-q", "-b", "side"]);
        commit("detached");
        let detached = g(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        g(&dir, &["checkout", "-q", "--detach", "HEAD"]);
        g(&dir, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), &detached]);
        g(&dir, &["branch", "-q", "-D", "side"]);
        commit("backed up");
        let backed = g(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        g(&dir, &["update-ref", "refs/glasir/backups/x", "HEAD"]);
        g(&dir, &["reset", "-q", "--hard", &detached]);
        g(&dir, &["checkout", "-q", "--detach", "HEAD~"]);
        let v = reflog(&dir).unwrap();
        let lost = |id: &str| v.iter().find(|e| e.id == id).unwrap().lost;
        assert!(!lost(&detached) && lost(&backed));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_reflog_records() {
        let out = "aaa\x1fHEAD@{1700000100}\x1freset: moving to HEAD~2\x1fFix login\0\nbbb\x1fHEAD@{1700000000}\x1fcommit (amend): Fix login: typo\x1fFix login: typo\0\nccc\x1fHEAD@{1699999999}\x1fpull\x1fSomething\0";
        let v = parse_reflog(out);
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].id.as_str(), v[0].time, v[0].action.as_str(), v[0].detail.as_str()), ("aaa", 1700000100, "reset", "moving to HEAD~2"));
        assert_eq!((v[1].action.as_str(), v[1].detail.as_str(), v[1].summary.as_str()), ("commit (amend)", "Fix login: typo", "Fix login: typo"));
        assert_eq!((v[2].action.as_str(), v[2].detail.as_str()), ("pull", ""));
        assert!(parse_reflog("").is_empty());
    }
}
