//! File, folder and line-range history (follows renames) and blame at any revision: the git
//! queries, their parsers, and the `FileView` back/forward state. Drawn by `crate::ui::history`.

use super::Task;
use crate::git::{self, Change, Hunk, Res};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug)]
pub enum Mode {
    /// Commits touching the file (following renames), or the folder when the path ends in `/`.
    History,
    /// Blame at a revision; `None` = the working tree.
    Blame(Option<String>),
    /// `git log -L<start>,<end>:<path> <rev>`; `None` = the index (an unstaged diff's old side).
    Lines { rev: Option<String>, start: u32, end: u32 },
}

pub struct Entry {
    pub id: String,
    pub parent: Option<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub date: String,
    pub summary: String,
    pub change: Change,
    /// Line-range history: how this commit changed the lines.
    pub hunks: Vec<Hunk>,
}

pub struct BlameCommit {
    pub id: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
    /// The file's path in this commit.
    pub file: String,
    /// (parent, path there) to re-blame at; `None` for a root/boundary commit.
    pub previous: Option<(String, String)>,
}

#[derive(Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// (commit index, line number in that commit's file, line text)
    pub lines: Vec<(usize, u32, String)>,
    /// Heat 0 (oldest) ..= 1 (newest) per commit.
    pub heat: Vec<f32>,
}

pub enum Data {
    History(Vec<Entry>),
    Blame(Blame),
}

pub struct FileView {
    pub path: String,
    pub mode: Mode,
    pub data: Option<Res<Data>>,
    task: Option<Task<Res<Data>>>,
    cancel: git::Cancel,
    pub sel: Option<usize>,
    /// Most commits a file or folder history lists (more on request: a big folder takes seconds).
    pub limit: usize,
    dir: PathBuf,
    /// Earlier / later (path, mode) views for Back and Forward.
    pub back: Vec<(String, Mode)>,
    pub fwd: Vec<(String, Mode)>,
}

impl FileView {
    pub fn open(ctx: &egui::Context, dir: &Path, path: &str, mode: Mode) -> Self {
        let mut fv = FileView { path: path.into(), mode, data: None, task: None, cancel: Default::default(), sel: None, limit: PAGE, dir: dir.into(), back: vec![], fwd: vec![] };
        fv.load(ctx, false);
        fv
    }

    /// `keep`: leave the old data shown until the new arrives (a reload of the same view).
    fn load(&mut self, ctx: &egui::Context, keep: bool) {
        if !keep {
            (self.data, self.sel, self.limit) = (None, None, PAGE);
        }
        let (dir, p, mode, limit) = (self.dir.clone(), self.path.clone(), self.mode.clone(), self.limit);
        self.cancel.cancel();
        self.cancel = Default::default();
        let cancel = self.cancel.clone();
        self.task = Some(Task::spawn(ctx, move || {
            cancel.scope(|| match mode {
                Mode::History => history(&dir, &p, limit).map(Data::History),
                Mode::Blame(rev) => blame(&dir, &p, rev.as_deref()).map(Data::Blame),
                Mode::Lines { rev, start, end } => line_history(&dir, &p, rev.as_deref(), start, end).map(Data::History),
            })
        }));
    }

    /// Shows another view, keeping this one for Back.
    pub fn go(&mut self, ctx: &egui::Context, path: String, mode: Mode) {
        if (&path, &mode) == (&self.path, &self.mode) {
            return;
        }
        self.back.push((std::mem::replace(&mut self.path, path), std::mem::replace(&mut self.mode, mode)));
        self.fwd.clear();
        self.load(ctx, false);
    }

    /// One view back, or forward.
    pub fn step(&mut self, ctx: &egui::Context, forward: bool) {
        let (from, to) = if forward { (&mut self.fwd, &mut self.back) } else { (&mut self.back, &mut self.fwd) };
        let Some((path, mode)) = from.pop() else { return };
        to.push((std::mem::replace(&mut self.path, path), std::mem::replace(&mut self.mode, mode)));
        self.load(ctx, false);
    }

