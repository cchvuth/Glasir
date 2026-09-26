//! GitHub / GHE profile pictures for commit authors, looked up from the origin host only.
//! Noreply emails map straight to an avatar URL; others go through one of their commits
//! (REST API, with the token git's credential helper has). Cached in memory and on disk for a week.

use super::RepoData;
use crate::git;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Requested size in pixels (2x the inspector's 30 pt circle).
const PX: u32 = 64;
const TTL: u64 = 7 * 86400;
const WORKERS: usize = 4;
/// API lookups per load; cached and noreply authors don't count.
const MAX_LOOKUPS: usize = 50;
/// Authors queued per load, most commits first; the next load takes the next ones.
const MAX_AUTHORS: usize = 200;

// Off in tests: no network, no writes to the real cache.
static ENABLED: AtomicBool = AtomicBool::new(!cfg!(test));
/// Hosts that answered 401/403/429: no more API calls to them this session.
static BLOCKED: Mutex<Vec<String>> = Mutex::new(vec![]);
/// `host/owner/repo[@sha]` the API answered 404 for, with the time: skipped for `MISS_TTL`
/// so each reload doesn't repeat them (a private repo 404s for every commit).
static MISSING: Mutex<Vec<(String, u64)>> = Mutex::new(vec![]);
const MISS_TTL: u64 = 3600;
/// `host/owner/repo` whose API page answered, so commit 404s there are just unpushed commits.
static REACHABLE: Mutex<Vec<String>> = Mutex::new(vec![]);
/// Serializes read-merge-write of the on-disk index.
static INDEX: Mutex<()> = Mutex::new(());

/// Settings → "Show GitHub avatars", mirrored every frame; off = no avatar network at all.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Per tab: email -> picture (`None` while loading or when there is none).
#[derive(Default)]
pub struct Avatars {
    tex: Arc<Mutex<HashMap<String, Option<egui::TextureHandle>>>>,
    /// The log (`Arc` address) authors were last queued for.
    seen: usize,
}

impl Avatars {
    pub fn get(&self, email: &str) -> Option<egui::TextureHandle> {
        if !enabled() {
            return None;
        }
        self.tex.lock().unwrap().get(email).cloned().flatten()
    }

    /// Queues the authors of a newly loaded log (or of the current one, once enabled).
    pub(super) fn sync(&mut self, ctx: &egui::Context, dir: &Path, d: &RepoData) {
        let key = Arc::as_ptr(&d.log) as usize;
        if !enabled() {
            // The results of a batch cut short were dropped; queue them again once enabled.
            self.seen = 0;
            return;
        }
        if key == self.seen {
            return;
        }
        self.seen = key;
        let Some((host, owner, repo)) = d.remotes.iter().find(|r| r.0 == "origin").and_then(|r| github_repo(&r.1)) else { return };
        let (tex, ctx, log) = (self.tex.clone(), ctx.clone(), d.log.clone());
        let c = Lookup::new(host, owner, repo, dir.to_path_buf(), eframe::storage_dir("Glasir").map(|d| d.join("avatars")));
        std::thread::spawn(move || {
            // The scan over the whole log stays off the UI thread.
            // email -> (commits, oldest commit): the oldest is the likeliest to be on the remote.
            let mut by: HashMap<&str, (usize, &str)> = HashMap::new();
            for c in log.iter().filter(|c| !c.email.is_empty()) {
                let e = by.entry(&c.email).or_default();
                *e = (e.0 + 1, &c.id);
            }
            let mut jobs: Vec<_> = {
                let tex = tex.lock().unwrap();
                by.into_iter().filter(|(e, _)| !tex.contains_key(*e)).collect()
            };
            jobs.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
            jobs.truncate(MAX_AUTHORS);
            let jobs: Vec<(String, String)> = jobs.into_iter().map(|(e, (_, id))| (e.to_owned(), id.to_owned())).collect();
            let mut t = tex.lock().unwrap();
            for (e, _) in &jobs {
                t.insert(e.clone(), None);
            }
            drop(t);
            let queue = Mutex::new(jobs.into_iter());
            std::thread::scope(|s| {
                for _ in 0..WORKERS {
                    s.spawn(|| loop {
                        let next = queue.lock().unwrap().next();
                        let Some((email, sha)) = next else { break };
                        if !enabled() {
                            tex.lock().unwrap().remove(&email);
                            continue;
                        }
                        match c.resolve(&email, &sha).filter(|_| enabled()) {
                            Some(bytes) => {
                                let t = bytes.and_then(|b| texture(&ctx, &email, &b));
                                let got = t.is_some();
                                tex.lock().unwrap().insert(email, t);
                                if got {
                                    ctx.request_repaint();
                                }
                            }
                            // Retried on a later load.
                            None => drop(tex.lock().unwrap().remove(&email)),
                        }
                    });
                }
            });
            c.save();
        });
    }
}

fn texture(ctx: &egui::Context, email: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.thumbnail(PX, PX).to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    Some(ctx.load_texture(format!("avatar {email}"), color, egui::TextureOptions::LINEAR))
}

/// `(host, owner, repo)` of an origin on github.com or GitHub Enterprise; never
/// another forge or a local path.
fn github_repo(origin: &str) -> Option<(String, String, String)> {
    let host = git::host(origin)?;
    let web = git::web_url(origin)?;
    let mut path = web.strip_prefix("https://")?.split('/').skip(1);
    let (owner, repo) = (path.next().filter(|s| !s.is_empty())?, path.next().filter(|s| !s.is_empty())?);
    let other = ["gitlab", "bitbucket", "azure.com", "visualstudio.com", "codeberg", "gitea", "sr.ht", "gitee"];
    let ok = host.contains('.') && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b)) && !other.iter().any(|o| host.contains(o));
    ok.then(|| (host, git::url_path(owner), git::url_path(repo)))
}

