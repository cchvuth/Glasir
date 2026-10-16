//! 3-way merge editor state (no drawing: `crate::ui::merge_editor`): conflict-marker parsing
//! (merge, diff3, zdiff3), loading a conflicted file with its side labels, and saving the output.

use super::{Backup, Op, Task};
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
    /// A regular file in the working tree, or none: what a backup can cover.
    pub(crate) file: bool,
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
pub(super) fn labels(dir: &Path) -> [String; 2] {
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

/// Conflicted files "Mark all resolved" may stage: both sides present (a deleted side is a
/// decision to make), a text file git merges (`-merge` / `binary`, or a side git sees as
/// binary content, keep our side without markers), and no conflict markers left.
/// A fixed number of git runs however many files conflict.
pub(super) fn resolvable(dir: &Path, st: &git::Status) -> Vec<String> {
    let both: Vec<&str> = st.conflicted.iter().map(|c| c.path.as_str()).filter(|p| matches!(st.conflict_xy.get(*p).map(String::as_str), Some("UU" | "AA"))).collect();
    let attrs = attrs_many(dir, &both);
    let free: Vec<&str> = both
        .into_iter()
        .filter(|&p| {
            let (n, merged) = attrs.get(p).copied().unwrap_or((7, true));
            let path = dir.join(p);
            let bytes = std::fs::symlink_metadata(&path).ok().filter(|m| m.is_file()).and_then(|_| std::fs::read(&path).ok());
            merged && bytes.is_some_and(|b| {
                let t = decode(&b);
                !b[..b.len().min(8000)].contains(&0) && conflicts(&parse(&t, n)).next().is_none() && !t.lines().any(|l| is_marker(l, b'<', n) || is_marker(l, b'>', n))
            })
        })
        .collect();
    let binary = binary_stages(dir, &free);
    free.into_iter().filter(|p| !binary.contains(p)).map(String::from).collect()
}

/// Which of `paths` have a stage whose blob git sees as binary (a NUL in its first 8000 bytes).
fn binary_stages<'a>(dir: &Path, paths: &[&'a str]) -> std::collections::HashSet<&'a str> {
    if paths.is_empty() {
        return Default::default();
    }
    let known: std::collections::HashMap<&str, &'a str> = paths.iter().map(|&p| (p, p)).collect();
    let ls = git::run(dir, &["--literal-pathspecs", "ls-files", "-u", "-z"]).unwrap_or_default();
    // `mode oid stage\tpath` -> (path, oid)
    let entries: Vec<(&'a str, &str)> = ls.split('\0').filter_map(|e| {
        let (meta, p) = e.split_once('\t')?;
        Some((*known.get(p)?, meta.split(' ').nth(1)?))
    }).collect();
    let mut oids: Vec<&str> = entries.iter().map(|e| e.1).collect();
    oids.sort_unstable();
    oids.dedup();
    let input: String = oids.iter().map(|o| format!("{o}\n")).collect();
    // ponytail: whole blobs come back through the pipe; `cat-file --batch` has no byte limit.
    let out = git::run_raw(dir, &["cat-file", "--batch"], Some(input.as_bytes())).unwrap_or_default();
    let (mut nul, mut at) = (std::collections::HashSet::new(), 0);
    for oid in &oids {
        // `<oid> <type> <size>\n<content>\n`
        let Some(nl) = out[at..].iter().position(|&b| b == b'\n') else { break };
        let size: usize = std::str::from_utf8(&out[at..at + nl]).ok().and_then(|h| h.rsplit(' ').next()?.parse().ok()).unwrap_or(0);
        let body = at + nl + 1;
        if out[body..(body + size.min(8000)).min(out.len())].contains(&0) {
            nul.insert(*oid);
        }
        at = body + size + 1;
    }
    entries.into_iter().filter(|e| nul.contains(e.1)).map(|e| e.0).collect()
}

/// The text of `bytes`: UTF-8, else each byte as the char of that code (Latin-1, lossless
/// to write back with [`save`]).
pub(crate) fn decode(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes).map_or_else(|_| bytes.iter().map(|&b| b as char).collect(), str::to_owned)
}

