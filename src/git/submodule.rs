//! Submodules: `git submodule status` and `git diff --submodule=log` parsing.

use super::{Res, run};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Submodule {
    /// Relative to the top repository (nested ones included).
    pub path: String,
    /// The commit checked out in the submodule (the recorded one when not initialized).
    pub head: String,
    /// The commit the superproject's index records.
    pub recorded: String,
    /// `git submodule status` prefix: ' ' in sync, '-' not initialized,
    /// '+' checked-out commit differs from the recorded one, 'U' conflicted.
    pub flag: char,
}

/// ` <sha> <path> (<describe>)` lines; `-`/`+`/`U` replace the leading space.
pub fn parse_status(out: &str) -> Vec<Submodule> {
    out.lines()
        .filter_map(|l| {
            let flag = l.chars().next()?;
            let (sha, rest) = l[flag.len_utf8()..].split_once(' ')?;
            // The describe suffix is only there when the submodule is checked out.
            let path = if flag != '-' { rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")).map_or(rest, |p| p.0) } else { rest };
            Some(Submodule { path: path.into(), head: sha.into(), recorded: sha.into(), flag })
        })
        .collect()
}

/// All submodules, recursively. Empty (and no git call) without a `.gitmodules`.
pub fn submodules(dir: &Path) -> Res<Vec<Submodule>> {
    if !dir.join(".gitmodules").exists() {
        return Ok(vec![]);
    }
    let mut list = parse_status(&run(dir, &["submodule", "status", "--recursive"])?);
    if list.iter().any(|s| s.flag == '+') {
        let cached = parse_status(&run(dir, &["submodule", "status", "--cached", "--recursive"])?);
        for s in list.iter_mut() {
            if let Some(c) = cached.iter().find(|c| c.path == s.path) {
                s.recorded.clone_from(&c.head);
            }
        }
    }
    Ok(list)
}

/// A submodule entry of `git diff --submodule=log`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubmoduleDiff {
    /// Abbreviated commits; `0000000` for an added / removed submodule.
    pub from: String,
    pub to: String,
    /// git's remark: `rewind`, `new submodule`, `commits not present`...
    pub note: String,
    /// Commit subjects in the range: `> new` / `< dropped (rewind)`.
    pub log: Vec<String>,
    /// `contains modified content` / `contains untracked content`.
    pub dirty: Vec<String>,
}