fn api_base(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".into()
    } else if host.ends_with(".ghe.com") {
        // GHE Cloud with data residency: the API has its own subdomain.
        format!("https://api.{host}")
    } else {
        format!("https://{host}/api/v3")
    }
}

/// `<id>+<login>@users.noreply.<host>` -> its avatar URL, no API call needed.
fn noreply_url(email: &str, host: &str) -> Option<String> {
    let (user, domain) = email.rsplit_once('@')?;
    let id = user.split_once('+')?.0;
    let ok = domain.eq_ignore_ascii_case(&format!("users.noreply.{host}")) && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| if host == "github.com" { format!("https://avatars.githubusercontent.com/u/{id}?s={PX}") } else { format!("https://{host}/avatars/u/{id}?s={PX}") })
}

fn sized(url: &str) -> String {
    format!("{url}{}s={PX}", if url.contains('?') { "&" } else { "?" })
}

/// Pictures come only from GitHub's avatar host, or from the GHE host and its subdomains.
fn image_ok(url: &str, host: &str) -> bool {
    let Some(h) = url.strip_prefix("https://").and_then(|r| r.split(['/', '?', '#']).next()) else { return false };
    !h.contains(['@', ':']) && if host == "github.com" { h == "avatars.githubusercontent.com" } else { h == host || h.ends_with(&format!(".{host}")) }
}

/// `avatar_url` of the first commit's author in a commit-list reply.
fn avatar_of(body: &[u8], host: &str) -> Option<Option<String>> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let c = v.as_array()?.first()?;
    Some(c["author"]["avatar_url"].as_str().map(sized).filter(|u| image_ok(u, host)))
}

fn is_image(bytes: &[u8]) -> bool {
    image::guess_format(bytes).is_ok()
}

/// A 401 to a request that carried the token: the token is stale, retry without it.
fn retry_bare(status: u16, had_token: bool) -> bool {
    status == 401 && had_token
}