fn load(dir: &Path, path: &str) -> Loaded {
    let p = dir.join(path);
    let meta = std::fs::symlink_metadata(&p);
    let original = match &meta {
        Ok(m) if m.is_file() => std::fs::read(&p).unwrap_or_default(),
        _ => vec![],
    };
    // git's own binary test; other encodings are text (see `decode`).
    let text = Some(decode(&original)).filter(|_| !original[..original.len().min(8000)].contains(&0));
    // Unmerged index entries: stage 2 = current, 3 = incoming. Not unmerged (or no repo): assume both.
    let unmerged = git::run(dir, &["--literal-pathspecs", "ls-files", "-u", "-z", "--", path]).unwrap_or_default();
    let has = |n: &str| unmerged.is_empty() || unmerged.split('\0').any(|e| e.split_once('\t').is_some_and(|(m, _)| m.ends_with(n)));
    let stages = [has(" 2"), has(" 3")];
    let size = marker_size(dir, path);
    let mut whole = match (&meta, &text) {
        // git leaves the modified version in the tree: it has no markers to edit.
        _ if stages.contains(&false) => Some("The file was deleted on one side."),
        (Err(_), _) => Some("The file is missing from the working tree."),
        (Ok(m), _) if !m.is_file() => Some("This is not a regular file (symlink or submodule)."),
        (_, None) => Some("Binary file."),
        _ => None,
    };
    let (mut orig, mut sides, mut width) = (vec![], [vec![], vec![]], [0, 0]);
    for c in parse(text.as_deref().unwrap_or(""), size) {
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
    let file = meta.as_ref().map_or(true, |m| m.is_file());
    Loaded { original, whole, stages, file, size, labels: labels(dir), orig, sides, width }
}

/// Writes the resolved file, refusing symlinks and files changed since they were loaded.
pub(crate) fn save(dir: &Path, path: &str, original: &[u8], output: &str) -> Res<()> {
    let p = dir.join(path);
    if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
        return Err(format!("{path} is not a regular file; resolve it manually."));
    }
    if std::fs::read(&p).map_err(|e| format!("{path}: {e}"))? != original {
        return Err(format!("{path} changed on disk since it was read here. Open it again and resolve it from there."));
    }
    // A file that isn't UTF-8 is written back byte for byte (see `decode`).
    let bytes = if std::str::from_utf8(original).is_ok() { output.as_bytes().to_vec() } else { output.chars().map(|c| u8::try_from(c as u32).map_err(|_| format!("{path} is not UTF-8 and the output has {c:?}, which it can't hold."))).collect::<Res<_>>()? };
    std::fs::write(&p, bytes).map_err(|e| format!("Could not write {path}: {e}"))
}

/// Claude's resolution of `path` from the dialog (outside the editor): written
/// over `original`, the bytes the job read, and the op that marks it resolved.
/// Refused if the reply kept conflict markers or the file changed since.
pub(crate) fn apply_ai(dir: &Path, path: &str, original: &[u8], reply: &str) -> Res<Op> {
    let out = like(original, reply)?;
    let n = marker_size(dir, path);
    // Every marker line the reply keeps must be one the file already had outside its conflicts.
    let mut had = stray(&decode(original), n);
    let new = stray(&out, n).into_iter().any(|l| had.iter().position(|h| *h == l).map(|i| had.swap_remove(i)).is_none());
    if conflicts(&parse(&out, n)).next().is_some() || new {
        return Err(format!("{}'s answer for {path} still has conflict markers; resolve it in the merge editor.", crate::ai::name()));
    }
    save(dir, path, original, &out)?;
    Ok(stage(path))
}

/// Marker lines outside well-formed conflict blocks.
fn stray(text: &str, n: usize) -> Vec<String> {
    let text = parse(text, n).into_iter().filter_map(|c| if let Chunk::Text(t) = c { Some(t) } else { None }).collect::<String>();
    text.lines().filter(|l| b"<|=>".iter().any(|&c| is_marker(l, c, n))).map(str::to_owned).collect()
}

