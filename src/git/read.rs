//! Read-only queries (log, refs, status, stashes, remotes, commit details,
//! worktrees, conflict prediction) and the parsers for their output.

use super::{Change, Commit, CommitInfo, Ref, RefKind, Res, Stash, State, Stats, Status, Worktree, cmd, run};
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
            "--format=%(refname)%1f%(objectname)%1f%(*objectname)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f%(symref)%1f%(creatordate:unix)%1f%(worktreepath)%1f%(if)%(*objectname)%(then)%(contents)%1f%(contents:signature)%(end)%00",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    // NUL-terminated records: an annotated tag's message spans lines.
    Ok(out
        .split('\0')
        .filter_map(|l| {
            let f: Vec<&str> = l.trim_start_matches('\n').splitn(11, '\x1f').collect();
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
                annotated: !f[2].is_empty(),
                // `%(contents)` keeps the message byte for byte (`subject` joins wrapped lines);
                // a signed tag's signature is not part of it.
                message: f.get(9).map_or("", |m| f.get(10).and_then(|sig| m.strip_suffix(sig)).unwrap_or(m).trim_end()).into(),
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
                // Either side can be the rename (`.R`: worktree rename of an intent-to-add file).
                let from = |c: char| old.clone().filter(|_| matches!(c, 'R' | 'C'));
                if xy[0] != '.' {
                    s.staged.push(Change { path: path.to_string(), old_path: from(xy[0]), code: xy[0] });
                }
                if xy[1] != '.' {
                    s.unstaged.push(Change { path: path.to_string(), old_path: from(xy[1]), code: xy[1] });
                }
            }
            'u' => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if let Some(path) = f.get(10) {
                    s.conflict_xy.insert(path.to_string(), f[1].into());
                    s.conflicted.push(Change { path: path.to_string(), old_path: None, code: 'U' });
                }
            }
            '?' => s.unstaged.push(Change { path: rec[2..].into(), old_path: None, code: '?' }),
            _ => {}
        }
    }
    s
}

/// What an unmerged `XY` code means, and whether (current, incoming) still has a version of the file.
/// X is our side (`D` = we deleted it, `A` = we added it), Y theirs.
pub fn conflict_kind(xy: &str) -> (&'static str, [bool; 2]) {
    match xy {
        "UU" => ("Both modified", [true, true]),
        "AA" => ("Both added", [true, true]),
        "DD" => ("Both deleted", [false, false]),
        "AU" => ("Added by us", [true, false]),
        "UA" => ("Added by them", [false, true]),
        "DU" => ("Deleted by us", [false, true]),
        "UD" => ("Deleted by them", [true, false]),
        _ => ("Conflicted", [true, true]),
    }
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
    } else if let Ok(todo) = std::fs::read_to_string(git_dir.join("sequencer/todo")) {
        // A multi-commit pick/revert whose conflicted step was committed by hand: more to do.
        if todo.trim_start().starts_with("revert") { State::Reverting } else { State::CherryPicking }
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

/// `remote.*`, `branch.*` and `user.*` settings, plus the commit template, comment char and
/// signing defaults and merge / diff tools, from every config scope, in git's order (last wins).
pub fn config(dir: &Path) -> Vec<(String, String)> {
    parse_config(&run(dir, &["config", "-z", "--get-regexp", r"^(remote|branch|user)\.|^(commit\.template|core\.(comment(char|string)|sshcommand)|(commit|tag)\.gpgsign|gpg\.format|(merge|diff)\.(gui)?tool)$"]).unwrap_or_default())
}

/// The (name, email) git authors commits with: author.* and GIT_AUTHOR_* beat user.*.
/// Empty when git has no identity configured (not a guessed `login@hostname` one).
pub fn author(dir: &Path) -> (String, String) {
    run(dir, &["-c", "user.useConfigOnly=true", "var", "GIT_AUTHOR_IDENT"]).ok().and_then(|s| parse_ident(&s)).unwrap_or_default()
}

/// `Name <email> 1700000000 +0100`.
fn parse_ident(s: &str) -> Option<(String, String)> {
    let (name, rest) = s.split_once('<')?;
    Some((name.trim().into(), rest.split_once('>')?.0.into()))
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

/// `diff-tree --numstat -z`: `added\tdeleted\tpath\0`, or `added\tdeleted\t\0old\0new\0` for a rename.
fn parse_numstat(out: &str) -> Stats {
    let mut stats = Stats::new();
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(rec) = it.next() {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        let path = if p.is_empty() {
            let _old = it.next();
            let Some(new) = it.next() else { break };
            new
        } else {
            p
        };
        stats.insert(path.into(), a.parse().ok().zip(d.parse().ok()));
    }
    stats
}

/// Line counts of `id` against `base` (`None`: a root commit). Slow on huge commits, so run it off the UI thread.
pub fn numstat(dir: &Path, base: Option<&str>, id: &str) -> Res<Stats> {
    let out = match (id, base) {
        // A comparison with the working tree: the files on disk against `base`.
        (WORKTREE, Some(b)) => run(dir, &["diff", "-z", "-M", "--numstat", b])?,
        _ => run(dir, &["diff-tree", "-r", "-z", "-M", "--numstat", "--no-commit-id", base.unwrap_or("--root"), id])?,
    };
    Ok(parse_numstat(&out))
}

/// `Co-authored-by: Name <email>` lines of a commit message.
fn parse_coauthors(msg: &str) -> Vec<(String, String)> {
    msg.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("co-authored-by") {
                return None;
            }
            let (name, email) = v.rsplit_once('<').map_or((v, ""), |(n, e)| (n, e.trim_end().trim_end_matches('>')));
            Some((name.trim().into(), email.trim().into()))
        })
        .collect()
}

