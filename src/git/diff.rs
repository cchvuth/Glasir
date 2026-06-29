//! Diffs: the parsed model ([`FileDiff`] / [`Hunk`] / [`Line`]), patches for
//! line-level staging, and per-file worktree / commit / untracked diffs.

use super::{Change, Res, run, run_raw};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    /// `diff --git`, `index`, `---`, `+++` ... lines, verbatim (used to rebuild patches).
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Not valid UTF-8: shown lossily, so it must not be patched line by line.
    pub lossy: bool,
    /// A submodule pointer change (shown instead of hunks).
    pub submodule: Option<super::SubmoduleDiff>,
    /// Git LFS pointer file: (old, new) sides.
    pub lfs: Option<(Option<super::LfsPointer>, Option<super::LfsPointer>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// b' ', b'+', b'-' or b'\\'
    pub kind: u8,
    pub text: String,
    /// 1-based line numbers, 0 = n/a.
    pub old: u32,
    pub new: u32,
}

pub fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = vec![];
    let (mut o, mut n) = (0u32, 0u32);
    for raw in text.split_terminator('\n') {
        if raw.starts_with("diff --git ") || files.is_empty() {
            files.push(FileDiff::default());
        }
        let f = files.last_mut().unwrap();
        if let Some(rest) = raw.strip_prefix("@@ ") {
            let mut nums = rest.split(' ').take(2).map(|s| s[1..].split(',').next().unwrap_or("0").parse().unwrap_or(0));
            let (os, ns) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
            (o, n) = (os, ns);
            f.hunks.push(Hunk { header: raw.into(), old_start: os, new_start: ns, lines: vec![] });
        } else if let Some(h) = f.hunks.last_mut() {
            let kind = raw.bytes().next().unwrap_or(b' ');
            let text = raw.get(1..).unwrap_or("").to_owned();
            let (lo, ln) = match kind {
                b'+' => (0, n),
                b'-' => (o, 0),
                b'\\' => (0, 0),
                _ => (o, n),
            };
            if lo > 0 {
                o += 1;
            }
            if ln > 0 {
                n += 1;
            }
            h.lines.push(Line { kind, text, old: lo, new: ln });
        } else {
            if raw.starts_with("Binary files") || raw.starts_with("GIT binary patch") {
                f.binary = true;
            }
            f.header.push(raw.into());
        }
    }
    files
}

/// Patch with only the selected change lines of `file`, for `git apply --cached`
/// (`reverse == false`, staging) or `git apply [--cached] --reverse`
/// (`reverse == true`, unstaging / discarding). `None` if nothing is selected.
pub fn partial_patch(file: &FileDiff, selected: impl Fn(usize, usize) -> bool, reverse: bool) -> Option<String> {
    if file.lossy || file.binary {
        return None;
    }
    let changes = || file.hunks.iter().enumerate().flat_map(|(hi, h)| h.lines.iter().enumerate().filter(|l| matches!(l.1.kind, b'+' | b'-')).map(move |(li, _)| (hi, li)));
    let whole = changes().all(|(h, l)| selected(h, l));
    let mut out: String = if whole {
        file.header.iter().map(|h| format!("{h}\n")).collect()
    } else {
        // Part of a rename / new / deleted file: express it as a plain edit of one path.
        let side = |p: &str| file.header.iter().find_map(|h| h.strip_prefix(p)).filter(|s| *s != "/dev/null");
        let path = side("+++ ").map(|s| s.replacen("b/", "", 1)).or_else(|| side("--- ").map(|s| s.replacen("a/", "", 1)))?;
        let (q, p) = match path.strip_prefix('"') { Some(rest) => ("\"", rest.to_owned()), None => ("", path) };
        format!("diff --git {q}a/{p} {q}b/{p}\n--- {q}a/{p}\n+++ {q}b/{p}\n")
    };
    let mut delta: i64 = 0;
    let mut any = false;
    for (hi, h) in file.hunks.iter().enumerate() {
        let (mut body, mut old_n, mut new_n, mut changed, mut kept) = (String::new(), 0i64, 0i64, false, true);
        for (li, l) in h.lines.iter().enumerate() {
            let kind = match l.kind {
                b'\\' if kept => Some(b'\\'),
                b'\\' => None,
                b' ' => Some(b' '),
                k if selected(hi, li) => {
                    changed = true;
                    Some(k)
                }
                b'-' if !reverse => Some(b' '),
                b'+' if reverse => Some(b' '),
                _ => None,
            };
            if l.kind != b'\\' {
                kept = kind.is_some();
            }
            let Some(k) = kind else { continue };
            match k {
                b' ' => (old_n, new_n) = (old_n + 1, new_n + 1),
                b'-' => old_n += 1,
                b'+' => new_n += 1,
                _ => {}
            }
            body.push(k as char);
            body.push_str(&l.text);
            body.push('\n');
        }
        if !changed {
            continue;
        }
        // A "no newline at end of file" marker can't be split safely: whole hunks only.
        let partial_hunk = h.lines.iter().enumerate().any(|(li, l)| matches!(l.kind, b'+' | b'-') && !selected(hi, li));
        if partial_hunk && h.lines.iter().any(|l| l.kind == b'\\') {
            return None;
        }
        any = true;
        let (os, ns) = if reverse {
            (h.new_start as i64 + delta, h.new_start as i64)
        } else {
            // A pure insertion (`-U0`) goes after line `old_start`: `git apply` places it by the new start.
            (h.old_start as i64, h.old_start as i64 + delta + (old_n == 0) as i64)
        };
        delta += if reverse { old_n - new_n } else { new_n - old_n };
        out += &format!("@@ -{os},{old_n} +{ns},{new_n} @@\n{body}");
    }
    any.then_some(out)
}