/// `text` ending in a newline, with CRLF line endings when `original` has them
/// (the reply comes back with LF). Refused when `original` mixes both: which
/// line had which can't be told from the reply.
fn like(original: &[u8], text: &str) -> Res<String> {
    let mut out = text.to_owned();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let crlf = original.windows(2).filter(|w| w == b"\r\n").count();
    if crlf > 0 && !out.contains("\r\n") {
        if crlf != original.iter().filter(|&&b| b == b'\n').count() {
            return Err(format!("The file mixes CRLF and LF line endings, which {}'s answer can't keep; resolve it with the merge editor's per-conflict choices.", crate::ai::name()));
        }
        out = out.replace('\n', "\r\n");
    }
    Ok(out)
}

fn marker_size(dir: &Path, path: &str) -> usize {
    attrs(dir, path).0
}

/// The path's conflict-marker size, and whether git text-merges it (`merge` not unset / binary).
fn attrs(dir: &Path, path: &str) -> (usize, bool) {
    attrs_many(dir, &[path]).get(path).copied().unwrap_or((7, true))
}

/// [`attrs`] of every path, in one git run.
fn attrs_many<'a>(dir: &Path, paths: &[&'a str]) -> std::collections::HashMap<&'a str, (usize, bool)> {
    let input: String = paths.iter().map(|p| format!("{p}\0")).collect();
    let out = git::run_input(dir, &["check-attr", "-z", "--stdin", "conflict-marker-size", "merge"], Some(input.as_bytes())).unwrap_or_default();
    // `path\0attr\0value\0` records.
    let f: Vec<&str> = out.split('\0').collect();
    let mut by: std::collections::HashMap<&'a str, (usize, bool)> = paths.iter().map(|&p| (p, (7, true))).collect();
    for r in f.chunks_exact(3) {
        let Some(e) = by.get_mut(r[0]) else { continue };
        match r[1] {
            "conflict-marker-size" => e.0 = r[2].parse().ok().filter(|&n| n > 0).unwrap_or(7),
            "merge" => e.1 = !matches!(r[2], "unset" | "binary"),
            _ => {}
        }
    }
    by
}

/// Whole-file choices for the current and incoming side. A side without a
/// version deleted the file: "Delete file"; the other side then keeps the working file.
pub(crate) fn whole_ops(path: &str, d: &Loaded) -> [Op; 2] {
    let gone = d.stages.contains(&false);
    [(0, "--ours", "Take current"), (1, "--theirs", "Take incoming")].map(|(i, side, name)| {
        if !d.stages[i] {
            overwrites(Op::new("Delete file", &["--literal-pathspecs", "rm", "-q", "--", path]).light(), path, d.file)
        } else if gone {
            Op::new("Keep file", &["--literal-pathspecs", "add", "--", path]).light()
        } else {
            take_side(name, side, path, d.file)
        }
    })
}

/// `checkout --ours|--theirs` and stage.
pub fn take_side(name: &str, side: &str, path: &str, file: bool) -> Op {
    overwrites(Op::new(name, &["--literal-pathspecs", "checkout", side, "--", path]).then(&["--literal-pathspecs", "add", "--", path]).light(), path, file)
}

/// `op` replaces the working `path`, which may hold a resolution made elsewhere:
/// backed up (⌘Z) when `file` (a regular file, or none); a backup would refuse
/// anything else (symlink, submodule), so that is confirmed instead.
fn overwrites(op: Op, path: &str, file: bool) -> Op {
    if file {
        op.backup(Backup::Paths(vec![path.into()]))
    } else {
        let what = op.label.clone();
        op.confirm(format!("{what} replaces {path} in the working tree. It isn't a regular file, so this can't be undone."))
    }
}

/// Stages resolved files in one op; paths go over stdin (a command line has a length limit).
pub fn stage_many(label: &str, paths: &[&str]) -> Op {
    let input: String = paths.iter().map(|p| format!("{p}\0")).collect();
    Op::new(label, &["--literal-pathspecs", "add", "--pathspec-from-file=-", "--pathspec-file-nul"]).input(input).light().backup(Backup::Staged(paths.iter().map(|p| p.to_string()).collect()))
}

