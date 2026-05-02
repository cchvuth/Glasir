//! 3-way merge editor state (no drawing: `crate::ui::merge_editor`): conflict-marker parsing
//! (merge, diff3, zdiff3), loading a conflicted file with its side labels, and saving the output.

use super::{Op, Task};
use crate::git::{self, Res};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub ours: String,
    /// Present in diff3 / zdiff3 style.
    pub base: Option<String>,
    pub theirs: String,
    /// The whole block, markers included.
    pub(crate) raw: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    Text(String),
    Conflict(Conflict),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Take {
    Current,
    Incoming,
    CurrentFirst,
    IncomingFirst,
    Base,
}

impl Conflict {
    pub fn take(&self, t: Take) -> String {
        let both = |a: &str, b: &str| {
            let mut s = a.to_owned();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s + b
        };
        match t {
            Take::Current => self.ours.clone(),
            Take::Incoming => self.theirs.clone(),
            Take::CurrentFirst => both(&self.ours, &self.theirs),
            Take::IncomingFirst => both(&self.theirs, &self.ours),
            Take::Base => self.base.clone().unwrap_or_default(),
        }
    }

    /// Both sides are the same, or differ only in whitespace (amount, line ends, blank lines).
    pub fn trivial(&self) -> bool {
        fn words(s: &str) -> Vec<Vec<&str>> {
            s.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|w| !w.is_empty()).collect()
        }
        words(&self.ours) == words(&self.theirs)
    }
}

/// `<<<<<<<`-style marker of `n` chars (7 unless the `conflict-marker-size` attribute
/// says otherwise). Longer runs (inner markers of a recursive merge base) and
/// look-alikes such as `<<<<<<<x` are not markers.
fn is_marker(line: &str, c: u8, n: usize) -> bool {
    let l = line.trim_end_matches(['\n', '\r']).as_bytes();
    l.len() >= n && l[..n].iter().all(|&b| b == c) && (l.len() == n || (c != b'=' && l[n] == b' '))
}

/// Splits a file into plain text and well-formed conflict blocks. Anything that
/// merely looks like a marker (no matching `=======`/`>>>>>>>`) stays text.
pub fn parse(text: &str, n: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (mut out, mut plain, mut i) = (vec![], String::new(), 0);
    while i < lines.len() {
        match block(&lines, i, n) {
            Some((c, next)) => {
                if !plain.is_empty() {
                    out.push(Chunk::Text(std::mem::take(&mut plain)));
                }
                out.push(Chunk::Conflict(c));
                i = next;
            }
            None => {
                plain.push_str(lines[i]);
                i += 1;
            }
        }
    }
    if !plain.is_empty() {
        out.push(Chunk::Text(plain));
    }
    out
}

/// The conflict starting at `lines[start]` and the index of the line after it.
fn block(lines: &[&str], start: usize, n: usize) -> Option<(Conflict, usize)> {
    let is_marker = |l: &str, c| is_marker(l, c, n);
    if !is_marker(lines[start], b'<') {
        return None;
    }
    let (mut ours, mut base, mut theirs) = (String::new(), None::<String>, String::new());
    let mut part = 0; // ours, base, theirs
    for (j, l) in lines.iter().enumerate().skip(start + 1) {
        if is_marker(l, b'<') {
            return None; // a new block starts before this one closed
        }
        match part {
            0 if is_marker(l, b'|') => (base, part) = (Some(String::new()), 1),
            0 | 1 if is_marker(l, b'=') => part = 2,
            2 if is_marker(l, b'>') => return Some((Conflict { ours, base, theirs, raw: lines[start..=j].concat() }, j + 1)),
            0 => ours.push_str(l),
            1 => base.get_or_insert_default().push_str(l),
            _ => theirs.push_str(l),
        }
    }
    None
}

/// Each line of a well-formed block with its part: 0 ours, 1 base, 2 theirs, 3 marker.
pub(crate) fn parts(raw: &str, size: usize) -> impl Iterator<Item = (u8, &str)> {
    let is_marker = move |l: &str, c| is_marker(l, c, size);
    let mut part = 0;
    raw.split_inclusive('\n').enumerate().map(move |(n, l)| {
        let marker = if n == 0 {
            true
        } else if part == 0 && is_marker(l, b'|') {
            part = 1;
            true
        } else if part < 2 && is_marker(l, b'=') {
            part = 2;
            true
        } else {
            part == 2 && is_marker(l, b'>')
        };
        (if marker { 3 } else { part }, l)
    })
}

