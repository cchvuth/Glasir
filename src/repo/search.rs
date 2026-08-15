//! ⌘F commit search: query parser, a background `git log` whose hits stream in, and the filtered
//! graph (no drawing; the bar is `crate::ui::graph_view::search`).

use super::RepoData;
use crate::git::{self, Commit, Res};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// More hits than this stop the search (each is a loaded commit).
pub const MAX_HITS: usize = 10_000;

/// A parsed query: `git log` options, literal pathspecs and the free text matched here
/// against message, author and SHA.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Query {
    pub args: Vec<String>,
    pub paths: Vec<String>,
    pub text: String,
}

/// `author:` `committer:` `message:` `path:` `code:` (-S) `regex:` (-G) `after:` `before:`,
/// `merges` / `no-merges`; values may be "quoted". Everything else is free text.
pub(crate) fn parse(q: &str, case: bool) -> Query {
    let (mut out, mut text, mut greps) = (Query::default(), vec![], 0);
    for (tok, quoted) in tokens(q) {
        let op = if quoted { None } else { tok.split_once(':') };
        let arg = match op.map(|(k, v)| (k.to_ascii_lowercase(), v)) {
            Some((k, v)) if KEYS.contains(&k.as_str()) => {
                if v.is_empty() {
                    continue; // still being typed
                }
                match k.as_str() {
                    "author" => format!("--author={v}"),
                    "committer" => format!("--committer={v}"),
                    "message" => {
                        greps += 1;
                        format!("--grep={v}")
                    }
                    "code" => format!("-S{v}"),
                    "regex" => format!("-G{v}"),
                    "after" => format!("--since={v}"),
                    "before" => format!("--until={v}"),
                    _ => {
                        out.paths.push(v.into());
                        continue;
                    }
                }
            }
            _ if !quoted && (tok == "merges" || tok == "no-merges") => format!("--{tok}"),
            _ => {
                text.push(tok);
                continue;
            }
        };
        out.args.push(arg);
    }
    if !out.args.is_empty() {
        // Literal author/committer/message patterns; -i also covers -S and -G.
        out.args.push("--fixed-strings".into());
        if !case {
            out.args.push("--regexp-ignore-case".into());
        }
        if greps > 1 {
            out.args.push("--all-match".into());
        }
    }
    out.text = text.join(" ");
    out
}

const KEYS: [&str; 9] = ["author", "committer", "message", "path", "code", "regex", "after", "before", "file"];

