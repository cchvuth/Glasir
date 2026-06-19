//! Read-only queries (log, refs, status, stashes, remotes, commit details,
//! worktrees, conflict prediction) and the parsers for their output.

use super::{Change, Commit, CommitInfo, Ref, RefKind, Res, Stash, State, Status, Worktree, cmd, run};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%s";
pub const DATE_FORMAT: &str = "--date=format-local:%Y-%m-%d %H:%M";

pub fn parse_log(out: &str) -> Vec<Commit> {
    out.split('\0')
        .filter_map(|rec| {
            let f: Vec<&str> = rec.trim_start_matches('\n').splitn(7, '\x1f').collect();
            (f.len() == 7).then(|| Commit {
                id: f[0].into(),
                parents: f[1].split_whitespace().map(Into::into).collect(),
                author: f[2].into(),
                email: f[3].into(),
                time: f[4].parse().unwrap_or(0),
                date: f[5].into(),
                summary: f[6].into(),
            })
        })
        .collect()
}

/// All branches, remotes, tags (and HEAD), newest first, children before parents.
pub fn log(dir: &Path, limit: usize, extra: &[&str]) -> Res<Vec<Commit>> {
    let n = format!("-n{limit}");
    let mut args = vec!["log", "--date-order", &n, "-z", DATE_FORMAT, LOG_FORMAT];
    args.extend_from_slice(extra);
    args.push("--");
    Ok(parse_log(&run(dir, &args)?))
}

pub fn refs(dir: &Path) -> Res<Vec<Ref>> {
    let out = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%1f%(objectname)%1f%(*objectname)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f%(symref)%1f%(creatordate:unix)%1f%(worktreepath)%1f%(if)%(*objectname)%(then)%(contents:subject)%0a%0a%(contents:body)%(end)%00",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    // NUL-terminated records: an annotated tag's message spans lines.
    Ok(out
        .split('\0')
        .filter_map(|l| {
            let f: Vec<&str> = l.trim_start_matches('\n').splitn(10, '\x1f').collect();
            if f.len() < 8 || !f[6].is_empty() {
                return None; // symrefs like origin/HEAD
            }
            let (kind, name) = if let Some(n) = f[0].strip_prefix("refs/heads/") {
                (RefKind::Local, n)
            } else if let Some(n) = f[0].strip_prefix("refs/remotes/") {
                (RefKind::Remote, n)
            } else {
                (RefKind::Tag, f[0].strip_prefix("refs/tags/")?)
            };
            let num = |key: &str| {
                f[4].split(", ").find_map(|p| p.strip_prefix(key)).and_then(|n| n.trim().parse().ok()).unwrap_or(0)
            };
            Some(Ref {
                kind,
                name: name.into(),
                full: f[0].into(),
                target: if f[2].is_empty() { f[1] } else { f[2] }.into(),
                upstream: f[3].into(),
                ahead: num("ahead "),
                behind: num("behind "),
                gone: f[4] == "gone",
                head: f[5] == "*",
                date: f[7].parse().unwrap_or(0),
                worktree: f.get(8).filter(|w| f[5] != "*" && !w.is_empty()).map(|w| w.to_string()),
                message: f.get(9).map_or("", |m| m.trim_end()).into(),
            })
        })
        .collect())
}