/// Each file of a `--no-renames` patch with its path from the `--name-only -z`
/// list of the same diff. A typechange (file <-> symlink) is two patches of one
/// listed path, a deletion then a creation: the first gets `(path, true)`, the second `None`.
pub fn file_paths<'a>(files: &[FileDiff], names: &'a str) -> Vec<Option<(&'a str, bool)>> {
    let mut names = names.split('\0');
    let id = |i: usize| files.get(i).and_then(|f| f.header.first());
    (0..files.len())
        .map(|i| (i == 0 || id(i - 1) != id(i)).then(|| (names.next().unwrap_or_default(), id(i + 1) == id(i))))
        .collect()
}

/// One file's diff; submodules come as `--submodule=log` summaries.
fn parse_one(text: &str) -> FileDiff {
    if text.starts_with("Submodule ") {
        return FileDiff { submodule: super::submodule::parse_submodule_diff(text), ..Default::default() };
    }
    let mut f = parse_diff(text).into_iter().next().unwrap_or_default();
    f.lfs = super::lfs::pointer_diff(&f);
    f
}

// Literal pathspecs: `data[1].csv` must not glob-match `data1.csv`, or hunk
// staging/discard would patch the wrong file.
const DIFF: [&str; 6] = ["--literal-pathspecs", "diff", "--no-color", "--no-ext-diff", "--submodule=log", "-M"];

/// How a diff is shown: `-w` and `-U<context>`. Line staging needs `ignore_ws` off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffOpts {
    pub ignore_ws: bool,
    pub context: u32,
}

impl Default for DiffOpts {
    fn default() -> Self {
        DiffOpts { ignore_ws: false, context: 3 }
    }
}

impl DiffOpts {
    /// Context that shows the whole file.
    pub const FILE: u32 = 999_999;

    fn args(self) -> Vec<String> {
        let mut a = vec![format!("-U{}", self.context)];
        if self.ignore_ws {
            a.push("-w".into());
        }
        a
    }
}

pub fn worktree_diff(dir: &Path, c: &Change, staged: bool, o: DiffOpts) -> Res<FileDiff> {
    if c.code == '?' {
        let mut f = untracked_diff(dir, &c.path)?;
        f.lfs = super::lfs::pointer_diff(&f);
        return Ok(f);
    }
    let opts = o.args();
    let mut args = DIFF.to_vec();
    args.extend(opts.iter().map(String::as_str));
    if staged {
        args.push("--cached");
    }
    args.push("--");
    if let Some(o) = &c.old_path {
        args.push(o);
    }
    args.push(&c.path);
    let raw = run_raw(dir, &args, None)?;
    let mut f = parse_one(&String::from_utf8_lossy(&raw));
    f.lossy = std::str::from_utf8(&raw).is_err();
    Ok(f)
}

pub fn commit_diff(dir: &Path, id: &str, parent: Option<&str>, c: &Change, o: DiffOpts) -> Res<FileDiff> {
    let opts = o.args();
    let mut args = match parent {
        Some(p) => [&DIFF[..], &[p, id]].concat(),
        None => vec!["--literal-pathspecs", "show", "--format=", "--no-color", "--no-ext-diff", "--submodule=log", id],
    };
    args.extend(opts.iter().map(String::as_str));
    args.push("--");
    if let Some(o) = &c.old_path {
        args.push(o);
    }
    args.push(&c.path);
    Ok(parse_one(&run(dir, &args)?))
}