/// Only a tokenless request failing says the host refuses us (or rate-limits us).
fn blocks_host(status: u16, had_token: bool) -> bool {
    matches!(status, 401 | 403 | 429) && !had_token
}

fn now_fresh(at: u64, now: u64) -> bool {
    now.saturating_sub(at) < MISS_TTL
}

fn fresh(at: u64, now: u64) -> bool {
    now.saturating_sub(at) < TTL
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    /// `None`: the author has no GitHub account (initials).
    url: Option<String>,
    at: u64,
}

fn load_index(cache: &Path) -> HashMap<String, Entry> {
    std::fs::read(cache.join("index.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// One load's lookups against one host, with the disk cache as it was at the start.
struct Lookup {
    host: String,
    owner: String,
    repo: String,
    dir: PathBuf,
    cache: Option<PathBuf>,
    index: HashMap<String, Entry>,
    updates: Mutex<Vec<(String, Entry)>>,
    now: u64,
    lookups: AtomicUsize,
    token: OnceLock<Option<String>>,
    /// The token got a 401: leave it out from now on.
    bad_token: AtomicBool,
    agent: ureq::Agent,
}

impl Lookup {
    fn new(host: String, owner: String, repo: String, dir: PathBuf, cache: Option<PathBuf>) -> Self {
        let agent = ureq::Agent::config_builder()
            .tls_config(crate::ai::tls())
            .timeout_global(Some(Duration::from_secs(20)))
            // Redirects could carry the token to another host; images get one checked hop.
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let index = cache.as_deref().map(load_index).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Lookup { host, owner, repo, dir, cache, index, updates: Mutex::new(vec![]), now, lookups: AtomicUsize::new(0), token: OnceLock::new(), bad_token: AtomicBool::new(false), agent }
    }

    /// `Some(None)`: no picture (initials); `None`: unknown for now, retry on a later load.
    fn resolve(&self, email: &str, sha: &str) -> Option<Option<Vec<u8>>> {
        let key = format!("{} {}", self.host, email.to_lowercase());
        let cached = self.index.get(&key);
        let stale = || cached.and_then(|e| self.read(e.url.as_deref()?)).map(Some);
        let url = match cached.filter(|e| fresh(e.at, self.now)) {
            Some(Entry { url: None, .. }) => return Some(None),
            Some(Entry { url: Some(u), .. }) => match self.read(u) {
                Some(b) => return Some(Some(b)),
                None => u.clone(),
            },
            None => match noreply_url(email, &self.host) {
                Some(u) => u,
                None => match self.lookup(sha) {
                    Some(Some(u)) => u,
                    Some(None) => {
                        self.record(key, None);
                        return Some(None);
                    }
                    None => return stale(),
                },
            },
        };
        match self.download(&url) {
            Some(b) => {
                if let Some(c) = &self.cache {
                    let _ = std::fs::create_dir_all(c).and_then(|_| std::fs::write(c.join(file_name(&url)), &b));
                }
                self.record(key, Some(url));
                Some(Some(b))
            }
            None => stale(),
        }
    }

    fn read(&self, url: &str) -> Option<Vec<u8>> {
        std::fs::read(self.cache.as_ref()?.join(file_name(url))).ok().filter(|b| is_image(b))
    }

    fn token(&self) -> Option<&str> {
        if self.bad_token.load(Ordering::Relaxed) {
            return None;
        }
        self.token.get_or_init(|| git::credential_token(&self.dir, &self.host)).as_deref()
    }

    fn skipped(&self, repo: &str, sha: &str) -> bool {
        let m = MISSING.lock().unwrap();
        m.iter().any(|(k, at)| now_fresh(*at, self.now) && (k == repo || k.strip_prefix(repo).and_then(|r| r.strip_prefix('@')) == Some(sha)))
    }

    /// Remembers a commit as missing; `gone`: the whole repo is unreachable.
    fn miss(&self, repo: &str, sha: &str, gone: bool) {
        let mut m = MISSING.lock().unwrap();
        m.retain(|(_, at)| now_fresh(*at, self.now));
        m.push((format!("{repo}@{sha}"), self.now));
        if gone {
            m.push((repo.to_owned(), self.now));
        }
    }

    /// An API GET with our token if we have one; a 401 marks the token bad and retries
    /// once without it. Also says whether the answer came from a request that had the token.
    fn api_get(&self, url: &str) -> Option<((u16, Vec<u8>, String), bool)> {
        // Decided by this request's own token, not by `bad_token`, which other workers may have set.
        let tok = self.token();
        let mut got = self.get(url, tok)?;
        let mut had = tok.is_some();
        if retry_bare(got.0, had) {
            // A stale token fails even on public repos; they answer without it.
            self.bad_token.store(true, Ordering::Relaxed);
            got = self.get(url, None)?;
            had = false;
        }
        Some((got, had))
    }

    /// Whether the repo's own API page answers (checked once per session): a public repo whose
    /// commits aren't pushed still does; a missing or private one doesn't.
    fn reachable(&self, repo: &str) -> bool {
        if REACHABLE.lock().unwrap().iter().any(|r| r == repo) {
            return true;
        }
        let url = format!("{}/repos/{}/{}", api_base(&self.host), self.owner, self.repo);
        match self.api_get(&url) {
            Some(((404 | 401 | 403, ..), _)) => false,
            // Unknown (network error, 5xx) counts as reachable: don't skip on a guess.
            _ => {
                REACHABLE.lock().unwrap().push(repo.to_owned());
                true
            }
        }
    }

    fn record(&self, key: String, url: Option<String>) {
        self.updates.lock().unwrap().push((key, Entry { url, at: self.now }));
    }

    /// The `avatar_url` of the author of `sha` (`Some(None)`: not a GitHub user).
    fn lookup(&self, sha: &str) -> Option<Option<String>> {
        let repo = format!("{}/{}/{}", self.host, self.owner, self.repo);
        if BLOCKED.lock().unwrap().contains(&self.host) || self.skipped(&repo, sha) || self.lookups.fetch_add(1, Ordering::Relaxed) >= MAX_LOOKUPS {
            return None;
        }
        // The commit list (one entry, no diff) carries the same author as the commit itself.
        // github.com's API lives on api.github.com; a GHE token goes to its own host only.
        let url = format!("{}/repos/{}/{}/commits?sha={sha}&per_page=1", api_base(&self.host), self.owner, self.repo);
        let (got, had_token) = self.api_get(&url)?;
        match got.0 {
            200 => avatar_of(&got.1, &self.host),
            s if blocks_host(s, had_token) => {
                BLOCKED.lock().unwrap().push(self.host.clone());
                None
            }
            // 404 / 422: that commit isn't on the remote (yet), or the repo is private to us.
            s @ (404 | 422) => {
                let gone = s == 404 && !self.reachable(&repo);
                self.miss(&repo, sha, gone);
                None
            }
            _ => None,
        }
    }

    fn download(&self, url: &str) -> Option<Vec<u8>> {
        let mut url = url.to_owned();
        for _ in 0..2 {
            if !image_ok(&url, &self.host) {
                return None;
            }
            // Private-mode GHE serves pictures to signed-in users only.
            let own = url.strip_prefix("https://").is_some_and(|r| r.split('/').next() == Some(self.host.as_str()));
            match self.get(&url, if own { self.token() } else { None })? {
                // A redirect to a login page is a 200 too.
                (200, body, _) => return is_image(&body).then_some(body),
                (301 | 302 | 307 | 308, _, loc) => url = loc,
                _ => return None,
            }
        }
        None
    }

    fn get(&self, url: &str, token: Option<&str>) -> Option<(u16, Vec<u8>, String)> {
        let mut r = self.agent.get(url).header("user-agent", "Glasir").header("accept", "application/vnd.github+json, image/*");
        if let Some(t) = token {
            r = r.header("authorization", format!("Bearer {t}"));
        }
        let resp = r.call().ok()?;
        let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
        Some((resp.status().as_u16(), resp.into_body().read_to_vec().ok()?, loc))
    }

    fn save(&self) {
        let (Some(cache), updates) = (&self.cache, std::mem::take(&mut *self.updates.lock().unwrap())) else { return };
        if updates.is_empty() {
            return;
        }
        let _g = INDEX.lock().unwrap();
        let mut index = load_index(cache);
        index.extend(updates);
        index.retain(|_, e| fresh(e.at, self.now));
        let _ = std::fs::create_dir_all(cache).and_then(|_| std::fs::write(cache.join("index.json"), serde_json::to_vec(&index).unwrap_or_default()));
    }
}

fn file_name(url: &str) -> String {
    url.trim_start_matches("https://").replace(|c: char| !c.is_ascii_alphanumeric(), "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noreply_emails() {
        assert_eq!(noreply_url("583231+octocat@users.noreply.github.com", "github.com").as_deref(), Some("https://avatars.githubusercontent.com/u/583231?s=64"));
        assert_eq!(noreply_url("7+me@users.noreply.ghe.corp.example", "ghe.corp.example").as_deref(), Some("https://ghe.corp.example/avatars/u/7?s=64"));
        for (email, host) in [
            ("octocat@users.noreply.github.com", "github.com"),
            ("583231+octocat@users.noreply.github.com", "ghe.corp.example"),
            ("x+octocat@users.noreply.github.com", "github.com"),
            ("1+me@gmail.com", "github.com"),
            ("no-at-sign", "github.com"),
        ] {
            assert_eq!(noreply_url(email, host), None, "{email}");
        }
    }

    #[test]
    fn urls_for_github_and_ghe() {
        let r = |s: &str| github_repo(s).map(|(h, o, r)| format!("{h} {o} {r}"));
        assert_eq!(r("git@github.com:octo/hello.git").as_deref(), Some("github.com octo hello"));
        assert_eq!(r("ssh://git@GHE.corp.example:7999/team/app.git").as_deref(), Some("ghe.corp.example team app"));
        assert_eq!(r("https://me@ghe.corp.example/team/app").as_deref(), Some("ghe.corp.example team app"));
        for none in ["git@gitlab.com:a/b.git", "https://bitbucket.org/a/b", "http://local:3000/a/b", "/srv/repo.git", "https://github.com/onlyowner"] {
            assert_eq!(r(none), None, "{none}");
        }
        assert_eq!(api_base("github.com"), "https://api.github.com");
        assert_eq!(api_base("ghe.corp.example"), "https://ghe.corp.example/api/v3");
        assert_eq!(api_base("octocorp.ghe.com"), "https://api.octocorp.ghe.com");
        assert_eq!(sized("https://avatars.githubusercontent.com/u/1?v=4"), "https://avatars.githubusercontent.com/u/1?v=4&s=64");
        assert_eq!(sized("https://ghe.corp/avatars/u/2"), "https://ghe.corp/avatars/u/2?s=64");
        assert!(image_ok("https://avatars.githubusercontent.com/u/1?s=64", "github.com"));
        assert!(image_ok("https://avatars.ghe.corp/u/1", "ghe.corp") && image_ok("https://ghe.corp/avatars/u/1", "ghe.corp"));
        for bad in ["https://evil.example/u/1", "http://avatars.githubusercontent.com/u/1", "https://x@avatars.githubusercontent.com/u/1", "https://avatars.githubusercontent.com.evil/u/1"] {
            assert!(!image_ok(bad, "github.com"), "{bad}");
        }
        assert!(!image_ok("https://ghe.corp.evil/u/1", "ghe.corp") && !image_ok("https://notghe.corp/u/1", "ghe.corp"));
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn commit_list_reply() {
        let a = |s: &str| avatar_of(s.as_bytes(), "github.com");
        assert_eq!(a(r#"[{"author":{"avatar_url":"https://avatars.githubusercontent.com/u/1?v=4"}}]"#), Some(Some("https://avatars.githubusercontent.com/u/1?v=4&s=64".into())));
        assert_eq!(a(r#"[{"author":null}]"#), Some(None));
        assert_eq!(a(r#"[{"author":{"avatar_url":"https://evil.example/a.png"}}]"#), Some(None));
        assert_eq!((a("[]"), a("{}"), a("<html>")), (None, None, None));
    }

    #[test]
    fn html_is_not_a_picture() {
        assert!(is_image(PNG) && !is_image(b"<!doctype html><title>Sign in</title>") && !is_image(b""));
    }

    /// A 404 skips that commit; the whole repo only when it is unreachable.
    #[test]
    fn misses_are_remembered() {
        let l = Lookup::new("miss.example".into(), "o".into(), "r".into(), PathBuf::new(), None);
        let repo = "miss.example/o/r";
        assert!(!l.skipped(repo, "a"));
        l.miss(repo, "a", false);
        l.miss(repo, "b", false);
        l.miss(repo, "c", false);
        assert!(l.skipped(repo, "a") && !l.skipped(repo, "zzz") && !l.skipped("miss.example/o/rr", "a"));
        l.miss(repo, "d", true);
        assert!(l.skipped(repo, "zzz"));
        let mut later = Lookup::new("miss.example".into(), "o".into(), "r".into(), PathBuf::new(), None);
        later.now = l.now + MISS_TTL;
        assert!(!later.skipped(repo, "a") && !later.skipped(repo, "zzz"));
    }

    #[test]
    fn stale_token_decisions() {
        assert!(retry_bare(401, true) && !retry_bare(401, false) && !retry_bare(403, true));
        for s in [401, 403, 429] {
            assert!(blocks_host(s, false) && !blocks_host(s, true), "{s}");
        }
        assert!(!blocks_host(404, false) && !blocks_host(200, false));
    }

    /// Fresh entries come from disk without the network; expired ones are refreshed,
    /// keeping the old picture when that can't happen (here: lookups used up).
    #[test]
    fn cache_expiry() {
        assert!(fresh(1000, 1000 + TTL - 1) && !fresh(1000, 1000 + TTL) && fresh(1000, 0));
        let cache = std::env::temp_dir().join(format!("glasir-avatars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let (pic, now) = ("https://avatars.githubusercontent.com/u/1?s=64", 10 * TTL);
        std::fs::write(cache.join(file_name(pic)), PNG).unwrap();
        let e = |url: Option<&str>, at| Entry { url: url.map(Into::into), at };
        let index: HashMap<_, _> = [
            ("github.com a@x.org".to_owned(), e(Some(pic), now - 60)),
            ("github.com none@x.org".to_owned(), e(None, now - 60)),
            ("github.com old@x.org".to_owned(), e(Some(pic), now - TTL - 1)),
        ]
        .into();
        std::fs::write(cache.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
        let mut l = Lookup::new("github.com".into(), "o".into(), "r".into(), cache.clone(), Some(cache.clone()));
        l.now = now;
        l.lookups.store(MAX_LOOKUPS, Ordering::Relaxed);
        assert_eq!(l.resolve("A@x.org", "abc"), Some(Some(PNG.to_vec())));
        assert_eq!(l.resolve("none@x.org", "abc"), Some(None));
        assert_eq!(l.resolve("old@x.org", "abc"), Some(Some(PNG.to_vec())));
        assert_eq!(l.resolve("new@x.org", "abc"), None);
        l.record("github.com new@x.org".into(), None);
        l.save();
        let saved = load_index(&cache);
        assert!(saved.contains_key("github.com new@x.org") && saved.contains_key("github.com a@x.org") && !saved.contains_key("github.com old@x.org"));
        let _ = std::fs::remove_dir_all(&cache);
    }
}