pub fn commit_info(dir: &Path, id: &str) -> Res<CommitInfo> {
    let out = run(
        dir,
        &["show", "-s", DATE_FORMAT, "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%ad%x1f%cn%x1f%ce%x1f%cd%x1f%G?%x1f%GS%x1f%GK%x1f%at%x1f%ct%x1f%B", id, "--"],
    )?;
    let f: Vec<&str> = out.splitn(14, '\x1f').collect();
    if f.len() < 14 {
        return Err(format!("unexpected output for {id}"));
    }
    let parents: Vec<String> = f[1].split_whitespace().map(Into::into).collect();
    let mut args = vec!["diff-tree", "-r", "-z", "--no-commit-id", "--name-status", "-M"];
    args.push(parents.first().map_or("--root", String::as_str));
    args.push(id);
    let files = parse_name_status(&run(dir, &args)?);
    Ok(CommitInfo {
        id: f[0].into(),
        parents,
        author: f[2].into(),
        email: f[3].into(),
        date: f[4].into(),
        time: f[11].parse().unwrap_or(0),
        committer: f[5].into(),
        committer_email: f[6].into(),
        committer_date: f[7].into(),
        committer_time: f[12].parse().unwrap_or(0),
        coauthors: parse_coauthors(f[13]),
        message: f[13].trim_end().into(),
        files,
        untracked: vec![],
        stats: Stats::new(),
        sig: sig_code(f[8], || run(dir, &["cat-file", "commit", f[0]]).unwrap_or_default()),
        signer: f[9].into(),
        key: f[10].into(),
    })
}

/// `%G?`, except 'E' for a signed commit git didn't check (SSH without
/// gpg.ssh.allowedSignersFile reports 'N'); `raw` is the `cat-file commit` text.
fn sig_code(code: &str, raw: impl FnOnce() -> String) -> char {
    match code.chars().next().unwrap_or('N') {
        'N' if raw().split("\n\n").next().unwrap_or("").lines().any(|l| l.starts_with("gpgsig")) => 'E',
        c => c,
    }
}

/// `git verify-tag`: whether the signature is good, and what gpg / ssh-keygen said (on stderr).
pub fn verify_tag(dir: &Path, full: &str) -> (bool, String) {
    match cmd(dir).args(["verify-tag", full]).stdin(Stdio::null()).output() {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stderr).trim().into()),
        Err(e) => (false, e.to_string()),
    }
}

