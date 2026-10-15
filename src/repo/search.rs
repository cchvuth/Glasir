//! ⌘F commit search: query parser, a background `git log` whose hits stream in, and the filtered
//! graph (no drawing; the bar is `crate::ui::graph_view::search`).

use super::{RepoData, Task};
use crate::git::{self, Commit, Res};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// More hits than this stop the search (each is a loaded commit).
pub const MAX_HITS: usize = 10_000;
/// Up to this many hits the filtered graph is built on the UI thread.
const SYNC_HITS: usize = 500;

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
    /// (log, wip, run) the `hits` were computed for, and how many of `found` they cover.
    rows_key: (usize, bool, usize),
    rows_n: usize,
    /// The query parsed to something git can search (a half-typed `author:` does not).
    pub(crate) active: bool,
    /// A refs-only rerun is running: `found` is the previous run's until the new one delivers.
    stale: bool,
    /// (query, case) of the previous run.
    last: (String, bool),
    /// The hit to be current again once the rerun finds it.
    want: Option<String>,
    /// (run, data generation, hits) the filtered graph was built from, and when.
    filtered_key: (usize, u64, usize),
    filtered_at: std::time::Instant,
    building: Option<Task<((usize, u64, usize), RepoData)>>,
    /// (commit, tip) -> is the commit an ancestor of the tip; `None` while git is still asked.
    ancestry: Arc<Mutex<HashMap<(String, String), Option<bool>>>>,
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
            rows_n: 0,
            active: false,
            stale: false,
            last: Default::default(),
            want: None,
            filtered_key: Default::default(),
            filtered_at: std::time::Instant::now(),
            building: None,
            ancestry: Default::default(),
        }
    }

    /// Is `commit` an ancestor of `tip` (or `tip` itself)? For hits older than the loaded page,
    /// which the loaded graph can't judge: git answers off the UI thread, cached per pair.
    /// `None` until then (a failing git counts as "no").
    // ponytail: one git process per visible old hit; batch them if that ever shows up in a profile.
    pub(crate) fn is_ancestor(&self, ctx: &egui::Context, commit: &str, tip: &str) -> Option<bool> {
        let key = (commit.to_owned(), tip.to_owned());
        let mut m = self.ancestry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&v) = m.get(&key) {
            return v;
        }
        m.insert(key.clone(), None);
        let (cache, dir, ctx) = (self.ancestry.clone(), self.dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let yes = git::run(&dir, &["merge-base", "--is-ancestor", &key.0, &key.1]).is_ok();
            cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Some(yes));
            ctx.request_repaint();
        });
        None
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
            let r = rx.try_recv();
            if self.stale && !matches!(r, Err(TryRecvError::Empty)) {
                // The rerun's first word replaces the previous results.
                (self.found, self.cur, self.stale) = (vec![], 0, false);
                self.run += 1;
            }
            match r {
                Ok(Ok(c)) => {
                    if self.want.as_deref() == Some(c.id.as_str()) {
                        self.cur = self.found.len();
                    }
                    self.found.push(c)
                }
                Ok(Err(e)) => self.error = Some(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }
        if let Some((key, f)) = Task::poll(&mut self.building) {
            self.filtered = Some(Arc::new(f));
            self.filtered_key = key;
        }
        let fkey = (self.run, d.generation, self.found.len());
        if !self.filter || self.found.is_empty() {
            (self.filtered, self.building) = (None, None);
        } else if self.filtered.is_none() {
            // Turning the filter on (or a new query's first hit): a few hits show in the same
            // frame, many are built off the UI thread (the full graph shows until then).
            if self.found.len() <= SYNC_HITS {
                (self.filtered_key, self.filtered_at, self.building) = (fkey, std::time::Instant::now(), None);
                self.filtered = Some(Arc::new(self.filtered_data(d)()));
            } else if self.building.is_none() {
                self.filtered_at = std::time::Instant::now();
                let job = self.filtered_data(d);
                self.building = Some(Task::spawn(ctx, move || (fkey, job())));
            }
        } else if fkey != self.filtered_key && self.building.is_none() {
            // A rebuild is O(hits), off the UI thread: while they stream in, only when their
            // number doubled or a moment passed; the end of the stream brings it up to date.
            const EVERY: std::time::Duration = std::time::Duration::from_millis(250);
            if !self.running() || fkey.2 >= 2 * self.filtered_key.2 || self.filtered_at.elapsed() >= EVERY {
                self.filtered_at = std::time::Instant::now();
                let job = self.filtered_data(d);
                self.building = Some(Task::spawn(ctx, move || (fkey, job())));
            } else {
                ctx.request_repaint_after(EVERY);
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, d: &RepoData) {
        self.cancel.cancel();
        let q = parse(&self.key.0, self.case);
        self.active = !q.is_empty();
        // Only the refs moved (a fetch): the old hits stay shown until the rerun delivers.
        let rerun = self.active && !d.log.is_empty() && !self.found.is_empty() && self.key.0 == self.last.0 && self.case == self.last.1;
        self.last = (self.key.0.clone(), self.case);
        self.want = rerun.then(|| self.found.get(self.cur).map(|c| c.id.clone())).flatten();
        (self.rx, self.cancel, self.error, self.stale) = (None, Default::default(), None, rerun);
        self.run += 1;
        if !rerun {
            (self.cur, self.filtered, self.building) = (0, None, None);
            self.found.clear();
        }
        if !self.active || d.log.is_empty() {
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

    /// The history of just the hits, as a job for any thread. Lanes join hits only, but `log` keeps the
    /// real commits: menus read their parents.
    fn filtered_data(&self, d: &RepoData) -> impl FnOnce() -> RepoData + Send + 'static {
        let found = self.found.clone();
        let parts = (d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), d.status.clone(), d.view.clone());
        let more = (d.worktrees.clone(), d.submodules.clone(), d.lfs.clone(), d.config.clone(), d.author.clone(), d.bisect.clone());
        move || {
            let ids: std::collections::HashSet<&str> = found.iter().map(|c| c.id.as_str()).collect();
            let log = found.iter().map(|c| Commit { parents: c.parents.iter().filter(|p| ids.contains(p.as_str())).cloned().collect(), ..c.clone() }).collect();
            let (refs, stashes, remotes, git_dir, origin_head, status, view) = parts;
            let mut f = RepoData::build(Arc::new(log), refs, stashes, remotes, git_dir, origin_head, status, view);
            (f.worktrees, f.submodules, f.lfs, f.config, f.author, f.bisect) = more;
            f.log = Arc::new(found);
            f
        }
    }

    /// Recomputes `hits` for the graph being drawn (the full or the filtered history).
    pub(crate) fn rows(&mut self, d: &RepoData) {
        let key = (Arc::as_ptr(&d.log) as usize, d.wip, self.run);
        if key != self.rows_key || self.rows_n > self.found.len() {
            (self.rows_key, self.rows_n) = (key, 0);
            self.hits.clear();
        }
        // `found` only appends within a run: look up just the new hits.
        let new = self.found[self.rows_n..].iter().filter_map(|c| d.row_of.get(&c.id).copied());
        let before = self.hits.len();
        self.hits.extend(new);
        self.rows_n = self.found.len();
        if self.hits.len() != before {
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

    /// Is a hit reachable from the hovered ref, per the loaded graph? `reach`: the rows the ref reaches
    /// (`None`: its tip isn't loaded), `complete`: no unloaded parent in its ancestry, `row`: the hit's row
    /// (`None`: older than the loaded page). `None` result: ask git.
    pub(crate) fn judge(reach: Option<&[bool]>, complete: bool, row: Option<usize>) -> Option<bool> {
        match (reach, row) {
            (Some(v), Some(r)) => Some(v[r]),
            (Some(_), None) if complete => Some(false),
            _ => None,
        }
    }

    pub(crate) fn dims(&self, r: usize) -> bool {
        self.active && self.hits.binary_search(&r).is_err()
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
        crate::repo::testutil::wait_for("the search", || {
            s.poll(&ctx, d);
            !s.running() && s.building.is_none()
        });
        assert_eq!(s.error, None);
        s
    }

    /// Polls until the background rebuild of the filtered graph (if any) is in.
    fn settle(s: &mut Search, d: &RepoData) {
        let ctx = egui::Context::default();
        s.poll(&ctx, d);
        while s.building.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(2));
            s.poll(&ctx, d);
        }
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
        put("[ab].txt", "glob\n", "Add glob");
        put("b.txt", "beta\n", "Add beta");
        put("a.txt", "fn alpha() {}\nfn gamma() {}\n", "Add gamma");
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "Tidy", "-m", "Mentions Needle in the body"]);
        // Two loaded commits: the search still covers everything.
        let d = load_all(&dir, 2, View::default()).unwrap();
        assert!(d.truncated);
        assert_eq!(search(&dir, &d, "path:a.txt"), ["Add gamma", "Add alpha"]);
        assert_eq!(search(&dir, &d, "path:[ab].txt"), ["Add glob"], "pathspecs are literal");
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
        settle(&mut s, &d2);
        assert_eq!(s.filtered.as_ref().unwrap().status.staged.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The filtered graph's lanes skip the commits between hits, so it can't tell what a remote
    /// branch reaches: menus, links and the hover fade must ask the full history instead.
    #[test]
    fn filtered_graph_is_no_reachability_source() {
        let dir = scratch("search-reach");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for m in ["needle a", "other", "needle c"] {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", m]);
        }
        sh(&dir, &["remote", "add", "origin", "https://github.com/o/r.git"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let f = finished(&dir, &d, "needle", true).filtered.clone().unwrap();
        let a = f.log[1].id.clone();
        assert!(d.on_remote_now(&a, "origin"));
        assert!(!f.on_remote_now(&a, "origin"), "if this holds now, the menus could take the filtered graph again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hit older than the loaded page is only in the filtered graph: menus take its parent from there.
    #[test]
    fn filtered_graph_holds_hits_beyond_the_page() {
        let dir = scratch("search-page");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for m in ["needle a", "b", "needle c", "d", "needle e", "f"] {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", m]);
        }
        let d = load_all(&dir, 3, View::default()).unwrap();
        let f = finished(&dir, &d, "needle", true).filtered.clone().unwrap();
        let old = f.log.iter().find(|c| c.summary == "needle a").unwrap().id.clone();
        assert!(!d.row_of.contains_key(&old));
        assert!(d.first_parent(&old).or_else(|| f.first_parent(&old)).is_none(), "the root has no parent");
        let c = f.log.iter().find(|c| c.summary == "needle c").unwrap().id.clone();
        assert!(!d.row_of.contains_key(&c) && f.first_parent(&c).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While hits stream in, the filtered graph is rebuilt in the background when their number
    /// doubles (or every so often), not per hit; the end of the stream brings it up to date.
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
            if sent == 2 {
                assert!(s.building.is_some() && rows(&s) == 1, "rebuilt off the UI thread; the old graph stays meanwhile");
            }
            settle(&mut s, &d);
            assert_eq!(rows(&s), want, "after {sent} hits");
        }
        tx.send(Ok(d.log[sent].clone())).unwrap();
        settle(&mut s, &d);
        assert_eq!(rows(&s), 4);
        drop(tx);
        settle(&mut s, &d);
        assert_eq!(rows(&s), 5, "the last hits once the stream ends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn judges_hover_reachability() {
        let v = [false, true];
        assert_eq!(Search::judge(Some(&v), false, Some(1)), Some(true));
        assert_eq!(Search::judge(Some(&v), false, Some(0)), Some(false));
        assert_eq!(Search::judge(Some(&v), true, None), Some(false), "a complete ancestry holds no older hit");
        assert_eq!(Search::judge(Some(&v), false, None), None, "an open ancestry may");
        assert_eq!(Search::judge(None, true, Some(1)), None, "an unloaded tip is git's to judge");
    }

    /// Hits older than the loaded page are judged by git, off the thread asking, and cached.
    #[test]
    fn asks_git_for_old_ancestry() {
        let dir = scratch("search-ancestry");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for m in ["a", "b"] {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", m]);
        }
        sh(&dir, &["switch", "-q", "-c", "side", "HEAD~"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "c"]);
        let rev = |r: &str| sh(&dir, &["rev-parse", r]).trim().to_owned();
        let (a, main, side) = (rev("main~"), rev("main"), rev("side"));
        let s = Search::open(dir.clone());
        let ctx = egui::Context::default();
        assert_eq!(s.is_ancestor(&ctx, &a, &main), None, "asked, not yet answered");
        let ask = |c: &str, t: &str| loop {
            if let Some(v) = s.is_ancestor(&ctx, c, t) {
                return v;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(ask(&a, &main));
        assert!(ask(&a, &side));
        assert!(!ask(&main, &side));
        assert_eq!(s.is_ancestor(&ctx, &main, &side), Some(false), "cached");
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

    #[test]
    fn half_typed_operator_dims_nothing() {
        let dir = scratch("search-half");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let mut s = Search::open(dir.clone());
        s.query = "author:".into();
        s.poll(&egui::Context::default(), &d);
        assert!(!s.dims(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ref_move_keeps_results_on_screen() {
        let dir = scratch("search-refmove");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for m in ["needle 1", "other", "needle 2"] {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", m]);
        }
        let d = load_all(&dir, 100, View::default()).unwrap();
        let mut s = finished(&dir, &d, "needle", true);
        s.cur = 1;
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let d2 = load_all(&dir, 100, View::default()).unwrap();
        s.poll(&egui::Context::default(), &d2);
        assert!(s.filtered.is_some() && s.found.len() == 2 && s.cur == 1, "old results stay until the new run delivers");
        settle(&mut s, &d2);
        crate::repo::testutil::wait_for("the rerun", || {
            s.poll(&egui::Context::default(), &d2);
            !s.running()
        });
        assert_eq!((s.found.len(), s.cur), (2, 1), "same current hit by SHA");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rerun_delivered_at_once_replaces_the_hit_rows() {
        let dir = scratch("search-rerun-rows");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for m in ["needle 1", "other", "needle 2", "needle 3"] {
            sh(&dir, &["commit", "-q", "--allow-empty", "-m", m]);
        }
        let d = load_all(&dir, 100, View::default()).unwrap();
        let mut s = finished(&dir, &d, "needle", false);
        s.rows(&d);
        assert_eq!(s.hits, [0, 1, 3]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "needle 4"]);
        let d2 = load_all(&dir, 100, View::default()).unwrap();
        let ctx = egui::Context::default();
        s.poll(&ctx, &d2);
        assert!(s.stale, "the rerun answered before its first poll");
        s.rows(&d2);
        assert_eq!(s.hits, [1, 2, 4], "the old results stay on the new graph meanwhile");
        let (tx, rx) = channel();
        s.rx = Some(rx);
        for c in d2.log.iter().filter(|c| c.summary.starts_with("needle")) {
            tx.send(Ok(c.clone())).unwrap();
        }
        drop(tx);
        s.poll(&ctx, &d2);
        s.rows(&d2);
        assert_eq!(s.hits, [0, 1, 2, 4]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_hits_build_the_filter_off_thread() {
        let dir = scratch("search-bigfilter");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let mut s = Search::open(dir.clone());
        s.poll(&egui::Context::default(), &d);
        (s.found, s.filter) = (vec![d.log[0].clone(); SYNC_HITS + 1], true);
        s.poll(&egui::Context::default(), &d);
        assert!(s.filtered.is_none() && s.building.is_some());
        settle(&mut s, &d);
        assert!(s.filtered.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