pub(crate) fn conflicts(chunks: &[Chunk]) -> impl Iterator<Item = &Conflict> {
    chunks.iter().filter_map(|c| match c {
        Chunk::Conflict(c) => Some(c),
        Chunk::Text(_) => None,
    })
}

/// The file with conflict `i` (or every conflict `pick` accepts) replaced by `take`.
pub(crate) fn rebuild(chunks: &[Chunk], pick: impl Fn(usize, &Conflict) -> Option<Take>) -> String {
    let mut n = 0;
    let mut out = String::new();
    for c in chunks {
        match c {
            Chunk::Text(t) => out += t,
            Chunk::Conflict(c) => {
                out += &pick(n, c).map_or_else(|| c.raw.clone(), |t| c.take(t));
                n += 1;
            }
        }
    }
    out
}


pub(crate) enum Row {
    /// Header of original conflict `k`.
    Head(usize),
    Line(String, Option<usize>),
}

pub(crate) struct Loaded {
    /// The file as read: saving refuses to overwrite edits made elsewhere meanwhile.
    pub(crate) original: Vec<u8>,
    /// Why only a whole side can be taken (binary, symlink, deleted, no markers).
    pub(crate) whole: Option<&'static str>,
    /// The index has a current, incoming version (false: that side deleted the file).
    pub(crate) stages: [bool; 2],
    /// Conflict marker length.
    pub(crate) size: usize,
    /// Current, incoming.
    pub(crate) labels: [String; 2],
    pub(crate) orig: Vec<Conflict>,
    pub(crate) sides: [Vec<Row>; 2],
    /// Longest line per side, in chars.
    pub(crate) width: [usize; 2],
}

/// Pane titles from the operation state (the markers' own labels are generic).
fn labels(dir: &Path) -> [String; 2] {
    let g = |args: &[&str]| git::run(dir, args).map(|s| s.trim().to_owned()).unwrap_or_default();
    // Skips the detached-HEAD pseudo entry ("(no branch, rebasing x)").
    let at = |rev: &str| g(&["branch", "-a", "--points-at", rev, "--format=%(refname:short)"]).lines().find(|l| !l.starts_with('(')).unwrap_or("").to_owned();
    let path = |p: &str| dir.join(g(&["rev-parse", "--git-path", p]));
    let rebase = ["rebase-merge", "rebase-apply"].into_iter().map(path).find(|p| p.is_dir());
    let read = |p: PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_owned()).unwrap_or_default();

    let mut cur = g(&["symbolic-ref", "-q", "--short", "HEAD"]);
    if cur.is_empty() {
        cur = rebase.as_ref().map(|r| at(&read(r.join("onto")))).filter(|s| !s.is_empty()).unwrap_or_else(|| "HEAD".into());
    }
    let current = format!("{cur} {}", g(&["rev-parse", "--short", "HEAD"]));

    let head = ["MERGE_HEAD", "REBASE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"].into_iter().find(|h| git::run(dir, &["rev-parse", "-q", "--verify", h]).is_ok());
    let incoming = head.map_or_else(String::new, |h| {
        let name = match rebase.as_ref().filter(|_| h == "REBASE_HEAD") {
            Some(r) => read(r.join("head-name")).trim_start_matches("refs/heads/").to_owned(),
            None => at(h),
        };
        let what = g(&["log", "-1", "--format=%h %s", h, "--"]);
        if name.is_empty() { what } else { format!("{name} {what}") }
    });
    [current, incoming]
}