    /// After a repo reload: the working-tree blame always, the others when HEAD moved.
    pub fn refresh(&mut self, ctx: &egui::Context, head_moved: bool) {
        if matches!(self.mode, Mode::Blame(None)) || (head_moved && !matches!(self.mode, Mode::Blame(_))) {
            self.load(ctx, true);
        }
    }

    /// Lists more commits of a file or folder history.
    pub fn more(&mut self, ctx: &egui::Context) {
        self.limit *= 4;
        self.load(ctx, true);
    }

    pub fn poll(&mut self) {
        if let Some(d) = Task::poll(&mut self.task) {
            (self.data, self.sel) = (Some(d), None);
        }
    }
}

pub fn parse_history(out: &str) -> Vec<Entry> {
    // Records: header \x1e... then --name-status lines (NUL-separated with -z).
    let mut v = vec![];
    for rec in out.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut parts = rec.split('\0').map(|s| s.trim_start_matches('\n'));
        let Some(head) = parts.next() else { continue };
        let f: Vec<&str> = head.splitn(7, '\x1f').collect();
        if f.len() < 7 {
            continue;
        }
        let rest: Vec<&str> = parts.filter(|s| !s.is_empty()).collect();
        let change = match rest.as_slice() {
            [st, old, new, ..] if st.starts_with('R') || st.starts_with('C') => {
                Change { path: (*new).into(), old_path: Some((*old).into()), code: st.chars().next().unwrap_or('R') }
            }
            [st, path, ..] => Change { path: (*path).into(), old_path: None, code: st.chars().next().unwrap_or('M') },
            _ => continue,
        };
        v.push(Entry {
            id: f[0].into(),
            parent: f[1].split_whitespace().next().map(Into::into),
            author: f[2].into(),
            email: f[3].into(),
            time: f[4].parse().unwrap_or(0),
            date: f[5].into(),
            summary: f[6].into(),
            change,
            hunks: vec![],
        });
    }
    v
}

const FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%s";

const PAGE: usize = 1000;

fn history(dir: &Path, path: &str, limit: usize) -> Res<Vec<Entry>> {
    // A folder (`dir/`) can't be followed through renames.
    let follow: &[&str] = if path.ends_with('/') { &[] } else { &["--follow", "-M"] };
    let n = format!("-n{limit}");
    let args = [&["--literal-pathspecs", "log", &n], follow, &["-z", "--name-status", git::DATE_FORMAT, FORMAT, "--", path]].concat();
    let mut v = parse_history(&git::run(dir, &args)?);
    // `--follow` finds copies too, but a copy's source still lives on: its commits are not this file's past.
    if let Some(i) = v.iter().position(|e| e.change.code == 'C') {
        v.truncate(i + 1);
    }
    Ok(v)
}