fn untracked_diff(dir: &Path, path: &str) -> Res<FileDiff> {
    let mut f = FileDiff { header: vec![format!("new file {path}")], ..Default::default() };
    let meta = std::fs::symlink_metadata(dir.join(path)).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 8 << 20 {
        f.binary = true;
        return Ok(f);
    }
    let bytes = std::fs::read(dir.join(path)).map_err(|e| e.to_string())?;
    f.lossy = std::str::from_utf8(&bytes).is_err();
    if bytes[..bytes.len().min(8000)].contains(&0) {
        f.binary = true;
        return Ok(f);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<Line> = text
        .lines()
        .enumerate()
        .map(|(i, l)| Line { kind: b'+', text: l.into(), old: 0, new: i as u32 + 1 })
        .collect();
    f.hunks.push(Hunk { header: format!("@@ -0,0 +1,{} @@", lines.len()), old_start: 0, new_start: 1, lines });
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_and_partial_patch() {
        let d = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -10,2 +10,3 @@\n x\n+y\n z\n";
        let f = &parse_diff(d)[0];
        assert_eq!(f.header.len(), 4);
        assert_eq!(f.hunks.len(), 2);
        assert_eq!((f.hunks[1].lines[1].kind, f.hunks[1].lines[1].new), (b'+', 11));
        // stage only "+B" (drop "-b" -> becomes context)
        let p = partial_patch(f, |h, l| h == 0 && l == 2, false).unwrap();
        assert!(p.ends_with("@@ -1,3 +1,4 @@\n a\n b\n+B\n c\n"), "{p}");
        // unstage only the second hunk
        let p = partial_patch(f, |h, _| h == 1, true).unwrap();
        assert!(p.ends_with("@@ -10,2 +10,3 @@\n x\n+y\n z\n"), "{p}");
        assert!(partial_patch(f, |_, _| false, false).is_none());
        // -U0 insertions after line 5 and 10: new starts as git writes them.
        let u0 = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -5,0 +6 @@\n+x\n@@ -10,0 +12 @@\n+y\n";
        let p = partial_patch(&parse_diff(u0)[0], |_, _| true, false).unwrap();
        assert!(p.ends_with("@@ -5,0 +6,1 @@\n+x\n@@ -10,0 +12,1 @@\n+y\n"), "{p}");
    }

    #[test]
    fn typechange_paths() {
        let d = "diff --git a/l b/l\ndeleted file mode 100644\n--- a/l\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
                 diff --git a/l b/l\nnew file mode 120000\n--- /dev/null\n+++ b/l\n@@ -0,0 +1 @@\n+m\n\
                 diff --git a/z b/z\n--- a/z\n+++ b/z\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(file_paths(&parse_diff(d), "l\0z\0"), [Some(("l", true)), None, Some(("z", false))]);
    }

    /// Real git: `-w` hides whitespace-only edits, `-U` widens the context.
    #[test]
    fn whitespace_and_context_options() {
        let dir = std::env::temp_dir().join(format!("glasir-diffopts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        g(&["init", "-q"]);
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("f"), &body).unwrap();
        g(&["add", "f"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "f"]);
        std::fs::write(dir.join("f"), body.replace("line 2\n", "line  2\n").replace("line 10\n", "LINE 10\n")).unwrap();
        let c = Change { path: "f".into(), old_path: None, code: 'M' };
        let diff = |o| worktree_diff(&dir, &c, false, o).unwrap();
        let changed = |f: &FileDiff| f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == b'-').count();
        assert_eq!(changed(&diff(DiffOpts::default())), 2);
        assert_eq!(changed(&diff(DiffOpts { ignore_ws: true, ..Default::default() })), 1);
        let whole = diff(DiffOpts { context: DiffOpts::FILE, ..Default::default() });
        assert_eq!((whole.hunks.len(), whole.hunks[0].lines.len()), (1, 22));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real git: a path with glob characters is matched literally, so hunk
    /// staging/discard of `data[1].csv` can't touch `data1.csv`.
    #[test]
    fn glob_characters_in_path_are_literal() {
        let dir = std::env::temp_dir().join(format!("glasir-diffglob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        g(&["init", "-q"]);
        for f in ["data1.csv", "data[1].csv"] {
            std::fs::write(dir.join(f), "a\n").unwrap();
        }
        g(&["add", "-A"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "f"]);
        for f in ["data1.csv", "data[1].csv"] {
            std::fs::write(dir.join(f), "b\n").unwrap();
        }
        let c = Change { path: "data[1].csv".into(), old_path: None, code: 'M' };
        let names = |f: FileDiff| f.header.into_iter().find(|h| h.starts_with("--- ")).unwrap();
        assert_eq!(names(worktree_diff(&dir, &c, false, DiffOpts::default()).unwrap()), "--- a/data[1].csv");
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-qam", "g"]);
        let head = g(&["rev-parse", "HEAD"]);
        assert_eq!(names(commit_diff(&dir, head.trim(), Some("HEAD~"), &c, DiffOpts::default()).unwrap()), "--- a/data[1].csv");
        let root = g(&["rev-parse", "HEAD~"]);
        assert_eq!(names(commit_diff(&dir, root.trim(), None, &c, DiffOpts::default()).unwrap()), "--- /dev/null");
        assert!(commit_diff(&dir, root.trim(), None, &c, DiffOpts::default()).unwrap().header[0].ends_with("b/data[1].csv"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