fn load(dir: &Path, path: &str) -> Loaded {
    let p = dir.join(path);
    let meta = std::fs::symlink_metadata(&p);
    let original = match &meta {
        Ok(m) if m.is_file() => std::fs::read(&p).unwrap_or_default(),
        _ => vec![],
    };
    let text = std::str::from_utf8(&original).ok().filter(|t| !t.contains('\0'));
    // Unmerged index entries: stage 2 = current, 3 = incoming. Not unmerged (or no repo): assume both.
    let unmerged = git::run(dir, &["--literal-pathspecs", "ls-files", "-u", "-z", "--", path]).unwrap_or_default();
    let has = |n: &str| unmerged.is_empty() || unmerged.split('\0').any(|e| e.split_once('\t').is_some_and(|(m, _)| m.ends_with(n)));
    let stages = [has(" 2"), has(" 3")];
    let attr = git::run(dir, &["check-attr", "conflict-marker-size", "--", path]).unwrap_or_default();
    let size = attr.trim().rsplit(": ").next().and_then(|v| v.parse().ok()).filter(|&n| n > 0).unwrap_or(7);
    let mut whole = match (&meta, text) {
        // git leaves the modified version in the tree: it has no markers to edit.
        _ if stages.contains(&false) => Some("The file was deleted on one side."),
        (Err(_), _) => Some("The file is missing from the working tree."),
        (Ok(m), _) if !m.is_file() => Some("This is not a regular file (symlink or submodule)."),
        (_, None) => Some("Binary file."),
        _ => None,
    };
    let (mut orig, mut sides, mut width) = (vec![], [vec![], vec![]], [0, 0]);
    for c in parse(text.unwrap_or(""), size) {
        let k = orig.len();
        let texts = match &c {
            Chunk::Text(t) => [(t, None), (t, None)],
            Chunk::Conflict(c) => [(&c.ours, Some(k)), (&c.theirs, Some(k))],
        };
        for (s, (t, at)) in texts.into_iter().enumerate() {
            if let Some(k) = at {
                sides[s].push(Row::Head(k));
            }
            for l in t.lines() {
                let l = l.replace('\t', "    ");
                width[s] = width[s].max(l.chars().count());
                sides[s].push(Row::Line(l, at));
            }
        }
        if let Chunk::Conflict(c) = c {
            orig.push(c);
        }
    }
    if whole.is_none() && orig.is_empty() && !unmerged.is_empty() {
        // Never offer to stage markers we couldn't read as "resolved".
        whole = Some("No conflict markers found in this file.");
    }
    Loaded { original, whole, stages, size, labels: labels(dir), orig, sides, width }
}

/// Writes the resolved file, refusing symlinks and files changed since they were loaded.
pub(crate) fn save(dir: &Path, path: &str, original: &[u8], output: &str) -> Res<()> {
    let p = dir.join(path);
    if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
        return Err(format!("{path} is not a regular file; resolve it manually."));
    }
    if std::fs::read(&p).map_err(|e| format!("{path}: {e}"))? != original {
        return Err(format!("{path} changed on disk since it was opened here. Close the editor and open it again."));
    }
    std::fs::write(&p, output).map_err(|e| format!("Could not write {path}: {e}"))
}

/// Whole-file choices for the current and incoming side. A side without a
/// version deleted the file: "Delete file"; the other side then keeps the working file.
pub(crate) fn whole_ops(path: &str, d: &Loaded) -> [Op; 2] {
    let gone = d.stages.contains(&false);
    [(0, "--ours", "Take current"), (1, "--theirs", "Take incoming")].map(|(i, side, name)| {
        if !d.stages[i] {
            Op::new("Delete file", &["--literal-pathspecs", "rm", "-q", "--", path]).light()
        } else if gone {
            Op::new("Keep file", &["--literal-pathspecs", "add", "--", path]).light()
        } else {
            Op::new(name, &["--literal-pathspecs", "checkout", side, "--", path]).then(&["--literal-pathspecs", "add", "--", path]).light()
        }
    })
}

pub fn stage(path: &str) -> Op {
    Op::new("Mark resolved", &["--literal-pathspecs", "add", "--", path]).light()
}


pub struct Editor {
    pub path: String,
    pub(crate) dir: PathBuf,
    pub(crate) task: Option<Task<Loaded>>,
    pub(crate) data: Option<Loaded>,
    pub output: String,
    pub(crate) chunks: Vec<Chunk>,
    /// Original conflict index of each conflict still in the output.
    pub(crate) orig_of: Vec<Option<usize>>,
    pub(crate) sel: usize,
    /// Scroll all panes to the selected conflict next frame.
    pub(crate) scroll: bool,
    pub(crate) saving: Option<Task<Res<()>>>,
    pub(crate) error: Option<String>,
}