/// Stages a resolved file; ⌘Z brings the conflict back (the file keeps its resolution).
pub fn stage(path: &str) -> Op {
    Op::new("Mark resolved", &["--literal-pathspecs", "add", "--", path]).light().backup(Backup::Staged(vec![path.into()]))
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
    /// Conflicts in `chunks` whose sides are [`Conflict::trivial`].
    pub(crate) trivial: usize,
    /// The output pane's last layout, reused while the text and style stay the same.
    pub(crate) galley: Option<OutLayout>,
}

/// (text, (text color, dim color, wrap width, pixels per point), galley)
pub(crate) type OutLayout = (String, (egui::Color32, egui::Color32, u32, u32), std::sync::Arc<egui::Galley>);

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
            trivial: 0,
            galley: None,
        }
    }

    pub fn edited(&self) -> bool {
        self.data.as_ref().is_some_and(|d| decode(&d.original) != self.output)
    }

    /// Replace the output (e.g. with Claude's resolution).
    pub fn fill(&mut self, text: &str) {
        match like(self.data.as_ref().map_or(&[][..], |d| &d.original[..]), text) {
            Ok(out) => self.output = out,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        }
        self.reparse();
    }

    pub(crate) fn reparse(&mut self) {
        self.chunks = parse(&self.output, self.data.as_ref().map_or(7, |d| d.size));
        let orig = self.data.as_ref().map_or(&[][..], |d| &d.orig[..]);
        // Matched by content (the first unused original with the same sides), so the side panes
        // follow even after hand edits.
        let mut by: std::collections::HashMap<(&str, &str), Vec<usize>> = Default::default();
        for (k, o) in orig.iter().enumerate().rev() {
            by.entry((&o.ours, &o.theirs)).or_default().push(k);
        }
        self.orig_of = conflicts(&self.chunks).map(|c| by.get_mut(&(c.ours.as_str(), c.theirs.as_str()))?.pop()).collect();
        self.trivial = conflicts(&self.chunks).filter(|c| c.trivial()).count();
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

    /// Claude's resolution from the dialog: refused over newer edits or leftover markers; keeps CRLF.
    #[test]
    fn apply_ai_resolution() {
        let dir = crate::repo::testutil::scratch("merge-ai-apply");
        let read = || std::fs::read_to_string(dir.join("f")).unwrap();
        let original = b"a\r\n<<<<<<< ours\r\nx\r\n=======\r\ny\r\n>>>>>>> theirs\r\n".to_vec();
        std::fs::write(dir.join("f"), &original).unwrap();
        assert!(apply_ai(&dir, "f", &original, "a\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs").unwrap_err().contains("conflict markers"));
        std::fs::write(dir.join("f"), "manual\n").unwrap();
        assert!(apply_ai(&dir, "f", &original, "a\nxy").unwrap_err().contains("changed on disk"));
        assert_eq!(read(), "manual\n");
        std::fs::write(dir.join("f"), &original).unwrap();
        assert_eq!(apply_ai(&dir, "f", &original, "a\nxy").unwrap().cmds, stage("f").cmds);
        assert_eq!(read(), "a\r\nxy\r\n");
        // A lone marker line left in the reply is refused; one the file already had outside its conflicts is kept.
        let original = b"T\n=======\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\n".to_vec();
        for reply in ["T\n=======\nOURS\n<<<<<<< HEAD\nx\n=======\n", "T\n=======\n<<<<<<< HEAD\nOURS\n", "T\n=======\na\n=======\nc", "T\n=======\nx\n>>>>>>> theirs", "T\n<<<<<<< HEAD\nx\n"] {
            std::fs::write(dir.join("f"), &original).unwrap();
            assert!(apply_ai(&dir, "f", &original, reply).unwrap_err().contains("conflict markers"), "{reply}");
        }
        assert!(apply_ai(&dir, "f", &original, "T\n=======\nxy").is_ok());
        // Mixed line endings: which line keeps which can't be told from the reply.
        let mixed = b"a\r\nb\n<<<<<<< ours\nx\n=======\ny\n>>>>>>> theirs\n".to_vec();
        std::fs::write(dir.join("f"), &mixed).unwrap();
        assert!(apply_ai(&dir, "f", &mixed, "a\nb\nxy").unwrap_err().contains("line endings"));
        assert_eq!(std::fs::read(dir.join("f")).unwrap(), mixed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `-merge` / `binary` files, and files whose content git sees as binary, keep our side
    /// without markers: never bulk-resolvable.
    #[test]
    fn unmergeable_files_are_not_resolvable() {
        use crate::repo::testutil::{commit, identity, scratch, sh};
        let dir = scratch("resolvable-attr");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitattributes", "yarn.lock -merge\n*.svg binary\n");
        for f in ["yarn.lock", "a.svg", "a.txt"] {
            commit(&dir, f, "base\n");
        }
        commit(&dir, "bin.txt", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        for f in ["yarn.lock", "a.svg", "a.txt"] {
            commit(&dir, f, "feature\n");
        }
        std::fs::write(dir.join("bin.txt"), "feature\0\n").unwrap();
        sh(&dir, &["commit", "-qam", "binary"]);
        sh(&dir, &["checkout", "-q", "main"]);
        for f in ["yarn.lock", "a.svg", "a.txt", "bin.txt"] {
            commit(&dir, f, "main\n");
        }
        assert!(git::run(&dir, &["merge", "feature"]).is_err());
        std::fs::write(dir.join("a.txt"), "both\n").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("yarn.lock")).unwrap(), "main\n");
        assert_eq!(resolvable(&dir, &git::status(&dir).unwrap()), ["a.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Latin-1 text file conflicts like any other: editable, bulk-resolvable, written back byte for byte.
    #[test]
    fn non_utf8_text_is_not_binary() {
        use crate::repo::testutil::{identity, scratch, sh};
        let dir = scratch("merge-latin1");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let commit = |text: &[u8]| {
            std::fs::write(dir.join("l.txt"), text).unwrap();
            sh(&dir, &["commit", "-qam", "c"]);
        };
        std::fs::write(dir.join("l.txt"), b"caf\xe9 base\nline\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "base"]);
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(b"caf\xe9 x\nline\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(b"caf\xe9 main\nline\n");
        assert!(git::run(&dir, &["merge", "x"]).is_err());
        let d = load(&dir, "l.txt");
        assert_eq!((d.whole, d.orig.len()), (None, 1));
        assert_eq!(d.orig[0].ours, "caf\u{e9} main\n");
        assert!(resolvable(&dir, &git::status(&dir).unwrap()).is_empty());
        let out = decode(&d.original).replace("<<<<<<< HEAD\n", "").replace("=======\ncaf\u{e9} x\n>>>>>>> x\n", "");
        save(&dir, "l.txt", &d.original, &out).unwrap();
        assert_eq!(std::fs::read(dir.join("l.txt")).unwrap(), b"caf\xe9 main\nline\n");
        assert_eq!(resolvable(&dir, &git::status(&dir).unwrap()), ["l.txt"]);
        std::fs::write(dir.join("l.txt"), b"caf\xe9 base\nline\n").unwrap();
        assert!(save(&dir, "l.txt", b"caf\xe9 base\nline\n", "\u{65e5}").unwrap_err().contains("can't hold"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Marking resolved (one file or many, paths sent on stdin) is undone by ⌘Z: the conflict is back.
    #[test]
    fn mark_resolved_is_undoable() {
        use crate::repo::testutil::{commit, identity, scratch, settle};
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("merge-mark-undo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        commit(&dir, "g", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "f", "x\n");
        commit(&dir, "g", "x\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main\n");
        commit(&dir, "g", "main\n");
        assert!(git::run(&dir, &["merge", "x"]).is_err());
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let status = || sh(&dir, &["status", "--porcelain"]);
        for (ops, paths) in [(stage("f"), vec!["f"]), (stage_many("Mark resolved", &["f", "g"]), vec!["f", "g"])] {
            for p in &paths {
                std::fs::write(dir.join(p), "mine\n").unwrap();
            }
            t.apply(&ctx, Action::Git(ops));
            settle(&ctx, &mut t);
            assert!(paths.iter().all(|p| status().contains(&format!("M  {p}"))), "{}", status());
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
            assert!(paths.iter().all(|p| status().contains(&format!("UU {p}"))), "{}", status());
            assert_eq!(std::fs::read_to_string(dir.join(paths[0])).unwrap(), "mine\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
            trivial: 0,
            galley: None,
        };
        ed.reparse();
        assert_eq!(ed.orig_of, [Some(0), Some(1)]);
        ed.resolve(0, Take::Incoming);
        assert_eq!((ed.orig_of.as_slice(), ed.sel), (&[Some(1)][..], 0));
        assert!(ed.output_line(0) > 1);
        ed.output = ed.output.replacen("two x", "two y", 1); // hand edit inside the block
        ed.reparse();
        assert_eq!(ed.orig_of, [None]);
        ed.fill("resolved by Claude");
        assert_eq!((ed.output.as_str(), ed.orig_of.len()), ("resolved by Claude\n", 0));

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

    /// Resolved elsewhere (no markers left), then "Take current": ⌘Z brings the resolution back.
    #[test]
    fn taking_a_side_backs_up_a_hand_resolution() {
        use crate::repo::testutil::{commit, identity, scratch, settle};
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("merge-hand");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "f", "x\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main\n");
        assert!(git::run(&dir, &["merge", "x"]).is_err());
        std::fs::write(dir.join("f"), "hand merged\n").unwrap();
        let d = load(&dir, "f");
        assert_eq!(d.whole, Some("No conflict markers found in this file."));
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let [ours, _] = whole_ops("f", &d);
        t.apply(&ctx, Action::Git(ours));
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "main\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "hand merged\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Modify/delete: "Delete file" over an edited survivor is undoable; a symlink side is confirmed.
    #[test]
    fn whole_file_choices_never_lose_the_working_file() {
        use crate::repo::testutil::{commit, identity, scratch, settle};
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("merge-du-undo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "f", "x\n");
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["rm", "-q", "f"]);
        sh(&dir, &["commit", "-q", "-m", "rm"]);
        assert!(git::run(&dir, &["merge", "x"]).is_err());
        std::fs::write(dir.join("f"), "edited\n").unwrap();
        let [del, _] = whole_ops("f", &load(&dir, "f"));
        assert_eq!(del.label, "Delete file");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(del));
        settle(&ctx, &mut t);
        assert!(!dir.join("f").exists());
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "edited\n");
        // The conflict comes back too, not a staged deletion beside an untracked copy; git 2.39
        // can't unresolve a path resolved by its removal (2.43 can): the file is back, untracked.
        let v = sh(&dir, &["--version"]);
        let num = |i: usize| v.split_whitespace().nth(2).and_then(|v| v.split('.').nth(i)?.parse::<u32>().ok());
        let old = matches!((num(0), num(1)), (Some(major), Some(minor)) if (major, minor) < (2, 43));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), if old { "?? f\n" } else { "DU f\n" });
        // Once the merge is committed, the file comes back untracked: no conflict without a merge.
        let [del, _] = whole_ops("f", &load(&dir, "f"));
        t.apply(&ctx, Action::Git(del));
        settle(&ctx, &mut t);
        sh(&dir, &["commit", "-q", "-m", "merged"]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "?? f\n");

        #[cfg(unix)]
        {
            std::fs::remove_file(dir.join("f")).unwrap();
            let link = |to: &str| {
                let _ = std::fs::remove_file(dir.join("l"));
                std::os::unix::fs::symlink(to, dir.join("l")).unwrap();
            };
            link("base");
            sh(&dir, &["add", "l"]);
            sh(&dir, &["commit", "-q", "-m", "l"]);
            sh(&dir, &["checkout", "-q", "-b", "y"]);
            link("y");
            sh(&dir, &["commit", "-q", "-am", "y"]);
            sh(&dir, &["checkout", "-q", "main"]);
            link("main");
            sh(&dir, &["commit", "-q", "-am", "main"]);
            assert!(git::run(&dir, &["merge", "y"]).is_err());
            link("merged");
            let d = load(&dir, "l");
            assert!(d.whole.is_some());
            assert!(whole_ops("l", &d).iter().all(|op| op.confirm.is_some()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
