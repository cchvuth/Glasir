//! The AI features: what each [`Purpose`] sends (git context + prompt), and
//! [`Job`], a completion streaming on a background thread into the UI.

use super::client::{Config, Ev, Provider, complete, need_token};
use crate::git::{self, Res};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

/// Diffs above this many bytes are refused with a message rather than truncated.
const MAX_CONTEXT: usize = 1_500_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    /// `amend`: the message for HEAD's changes plus the staged ones.
    CommitMessage { amend: bool },
    /// What the uncommitted changes (staged, unstaged, untracked) do.
    ExplainWip,
    /// Commit Composer: split all uncommitted changes into a commit plan (JSON).
    /// The draft is the numbered change units (`repo::compose`).
    Compose,
    Explain(String),
    PullRequest { branch: String, base: String },
    StashMessage,
    Resolve(String),
    /// What changed between two revisions (branch vs base, or a commit range).
    ExplainRange { from: String, to: String },
}

impl Purpose {
    pub fn title(&self) -> String {
        match self {
            Purpose::CommitMessage { .. } => "Commit message".into(),
            Purpose::ExplainWip => "Working changes".into(),
            Purpose::Compose => "Compose commits".into(),
            Purpose::Explain(id) => format!("Explain {}", &id[..id.len().min(7)]),
            Purpose::PullRequest { branch, .. } => format!("Pull request for {branch}"),
            Purpose::StashMessage => "Stash".into(),
            Purpose::Resolve(p) => format!("Resolve {p}"),
            Purpose::ExplainRange { from, to } => format!("Changes in {to} since {from}"),
        }
    }
}

pub struct Job {
    pub purpose: Purpose,
    pub text: String,
    pub done: Option<Res<()>>,
    /// The text the prompt was built from (Resolve: the file as sent).
    pub draft: String,
    rx: Receiver<Ev>,
}

impl Job {
    pub fn running(&self) -> bool {
        self.done.is_none()
    }

    /// Drains streamed text; returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            changed = true;
            match ev {
                Ev::Text(t) => self.text.push_str(&t),
                Ev::Done(r) => {
                    if r.is_err() {
                        self.text.clear(); // refusals and errors: discard partial output
                    }
                    self.done = Some(r);
                }
            }
        }
        changed
    }
}

/// Lockfiles add cost but no meaning; they still appear in `--stat`.
const NOISE: [&str; 7] = [
    ":(exclude,glob)**/package-lock.json",
    ":(exclude,glob)**/yarn.lock",
    ":(exclude,glob)**/pnpm-lock.yaml",
    ":(exclude,glob)**/Cargo.lock",
    ":(exclude,glob)**/poetry.lock",
    ":(exclude,glob)**/go.sum",
    ":(exclude,glob)**/*.min.js",
];

fn diff_with_stat(dir: &Path, range: &[&str]) -> Res<String> {
    let mut stat = vec!["diff", "--no-color", "--no-ext-diff", "--stat=200"];
    stat.extend_from_slice(range);
    let mut patch = vec!["diff", "--no-color", "--no-ext-diff", "-M"];
    patch.extend_from_slice(range);
    patch.push("--");
    patch.push(".");
    patch.extend_from_slice(&NOISE);
    let out = format!("{}\n{}", git::run(dir, &stat)?, git::run(dir, &patch)?);
    if out.len() > MAX_CONTEXT {
        return Err(format!(
            "These changes are too large to send to {} ({} KB, limit {} KB). Narrow them down (e.g. stage fewer files).",
            super::name(),
            out.len() / 1024,
            MAX_CONTEXT / 1024
        ));
    }
    Ok(out)
}

/// `rev`'s parent, or the empty tree for a root commit.
fn parent_or_empty(dir: &Path, rev: &str) -> Res<String> {
    match git::run(dir, &["rev-parse", "-q", "--verify", &format!("{rev}^")]) {
        Ok(p) => Ok(p.trim().to_owned()),
        Err(_) => Ok(git::run_input(dir, &["hash-object", "-t", "tree", "--stdin"], Some(b""))?.trim().to_owned()),
    }
}

/// The Commit Composer's reply: `{"commits": [{"message", "units": [ids]}]}`.
fn compose_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "commits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "units": { "type": "array", "items": { "type": "integer" } }
                    },
                    "required": ["message", "units"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["commits"],
        "additionalProperties": false
    })
}

const SYSTEM: &str = "You are an expert software engineer embedded in a Git client. \
Follow the requested output format exactly. Never wrap the answer in code fences and never add a preamble.";

