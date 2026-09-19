//! Commit-box message helpers: `commit.template` and `Co-authored-by` trailers.

use super::RepoData;
use std::path::Path;

/// `commit.template` as it prefills the box: the comment prefix to strip on commit, plus the
/// text split into summary and description.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub prefix: String,
    pub summary: String,
    pub body: String,
}

/// The configured template, if the file reads and isn't empty. `~/` expands; a relative path
/// is relative to the working tree.
pub fn template(d: &RepoData, dir: &Path) -> Option<Template> {
    let path = d.cfg("commit.template").filter(|p| !p.is_empty())?;
    let path = match path.strip_prefix("~/") {
        Some(rest) => Path::new(&std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?).join(rest),
        None => dir.join(path),
    };
    let text = std::fs::read_to_string(path).ok().filter(|t| !t.trim().is_empty())?;
    let prefix = d.cfg("core.commentstring").or(d.cfg("core.commentchar")).filter(|c| !c.is_empty() && *c != "auto").unwrap_or("#").to_owned();
    // A leading comment is guidance for the description, not a summary.
    let (summary, body) = match text.split_once('\n') {
        Some((first, rest)) if !first.starts_with(&prefix) => (first.trim_end().to_owned(), rest.trim_matches('\n').to_owned()),
        _ if text.starts_with(&prefix) => (String::new(), text.trim_end().to_owned()),
        _ => (text.trim().to_owned(), String::new()),
    };
    Some(Template { prefix, summary, body })
}

/// Drops the template's comment lines like git's `--cleanup=strip`, and collapses blank runs.
/// Only the template's own: the box may hold a split's or Claude's message with `## Why` lines.
pub fn strip_comments(msg: &str, t: &Template) -> String {
    let ours = |l: &&str| l.starts_with(&t.prefix) && t.body.lines().any(|c| c.trim_end() == l.trim_end());
    let mut out: Vec<&str> = vec![];
    for l in msg.lines().filter(|l| !ours(l)).map(str::trim_end) {
        if !(l.is_empty() && out.last().is_none_or(|p| p.is_empty())) {
            out.push(l);
        }
    }
    out.join("\n").trim().to_owned()
}