pub fn parse_status(out: &str) -> Status {
    let mut s = Status::default();
    let mut it = out.split('\0');
    while let Some(rec) = it.next() {
        let Some(kind) = rec.chars().next() else { continue };
        match kind {
            '#' => {
                let mut p = rec[2..].splitn(2, ' ');
                let (k, v) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
                match k {
                    "branch.oid" if v != "(initial)" => s.head = Some(v.into()),
                    "branch.head" if v != "(detached)" => s.branch = Some(v.into()),
                    "branch.upstream" => s.upstream = Some(v.into()),
                    "branch.ab" => {
                        let mut n = v.split(' ').map(|x| x[1..].parse().unwrap_or(0));
                        s.ahead = n.next().unwrap_or(0);
                        s.behind = n.next().unwrap_or(0);
                    }
                    _ => {}
                }
            }
            '1' | '2' => {
                let fields = if kind == '1' { 9 } else { 10 };
                let f: Vec<&str> = rec.splitn(fields, ' ').collect();
                let Some(path) = f.get(fields - 1) else { continue };
                let old = if kind == '2' { it.next().map(String::from) } else { None };
                let xy: Vec<char> = f[1].chars().collect();
                if f[2].starts_with('S') {
                    s.submodules.insert(path.to_string(), f[2].into());
                }
                if xy[0] != '.' {
                    s.staged.push(Change { path: path.to_string(), old_path: old, code: xy[0] });
                }
                if xy[1] != '.' {
                    s.unstaged.push(Change { path: path.to_string(), old_path: None, code: xy[1] });
                }
            }
            'u' => {
                if let Some(path) = rec.splitn(11, ' ').nth(10) {
                    s.conflicted.push(Change { path: path.into(), old_path: None, code: 'U' });
                }
            }
            '?' => s.unstaged.push(Change { path: rec[2..].into(), old_path: None, code: '?' }),
            _ => {}
        }
    }
    s
}

pub fn status(dir: &Path) -> Res<Status> {
    run(dir, &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"]).map(|o| parse_status(&o))
}

pub fn stashes(dir: &Path) -> Res<Vec<Stash>> {
    let out = run(dir, &["stash", "list", "-z", "--format=%H%x1f%P%x1f%gd%x1f%gs"])?;
    Ok(out
        .split('\0')
        .filter_map(|r| {
            let f: Vec<&str> = r.trim_start_matches('\n').splitn(4, '\x1f').collect();
            (f.len() == 4).then(|| Stash {
                id: f[0].into(),
                base: f[1].split(' ').next().unwrap_or("").into(),
                name: f[2].into(),
                message: f[3].into(),
            })
        })
        .collect())
}

pub fn git_dir(dir: &Path) -> Res<PathBuf> {
    Ok(PathBuf::from(run(dir, &["rev-parse", "--absolute-git-dir"])?.trim()))
}

pub fn state(git_dir: &Path) -> State {
    let has = |f: &str| git_dir.join(f).exists();
    if has("rebase-apply/applying") {
        State::Applying
    } else if has("rebase-merge") || has("rebase-apply") {
        State::Rebasing
    } else if has("MERGE_HEAD") {
        State::Merging
    } else if has("CHERRY_PICK_HEAD") {
        State::CherryPicking
    } else if has("REVERT_HEAD") {
        State::Reverting
    } else if has("BISECT_LOG") {
        State::Bisecting
    } else {
        State::Clean
    }
}

/// `(name, fetch url)` for each remote, with `insteadOf` rewrites applied.
pub fn remotes(dir: &Path) -> Res<Vec<(String, String)>> {
    let out = run(dir, &["remote", "-v"]).unwrap_or_default();
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once('\t')?;
            let url = rest.strip_suffix(" (fetch)")?;
            Some((name.to_owned(), url.to_owned()))
        })
        .collect())
}

/// `remote.*`, `branch.*` and `user.*` settings from every config scope, in git's order (last wins).
pub fn config(dir: &Path) -> Vec<(String, String)> {
    parse_config(&run(dir, &["config", "-z", "--get-regexp", r"^(remote|branch|user)\."]).unwrap_or_default())
}

/// `git config -z --get-regexp`: `key\nvalue\0` records (a bare `key\0` for a valueless key).
fn parse_config(out: &str) -> Vec<(String, String)> {
    out.split('\0').filter(|r| !r.is_empty()).map(|r| r.split_once('\n').map_or((r.into(), String::new()), |(k, v)| (k.into(), v.into()))).collect()
}

fn parse_name_status(out: &str) -> Vec<Change> {
    let mut files = vec![];
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(st) = it.next() {
        let code = st.chars().next().unwrap_or('M');
        let Some(a) = it.next() else { break };
        if code == 'R' || code == 'C' {
            let Some(b) = it.next() else { break };
            files.push(Change { path: b.into(), old_path: Some(a.into()), code });
        } else {
            files.push(Change { path: a.into(), old_path: None, code });
        }
    }
    files
}