/// `commit_info` for a stash, plus the untracked files it saved (`<stash>^3`).
pub fn stash_info(dir: &Path, id: &str) -> Res<CommitInfo> {
    let mut info = commit_info(dir, id)?;
    info.sig = '\0';
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

/// All worktrees of the repository, with their change counts (scanned in parallel). `dir`'s own
/// worktree is left at 0: the caller already has its status.
pub fn worktrees(dir: &Path) -> Res<Vec<Worktree>> {
    let mut list = parse_worktrees(&run(dir, &["worktree", "list", "--porcelain", "-z"])?);
    if list.len() > 1 {
        let me = dir.canonicalize().ok();
        std::thread::scope(|s| {
            for w in list.iter_mut().filter(|w| !w.prunable && !w.bare && Path::new(&w.path).canonicalize().ok() != me) {
                s.spawn(move || {
                    let st = run(Path::new(&w.path), &["status", "--porcelain=v2", "-z", "--untracked-files=normal"]).unwrap_or_default();
                    w.changes = st.split('\0').filter(|r| matches!(r.get(..2), Some("1 " | "2 " | "u " | "? "))).count();
                });
            }
        });
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

/// `to` of a comparison with the working tree (not a valid ref name, so it can't collide).
pub const WORKTREE: &str = "(working tree)";

/// Files changed from `from` to `to`, shaped like a commit so the inspector can
/// show it. `three_dot`: compare against their merge base (what a PR would show).
/// `to == WORKTREE`: tracked files as they are on disk now (`git diff <from>`).
pub fn compare_info(dir: &Path, from: &str, to: &str, three_dot: bool, label: &str) -> Res<CommitInfo> {
    let wt = to == WORKTREE;
    let base = match three_dot && !wt {
        true => run(dir, &["merge-base", from, to])?,
        false => match run(dir, &["rev-parse", "-q", "--verify", from]) {
            // The root commit's `^`: compare against the empty tree.
            Err(_) if from.ends_with('^') => super::run_input(dir, &["hash-object", "-t", "tree", "--stdin"], Some(b""))?,
            r => r?,
        },
    };
    let base = base.trim().to_owned();
    let (to_id, out) = match wt {
        true => (to.to_owned(), run(dir, &["diff", "-z", "--name-status", "-M", &base])?),
        false => {
            let id = run(dir, &["rev-parse", to])?.trim().to_owned();
            let out = run(dir, &["diff-tree", "-r", "-z", "--name-status", "-M", &base, &id])?;
            (id, out)
        }
    };
    let files = parse_name_status(&out);
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
    fn ident() {
        assert_eq!(parse_ident("Ann Lee <a@x.example> 1700000000 +0100\n"), Some(("Ann Lee".into(), "a@x.example".into())));
        assert_eq!(parse_ident(" <> 1 +0000"), Some((String::new(), String::new())));
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
2 .R N... 100644 100644 100644 a a R100 moved.rs\0was.rs\0\
u UU N... 100644 100644 100644 100644 a b c conf.rs\0? new file.txt\0";
        let s = parse_status(out);
        assert_eq!((s.branch.as_deref(), s.head.as_deref(), s.ahead, s.behind), (Some("main"), Some("abc"), 2, 1));
        assert_eq!(s.staged.len(), 2);
        assert_eq!(s.staged[0].path, "src/a b.rs");
        assert_eq!(s.staged[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(s.unstaged.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["x.rs", "sub mod", "moved.rs", "new file.txt"]);
        assert_eq!((s.unstaged[0].old_path.as_deref(), s.unstaged[2].old_path.as_deref()), (None, Some("was.rs")));
        assert_eq!((s.submodules.len(), s.submodules["sub mod"].as_str()), (1, "S.MU"));
        assert_eq!(s.conflicted[0].path, "conf.rs");
        assert_eq!(s.conflict_xy["conf.rs"], "UU");
    }

    #[test]
    fn conflict_codes() {
        assert_eq!(conflict_kind("UU"), ("Both modified", [true, true]));
        assert_eq!(conflict_kind("AA").0, "Both added");
        assert_eq!(conflict_kind("DD"), ("Both deleted", [false, false]));
        assert_eq!(conflict_kind("DU"), ("Deleted by us", [false, true]));
        assert_eq!(conflict_kind("UD"), ("Deleted by them", [true, false]));
        assert_eq!(conflict_kind("AU"), ("Added by us", [true, false]));
        assert_eq!(conflict_kind("UA"), ("Added by them", [false, true]));
        assert_eq!(conflict_kind("??").0, "Conflicted");
    }

    #[test]
    fn numstat_records() {
        let out = "3\t1\tsrc/a b.rs\0-\t-\timg.png\00\t0\t\0old.rs\0new.rs\0";
        let s = parse_numstat(out);
        assert_eq!((s["src/a b.rs"], s["img.png"], s["new.rs"]), (Some((3, 1)), None, Some((0, 0))));
        assert_eq!(s.len(), 3);
        assert!(parse_numstat("").is_empty());
    }

    #[test]
    fn coauthor_trailers() {
        let msg = "Fix it\n\nBody: text\n\nCo-authored-by: Ann Lee <ann@x.example>\nco-authored-by:Bo <bo@y>\nSigned-off-by: Me <me@z>\nCo-authored-by: No Mail\n";
        assert_eq!(parse_coauthors(msg), [("Ann Lee".into(), "ann@x.example".into()), ("Bo".into(), "bo@y".into()), ("No Mail".into(), String::new())]);
        assert!(parse_coauthors("plain").is_empty());
    }

    #[test]
    fn commit_info_details_and_numstat() {
        let dir = std::env::temp_dir().join(format!("glasir-details-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("old.txt"), "1\n2\n3\n4\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "x\ny\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "one"]);
        let root = g(&["rev-parse", "HEAD"]).trim().to_owned();
        g(&["mv", "old.txt", "new.txt"]);
        std::fs::write(dir.join("new.txt"), "1\n2\n3\n4\n5\n").unwrap();
        g(&["rm", "-q", "gone.txt"]);
        std::fs::write(dir.join("added.txt"), "a\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "two\n\nCo-authored-by: Ann <ann@x>"]);
        let info = commit_info(&dir, "HEAD").unwrap();
        assert_eq!(info.coauthors, [("Ann".into(), "ann@x".into())]);
        assert!(info.time > 0 && info.committer_time > 0 && info.committer_email == "t@t");
        let s = numstat(&dir, info.parents.first().map(String::as_str), &info.id).unwrap();
        assert_eq!((s["new.txt"], s["gone.txt"], s["added.txt"]), (Some((1, 0)), Some((0, 2)), Some((1, 0))));
        assert_eq!(s.len(), 3);
        // Root commit: everything is added.
        let s = numstat(&dir, None, &root).unwrap();
        assert_eq!((s["old.txt"], s["gone.txt"]), (Some((4, 0)), Some((2, 0))));
        // Against the working tree: unstaged edits count too.
        std::fs::write(dir.join("added.txt"), "a\nb\nc\n").unwrap();
        let s = numstat(&dir, Some(&info.id), WORKTREE).unwrap();
        assert_eq!((s["added.txt"], s.len()), (Some((2, 0)), 1));
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A cherry-pick/revert of several commits leaves only `sequencer/` once a
    /// conflicted step is committed by hand; the rest still has to run.
    #[test]
    fn leftover_sequencer_is_in_progress() {
        let dir = std::env::temp_dir().join(format!("glasir-seq-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sequencer")).unwrap();
        assert_eq!(state(&dir), State::Clean);
        std::fs::write(dir.join("sequencer/todo"), "pick abc B\npick def C\n").unwrap();
        assert_eq!(state(&dir), State::CherryPicking);
        std::fs::write(dir.join("sequencer/todo"), "revert abc B\n").unwrap();
        assert_eq!(state(&dir), State::Reverting);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