impl Editor {
    pub fn open(ctx: &egui::Context, dir: &Path, path: &str) -> Self {
        let (d, p) = (dir.to_path_buf(), path.to_owned());
        let task = Some(Task::spawn(ctx, move || load(&d, &p)));
        Editor {
            path: path.into(),
            dir: dir.into(),
            task,
            data: None,
            output: String::new(),
            chunks: vec![],
            orig_of: vec![],
            sel: 0,
            scroll: true,
            saving: None,
            error: None,
        }
    }

    pub(crate) fn reparse(&mut self) {
        self.chunks = parse(&self.output, self.data.as_ref().map_or(7, |d| d.size));
        let orig = self.data.as_ref().map_or(&[][..], |d| &d.orig[..]);
        let mut used = vec![false; orig.len()];
        // Matched by content, so the side panes follow even after hand edits.
        self.orig_of = conflicts(&self.chunks)
            .map(|c| {
                let k = (0..orig.len()).find(|&k| !used[k] && orig[k].ours == c.ours && orig[k].theirs == c.theirs)?;
                used[k] = true;
                Some(k)
            })
            .collect();
        self.sel = self.sel.min(self.orig_of.len().saturating_sub(1));
        self.scroll = true;
    }

    pub(crate) fn resolve(&mut self, i: usize, t: Take) {
        self.output = rebuild(&self.chunks, |n, _| (n == i).then_some(t));
        self.reparse();
    }