fn is_trailer(l: &str) -> bool {
    l.split_once(": ").is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Appends a `Co-authored-by` trailer per (name, email), joining an existing trailer block
/// and skipping ones already there.
pub fn add_coauthors(msg: &str, who: &[(String, String)]) -> String {
    let lines: Vec<String> = who
        .iter()
        .map(|(n, e)| format!("Co-authored-by: {n} <{e}>"))
        .filter(|l| !msg.lines().any(|m| m.trim().eq_ignore_ascii_case(l)))
        .collect();
    if lines.is_empty() {
        return msg.to_owned();
    }
    let msg = msg.trim_end();
    let block = msg.rsplit_once("\n\n").is_some_and(|(_, last)| last.lines().all(is_trailer));
    format!("{msg}{}{}", if block { "\n" } else { "\n\n" }, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comment_lines() {
        let tpl = |prefix: &str, body: &str| Template { prefix: prefix.into(), summary: String::new(), body: body.into() };
        let t = "feat: x\n\n# Explain why\n# more\n\nBody  \n\n\n#end\n";
        assert_eq!(strip_comments(t, &tpl("#", "# Explain why\n# more\n#end")), "feat: x\n\nBody");
        assert_eq!(strip_comments("a\n; c\nb", &tpl(";", "; c")), "a\nb");
        assert_eq!(strip_comments("  # indented stays\n", &tpl("#", "# indented stays")), "# indented stays");
        // Only the template's own comments: a split's or Claude's `## Why` / `#123` lines stay.
        assert_eq!(strip_comments("Fix\n\n## Why\n#123 was hit\n# explain", &tpl("#", "# explain")), "Fix\n\n## Why\n#123 was hit");
    }

    #[test]
    fn builds_trailers() {
        let who = vec![("Ann Lee".to_owned(), "ann@x.io".to_owned()), ("Bo".to_owned(), "bo@x.io".to_owned())];
        assert_eq!(add_coauthors("fix: a", &who), "fix: a\n\nCo-authored-by: Ann Lee <ann@x.io>\nCo-authored-by: Bo <bo@x.io>");
        assert_eq!(add_coauthors("fix\n\nbody\n\nSigned-off-by: Me <m@x>\nco-authored-by: Bo <bo@x.io>", &who), "fix\n\nbody\n\nSigned-off-by: Me <m@x>\nco-authored-by: Bo <bo@x.io>\nCo-authored-by: Ann Lee <ann@x.io>");
        // A body paragraph that isn't a trailer block starts a new one.
        assert_eq!(add_coauthors("fix\n\nNote: see this\nand that", &who[..1]), "fix\n\nNote: see this\nand that\n\nCo-authored-by: Ann Lee <ann@x.io>");
        assert_eq!(add_coauthors("fix", &[]), "fix");
    }

    #[test]
    fn reads_template() {
        use super::super::testutil::{commit, identity, scratch, sh};
        let dir = scratch("template");
        sh(&dir, &["init", "-q"]);
        identity(&dir);
        commit(&dir, "a", "one");
        let load = || super::super::data::load_all(&dir, 100, Default::default()).unwrap();
        assert!(template(&load(), &dir).is_none());
        std::fs::write(dir.join("t.txt"), "# Say why\n\n# more\n").unwrap();
        sh(&dir, &["config", "commit.template", "t.txt"]);
        let t = template(&load(), &dir).unwrap();
        assert_eq!((t.prefix.as_str(), t.summary.as_str()), ("#", ""));
        assert!(t.body.starts_with("# Say why"));
        std::fs::write(dir.join("t.txt"), "feat: \n\nWhy:\n# hint\n").unwrap();
        let t = template(&load(), &dir).unwrap();
        assert_eq!((t.summary.as_str(), t.body.as_str()), ("feat:", "Why:\n# hint"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The box end to end: template comments stripped, trailers added, hooks skippable,
    /// commit-and-push reaches the remote, and a failing hook keeps the typed message.
    #[test]
    fn commit_box_options() {
        use super::super::testutil::{commit, identity, scratch, settle, sh};
        use super::super::{RepoTab, View};
        let root = scratch("commit-box");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "up.git"]);
        sh(&root, &["clone", "-q", "up.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "one");
        sh(&a, &["push", "-q", "-u", "origin", "HEAD"]);
        std::fs::write(a.join(".git/tpl"), "feat: \n\n# explain\n").unwrap();
        sh(&a, &["config", "commit.template", ".git/tpl"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        assert_eq!((t.summary.as_str(), t.body.as_str()), ("feat:", "# explain"));

        std::fs::write(a.join("f"), "two").unwrap();
        sh(&a, &["add", "f"]);
        let hook = a.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        t.summary = "feat: two".into();
        t.coauthors = vec![("Ann".into(), "ann@x.io".into())];
        t.run(&ctx, t.commit_op());
        let msgs = settle(&ctx, &mut t);
        // Git for Windows runs a hook without an exec bit too.
        assert!(msgs.iter().any(|m| m.error), "hook must fail the commit");
        assert_eq!(t.summary, "feat: two");
        assert_eq!(sh(&a, &["rev-list", "--count", "HEAD"]).trim(), "1");
        t.no_verify = true;
        t.push_after = true;
        t.run(&ctx, t.commit_op());
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["log", "-1", "--format=%B"]).trim(), "feat: two\n\nCo-authored-by: Ann <ann@x.io>");
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), sh(&root.join("up.git"), &["rev-parse", "main"]));
        assert_eq!((t.summary.as_str(), t.coauthors.len()), ("feat:", 0));
        assert_eq!(t.undo_label(), Some("Commit"));

        // A rejected push after the commit offers the push fixes; ⌘Z is the commit's alone.
        sh(&root, &["clone", "-q", "up.git", "b"]);
        identity(&root.join("b"));
        commit(&root.join("b"), "g", "theirs");
        sh(&root.join("b"), &["push", "-q"]);
        std::fs::write(a.join("f"), "three").unwrap();
        sh(&a, &["add", "f"]);
        t.summary = "feat: three".into();
        t.run(&ctx, t.commit_op());
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["log", "-1", "--format=%s"]).trim(), "feat: three");
        assert_eq!(t.choice.as_ref().map(|c| c.title.as_str()), Some("Push rejected"));
        assert_eq!((t.summary.as_str(), t.undo_label()), ("feat:", Some("Commit")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