/// Whitespace-separated tokens; `"..."` groups (a token that starts quoted is free text).
fn tokens(q: &str) -> Vec<(String, bool)> {
    let (mut out, mut cur, mut quoted, mut inq) = (vec![], String::new(), false, false);
    for c in q.chars() {
        match c {
            '"' => {
                quoted |= cur.is_empty() && !inq;
                inq = !inq;
            }
            c if c.is_whitespace() && !inq => {
                if !cur.is_empty() {
                    out.push((std::mem::take(&mut cur), quoted));
                }
                quoted = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push((cur, quoted));
    }
    out
}

impl Query {
    fn is_empty(&self) -> bool {
        self.args.is_empty() && self.paths.is_empty() && self.text.is_empty()
    }

    /// Free text against the full message, author name / email and a SHA prefix.
    fn matches(&self, c: &Commit, body: &str, case: bool) -> bool {
        let t = &self.text;
        if t.is_empty() || (t.len() >= 4 && c.id.starts_with(&t.to_lowercase())) {
            return true;
        }
        let has = |s: &str| if case { s.contains(t.as_str()) } else { s.to_lowercase().contains(&t.to_lowercase()) };
        has(body) || has(&c.author) || has(&c.email)
    }
}

/// ⌘F search over all history reachable from the shown refs.
pub struct Search {
    pub query: String,
    pub case: bool,
    /// Show only the matching commits in the graph.
    pub filter: bool,
    /// Matching commits in graph order, as they stream in.
    pub(crate) found: Vec<Commit>,
    /// Rows of the shown graph holding a match, ascending.
    pub(crate) hits: Vec<usize>,
    /// Index into `found`.
    pub(crate) cur: usize,
    pub(crate) error: Option<String>,
    /// The graph of just the matches while `filter` is on.
    pub(crate) filtered: Option<Arc<RepoData>>,
    /// Recent queries, newest first (mirrored with `Settings`).
    pub history: Vec<String>,
    pub(crate) focus: bool,
    dir: PathBuf,
    rx: Option<Receiver<Res<Commit>>>,
    cancel: git::Cancel,
    /// (query, case, refs fingerprint) of the current search, bumped `run` on change.
    key: (String, bool, u64),
    run: usize,
    log_ptr: usize,
    refs_fp: u64,
    rows_key: (usize, bool, usize, usize),
    /// (run, data generation, hits) the filtered graph was built from, and when.
    filtered_key: (usize, u64, usize),
    filtered_at: std::time::Instant,
}

impl Search {
    pub fn open(dir: PathBuf) -> Self {
        Search {
            query: String::new(),
            case: false,
            filter: false,
            found: vec![],
            hits: vec![],
            cur: 0,
            error: None,
            filtered: None,
            history: vec![],
            focus: true,
            dir,
            rx: None,
            cancel: Default::default(),
            key: Default::default(),
            run: 0,
            log_ptr: 0,
            refs_fp: 0,
            rows_key: Default::default(),
            filtered_key: Default::default(),
            filtered_at: std::time::Instant::now(),
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// (Re)starts the git search when the query or the refs changed, takes in streamed hits
    /// and rebuilds the filtered graph. `d` is the full history.
    pub(crate) fn poll(&mut self, ctx: &egui::Context, d: &RepoData) {
        let ptr = Arc::as_ptr(&d.log) as usize;
        if ptr != self.log_ptr {
            // A reload: re-run only if a ref moved (new commits), not for every refresh.
            self.log_ptr = ptr;
            let mut h = DefaultHasher::new();
            (d.view.revs(), d.refs.iter().map(|r| (&r.full, &r.target)).collect::<Vec<_>>(), &d.status.head).hash(&mut h);
            self.refs_fp = h.finish();
        }
        let key = (self.query.trim().to_owned(), self.case, self.refs_fp);
        if key != self.key {
            self.key = key;
            self.start(ctx, d);
        }
        while let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok(c)) => self.found.push(c),
                Ok(Err(e)) => self.error = Some(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }
        let fkey = (self.run, d.generation, self.found.len());
        if !self.filter || self.found.is_empty() {
            self.filtered = None;
        } else if self.filtered.is_none() || fkey != self.filtered_key {
            // A rebuild is O(hits): while they stream in, only when their number doubled
            // or a moment passed; the end of the stream brings it up to date.
            const EVERY: std::time::Duration = std::time::Duration::from_millis(250);
            if self.filtered.is_none() || !self.running() || fkey.2 >= 2 * self.filtered_key.2 || self.filtered_at.elapsed() >= EVERY {
                (self.filtered_key, self.filtered_at) = (fkey, std::time::Instant::now());
                self.filtered = Some(Arc::new(self.filtered_data(d)));
            } else {
                ctx.request_repaint_after(EVERY);
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, d: &RepoData) {
        self.cancel.cancel();
        (self.rx, self.cancel, self.error, self.cur, self.filtered) = (None, Default::default(), None, 0, None);
        self.found.clear();
        self.run += 1;
        let q = parse(&self.key.0, self.case);
        if q.is_empty() || d.log.is_empty() {
            return;
        }
        let fmt = format!("{}%x1f%B", git::LOG_FORMAT);
        let mut args: Vec<String> = ["--literal-pathspecs", "log", "--date-order", "-z", git::DATE_FORMAT, &fmt].map(String::from).into();
        args.extend(q.args.iter().cloned());
        args.extend(d.view.revs());
        args.push("--".into());
        args.extend(q.paths.iter().cloned());
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let (ctx, dir, cancel, case) = (ctx.clone(), self.dir.clone(), self.cancel.clone(), self.case);
        std::thread::spawn(move || {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut n = 0;
            let r = cancel.scope(|| {
                git::stream(&dir, &a, |rec| {
                    let Some((c, body)) = record(rec) else { return true };
                    if !q.matches(&c, body, case) {
                        return true;
                    }
                    n += 1;
                    ctx.request_repaint();
                    tx.send(Ok(c)).is_ok() && n < MAX_HITS
                })
            });
            if let Err(e) = r
                && e != git::CANCELLED
            {
                let _ = tx.send(Err(e));
            }
            ctx.request_repaint();
        });
    }

    /// The history of just the hits. Lanes join hits only (a parent that is no hit would be
    /// a lane to nowhere), but `log` keeps the real commits: menus read their parents.
    fn filtered_data(&self, d: &RepoData) -> RepoData {
        let ids: std::collections::HashSet<&str> = self.found.iter().map(|c| c.id.as_str()).collect();
        let log = self.found.iter().map(|c| Commit { parents: c.parents.iter().filter(|p| ids.contains(p.as_str())).cloned().collect(), ..c.clone() }).collect();
        let mut f = RepoData::build(Arc::new(log), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), d.status.clone(), d.view.clone());
        (f.worktrees, f.submodules, f.lfs, f.config, f.author, f.bisect) = (d.worktrees.clone(), d.submodules.clone(), d.lfs.clone(), d.config.clone(), d.author.clone(), d.bisect.clone());
        f.log = Arc::new(self.found.clone());
        f
    }

    /// Recomputes `hits` for the graph being drawn (the full or the filtered history).
    pub(crate) fn rows(&mut self, d: &RepoData) {
        let key = (Arc::as_ptr(&d.log) as usize, d.wip, self.run, self.found.len());
        if key != self.rows_key {
            self.rows_key = key;
            self.hits = self.found.iter().filter_map(|c| d.row_of.get(&c.id).copied()).collect();
            self.hits.sort_unstable();
        }
    }

    /// Puts the current query on top of the history (last 10).
    pub(crate) fn remember(&mut self) {
        let q = self.query.trim().to_owned();
        if !q.is_empty() {
            self.history.retain(|h| *h != q);
            self.history.insert(0, q);
            self.history.truncate(10);
        }
    }

    pub(crate) fn dims(&self, r: usize) -> bool {
        !self.query.trim().is_empty() && self.hits.binary_search(&r).is_err()
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// One `LOG_FORMAT%x1f%B` record: the commit and its full message.
fn record(rec: &str) -> Option<(Commit, &str)> {
    let (i, _) = rec.match_indices('\x1f').nth(6)?;
    Some((git::parse_log(&rec[..i]).pop()?, &rec[i + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::View;
    use crate::repo::data::load_all;
    use crate::repo::testutil::{identity, scratch, sh};

    fn q(args: &[&str], paths: &[&str], text: &str) -> Query {
        Query { args: args.iter().map(|s| s.to_string()).collect(), paths: paths.iter().map(|s| s.to_string()).collect(), text: text.into() }
    }

    #[test]
    fn parses_operators() {
        assert_eq!(parse("fix login", false), q(&[], &[], "fix login"));
        assert_eq!(
            parse(r#"author:"Ann Lee" path:src/a.rs bug code:foo() merges"#, false),
            q(&["--author=Ann Lee", "-Sfoo()", "--merges", "--fixed-strings", "--regexp-ignore-case"], &["src/a.rs"], "bug")
        );
        assert_eq!(parse("message:a message:b before:2024-01-01", true), q(&["--grep=a", "--grep=b", "--until=2024-01-01", "--fixed-strings", "--all-match"], &[], ""));
        // Half-typed operators are ignored; unknown keys, quoted words and colons are free text.
        assert_eq!(parse("author: fix: \"merges\" regex:a.*b", false), q(&["-Ga.*b", "--fixed-strings", "--regexp-ignore-case"], &[], "fix: merges"));
        assert_eq!(parse(r#"path:"dir with space/f" after:"2 weeks ago" no-merges"#, true), q(&["--since=2 weeks ago", "--no-merges", "--fixed-strings"], &["dir with space/f"], ""));
        assert!(parse("  author:  ", false).is_empty());
    }

    fn finished(dir: &std::path::Path, d: &RepoData, query: &str, filter: bool) -> Search {
        let ctx = egui::Context::default();
        let mut s = Search::open(dir.to_path_buf());
        (s.query, s.filter) = (query.into(), filter);
        s.poll(&ctx, d);
        for _ in 0..1000 {
            if !s.running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            s.poll(&ctx, d);
        }
        assert_eq!(s.error, None);
        s
    }

    fn search(dir: &std::path::Path, d: &RepoData, query: &str) -> Vec<String> {
        finished(dir, d, query, false).found.iter().map(|c| c.summary.clone()).collect()
    }

    /// path: (literal, over the whole history), code: (-S), free text on the body and author.
    #[test]
    fn searches_all_history() {
        let dir = scratch("search");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let put = |file: &str, text: &str, msg: &str| {
            std::fs::write(dir.join(file), text).unwrap();
            sh(&dir, &["add", "--", file]);
            sh(&dir, &["commit", "-q", "-m", msg]);
        };
        put("a.txt", "fn alpha() {}\n", "Add alpha");
        put("*.txt", "star\n", "Add star");
        put("b.txt", "beta\n", "Add beta");
        put("a.txt", "fn alpha() {}\nfn gamma() {}\n", "Add gamma");
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "Tidy", "-m", "Mentions Needle in the body"]);
        // Two loaded commits: the search still covers everything.
        let d = load_all(&dir, 2, View::default()).unwrap();
        assert!(d.truncated);
        assert_eq!(search(&dir, &d, "path:a.txt"), ["Add gamma", "Add alpha"]);
        assert_eq!(search(&dir, &d, "path:*.txt"), ["Add star"], "pathspecs are literal");
        assert_eq!(search(&dir, &d, "code:gamma()"), ["Add gamma"]);
        assert_eq!(search(&dir, &d, "code:GAMMA"), ["Add gamma"], "case-insensitive by default");
        assert_eq!(search(&dir, &d, "path:a.txt alpha"), ["Add alpha"], "free text narrows operators");
        assert_eq!(search(&dir, &d, "needle"), ["Tidy"]);
        assert_eq!(search(&dir, &d, "author:t path:b.txt"), ["Add beta"]);
        assert!(search(&dir, &d, "author:nobody").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The filtered graph draws hit-to-hit lanes only, but its commits keep their real
    /// parents (menus pick merge flags from them) and it follows status-only reloads.
    #[test]
    fn filtered_graph_keeps_parents_and_status() {
        let dir = scratch("search-filter");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        sh(&dir, &["switch", "-q", "-c", "side"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "on side"]);
        sh(&dir, &["switch", "-q", "main"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "on main"]);
        sh(&dir, &["merge", "-q", "--no-ff", "-m", "Merge needle", "side"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let mut s = finished(&dir, &d, "needle", true);
        let f = s.filtered.clone().unwrap();
        assert_eq!(f.log[0].parents.len(), 2, "still a merge");
        assert!(f.graph.edges.is_empty(), "no lanes to commits that are not shown");
        // A status-only reload keeps the log Arc.
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        sh(&dir, &["add", "new.txt"]);
        let d2 = RepoData::build(d.log.clone(), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), git::status(&dir).unwrap(), d.view.clone());
        s.poll(&egui::Context::default(), &d2);
        assert_eq!(s.filtered.as_ref().unwrap().status.staged.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While hits stream in, the filtered graph is rebuilt when their number doubles (or
    /// every so often), not per hit; the end of the stream brings it up to date.
    #[test]
    fn filtered_graph_rebuilds_throttled() {
        let dir = scratch("search-throttle");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for i in 0..5 {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
        }
        let d = load_all(&dir, 100, View::default()).unwrap();
        let ctx = egui::Context::default();
        let mut s = Search::open(dir.clone());
        s.poll(&ctx, &d);
        let (tx, rx) = channel();
        (s.rx, s.filter) = (Some(rx), true);
        let rows = |s: &Search| s.filtered.as_ref().map_or(0, |f| f.log.len());
        let mut sent = 0;
        for want in [1, 2, 2, 4] {
            tx.send(Ok(d.log[sent].clone())).unwrap();
            sent += 1;
            s.poll(&ctx, &d);
            assert_eq!(rows(&s), want, "after {sent} hits");
        }
        tx.send(Ok(d.log[sent].clone())).unwrap();
        s.poll(&ctx, &d);
        assert_eq!(rows(&s), 4);
        drop(tx);
        s.poll(&ctx, &d);
        assert_eq!(rows(&s), 5, "the last hits once the stream ends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unborn HEAD (orphan branch) next to other branches still searches them.
    #[test]
    fn searches_from_an_orphan_branch() {
        let dir = scratch("search-orphan");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "On main"]);
        sh(&dir, &["switch", "-q", "--orphan", "pages"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!(search(&dir, &d, "main"), ["On main"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