pub fn commit_info(dir: &Path, id: &str) -> Res<CommitInfo> {
    let out = run(
        dir,
        &["show", "-s", DATE_FORMAT, "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%ad%x1f%cn%x1f%ce%x1f%cd%x1f%B", id, "--"],
    )?;
    let f: Vec<&str> = out.splitn(9, '\x1f').collect();
    if f.len() < 9 {
        return Err(format!("unexpected output for {id}"));
    }
    let parents: Vec<String> = f[1].split_whitespace().map(Into::into).collect();
    let mut args = vec!["diff-tree", "-r", "-z", "--no-commit-id", "--name-status", "-M"];
    match parents.first() {
        Some(p) => args.push(p),
        None => args.push("--root"),
    }
    args.push(id);
    let files = parse_name_status(&run(dir, &args)?);
    Ok(CommitInfo {
        id: f[0].into(),
        parents,
        author: f[2].into(),
        email: f[3].into(),
        date: f[4].into(),
        committer: f[5].into(),
        committer_date: f[7].into(),
        message: f[8].trim_end().into(),
        files,
        untracked: vec![],
    })
}

/// `commit_info` for a stash, plus the untracked files it saved (`<stash>^3`).
pub fn stash_info(dir: &Path, id: &str) -> Res<CommitInfo> {
    let mut info = commit_info(dir, id)?;
    if info.parents.len() > 2 {
        info.untracked = parse_name_status(&run(dir, &["diff-tree", "-r", "-z", "--no-commit-id", "--name-status", "--root", &info.parents[2]])?);
    }
    Ok(info)
}

pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    out.split("\0\0")
        .filter_map(|rec| {
            let mut w = Worktree::default();
            for line in rec.split('\0') {
                let (k, v) = line.split_once(' ').unwrap_or((line, ""));
                match k {
                    "worktree" => w.path = v.into(),
                    "HEAD" => w.head = v.into(),
                    "branch" => w.branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v).into()),
                    "locked" => w.locked = Some(v.into()),
                    "prunable" => w.prunable = true,
                    "bare" => w.bare = true,
                    _ => {}
                }
            }
            (!w.path.is_empty()).then_some(w)
        })
        .collect()
}

/// All worktrees of the repository, with their change counts.
pub fn worktrees(dir: &Path) -> Res<Vec<Worktree>> {
    let mut list = parse_worktrees(&run(dir, &["worktree", "list", "--porcelain", "-z"])?);
    if list.len() > 1 {
        for w in list.iter_mut().filter(|w| !w.prunable && !w.bare) {
            let st = run(Path::new(&w.path), &["status", "--porcelain=v2", "-z", "--untracked-files=normal"]).unwrap_or_default();
            w.changes = st.split('\0').filter(|r| matches!(r.get(..2), Some("1 " | "2 " | "u " | "? "))).count();
        }
    }
    Ok(list)
}

/// Files that would conflict if `other` were merged into HEAD, via an in-memory
/// `merge-tree` (git ≥ 2.38). `None` when it can't be determined.
pub fn predict_conflicts(dir: &Path, other: &str) -> Option<Vec<String>> {
    let out = cmd(dir)
        .args(["merge-tree", "--write-tree", "--name-only", "--no-messages", "-z", "HEAD", other])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    match out.status.code() {
        Some(0) => Some(vec![]),
        Some(1) => Some(String::from_utf8_lossy(&out.stdout).split('\0').skip(1).filter(|s| !s.is_empty()).map(String::from).collect()),
        _ => None,
    }
}