    /// Output line where conflict `i` starts.
    pub(crate) fn output_line(&self, i: usize) -> usize {
        let mut bytes = 0;
        let mut seen = 0;
        for c in &self.chunks {
            match c {
                Chunk::Text(t) => bytes += t.len(),
                Chunk::Conflict(_) if seen == i => break,
                Chunk::Conflict(c) => {
                    bytes += c.raw.len();
                    seen += 1;
                }
            }
        }
        self.output[..bytes].matches('\n').count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(chunks: &[Chunk]) -> Vec<&Conflict> {
        conflicts(chunks).collect()
    }

    #[test]
    fn parses_merge_style() {
        let text = "a\n<<<<<<< HEAD\nmine\n=======\nyours\nmore\n>>>>>>> feature\nz\n";
        let ch = parse(text, 7);
        assert_eq!(ch.len(), 3);
        let c = only(&ch)[0];
        assert_eq!((c.ours.as_str(), c.base.as_deref(), c.theirs.as_str()), ("mine\n", None, "yours\nmore\n"));
        assert_eq!(rebuild(&ch, |_, _| None), text);
        assert_eq!(rebuild(&ch, |_, _| Some(Take::Current)), "a\nmine\nz\n");
        assert_eq!(rebuild(&ch, |_, _| Some(Take::IncomingFirst)), "a\nyours\nmore\nmine\nz\n");
    }

    #[test]
    fn parses_diff3_and_zdiff3() {
        let text = "<<<<<<< ours\r\nx = 1\r\n||||||| base\r\nx = 0\r\n=======\r\nx = 2\r\n>>>>>>> theirs\r\n";
        let ch = parse(text, 7);
        let c = only(&ch)[0];
        assert_eq!((c.ours.as_str(), c.base.as_deref(), c.theirs.as_str()), ("x = 1\r\n", Some("x = 0\r\n"), "x = 2\r\n"));
        assert_eq!(c.take(Take::Base), "x = 0\r\n");
        let kinds: Vec<u8> = parts(&c.raw, 7).map(|p| p.0).collect();
        assert_eq!(kinds, [3, 0, 3, 1, 3, 2, 3]);
        // zdiff3 with an empty base and a recursive merge's longer inner markers in the base.
        let z = "<<<<<<< HEAD\nA\n||||||| merged common ancestors\n<<<<<<<<< Temporary\nq\n>>>>>>>>> Temporary\n=======\nB\n>>>>>>> b\n";
        let c = only(&parse(z, 7))[0].clone();
        assert_eq!(c.base.as_deref(), Some("<<<<<<<<< Temporary\nq\n>>>>>>>>> Temporary\n"));
        let e = parse("<<<<<<< HEAD\n||||||| base\n=======\nB\n>>>>>>> b\n", 7);
        assert_eq!(only(&e)[0].base.as_deref(), Some(""));
        assert_eq!(only(&e)[0].ours, "");
    }

    #[test]
    fn look_alikes_stay_text() {
        for text in [
            "Title\n=======\n\nbody\n",                              // setext heading
            "<<<<<<< not closed\nfoo\n=======\nbar\n",                // no >>>>>>>
            "<<<<<<<< eight\nx\n=======\ny\n>>>>>>> z\n",             // wrong marker size
            "<<<<<<<x\nx\n=======\ny\n>>>>>>> z\n",                   // no space after marker
            "a <<<<<<< b\n=======\n>>>>>>> c\n",                       // not at line start
        ] {
            assert!(only(&parse(text, 7)).is_empty(), "{text:?}");
            assert_eq!(rebuild(&parse(text, 7), |_, _| None), text);
        }
        // An unclosed start doesn't swallow the real block after it.
        let text = "<<<<<<< stray\na\n=======\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> x\n";
        let ch = parse(text, 7);
        assert_eq!(only(&ch).len(), 1);
        assert_eq!(only(&ch)[0].ours, "b\n");
        assert_eq!(rebuild(&ch, |_, _| Some(Take::Incoming)), "<<<<<<< stray\na\n=======\nc\n");
        // `=======` inside the incoming side is content.
        let ch = parse("<<<<<<< a\nx\n=======\ny\n=======\n>>>>>>> b\n", 7);
        assert_eq!(only(&ch)[0].theirs, "y\n=======\n");
    }

    #[test]
    fn trivial_and_both() {
        let c = |o: &str, t: &str| Conflict { ours: o.into(), base: None, theirs: t.into(), raw: String::new() };
        assert!(c("a\n", "a\n").trivial());
        assert!(c("  f(x,  y);\n", "f(x, y);  \r\n\n").trivial());
        assert!(!c("int x;\n", "intx;\n").trivial());
        assert!(!c("a\n", "b\n").trivial());
        assert_eq!(c("a", "b\n").take(Take::CurrentFirst), "a\nb\n");
        assert_eq!(c("", "b\n").take(Take::CurrentFirst), "b\n");
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        git::run(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    #[test]
    fn resolves_a_real_merge_conflict() {
        let dir = std::env::temp_dir().join(format!("glasir-merge-editor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        for kv in [["user.email", "t@t"], ["user.name", "T"], ["commit.gpgsign", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        let commit = |text: &str, msg: &str| {
            std::fs::write(dir.join("f.txt"), text).unwrap();
            sh(&dir, &["add", "f.txt"]);
            sh(&dir, &["commit", "-q", "-m", msg]);
        };
        let file = |one: &str, two: &str| format!("top\n{one}\n1\n2\n3\n4\n5\n6\n{two}\nend\n");
        commit(&file("one", "two"), "base");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&file("one feature", "two  x"), "feature work");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&file("one main", "two x"), "main work");
        assert!(git::run(&dir, &["merge", "feature"]).is_err());
        sh(&dir, &["checkout", "--conflict=zdiff3", "--", "f.txt"]);

        let d = load(&dir, "f.txt");
        assert!(d.whole.is_none());
        assert!(d.labels[0].starts_with("main "), "{:?}", d.labels);
        assert!(d.labels[1].starts_with("feature ") && d.labels[1].ends_with("feature work"), "{:?}", d.labels);
        assert_eq!(d.orig.len(), 2);
        assert!(d.orig[0].base.is_some());
        assert!(matches!(d.sides[1][1], Row::Head(0)));

        let ch = parse(std::str::from_utf8(&d.original).unwrap(), 7);
        assert_eq!(conflicts(&ch).filter(|c| c.trivial()).count(), 1);
        let out = rebuild(&ch, |n, c| Some(if c.trivial() { Take::Current } else if n == 0 { Take::Incoming } else { Take::Current }));
        assert_eq!(out, file("one feature", "two x"));

        // The editor: resolving and hand edits keep the side panes mapped by content.
        let mut ed = Editor {
            path: "f.txt".into(),
            dir: dir.clone(),
            task: None,
            output: String::from_utf8(d.original.clone()).unwrap(),
            data: Some(load(&dir, "f.txt")),
            chunks: vec![],
            orig_of: vec![],
            sel: 1,
            scroll: false,
            saving: None,
            error: None,
        };
        ed.reparse();
        assert_eq!(ed.orig_of, [Some(0), Some(1)]);
        ed.resolve(0, Take::Incoming);
        assert_eq!((ed.orig_of.as_slice(), ed.sel), (&[Some(1)][..], 0));
        assert!(ed.output_line(0) > 1);
        ed.output = ed.output.replacen("two x", "two y", 1); // hand edit inside the block
        ed.reparse();
        assert_eq!(ed.orig_of, [None]);

        // Edits made elsewhere since loading are never overwritten; symlinks are refused.
        std::fs::write(dir.join("f.txt"), "changed\n").unwrap();
        assert!(save(&dir, "f.txt", &d.original, &out).unwrap_err().contains("changed on disk"));
        std::fs::write(dir.join("f.txt"), &d.original).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("f.txt", dir.join("link")).unwrap();
            assert!(save(&dir, "link", &d.original, &out).is_err());
            std::fs::remove_file(dir.join("link")).unwrap();
        }
        save(&dir, "f.txt", &d.original, &out).unwrap();
        sh(&dir, &stage("f.txt").cmds[0].iter().map(String::as_str).collect::<Vec<_>>());
        assert!(git::status(&dir).unwrap().conflicted.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), out);

        // Rebase: current is the branch being rebased onto, incoming the replayed commit.
        sh(&dir, &["merge", "--abort"]);
        sh(&dir, &["checkout", "-q", "feature"]);
        assert!(git::run(&dir, &["rebase", "main"]).is_err());
        let [cur, inc] = labels(&dir);
        assert!(cur.starts_with("main "), "{cur}");
        assert!(inc.starts_with("feature ") && inc.ends_with("feature work"), "{inc}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_conflict_is_whole_file_only() {
        let dir = std::env::temp_dir().join(format!("glasir-merge-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.bin"), b"\0\x01\x02").unwrap();
        assert_eq!(load(&dir, "b.bin").whole, Some("Binary file."));
        assert!(load(&dir, "gone.txt").whole.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_modify_and_marker_size() {
        let dir = std::env::temp_dir().join(format!("glasir-merge-du-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        for kv in [["user.email", "t@t"], ["user.name", "T"], ["commit.gpgsign", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        std::fs::write(dir.join(".gitattributes"), "*.md conflict-marker-size=32\n").unwrap();
        std::fs::write(dir.join("f.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.join("a.md"), "x\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("f.txt"), "a\nB\n").unwrap();
        std::fs::write(dir.join("a.md"), "feature\n").unwrap();
        sh(&dir, &["commit", "-q", "-am", "modify"]);
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["rm", "-q", "f.txt"]);
        std::fs::write(dir.join("a.md"), "main\n").unwrap();
        sh(&dir, &["commit", "-q", "-am", "delete"]);
        assert!(git::run(&dir, &["merge", "feature"]).is_err());

        // Long markers are read, never staged as "no conflicts left".
        let md = load(&dir, "a.md");
        assert_eq!((md.size, md.orig.len(), md.whole), (32, 1, None));
        assert_eq!((md.orig[0].ours.as_str(), md.orig[0].theirs.as_str()), ("main\n", "feature\n"));
        std::fs::write(dir.join("a.md"), "<<<<<<< x\n").unwrap();
        assert_eq!(load(&dir, "a.md").whole, Some("No conflict markers found in this file."));

        // Deleted on our side, modified version left in the tree: keep or delete, never `checkout --ours`.
        let d = load(&dir, "f.txt");
        assert_eq!((d.stages, d.whole.is_some()), ([false, true], true));
        let ops = whole_ops("f.txt", &d);
        assert_eq!((ops[0].label.as_str(), ops[1].label.as_str()), ("Delete file", "Keep file"));
        let run = |op: &Op| op.cmds.iter().for_each(|c| _ = sh(&dir, &c.iter().map(String::as_str).collect::<Vec<_>>()));
        run(&ops[0]);
        assert!(!dir.join("f.txt").exists());
        assert!(!git::status(&dir).unwrap().conflicted.iter().any(|c| c.path == "f.txt"));
        sh(&dir, &["merge", "--abort"]);
        assert!(git::run(&dir, &["merge", "feature"]).is_err());
        run(&whole_ops("f.txt", &load(&dir, "f.txt"))[1]);
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "a\nB\n");
        assert!(!git::status(&dir).unwrap().conflicted.iter().any(|c| c.path == "f.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