/// Undoes git's C-style path quoting (`"\303\274 \"x\""`); plain paths pass through.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return s.into() };
    let (mut out, mut b) = (vec![], inner.bytes());
    while let Some(c) = b.next() {
        if c != b'\\' {
            out.push(c);
            continue;
        }
        match b.next() {
            Some(d @ b'0'..=b'7') => {
                let v = b.by_ref().take(2).fold((d - b'0') as u32, |v, x| v * 8 + x.wrapping_sub(b'0') as u32);
                out.push(v as u8);
            }
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(x) => out.push(x),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `git log -L` output: a header record, then the commit's diff of the range (path as of that commit).
pub fn parse_lines(out: &str, path: &str) -> Vec<Entry> {
    let mut v = vec![];
    for rec in out.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let (head, patch) = rec.split_once('\n').unwrap_or((rec, ""));
        let f: Vec<&str> = head.splitn(7, '\x1f').collect();
        if f.len() < 7 {
            continue;
        }
        let diff = git::parse_diff(patch).pop().unwrap_or_default();
        // git >= 2.52 prints `log -L` like `diff`: quoted names, a tab after one with a space.
        let side = |p: &str, pre: &str| {
            let s = unquote(diff.header.iter().find_map(|h| h.strip_prefix(p)).map(|s| s.strip_suffix('\t').unwrap_or(s))?);
            s.strip_prefix(pre).map(str::to_owned)
        };
        let (old, new) = (side("--- ", "a/"), side("+++ ", "b/"));
        let change = match (old, new) {
            (None, n) => Change { path: n.unwrap_or_else(|| path.into()), old_path: None, code: 'A' },
            (Some(o), Some(n)) if o != n => Change { path: n, old_path: Some(o), code: 'R' },
            (Some(o), _) => Change { path: o, old_path: None, code: 'M' },
        };
        v.push(Entry {
            id: f[0].into(),
            parent: f[1].split_whitespace().next().map(Into::into),
            author: f[2].into(),
            email: f[3].into(),
            time: f[4].parse().unwrap_or(0),
            date: f[5].into(),
            summary: f[6].into(),
            change,
            hunks: diff.hunks,
        });
    }
    v
}

const INDEX_LINES: &str = "Some of this file is staged, so the unstaged diff's line numbers don't match HEAD: pick the lines in the staged diff, or commit first.";

fn line_history(dir: &Path, path: &str, rev: Option<&str>, start: u32, end: u32) -> Res<Vec<Entry>> {
    // `log -L` reads a commit; the index stands in for HEAD only while nothing of the file is staged.
    let rev = match rev {
        Some(r) => r,
        None if git::run(dir, &["--literal-pathspecs", "diff", "--cached", "--quiet", "--", path]).is_ok() => "HEAD",
        None => return Err(INDEX_LINES.into()),
    };
    let range = format!("-L{start},{end}:{path}");
    let args = ["log", "--no-color", "--src-prefix=a/", "--dst-prefix=b/", git::DATE_FORMAT, FORMAT, &range, rev, "--"];
    Ok(parse_lines(&git::run(dir, &args)?, path))
}

pub fn parse_blame(out: &str) -> Blame {
    let mut b = Blame::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let (mut cur, mut orig) = (0, 0);
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            b.lines.push((cur, orig, text.to_owned()));
            continue;
        }
        let mut it = line.splitn(2, ' ');
        let (key, val) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if key.len() >= 40 && key.bytes().all(|c| c.is_ascii_hexdigit()) {
            cur = *index.entry(key.to_owned()).or_insert_with(|| {
                b.commits.push(BlameCommit { id: key.to_owned(), author: String::new(), time: 0, summary: String::new(), file: String::new(), previous: None });
                b.commits.len() - 1
            });
            orig = val.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            continue;
        }
        let Some(c) = b.commits.get_mut(cur) else { continue };
        match key {
            "author" => c.author = val.into(),
            "author-time" => c.time = val.parse().unwrap_or(0),
            "summary" => c.summary = val.into(),
            // ponytail: one path per commit; a commit blamed under two paths (copies) keeps the last.
            "filename" => c.file = unquote(val),
            "previous" => c.previous = val.split_once(' ').map(|(id, p)| (id.into(), unquote(p))),
            _ => {}
        }
    }
    let mut order: Vec<usize> = (0..b.commits.len()).collect();
    order.sort_by_key(|&i| b.commits[i].time);
    b.heat = vec![1.0; b.commits.len()];
    let n = order.len().max(2) - 1;
    for (rank, i) in order.into_iter().enumerate() {
        b.heat[i] = rank as f32 / n as f32;
    }
    b
}