/// Files changed from `from` to `to`, shaped like a commit so the inspector can
/// show it. `three_dot`: compare against their merge base (what a PR would show).
pub fn compare_info(dir: &Path, from: &str, to: &str, three_dot: bool, label: &str) -> Res<CommitInfo> {
    let base = match three_dot {
        true => run(dir, &["merge-base", from, to])?,
        false => match run(dir, &["rev-parse", "-q", "--verify", from]) {
            // The root commit's `^`: compare against the empty tree.
            Err(_) if from.ends_with('^') => super::run_input(dir, &["hash-object", "-t", "tree", "--stdin"], Some(b""))?,
            r => r?,
        },
    };
    let base = base.trim().to_owned();
    let to_id = run(dir, &["rev-parse", to])?.trim().to_owned();
    let files = parse_name_status(&run(dir, &["diff-tree", "-r", "-z", "--name-status", "-M", &base, &to_id])?);
    Ok(CommitInfo { id: to_id, parents: vec![base], message: label.into(), files, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_records() {
        let c = parse_config("remote.origin.url\ngit@ghe:o/r.git\0remote.origin.fetch\n+refs/heads/*:refs/remotes/origin/*\0user.email\na@b\nc\0remote.x.mirror\0");
        assert_eq!(c[0], ("remote.origin.url".into(), "git@ghe:o/r.git".into()));
        assert_eq!(c[2], ("user.email".into(), "a@b\nc".into()));
        assert_eq!(c[3], ("remote.x.mirror".into(), String::new()));
    }

    #[test]
    fn worktree_porcelain() {
        let out = "worktree /r\0HEAD aaa\0branch refs/heads/main\0\0worktree /r-det\0HEAD bbb\0detached\0locked on usb\0\0worktree /gone\0HEAD ccc\0branch refs/heads/f/x\0prunable gitdir file points to non-existent location\0\0";
        let w = parse_worktrees(out);
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].path.as_str(), w[0].branch.as_deref()), ("/r", Some("main")));
        assert_eq!((w[1].branch.as_deref(), w[1].locked.as_deref()), (None, Some("on usb")));
        assert_eq!((w[2].branch.as_deref(), w[2].prunable), (Some("f/x"), true));
    }

    #[test]
    fn status_v2() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 M. N... 100644 100644 100644 a b src/a b.rs\0\
1 .M N... 100644 100644 100644 a b x.rs\0\
1 .M S.MU 160000 160000 160000 a a sub mod\0\
2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conf.rs\0? new file.txt\0";
        let s = parse_status(out);
        assert_eq!((s.branch.as_deref(), s.head.as_deref(), s.ahead, s.behind), (Some("main"), Some("abc"), 2, 1));
        assert_eq!(s.staged.len(), 2);
        assert_eq!(s.staged[0].path, "src/a b.rs");
        assert_eq!(s.staged[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(s.unstaged.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["x.rs", "sub mod", "new file.txt"]);
        assert_eq!((s.submodules.len(), s.submodules["sub mod"].as_str()), (1, "S.MU"));
        assert_eq!(s.conflicted[0].path, "conf.rs");
    }

    #[test]
    fn compare_info_lists_branch_changes() {
        let dir = std::env::temp_dir().join(format!("glasir-compare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        for (n, f) in [("c1", "a"), ("c2", "b"), ("c3", "c")] {
            std::fs::write(dir.join(f), format!("{n}\n")).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", n]);
        }
        g(&["checkout", "-q", "-b", "feat", "HEAD~1"]);
        std::fs::write(dir.join("f"), "x\n").unwrap();
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "feat"]);
        let info = compare_info(&dir, "main", "feat", true, "label").unwrap();
        // Only feat's own change, not main's c3 (merge-base comparison).
        assert_eq!(info.files.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["f"]);
        let root = g(&["rev-list", "--max-parents=0", "HEAD"]);
        let info = compare_info(&dir, &format!("{}^", root.trim()), "main", false, "label").unwrap();
        assert_eq!(info.files.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);

        // Annotated tag messages ride along with the refs; lightweight tags and branches have none.
        g(&["tag", "-a", "v1", "-m", "Release one\n\nWith notes", "main"]);
        g(&["tag", "light", "main"]);
        let refs = refs(&dir).unwrap();
        let msg = |n: &str| refs.iter().find(|r| r.name == n).map(|r| r.message.clone()).unwrap();
        assert_eq!((msg("v1").as_str(), msg("light").as_str(), msg("main").as_str()), ("Release one\n\nWith notes", "", ""));
        assert_eq!(refs.iter().find(|r| r.name == "feat").map(|r| r.head), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
