//! Connection doctor for one remote: a never-prompting `ls-remote`, its failure
//! classified with a fix hint, and the credential / proxy settings git uses for the URL.

use super::Task;
use crate::git;
use std::path::Path;

pub struct Report {
    /// The remote URL with any credentials hidden (shown and copied).
    pub url: String,
    /// `None`: connected. Otherwise (problem, fix hint) from `git::diagnose`.
    pub problem: Option<(&'static str, &'static str)>,
    /// git's output (stderr on failure).
    pub output: String,
    /// (setting, value) pairs git will use for this URL, with where each comes from.
    pub settings: Vec<(String, String)>,
}

pub struct Doctor {
    pub remote: String,
    pub report: Option<Report>,
    task: Option<Task<Report>>,
}

/// scp-like `git@host:path` or `ssh://` (not a local path or http).
fn is_ssh(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && url.split_once(':').is_some_and(|(h, _)| h.len() > 1 && !h.contains(['/', '\\'])))
}

pub fn check(dir: &Path, remote: &str) -> Report {
    let get = |args: &[&str]| git::run(dir, args).unwrap_or_default();
    let url = get(&["remote", "get-url", remote]).trim().to_owned();
    let res = git::ls_remote_quiet(dir, remote);
    let mut settings: Vec<(String, String)> = get(&["config", "--show-origin", "--get-all", "credential.helper"])
        .lines()
        .map(|l| l.split_once('\t').map_or(l.into(), |(origin, v)| format!("{v}   ({origin})")))
        .filter(|v: &String| !v.trim().is_empty())
        .map(|v| ("credential.helper".into(), v))
        .collect();
    if settings.is_empty() {
        settings.push(("credential.helper".into(), "none: git can't remember HTTPS passwords or tokens".into()));
    }
    if is_ssh(&url) {
        let ssh = std::env::var("GIT_SSH_COMMAND").ok().or_else(|| Some(get(&["config", "core.sshCommand"]).trim().to_owned()).filter(|s| !s.is_empty()));
        settings.push(("ssh command".into(), ssh.unwrap_or_else(|| "ssh (default)".into())));
    } else if url.starts_with("http") {
        // http.<url>.* resolved for this URL: proxy, sslCAInfo, sslBackend, extraHeader...
        for l in get(&["config", "--get-urlmatch", "http", &url]).lines() {
            let (k, v) = l.split_once(' ').unwrap_or((l, ""));
            let v = if k.eq_ignore_ascii_case("http.extraheader") { "(set, hidden)" } else { v };
            settings.push((k.into(), v.into()));
        }
    }
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy"] {
        if let Ok(v) = std::env::var(k).map(|v| v.trim().to_owned()).as_deref()
            && !v.is_empty()
        {
            settings.push((format!("${k}"), v.into()));
        }
    }
    // Tokens in the URL or a proxy's password must not reach the screen or a pasted report.
    for (_, v) in &mut settings {
        *v = git::redact(v);
    }
    let problem = res.as_ref().err().map(|e| git::diagnose(e, is_ssh(&url)));
    let output = res.unwrap_or_else(|e| e).trim().into();
    Report { url: git::redact(&url), problem, output, settings }
}

impl Doctor {
    pub fn open(ctx: &egui::Context, dir: &Path, remote: &str) -> Self {
        let (dir, r) = (dir.to_path_buf(), remote.to_owned());
        Doctor { remote: remote.into(), report: None, task: Some(Task::spawn(ctx, move || check(&dir, &r))) }
    }

    pub fn poll(&mut self) {
        if let Some(r) = Task::poll(&mut self.task) {
            self.report = Some(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{identity, scratch, sh};

    #[test]
    fn diagnoses_a_remote() {
        let dir = scratch("doctor");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["remote", "add", "gone", &dir.join("nope").to_string_lossy()]);
        sh(&dir, &["remote", "add", "self", &dir.to_string_lossy()]);
        sh(&dir, &["config", "credential.helper", "store"]);
        let r = check(&dir, "gone");
        assert_eq!(r.problem.map(|p| p.0), Some("Repository not found"));
        assert!(r.settings.iter().any(|(k, v)| k == "credential.helper" && v.starts_with("store ")), "{:?}", r.settings);
        assert!(check(&dir, "self").problem.is_none());
        assert!(is_ssh("git@ghe.corp:o/r.git") && is_ssh("ssh://git@ghe.corp/o/r"));
        assert!(!is_ssh("https://ghe.corp/o/r") && !is_ssh("/srv/r.git") && !is_ssh("C:\\r") && !is_ssh("./a:b"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
