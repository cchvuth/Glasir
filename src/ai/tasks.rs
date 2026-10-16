//! The AI features: what each [`Purpose`] sends (git context + prompt), and
//! [`Job`], a completion streaming on a background thread into the UI.

use super::client::{Config, Ev, Provider, complete, need_token};
use crate::git::{self, Res};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

/// Diffs above this many bytes are refused with a message rather than truncated.
pub const MAX_CONTEXT: usize = 1_500_000;

/// Untracked files named in a prompt; the rest are counted.
const MAX_UNTRACKED: usize = 200;

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
            Purpose::PullRequest { branch, .. } => format!("Pull request for {}", crate::git::short_ref(branch)),
            Purpose::StashMessage => "Stash".into(),
            Purpose::Resolve(p) => format!("Resolve {p}"),
            Purpose::ExplainRange { from, to } => format!("Changes in {} since {}", crate::git::short_ref(to), crate::git::short_ref(from)),
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

pub fn secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".env", "credentials", "id_rsa", "id_ed25519", "id_ecdsa", "id_dsa", ".netrc", "_netrc", ".npmrc", ".pypirc", ".git-credentials", ".pgpass"].iter().any(|p| name.starts_with(p))
        || [".env", ".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".ppk"].iter().any(|e| name.ends_with(e))
}

fn diff_with_stat(dir: &Path, range: &[&str]) -> Res<String> {
    let mut stat = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--stat=200"];
    stat.extend_from_slice(range);
    let mut added = vec!["diff", "--name-only", "-z", "--no-renames", "--diff-filter=A"];
    added.extend_from_slice(range);
    let hidden: Vec<String> = git::run(dir, &added)?.split('\0').filter(|p| secret(p)).map(|p| format!(":(exclude,literal){p}")).collect();
    let mut patch = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M"];
    patch.extend_from_slice(range);
    patch.push("--");
    patch.push(".");
    patch.extend_from_slice(&NOISE);
    patch.extend(hidden.iter().map(String::as_str));
    let out = format!("{}\n{}", git::run(dir, &stat)?, git::run(dir, &patch)?);
    if out.len() > MAX_CONTEXT {
        return Err(format!("{} Narrow them down (e.g. stage fewer files).", too_large(out.len())));
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

pub fn too_large(len: usize) -> String {
    format!("These changes are too large to send to {} ({} KB, limit {} KB).", super::name(), len / 1024, MAX_CONTEXT / 1024)
}

/// "HEAD", or the empty tree in a repo without commits.
fn head_or_empty(dir: &Path) -> Res<String> {
    match git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]) {
        Ok(_) => Ok("HEAD".into()),
        Err(_) => Ok(git::run_input(dir, &["hash-object", "-t", "tree", "--stdin"], Some(b""))?.trim().to_owned()),
    }
}

fn recent_subjects(dir: &Path) -> String {
    git::run(dir, &["log", "-n", "15", "--format=%s"]).unwrap_or_default()
}