fn blame(dir: &Path, path: &str, rev: Option<&str>) -> Res<Blame> {
    let args = [&["blame", "--porcelain", "-w"][..], rev.as_slice(), &["--", path]].concat();
    Ok(parse_blame(&git::run(dir, &args)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blame_porcelain() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 2\nauthor Ann\nauthor-mail <a@x>\nauthor-time 100\nsummary first\nfilename f\n\tline one\n{a} 2 2\n\tline two\n\
             {b} 3 3 1\nauthor Bob\nauthor-time 200\nsummary second\nprevious {a} f\nfilename f\n\t\tindented\n"
        );
        let bl = parse_blame(&out);
        assert_eq!(bl.commits.len(), 2);
        assert_eq!(bl.lines.iter().map(|l| (l.0, l.1)).collect::<Vec<_>>(), [(0, 1), (0, 2), (1, 3)]);
        assert_eq!(bl.lines[2].2, "\tindented");
        assert_eq!((bl.commits[1].author.as_str(), bl.commits[1].time), ("Bob", 200));
        assert_eq!(bl.commits[1].previous, Some((a.clone(), "f".into())));
        assert_eq!((bl.commits[0].previous.as_ref(), bl.commits[0].file.as_str()), (None, "f"));
        assert_eq!(bl.heat, [0.0, 1.0]);
    }

    #[test]
    fn quoted_paths() {
        assert_eq!(unquote("plain name.txt"), "plain name.txt");
        assert_eq!(unquote(r#""\303\274 \"q\"\tx""#), "ü \"q\"\tx");
    }

    #[test]
    fn line_log() {
        let out = "\x1eh3\x1fh2\x1fAnn\x1fa@x\x1f300\x1f2026-01-03 10:00\x1fedit\n\ndiff --git a/old b/\"n\\303\\274\"\n--- a/old\n+++ \"b/n\\303\\274\"\n@@ -2,1 +2,1 @@\n-b\n+B\n\
                   \x1eh1\x1f\x1fAnn\x1fa@x\x1f100\x1f2026-01-01 10:00\x1fadd\n\ndiff --git a/old b/old\n--- /dev/null\n+++ b/old\n@@ -0,0 +2,1 @@\n+b\n";
        let h = parse_lines(out, "nü");
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].change.path.as_str(), h[0].change.old_path.as_deref(), h[0].change.code), ("nü", Some("old"), 'R'));
        assert_eq!(h[0].hunks[0].lines.iter().map(|l| (l.kind, l.text.as_str())).collect::<Vec<_>>(), [(b'-', "b"), (b'+', "B")]);
        assert_eq!((h[1].change.path.as_str(), h[1].change.code, h[1].parent.as_deref()), ("old", 'A', None));
        assert_eq!((h[1].hunks[0].new_start, h[1].summary.as_str()), (2, "add"));
        // git >= 2.52: an index line, and a tab after a name with a space.
        let out = "\x1eh2\x1fh1\x1fAnn\x1fa@x\x1f300\x1f2026-01-03 10:00\x1fmv\n\ndiff --git a/o d \"b/n \\303\\274\"\nindex 1..2 100644\n--- a/o d\t\n+++ \"b/n \\303\\274\"\t\n@@ -2,1 +2,1 @@\n-b\n+B\n";
        let c = &parse_lines(out, "n ü")[0].change;
        assert_eq!((c.path.as_str(), c.old_path.as_deref(), c.code), ("n ü", Some("o d"), 'R'));
    }

    /// Lines followed through a rename, from a revision, under awkward names; blame at the
    /// parent via `previous`; folder history; the unstaged-diff guard.
    #[test]
    fn line_folder_and_parent_blame() {
        use crate::repo::testutil::*;
        let dir = scratch("history-lines");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        std::fs::create_dir(dir.join("d[1]")).unwrap();
        let c = |file: &str, text: &str, msg: &str| {
            std::fs::write(dir.join(file), text).unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-qm", msg]);
        };
        let rest = "3\n4\n5\n6\n7\n8\n9\n";
        let old = if cfg!(windows) { "d[1]/a;b.txt" } else { "d[1]/a:b.txt" }; // no ':' in Windows names
        c(old, &format!("1\n2\n{rest}"), "add");
        c(old, &format!("1\nTWO\n{rest}"), "two");
        c("d1.txt", "x", "outside the folder");
        sh(&dir, &["mv", old, "n ü.txt"]);
        c("n ü.txt", &format!("0\n1\nTWO!\n{rest}"), "move and edit");
        c("n ü.txt", &format!("0\n1\nTWO!\n{rest}ten\n"), "ten");

        let h = line_history(&dir, "n ü.txt", Some("HEAD"), 3, 3).unwrap();
        assert_eq!(h.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), ["move and edit", "two", "add"]);
        assert_eq!((h[0].change.path.as_str(), h[0].change.old_path.as_deref()), ("n ü.txt", Some(old)));
        assert_eq!(h[2].change.code, 'A');
        // From a revision: the file there, with its old name.
        assert_eq!(line_history(&dir, old, Some("HEAD~2"), 2, 2).unwrap().len(), 2);

        let f = history(&dir, "d[1]/", PAGE).unwrap();
        assert_eq!(f.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), ["move and edit", "two", "add"]);

        let b = blame(&dir, "n ü.txt", Some("HEAD")).unwrap();
        let c = &b.commits[b.lines[2].0];
        assert_eq!((c.summary.as_str(), c.file.as_str()), ("move and edit", "n ü.txt"));
        let (prev, prev_path) = c.previous.clone().unwrap();
        let p = blame(&dir, &prev_path, Some(&prev)).unwrap();
        assert_eq!((prev_path.as_str(), p.lines[1].2.as_str()), (old, "TWO"));

        // The unstaged diff counts lines from the index: fine while it matches HEAD.
        std::fs::write(dir.join("n ü.txt"), "x\n").unwrap();
        assert!(line_history(&dir, "n ü.txt", None, 3, 3).is_ok_and(|h| h.len() == 3));
        sh(&dir, &["add", "n ü.txt"]);
        assert_eq!(line_history(&dir, "n ü.txt", None, 1, 1).err().as_deref(), Some(INDEX_LINES));

        let ctx = egui::Context::default();
        let mut fv = FileView::open(&ctx, &dir, "d[1]/", Mode::History);
        fv.go(&ctx, prev_path.clone(), Mode::Blame(Some(prev.clone())));
        fv.go(&ctx, prev_path.clone(), Mode::Blame(Some(prev.clone()))); // the same view: no new step
        assert_eq!((fv.back.len(), fv.fwd.len()), (1, 0));
        fv.step(&ctx, false);
        assert_eq!((fv.path.as_str(), &fv.mode, fv.fwd.len()), ("d[1]/", &Mode::History, 1));
        fv.step(&ctx, true);
        wait_for("the blame", || {
            fv.poll();
            fv.data.is_some()
        });
        assert!(matches!(&fv.data, Some(Ok(Data::Blame(b))) if b.lines[1].2 == "TWO"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_with_rename() {
        let out = "\x1eh2\x1fh1\x1fAnn\x1fa@x\x1f200\x1f2026-01-02 10:00\x1fmove it\n\0R100\0old.rs\0new.rs\0\
                   \x1eh1\x1f\x1fAnn\x1fa@x\x1f100\x1f2026-01-01 10:00\x1fadd\n\0A\0old.rs\0";
        let h = parse_history(out);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].change.path.as_str(), h[0].change.old_path.as_deref()), ("new.rs", Some("old.rs")));
        assert_eq!((h[1].change.code, h[1].parent.as_deref()), ('A', None));
    }

    #[test]
    fn history_path_is_literal() {
        use crate::repo::testutil::*;
        let dir = scratch("history-literal");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        commit(&dir, "f1.txt", "one");
        commit(&dir, "f[1].txt", "two");
        commit(&dir, "f1.txt", "three");
        let h = history(&dir, "f[1].txt", PAGE).unwrap();
        assert_eq!(h.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), ["two"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_ends_the_history() {
        use crate::repo::testutil::*;
        let dir = scratch("history-copy");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/__init__.py"), "").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "package a"]);
        std::fs::write(dir.join("b/__init__.py"), "").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "package b"]);
        let h = history(&dir, "b/__init__.py", PAGE).unwrap();
        assert_eq!(h.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), ["package b"], "a's commits are not b's past");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_follows_new_commits() {
        use crate::repo::testutil::*;
        let dir = scratch("history-refresh");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        commit(&dir, "f.txt", "one\n");
        let ctx = egui::Context::default();
        let mut fv = FileView::open(&ctx, &dir, "f.txt", Mode::History);
        let settle = |fv: &mut FileView| {
            wait_for("the history", || {
                fv.poll();
                fv.task.is_none()
            })
        };
        settle(&mut fv);
        commit(&dir, "f.txt", "two\n");
        fv.refresh(&ctx, true);
        settle(&mut fv);
        assert!(matches!(&fv.data, Some(Ok(Data::History(h))) if h.len() == 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_view_lists_the_first_page_again() {
        use crate::repo::testutil::*;
        let dir = scratch("history-limit");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        commit(&dir, "f.txt", "one\n");
        commit(&dir, "g.txt", "two\n");
        let ctx = egui::Context::default();
        let mut fv = FileView::open(&ctx, &dir, "f.txt", Mode::History);
        let settle = |fv: &mut FileView| {
            wait_for("the history", || {
                fv.poll();
                fv.task.is_none()
            })
        };
        let listed = || {
            let log = git::LOG.lock().unwrap();
            log.iter().rev().find(|l| l.cwd == dir && l.args.iter().any(|a| a == "log")).and_then(|l| l.args.iter().find(|a| a.starts_with("-n")).cloned())
        };
        settle(&mut fv);
        fv.more(&ctx);
        settle(&mut fv);
        assert_eq!(listed().as_deref(), Some("-n4000"));
        fv.go(&ctx, "g.txt".into(), Mode::History);
        settle(&mut fv);
        assert_eq!((listed().as_deref(), fv.limit), (Some("-n1000"), PAGE));
        fv.more(&ctx);
        settle(&mut fv);
        fv.step(&ctx, false);
        settle(&mut fv);
        assert_eq!((listed().as_deref(), fv.limit), (Some("-n1000"), PAGE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_newer_load_stops_the_blame_still_running() {
        use crate::repo::testutil::*;
        let dir = scratch("history-cancel");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        commit(&dir, "f.txt", "one\n");
        std::fs::write(dir.join(".gitattributes"), "f.txt diff=slow\n").unwrap();
        let (gate, script) = (dir.join(".git/gate"), dir.join(".git/slow.sh"));
        std::fs::write(&script, format!("if [ -e '{0}' ]; then exec cat \"$1\"; fi\n: > '{0}'\nsleep 30\ncat \"$1\"\n", gate.display())).unwrap();
        sh(&dir, &["config", "diff.slow.textconv", &format!("sh '{}'", script.display())]);
        let ctx = egui::Context::default();
        let mut fv = FileView::open(&ctx, &dir, "f.txt", Mode::Blame(None));
        wait_for("the first blame to start", || gate.exists());
        fv.refresh(&ctx, false);
        wait_for("the second blame", || {
            fv.poll();
            fv.data.is_some()
        });
        assert!(matches!(&fv.data, Some(Ok(Data::Blame(b))) if b.lines.len() == 1));
        let stopped = || git::LOG.lock().unwrap().iter().any(|l| l.cwd == dir && l.args.iter().any(|a| a == "blame") && l.code.is_none());
        for _ in 0..500 {
            if stopped() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(stopped(), "the superseded blame was left running");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