/// `Submodule lib 00f9310..7999168 (rewind):` + `  < subject` lines. `None` if it isn't one.
pub fn parse_submodule_diff(out: &str) -> Option<SubmoduleDiff> {
    let mut d = SubmoduleDiff::default();
    let mut any = false;
    for l in out.lines() {
        if let Some(subject) = l.strip_prefix("  ") {
            d.log.push(subject.into());
            continue;
        }
        let rest = l.strip_prefix("Submodule ")?;
        any = true;
        if let Some((_, what)) = rest.rsplit_once(" contains ") {
            d.dirty.push(format!("contains {what}"));
            continue;
        }
        let rest = rest.trim_end_matches(':');
        let (head, note) = rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")).unwrap_or((rest, ""));
        let range = head.rsplit_once(' ')?.1;
        let (a, b) = range.split_once("...").or_else(|| range.split_once(".."))?;
        (d.from, d.to, d.note) = (a.into(), b.into(), note.into());
    }
    any.then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let out = "+7999168a lib (7999168)\n b0716c2c lib/deep (heads/main)\n-b0716c2c vendor/other (thing)\n b0716c2c with space (v1.0-3-gabc)\n";
        let s = parse_status(out);
        let got: Vec<(&str, char)> = s.iter().map(|s| (s.path.as_str(), s.flag)).collect();
        // Uninitialized: no describe, so a "(...)" is part of the path.
        assert_eq!(got, [("lib", '+'), ("lib/deep", ' '), ("vendor/other (thing)", '-'), ("with space", ' ')]);
        assert_eq!(s[0].head, "7999168a");
    }

    #[test]
    fn submodule_log_diff() {
        let d = parse_submodule_diff("Submodule lib 00f9310..7999168 (rewind):\n  < lib: add c\n").unwrap();
        assert_eq!((d.from.as_str(), d.to.as_str(), d.note.as_str()), ("00f9310", "7999168", "rewind"));
        assert_eq!(d.log, ["< lib: add c"]);
        let d = parse_submodule_diff("Submodule a b 0000000...c067836 (new submodule)\n").unwrap();
        assert_eq!((d.from.as_str(), d.to.as_str(), d.note.as_str()), ("0000000", "c067836", "new submodule"));
        let d = parse_submodule_diff("Submodule x contains modified content\nSubmodule x c067836..00f9310:\n  > b\n  > a\n").unwrap();
        assert_eq!((d.dirty.len(), d.log.len(), d.to.as_str()), (1, 2, "00f9310"));
        assert!(parse_submodule_diff("diff --git a/f b/f\n").is_none());
    }

    /// Real git: status flags, recorded vs checked-out commit, pointer diffs with subjects.
    #[test]
    fn submodule_workflow() {
        use crate::git::{Change, commit_diff, status, worktree_diff};
        let root = std::env::temp_dir().join(format!("glasir-submodules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (lib, app) = (root.join("lib"), root.join("app"));
        let g = |dir: &Path, args: &[&str]| {
            let mut a = vec!["-c", "protocol.file.allow=always", "-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false"];
            a.extend_from_slice(args);
            run(dir, &a).unwrap_or_else(|e| panic!("{args:?}: {e}"))
        };
        let commit = |dir: &Path, f: &str| {
            std::fs::write(dir.join(f), f).unwrap();
            g(dir, &["add", f]);
            g(dir, &["commit", "-q", "-m", &format!("add {f}")]);
        };
        for d in [&lib, &app] {
            std::fs::create_dir_all(d).unwrap();
            g(d, &["init", "-q", "-b", "main"]);
            commit(d, "a");
        }
        assert!(submodules(&app).unwrap().is_empty());
        g(&app, &["submodule", "add", "-q", "--", lib.to_str().unwrap(), "lib"]);
        g(&app, &["commit", "-q", "-m", "add lib"]);
        commit(&lib, "b");
        commit(&lib, "c");
        let sub = app.join("lib");
        g(&sub, &["pull", "-q", "origin", "main"]);
        g(&app, &["commit", "-q", "-am", "bump lib"]);
        let s = submodules(&app).unwrap();
        assert_eq!((s.len(), s[0].path.as_str(), s[0].flag), (1, "lib", ' '));

        // The bump commit's diff names the commits it brings in.
        let lib_change = Change { path: "lib".into(), old_path: None, code: 'M' };
        let d = commit_diff(&app, "HEAD", Some("HEAD~1"), &lib_change).unwrap().submodule.unwrap();
        assert_eq!(d.log, ["> add c", "> add b"]);

        // Check out an older commit in the submodule and dirty it.
        g(&sub, &["checkout", "-q", "HEAD~1"]);
        std::fs::write(sub.join("a"), "changed").unwrap();
        let s = submodules(&app).unwrap();
        assert_eq!(s[0].flag, '+');
        assert_ne!(s[0].head, s[0].recorded);
        assert_eq!(s[0].recorded, run(&app, &["rev-parse", "HEAD:lib"]).unwrap().trim());
        assert_eq!(status(&app).unwrap().submodules["lib"], "SCM.");
        let d = worktree_diff(&app, &lib_change, false).unwrap().submodule.unwrap();
        assert_eq!((d.note.as_str(), d.log.as_slice(), d.dirty.as_slice()), ("rewind", &["< add c".to_owned()][..], &["contains modified content".to_owned()][..]));
        let _ = std::fs::remove_dir_all(&root);
    }
}