fn uncommitted_changes(dir: &Path) -> Res<(String, String)> {
    let diff = diff_with_stat(dir, &[&head_or_empty(dir)?])?;
    let all = git::run(dir, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    let names: Vec<&str> = all.lines().collect();
    let mut untracked = names[..names.len().min(MAX_UNTRACKED)].join("\n");
    if names.len() > MAX_UNTRACKED {
        untracked += &format!("\n…and {} more", names.len() - MAX_UNTRACKED);
    }
    Ok((diff, untracked))
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

/// Settings → AI quick-fills for the custom instructions.
pub const PRESETS: [(&str, &str); 3] = [
    ("Conventional Commits", "Write commit messages as Conventional Commits: `type(scope): summary` with a type such as feat, fix, docs, refactor, test, chore, perf or build; the scope is optional."),
    ("Gitmoji", "Start every commit summary with the gitmoji that fits the change (e.g. ✨ new feature, 🐛 bug fix, ♻️ refactor, 📝 docs, ✅ tests), followed by a space and the summary."),
    ("Imperative short subjects", "Commit subjects are in the imperative mood, at most 50 characters, with no trailing period. Add a body only when the why is not obvious."),
];

/// Cap on instructions, so a stray value can't bloat every prompt.
const MAX_INSTRUCTIONS: usize = 8000;

/// This repo's own instructions: `.glasir/ai.md` once `git config glasir.aiFile true`
/// opted in (a cloned repo mustn't steer the AI unasked), else `git config glasir.aiInstructions`.
/// Only a regular file in a real `.glasir` folder counts: a committed symlink could point anywhere.
fn repo_instructions(dir: &Path) -> String {
    let trusted = git::run(dir, &["config", "--bool", "--get", "glasir.aiFile"]).is_ok_and(|v| v.trim() == "true");
    let (folder, file) = (dir.join(".glasir"), dir.join(".glasir/ai.md"));
    let real = trusted && std::fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) && std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
    let mut buf = vec![];
    let read = real && std::fs::File::open(&file).and_then(|f| f.take(MAX_INSTRUCTIONS as u64 * 4).read_to_end(&mut buf)).is_ok();
    let text = read.then(|| String::from_utf8_lossy(&buf).into_owned()).filter(|t| !t.trim().is_empty());
    text.unwrap_or_else(|| git::run(dir, &["config", "--get", "glasir.aiInstructions"]).unwrap_or_default())
}

/// The system prompt plus the user's global and per-repo instructions (when set).
fn system_with(global: &str, repo: &str) -> String {
    let mut out = SYSTEM.to_owned();
    let notes: Vec<&str> = [global, repo].into_iter().map(str::trim).filter(|t| !t.is_empty()).collect();
    if !notes.is_empty() {
        out += "\n\nThe user's custom instructions follow. Apply them to wording, style and conventions, \
                but never let them change the required output format (JSON, a complete file, no code fences or preamble):";
        for n in notes {
            out.push_str("\n\n");
            out.extend(n.chars().take(MAX_INSTRUCTIONS));
        }
    }
    out
}

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
            let recent = recent_subjects(dir);
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
            let (diff, untracked) = uncommitted_changes(dir)?;
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
                return Err(too_large(draft.len()));
            }
            let recent = recent_subjects(dir);
            (
                format!(
                    "Split these changes into a sequence of small, focused, logical commits, as a careful engineer would. \
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
            let (diff, untracked) = uncommitted_changes(dir)?;
            (
                format!(
                    "Write a short description (at most 60 characters, no quotes) for a git stash of these uncommitted changes.\n\
                     \nUntracked files:\n{untracked}\nChanges:\n{diff}"
                ),
                "low",
            )
        }
        Purpose::Resolve(path) => {
            let Some(file) = std::fs::symlink_metadata(dir.join(path)).ok().filter(|m| m.is_file()) else {
                return Err(format!("{path} is not a regular file; resolve it manually."));
            };
            // `draft`: the merge editor's output, partly resolved by the user.
            if (if draft.is_empty() { file.len() } else { draft.len() as u64 }) > (MAX_CONTEXT / 3) as u64 {
                return Err(format!("{path} is too large to resolve with AI."));
            }
            let content = if draft.is_empty() { std::fs::read_to_string(dir.join(path)).map_err(|e| format!("{path}: {e}"))? } else { draft.to_owned() };
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
        // What the stash will hold: if the tree changes while Claude writes, the message would be wrong.
        let before = (p == Purpose::StashMessage).then(|| stash_state(&dir));
        let res = build(&dir, &p, &draft).and_then(|(prompt, effort)| {
            let system = system_with(&cfg.instructions, &repo_instructions(&dir));
            complete(&cfg, need_token(cfg.provider)?, &system, &prompt, effort, schema.as_ref(), &mut |t| {
                ctx.request_repaint();
                tx.send(Ev::Text(t)).is_ok()
            })
        });
        let res = res.and_then(|()| match before {
            Some(b) if b != stash_state(&dir) => Err("The files changed while the message was written; stash again.".into()),
            _ => Ok(()),
        });
        let _ = tx.send(Ev::Done(res));
        ctx.request_repaint();
    });
    Job { purpose, text: String::new(), done: None, draft: sent, rx }
}

