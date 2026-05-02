//! Read-only queries (log, refs, status, stashes, remotes, commit details,
//! conflict prediction) and the parsers for their output.

use super::{Change, Commit, CommitInfo, Ref, RefKind, Res, Stash, State, Status, cmd, run};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%s";
const DATE_FORMAT: &str = "--date=format-local:%Y-%m-%d %H:%M";

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
            "--format=%(refname)%1f%(objectname)%1f%(*objectname)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f%(symref)%1f%(creatordate:unix)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
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
    if has("rebase-merge") || has("rebase-apply") {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_v2() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 M. N... 100644 100644 100644 a b src/a b.rs\0\
1 .M N... 100644 100644 100644 a b x.rs\0\
2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conf.rs\0? new file.txt\0";
        let s = parse_status(out);
        assert_eq!((s.branch.as_deref(), s.head.as_deref(), s.ahead, s.behind), (Some("main"), Some("abc"), 2, 1));
        assert_eq!(s.staged.len(), 2);
        assert_eq!(s.staged[0].path, "src/a b.rs");
        assert_eq!(s.staged[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(s.unstaged.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["x.rs", "new file.txt"]);
        assert_eq!(s.conflicted[0].path, "conf.rs");
    }
}
