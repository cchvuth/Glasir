//! File history (follows renames) and blame: the git queries, their parsers
//! and the `FileView` state. Drawn by `crate::ui::history`.

use super::Task;
use crate::git::{self, Change, Res};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    History,
    Blame,
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
}

pub struct BlameCommit {
    pub id: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
}

#[derive(Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// (commit index, line text)
    pub lines: Vec<(usize, String)>,
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
    pub sel: Option<usize>,
}

impl FileView {
    pub fn open(ctx: &egui::Context, dir: &Path, path: &str, mode: Mode) -> Self {
        let (dir, p) = (dir.to_path_buf(), path.to_owned());
        let task = Task::spawn(ctx, move || match mode {
            Mode::History => history(&dir, &p).map(Data::History),
            Mode::Blame => blame(&dir, &p).map(Data::Blame),
        });
        FileView { path: path.into(), mode, data: None, task: Some(task), sel: None }
    }

    pub fn poll(&mut self) {
        if let Some(d) = Task::poll(&mut self.task) {
            self.data = Some(d);
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
        });
    }
    v
}

fn history(dir: &Path, path: &str) -> Res<Vec<Entry>> {
    let out = git::run(
        dir,
        &[
            "--literal-pathspecs",
            "log",
            "--follow",
            "-M",
            "-z",
            "--name-status",
            git::DATE_FORMAT,
            "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%s",
            "--",
            path,
        ],
    )?;
    Ok(parse_history(&out))
}

pub fn parse_blame(out: &str) -> Blame {
    let mut b = Blame::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut cur = 0;
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            b.lines.push((cur, text.to_owned()));
            continue;
        }
        let mut it = line.splitn(2, ' ');
        let (key, val) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if key.len() >= 40 && key.bytes().all(|c| c.is_ascii_hexdigit()) {
            cur = *index.entry(key.to_owned()).or_insert_with(|| {
                b.commits.push(BlameCommit { id: key.to_owned(), author: String::new(), time: 0, summary: String::new() });
                b.commits.len() - 1
            });
            continue;
        }
        let Some(c) = b.commits.get_mut(cur) else { continue };
        match key {
            "author" => c.author = val.into(),
            "author-time" => c.time = val.parse().unwrap_or(0),
            "summary" => c.summary = val.into(),
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

fn blame(dir: &Path, path: &str) -> Res<Blame> {
    Ok(parse_blame(&git::run(dir, &["blame", "--porcelain", "-w", "--", path])?))
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
        assert_eq!(bl.lines.iter().map(|l| l.0).collect::<Vec<_>>(), [0, 0, 1]);
        assert_eq!(bl.lines[2].1, "\tindented");
        assert_eq!((bl.commits[1].author.as_str(), bl.commits[1].time), ("Bob", 200));
        assert_eq!(bl.heat, [0.0, 1.0]);
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
        let h = history(&dir, "f[1].txt").unwrap();
        assert_eq!(h.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), ["two"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