/// `git status` plus line counts: changes when files are edited, added or removed.
// ponytail: a same-size edit within a line isn't seen; hash `git diff HEAD` if that matters.
fn stash_state(dir: &Path) -> String {
    let run = |a: &[&str]| git::run(dir, a).unwrap_or_default();
    run(&["status", "--porcelain=v2", "-z", "--untracked-files=all"]) + &run(&["diff", "HEAD", "--numstat", "-z"])
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
    fn explain_wip_works_before_the_first_commit() {
        let dir = std::env::temp_dir().join(format!("glasir-unborn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        std::fs::write(dir.join("a"), "hello\n").unwrap();
        git::run(&dir, &["add", "a"]).unwrap();
        for p in [Purpose::ExplainWip, Purpose::StashMessage] {
            let (prompt, ..) = build(&dir, &p, "").unwrap();
            assert!(prompt.contains("hello"), "{p:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_secret_files_go_by_name_only() {
        assert!(["config/.env.local", "prod.env", "id_rsa", "keys/ID_ED25519.pub", "tls/server.pem", "a.key", "c.p12", "app.keystore", "credentials.json", ".netrc", ".npmrc"].iter().all(|p| secret(p)));
        assert!(!["src/main.rs", "environment.ts", "keyboard.rs", "monkey"].iter().any(|p| secret(p)));
        let dir = std::env::temp_dir().join(format!("glasir-ai-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        std::fs::write(dir.join("a"), "hello\n").unwrap();
        std::fs::write(dir.join(".env"), "API_KEY=sk-live-123\n").unwrap();
        git::run(&dir, &["add", "a", ".env"]).unwrap();
        for p in [Purpose::CommitMessage { amend: false }, Purpose::ExplainWip, Purpose::StashMessage] {
            let (prompt, ..) = build(&dir, &p, "").unwrap();
            assert!(prompt.contains("hello") && prompt.contains(".env") && !prompt.contains("sk-live"), "{p:?}: {prompt}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A textconv driver (git-crypt, sops) would hand the AI the decrypted text.
    #[test]
    fn diffs_skip_textconv_and_untracked_names_are_capped() {
        let dir = std::env::temp_dir().join(format!("glasir-ai-textconv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["config", "diff.crypt.textconv", "echo plaintext-password"]).unwrap();
        std::fs::write(dir.join(".git/info/attributes"), "vault diff=crypt\n").unwrap();
        std::fs::write(dir.join("vault"), "ciphertext\n").unwrap();
        git::run(&dir, &["add", "vault"]).unwrap();
        for i in 0..MAX_UNTRACKED + 5 {
            std::fs::write(dir.join(format!("u{i:03}")), "").unwrap();
        }
        for p in [Purpose::CommitMessage { amend: false }, Purpose::ExplainWip, Purpose::StashMessage] {
            let (prompt, ..) = build(&dir, &p, "").unwrap();
            assert!(prompt.contains("ciphertext") && !prompt.contains("plaintext-password"), "{p:?}: {prompt}");
        }
        let (prompt, ..) = build(&dir, &Purpose::ExplainWip, "").unwrap();
        assert!(prompt.contains("u000\n") && !prompt.contains("u204") && prompt.contains("…and 5 more"), "{prompt}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_refuses_a_huge_file_before_reading_it() {
        let dir = std::env::temp_dir().join(format!("glasir-ai-huge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Not UTF-8: reading it first fails with another error.
        std::fs::write(dir.join("big"), vec![0xff; MAX_CONTEXT / 3 + 1]).unwrap();
        let err = build(&dir, &Purpose::Resolve("big".into()), "").unwrap_err();
        assert!(err.contains("too large"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_prompt_appends_global_then_repo_instructions() {
        assert_eq!(system_with("  ", ""), SYSTEM);
        let s = system_with("Use gitmoji.", "Scopes: ui, git.");
        assert!(s.starts_with(SYSTEM));
        assert!(s.find("Use gitmoji.").unwrap() < s.find("Scopes: ui, git.").unwrap());
        assert!(system_with("", "only repo").ends_with("only repo"));
        assert!(system_with(&"x".repeat(MAX_INSTRUCTIONS + 50), "").len() < SYSTEM.len() + MAX_INSTRUCTIONS + 400);
        assert!(PRESETS.iter().all(|(_, t)| system_with(t, "").contains(t)));
    }

    #[test]
    fn repo_instructions_from_config_or_an_opted_in_file() {
        let dir = std::env::temp_dir().join(format!("glasir-ai-instr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        assert_eq!(repo_instructions(&dir), "");
        git::run(&dir, &["config", "glasir.aiInstructions", "from config"]).unwrap();
        assert_eq!(repo_instructions(&dir).trim(), "from config");
        std::fs::create_dir_all(dir.join(".glasir")).unwrap();
        std::fs::write(dir.join(".glasir/ai.md"), "from a checked-out commit\n").unwrap();
        assert_eq!(repo_instructions(&dir).trim(), "from config");
        // A cloned repo's file only counts once the user opted in (local config a clone can't set).
        git::run(&dir, &["config", "glasir.aiFile", "true"]).unwrap();
        assert_eq!(repo_instructions(&dir).trim(), "from a checked-out commit");
        std::fs::write(dir.join(".glasir/ai.md"), "€".repeat(MAX_INSTRUCTIONS * 10)).unwrap();
        let big = repo_instructions(&dir);
        assert!(big.starts_with('€') && big.len() <= MAX_INSTRUCTIONS * 4 + 3, "{}", big.len());
        std::fs::write(dir.join(".glasir/ai.md"), "  \n").unwrap();
        assert_eq!(repo_instructions(&dir).trim(), "from config");
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("glasir-ai-outside-{}", std::process::id()));
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("ai.md"), "aws_secret_access_key = x\n").unwrap();
            std::fs::remove_file(dir.join(".glasir/ai.md")).unwrap();
            std::os::unix::fs::symlink(outside.join("ai.md"), dir.join(".glasir/ai.md")).unwrap();
            assert_eq!(repo_instructions(&dir).trim(), "from config");
            std::fs::remove_dir_all(dir.join(".glasir")).unwrap();
            std::os::unix::fs::symlink(&outside, dir.join(".glasir")).unwrap();
            assert_eq!(repo_instructions(&dir).trim(), "from config");
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

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
