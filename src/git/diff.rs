//! Diffs: the parsed model ([`FileDiff`] / [`Hunk`] / [`Line`]), patches for
//! line-level staging, and per-file worktree / commit / untracked diffs.

use super::{Change, Res, run, run_raw};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    /// `diff --git`, `index`, `---`, `+++` ... lines, verbatim (used to rebuild patches).
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Not valid UTF-8: shown lossily, so it must not be patched line by line.
    pub lossy: bool,
    /// An untracked file `git add` would convert (CRLF, a clean filter): its lines aren't the
    /// blob's, so only all of it can be staged.
    pub whole_only: bool,
    /// A submodule pointer change (shown instead of hunks).
    pub submodule: Option<super::SubmoduleDiff>,
    /// Git LFS pointer file: (old, new) sides.
    pub lfs: Option<(Option<super::LfsPointer>, Option<super::LfsPointer>)>,
    /// Byte sizes (old, new) of a binary file's sides; `None` = the side doesn't exist.
    pub sizes: Option<(Option<u64>, Option<u64>)>,
    /// Withheld as too big to show: (lines, bytes). Load again with [`DiffOpts::all`].
    pub large: Option<(usize, usize)>,
}

impl FileDiff {
    /// The `(old, new)` mode of a permission change (`old mode` / `new mode` header lines).
    pub fn mode_change(&self) -> Option<(&str, &str)> {
        let m = |p: &str| self.header.iter().find_map(|h| h.strip_prefix(p));
        Some((m("old mode ")?, m("new mode ")?))
    }
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
/// Whether `selected` picks every changed line of `file`.
pub fn selects_all(file: &FileDiff, selected: impl Fn(usize, usize) -> bool) -> bool {
    file.hunks.iter().enumerate().all(|(hi, h)| h.lines.iter().enumerate().all(|(li, l)| !matches!(l.kind, b'+' | b'-') || selected(hi, li)))
}

/// A patch header editing just the file's new path (its old one if deleted).
pub fn plain_header(file: &FileDiff) -> Option<String> {
    let side = |p: &str| file.header.iter().find_map(|h| h.strip_prefix(p)).filter(|s| *s != "/dev/null");
    let path = side("+++ ").map(|s| s.replacen("b/", "", 1)).or_else(|| side("--- ").map(|s| s.replacen("a/", "", 1)))?;
    let (q, p) = match path.strip_prefix('"') { Some(rest) => ("\"", rest.to_owned()), None => ("", path) };
    Some(format!("diff --git {q}a/{p} {q}b/{p}\n--- {q}a/{p}\n+++ {q}b/{p}\n"))
}

pub fn partial_patch(file: &FileDiff, selected: impl Fn(usize, usize) -> bool, reverse: bool) -> Option<String> {
    if file.lossy || file.binary {
        return None;
    }
    let whole = selects_all(file, &selected);
    if file.whole_only && !whole {
        return None;
    }
    let mut out: String = match whole {
        true => file.header.iter().map(|h| format!("{h}\n")).collect(),
        // Part of a rename / new / deleted file: express it as a plain edit of one path.
        false => plain_header(file)?,
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
        // A "no newline at end of file" marker can't be split safely: whole hunks only. In a
        // pure insertion it follows its line, kept or dropped with it.
        let partial_hunk = h.lines.iter().enumerate().any(|(li, l)| matches!(l.kind, b'+' | b'-') && !selected(hi, li));
        if partial_hunk && h.lines.iter().any(|l| l.kind == b'\\') && h.lines.iter().any(|l| l.kind == b'-') {
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
    /// Load a diff over [`LARGE_LINES`] / [`LARGE_BYTES`] too.
    pub all: bool,
    /// Skip the per-file `check-attr` for untracked files (`whole_only` stays false).
    pub no_attrs: bool,
}

impl Default for DiffOpts {
    fn default() -> Self {
        DiffOpts { ignore_ws: false, context: 3, all: false, no_attrs: false }
    }
}

pub const LARGE_LINES: usize = 5_000;
pub const LARGE_BYTES: usize = 1 << 20;

/// The stand-in for a diff of `lines` / `bytes` that is too big to show unasked.
fn large(lines: usize, bytes: usize, o: DiffOpts) -> Option<FileDiff> {
    (!o.all && (lines > LARGE_LINES || bytes > LARGE_BYTES)).then(|| FileDiff { large: Some((lines, bytes)), ..Default::default() })
}

/// Size in bytes of a `git cat-file` object (`rev:path`, `:path` = index).
fn blob_size(dir: &Path, spec: &str) -> Option<u64> {
    run(dir, &["cat-file", "-s", spec]).ok()?.trim().parse().ok()
}

fn file_size(dir: &Path, path: &str) -> Option<u64> {
    std::fs::metadata(dir.join(path)).ok().map(|m| m.len())
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
        let mut f = untracked_diff(dir, &c.path, o)?;
        f.lfs = super::lfs::pointer_diff(&f);
        return Ok(f);
    }
    let opts = o.args();
    let mut args = DIFF.to_vec();
    args.extend(opts.iter().map(String::as_str));
    // Hunks get staged / discarded: they must be the real content, not a textconv rendering.
    args.push("--no-textconv");
    if staged {
        args.push("--cached");
    }
    args.push("--");
    if let Some(o) = &c.old_path {
        args.push(o);
    }
    args.push(&c.path);
    let raw = run_raw(dir, &args, None)?;
    if let Some(f) = large(raw.iter().filter(|&&b| b == b'\n').count(), raw.len(), o) {
        return Ok(f);
    }
    let mut f = parse_one(&String::from_utf8_lossy(&raw));
    f.lossy = std::str::from_utf8(&raw).is_err();
    if f.binary {
        let old = c.old_path.as_deref().unwrap_or(&c.path);
        f.sizes = Some(match staged {
            true => (blob_size(dir, &format!("HEAD:{old}")), blob_size(dir, &format!(":{}", c.path))),
            false => (blob_size(dir, &format!(":{old}")), file_size(dir, &c.path)),
        });
    }
    Ok(f)
}

pub fn commit_diff(dir: &Path, id: &str, parent: Option<&str>, c: &Change, o: DiffOpts) -> Res<FileDiff> {
    let opts = o.args();
    let mut args = match parent {
        // Compared with the working tree: no second revision.
        Some(p) => [&DIFF[..], &[p, id][..1 + (id != super::WORKTREE) as usize]].concat(),
        None => vec!["--literal-pathspecs", "show", "--format=", "--no-color", "--no-ext-diff", "--submodule=log", id],
    };
    args.extend(opts.iter().map(String::as_str));
    args.push("--no-textconv");
    args.push("--");
    if let Some(o) = &c.old_path {
        args.push(o);
    }
    args.push(&c.path);
    let raw = run_raw(dir, &args, None)?;
    if let Some(f) = large(raw.iter().filter(|&&b| b == b'\n').count(), raw.len(), o) {
        return Ok(f);
    }
    let mut f = parse_one(&String::from_utf8_lossy(&raw));
    f.lossy = std::str::from_utf8(&raw).is_err();
    if f.binary {
        let old = c.old_path.as_deref().unwrap_or(&c.path);
        let new = if id == super::WORKTREE { file_size(dir, &c.path) } else { blob_size(dir, &format!("{id}:{}", c.path)) };
        f.sizes = Some((parent.and_then(|p| blob_size(dir, &format!("{p}:{old}"))), new));
    }
    Ok(f)
}

/// `b/path` as a patch header names it: C-quoted when it holds a quote, backslash or control
/// character (a TAB would otherwise end the name for `git apply`).
fn b_path(path: &str) -> String {
    if !path.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        return format!("b/{path}");
    }
    let esc: String = path.chars().flat_map(|c| match c {
        '"' | '\\' => vec!['\\', c],
        '\t' => vec!['\\', 't'],
        '\n' => vec!['\\', 'n'],
        c if c.is_control() => format!("\\{:03o}", c as u32).chars().collect(),
        c => vec![c],
    }).collect();
    format!("\"b/{esc}\"")
}

fn untracked_diff(dir: &Path, path: &str, o: DiffOpts) -> Res<FileDiff> {
    // The ---/+++ pair lets `partial_patch` write a patch for part of it (after `add -N`).
    let mut f = FileDiff { header: vec![format!("new file {path}"), "--- /dev/null".into(), format!("+++ {}", b_path(path))], ..Default::default() };
    let meta = std::fs::symlink_metadata(dir.join(path)).map_err(|e| e.to_string())?;
    // Past 8 MB only the start is read (text: the "too big" stand-in, unless asked for), so a huge file isn't loaded.
    let big = meta.len() > 8 << 20 && !o.all;
    let bytes = if !meta.is_file() {
        vec![0]
    } else if big {
        let mut b = vec![];
        std::fs::File::open(dir.join(path)).map_err(|e| e.to_string())?.take(8000).read_to_end(&mut b).map_err(|e| e.to_string())?;
        b
    } else {
        std::fs::read(dir.join(path)).map_err(|e| e.to_string())?
    };
    if !meta.is_file() || bytes[..bytes.len().min(8000)].contains(&0) {
        f.binary = true;
        f.sizes = Some((None, Some(meta.len())));
        return Ok(f);
    }
    if big {
        let lines = bytes.iter().filter(|&&b| b == b'\n').count() * (meta.len() as usize / bytes.len().max(1));
        return Ok(large(lines, meta.len() as usize, o).unwrap_or_default());
    }
    f.lossy = std::str::from_utf8(&bytes).is_err();
    if let Some(l) = large(bytes.iter().filter(|&&b| b == b'\n').count(), bytes.len(), o) {
        return Ok(l);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<Line> = text
        .lines()
        .enumerate()
        .map(|(i, l)| Line { kind: b'+', text: l.into(), old: 0, new: i as u32 + 1 })
        .collect();
    let header = format!("@@ -0,0 +1,{} @@", lines.len());
    if !bytes.ends_with(b"\n") && !lines.is_empty() {
        lines.push(Line { kind: b'\\', text: " No newline at end of file".into(), old: 0, new: 0 });
    }
    let attrs = || if o.no_attrs { String::new() } else { run(dir, &["check-attr", "-z", "filter", "working-tree-encoding", "ident", "--", path]).unwrap_or_default() };
    f.whole_only = bytes.contains(&b'\r') || attrs().split('\0').skip(2).step_by(3).any(|v| !matches!(v, "" | "unspecified" | "unset"));
    f.hunks.push(Hunk { header, old_start: 0, new_start: 1, lines });
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Untracked files: a TAB in the name is quoted in the patch header, a big text file is the
    /// "too big" stand-in (not binary), a binary is not flagged as lossy.
    #[test]
    fn untracked_diff_edge_cases() {
        assert_eq!(b_path("a b.txt"), "b/a b.txt");
        assert_eq!(b_path("tab\there"), "\"b/tab\\there\"");
        let dir = std::env::temp_dir().join(format!("glasir-untracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big.txt"), "line\n".repeat(2_000_000)).unwrap();
        std::fs::write(dir.join("bin"), [0xff, 0, 1]).unwrap();
        let big = untracked_diff(&dir, "big.txt", DiffOpts::default()).unwrap();
        assert!(!big.binary && big.large.is_some());
        let all = untracked_diff(&dir, "big.txt", DiffOpts { all: true, ..Default::default() }).unwrap();
        assert!(!all.binary && all.hunks.len() == 1);
        let bin = untracked_diff(&dir, "bin", DiffOpts::default()).unwrap();
        assert!(bin.binary && !bin.lossy);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        // A missing final newline: part of a hunk with removals is refused, a pure insertion is fine.
        let nl = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1,2 @@\n-a\n\\ No newline at end of file\n+a\n+b\n\\ No newline at end of file\n";
        assert!(partial_patch(&parse_diff(nl)[0], |_, l| l == 2, false).is_none());
        let ins = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1,3 @@\n a\n+b\n+c\n\\ No newline at end of file\n";
        let p = partial_patch(&parse_diff(ins)[0], |_, l| l == 2, false).unwrap();
        assert!(p.ends_with("@@ -1,1 +1,2 @@\n a\n+c\n\\ No newline at end of file\n"), "{p}");
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

    /// Real git: staging a whole file's hunks from its diff stages exactly that file's
    /// real content, whatever its name (glob characters), textconv or diff prefixes.
    #[test]
    fn staged_hunks_hit_the_right_file() {
        let dir = std::env::temp_dir().join(format!("glasir-stage-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        let w = |p: &str, t: &str| std::fs::write(dir.join(p), t).unwrap();
        g(&["init", "-q"]);
        for (k, v) in [("diff.redact.textconv", "sed 's/=.*/=***/'"), ("diff.srcPrefix", ""), ("diff.dstPrefix", "")] {
            g(&["config", k, v]);
        }
        w(".gitattributes", "*.env diff=redact\n");
        let lines = "a\nb\nc\nd\n";
        for (p, t) in [("f1.txt", "one\n"), ("f[0-9].txt", "lit\n"), ("a.env", "# c1\n# c2\n"), ("src/main.rs", lines), ("main.rs", lines)] {
            w(p, t);
        }
        g(&["add", "-A"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"]);
        // Unsaved work in a file the glob `f[0-9].txt` matches: must stay unstaged.
        w("f1.txt", "ONE precious\n");
        for (p, t) in [("f[0-9].txt", "LIT\n"), ("a.env", "# c1\n# c2\nPASSWORD=hunter2\n"), ("src/main.rs", "a\nB\nc\nd\n")] {
            w(p, t);
            let c = Change { path: p.into(), old_path: None, code: 'M' };
            let patch = partial_patch(&worktree_diff(&dir, &c, false, DiffOpts::default()).unwrap(), |_, _| true, false).unwrap();
            run_raw(&dir, &["apply", "--recount", "--whitespace=nowarn", "--cached", "-"], Some(patch.as_bytes())).unwrap();
        }
        let index = |p: &str| g(&["show", &format!(":{p}")]);
        assert_eq!(index("f1.txt"), "one\n");
        assert_eq!(index("f[0-9].txt"), "LIT\n");
        assert_eq!(index("a.env"), "# c1\n# c2\nPASSWORD=hunter2\n");
        assert_eq!(index("src/main.rs"), "a\nB\nc\nd\n");
        assert_eq!(index("main.rs"), lines);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_hunks_are_the_real_content() {
        let dir = std::env::temp_dir().join(format!("glasir-commit-textconv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        g(&["init", "-q"]);
        g(&["config", "diff.redact.textconv", "sed 's/=.*/=***/'"]);
        std::fs::write(dir.join(".gitattributes"), "*.env diff=redact\n").unwrap();
        std::fs::write(dir.join("a.env"), "x\n").unwrap();
        let commit = |m: &str| g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", m]);
        g(&["add", "-A"]);
        commit("one");
        std::fs::write(dir.join("a.env"), "x=1\n").unwrap();
        g(&["add", "-A"]);
        commit("two");
        let c = Change { path: "a.env".into(), old_path: None, code: 'M' };
        for parent in [Some("HEAD~"), None] {
            let f = commit_diff(&dir, "HEAD", parent, &c, DiffOpts::default()).unwrap();
            assert_eq!(partial_patch(&f, |_, _| true, false).unwrap().lines().last(), Some("+x=1"), "{parent:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_utf8_commit_diff_is_lossy() {
        let dir = std::env::temp_dir().join(format!("glasir-commit-lossy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        g(&["init", "-q"]);
        for text in [&b"cafe\n"[..], b"caf\xe9\n"] {
            std::fs::write(dir.join("f"), text).unwrap();
            g(&["add", "f"]);
            g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "f"]);
        }
        let c = Change { path: "f".into(), old_path: None, code: 'M' };
        let f = commit_diff(&dir, "HEAD", Some("HEAD~"), &c, DiffOpts::default()).unwrap();
        assert!(f.lossy && partial_patch(&f, |_, _| true, true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_commit_diff_despite_log_show_root_off() {
        let dir = std::env::temp_dir().join(format!("glasir-show-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run(&dir, a).unwrap();
        g(&["init", "-q"]);
        g(&["config", "log.showRoot", "false"]);
        std::fs::write(dir.join("f"), "a\n").unwrap();
        g(&["add", "f"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "f"]);
        let c = Change { path: "f".into(), old_path: None, code: 'A' };
        assert_eq!(commit_diff(&dir, "HEAD", None, &c, DiffOpts::default()).unwrap().hunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