fn build(dir: &Path, purpose: &Purpose, draft: &str) -> Res<(String, &'static str)> {
    Ok(match purpose {
        Purpose::CommitMessage { amend } => {
            let base = if *amend { Some(parent_or_empty(dir, "HEAD")?) } else { None };
            let mut range = vec!["--cached"];
            range.extend(base.as_deref());
            let diff = diff_with_stat(dir, &range)?;
            if diff.trim().is_empty() {
                return Err("Stage some changes first.".into());
            }
            let what = if *amend {
                let old = git::run(dir, &["log", "-1", "--format=%B"]).unwrap_or_default();
                format!("the amended last commit; it replaces the commit below and contains its changes plus the newly staged ones.\nThe commit being amended:\n{}\n", old.trim())
            } else {
                "the staged changes below.".into()
            };
            let recent = git::run(dir, &["log", "-n", "15", "--format=%s"]).unwrap_or_default();
            let branch = git::run(dir, &["branch", "--show-current"]).unwrap_or_default();
            let draft = if draft.trim().is_empty() { String::new() } else { format!("\nThe author's draft (improve it, keep its intent):\n{draft}\n") };
            (
                format!(
                    "Write the commit message for {what}\n\
                     Format: a summary line in the imperative mood, at most 72 characters. If the change needs explaining, \
                     add a blank line and a short body (wrapped at 72) on what changed and why. \
                     Match the conventions of this repository's recent commits (e.g. prefixes like `feat:` or `area:` if they use them).\n\
                     \nBranch: {}\nRecent commit subjects:\n{recent}{draft}\nChanges:\n{diff}",
                    branch.trim()
                ),
                "low",
            )
        }
        Purpose::Explain(id) => {
            let info = git::run(dir, &["log", "-1", "--format=fuller", id, "--"])?;
            let diff = diff_with_stat(dir, &[&parent_or_empty(dir, id)?, id])?;
            (
                format!(
                    "Explain this commit to a developer reading the history: what it does, why it was likely made, \
                     and anything risky or noteworthy. Start with a 1-3 sentence summary, then the key changes as short \
                     bullet points (\"- \"). Plain text, no headings.\n\n{info}\n{diff}"
                ),
                "medium",
            )
        }
        Purpose::PullRequest { branch, base } => {
            let range = format!("{base}...{branch}");
            let log = git::run(dir, &["log", "--format=- %s%n%w(0,2,2)%b", &format!("{base}..{branch}"), "--"])?;
            if log.trim().is_empty() {
                return Err(format!("{branch} has no commits that are not in {base}."));
            }
            let diff = diff_with_stat(dir, &[&range])?;
            (
                format!(
                    "Write a pull request for merging `{branch}` into `{base}`.\n\
                     Format: the first line is the PR title (at most 72 characters, no prefix like \"Title:\"). \
                     Then a blank line, then the description in GitHub Markdown with the sections \
                     \"## Summary\", \"## Changes\" and \"## Testing\". Be concrete and concise; don't invent test results.\n\
                     \nCommits:\n{log}\nDiff:\n{diff}"
                ),
                "medium",
            )
        }
        Purpose::ExplainRange { from, to } => {
            // `<root>^` (a compare that starts at the root commit) doesn't resolve: everything up to `to`.
            let root = from.strip_suffix('^').filter(|_| git::run(dir, &["rev-parse", "-q", "--verify", from]).is_err());
            let range = if root.is_some() { to.clone() } else { format!("{from}..{to}") };
            let log = git::run(dir, &["log", "--format=- %h %s (%an)", &range, "--"])?;
            let diff = match root {
                Some(r) => diff_with_stat(dir, &[&parent_or_empty(dir, r)?, to])?,
                None => diff_with_stat(dir, &[&format!("{from}...{to}")])?,
            };
            (
                format!(
                    "Explain what changed in `{to}` compared to `{from}`, for a developer catching up. \
                     Start with a 2-4 sentence overview, then the main changes as short bullet points (\"- \"), \
                     then anything risky or worth reviewing. Plain text, no headings.\n\nCommits:\n{log}\nDiff:\n{diff}"
                ),
                "medium",
            )
        }
        Purpose::ExplainWip => {
            let diff = diff_with_stat(dir, &["HEAD"])?;
            let untracked = git::run(dir, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
            (
                format!(
                    "Explain these uncommitted changes to the developer who made them, as a reviewer would: \
                     start with a 1-3 sentence summary of what they do together, then the key changes as short bullet points (\"- \"), \
                     then anything that looks unfinished, risky or accidental (debug code, stray files, missing pieces). Plain text, no headings.\n\
                     \nUntracked files (contents not shown):\n{untracked}\nChanges (staged and unstaged, against HEAD):\n{diff}"
                ),
                "medium",
            )
        }
        Purpose::Compose => {
            if draft.len() > MAX_CONTEXT {
                return Err(format!("These changes are too large to send to {} ({} KB, limit {} KB).", super::name(), draft.len() / 1024, MAX_CONTEXT / 1024));
            }
            let recent = git::run(dir, &["log", "-n", "15", "--format=%s"]).unwrap_or_default();
            (
                format!(
                    "Split these uncommitted changes into a sequence of small, focused, logical commits, as a careful engineer would. \
                     The changes are numbered units (diff hunks, or whole files). Plan the commits in the order they should be made \
                     (foundations first), and give each one a commit message: a summary line in the imperative mood (at most 72 characters), \
                     plus a blank line and a short body only if the change needs explaining. Match the conventions of the recent commits.\n\
                     Every unit must appear in exactly one commit; keep hunks that only make sense together in the same commit.\n\
                     \nRecent commit subjects:\n{recent}\nUnits:\n{draft}"
                ),
                "medium",
            )
        }
        Purpose::StashMessage => {
            let diff = diff_with_stat(dir, &["HEAD"])?;
            let untracked = git::run(dir, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
            (
                format!(
                    "Write a short description (at most 60 characters, no quotes) for a git stash of these uncommitted changes.\n\
                     \nUntracked files:\n{untracked}\nChanges:\n{diff}"
                ),
                "low",
            )
        }
        Purpose::Resolve(path) => {
            if !std::fs::symlink_metadata(dir.join(path)).is_ok_and(|m| m.is_file()) {
                return Err(format!("{path} is not a regular file; resolve it manually."));
            }
            // `draft`: the merge editor's output, partly resolved by the user.
            let content = if draft.is_empty() { std::fs::read_to_string(dir.join(path)).map_err(|e| format!("{path}: {e}"))? } else { draft.to_owned() };
            if content.len() > MAX_CONTEXT / 3 {
                return Err(format!("{path} is too large to resolve with AI."));
            }
            let side = |n: &str| git::run(dir, &["log", "-n", "5", "--format=- %s", n, "--", path]).unwrap_or_default();
            let theirs = ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"]
                .into_iter()
                .find(|h| git::run(dir, &["rev-parse", "-q", "--verify", h]).is_ok())
                .map(side)
                .unwrap_or_default();
            (
                format!(
                    "Resolve the merge conflict in `{path}`. Combine both sides so that the intent of each is preserved; \
                     if they truly contradict, prefer the incoming change and keep the code compiling. \
                     Reply with ONLY the complete resolved file content, with no conflict markers and nothing else.\n\
                     \nRecent commits on our side touching this file:\n{}\nRecent commits on their side:\n{theirs}\n\
                     \nFile with conflict markers:\n{content}",
                    side("HEAD")
                ),
                "high",
            )
        }
    })
}

pub fn start(ctx: &egui::Context, cfg: Config, dir: PathBuf, purpose: Purpose, draft: String) -> Job {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    let (p, sent) = (purpose.clone(), draft.clone());
    std::thread::spawn(move || {
        let schema = (p == Purpose::Compose).then(compose_schema);
        let res = build(&dir, &p, &draft).and_then(|(prompt, effort)| {
            complete(&cfg, need_token(cfg.provider)?, SYSTEM, &prompt, effort, schema.as_ref(), &mut |t| {
                ctx.request_repaint();
                tx.send(Ev::Text(t)).is_ok()
            })
        });
        let _ = tx.send(Ev::Done(res));
        ctx.request_repaint();
    });
    Job { purpose, text: String::new(), done: None, draft: sent, rx }
}

/// Quick round-trip used by Settings → Test.
pub fn test(cfg: &Config) -> Res<String> {
    let mut out = String::new();
    complete(cfg, need_token(cfg.provider)?, SYSTEM, "Reply with exactly: OK", "low", None, &mut |t| {
        out += &t;
        true
    })?;
    let model = if cfg.provider == Provider::OpenAi { &cfg.openai.model } else { &cfg.model };
    Ok(format!("{model} replied: {}", out.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_range_from_the_root_commit() {
        let dir = std::env::temp_dir().join(format!("glasir-explain-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git::run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.email", "t@t"), ("user.name", "T"), ("commit.gpgsign", "false")] {
            g(&["config", k, v]);
        }
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
        }
        let root = g(&["rev-list", "--max-parents=0", "HEAD"]).trim().to_owned();
        let (prompt, _) = build(&dir, &Purpose::ExplainRange { from: format!("{root}^"), to: "main".into() }, "").unwrap();
        assert!(prompt.contains("add a") && prompt.contains("add b") && prompt.contains("+a"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
