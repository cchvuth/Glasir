//! Tag ops beyond create: move a tag (keeping its annotation), delete it
//! locally and on a remote in one go, push every tag.

use super::{Op, Undo};
use crate::git::{self, Ref};
use std::path::Path;

/// Point tag `r` at `target`; an annotated tag is re-created with its exact message, read
/// from the tag object (minus its signature).
pub fn move_tag(dir: &Path, r: &Ref, target: &str) -> Op {
    let msg = r.annotated.then(|| match git::run(dir, &["cat-file", "tag", &r.full]) {
        Ok(o) => tag_message(&o).to_owned(),
        Err(_) => r.message.clone(),
    });
    let mut args = vec!["tag", "-f"];
    if msg.is_some() {
        args.extend(["-a", "--cleanup=verbatim", "-F", "-"]);
    }
    args.extend([r.name.as_str(), target]);
    let op = Op::new(format!("Move tag {}", r.name), &args).undo(Undo::Keep);
    match msg {
        Some(m) => op.input(m),
        None => op,
    }
}

/// A tag object's message without its signature, which wouldn't match the moved tag.
fn tag_message(obj: &str) -> &str {
    let msg = obj.split_once("\n\n").map_or("", |(_, m)| m);
    let sig = ["PGP SIGNATURE", "PGP MESSAGE", "SIGNED MESSAGE", "SSH SIGNATURE"].iter().filter_map(|k| {
        let line = format!("-----BEGIN {k}-----");
        if msg.starts_with(&line) { Some(0) } else { msg.rfind(&format!("\n{line}")).map(|p| p + 1) }
    });
    &msg[..sig.max().unwrap_or(msg.len())]
}

/// Delete tag `r` here, then on `remote` (only the local half can be undone).
pub fn delete_everywhere(r: &Ref, remote: &str) -> Op {
    Op::new(format!("Delete tag {}", r.name), &["tag", "-d", &r.name])
        .then(&["push", remote, "--delete", &r.full])
        .undo(Undo::Keep)
        .confirm(format!("Delete tag {} here and on {remote}? Undo restores only the local tag.", r.name))
}

/// No `--progress`: a rejected tag must not get the rejected-branch-push offer (`offer.rs`).
pub fn push_all(remote: &str) -> Op {
    Op::new(format!("Push all tags to {remote}"), &["push", remote, "--tags"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::repo::op::exec;
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn move_delete_and_push_tags() {
        let dir = scratch("tags");
        let remote = scratch("tags-remote");
        sh(&remote, &["init", "-q", "--bare"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "v1", "-m", "Release one\n\n## Notes"]);
        sh(&dir, &["tag", "light"]);
        // Messages the parsed `Ref::message` can't round-trip: a multi-line subject with
        // a trailing newline, and an empty one (still an annotated tag).
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "v2", "-m", "line1\nline2\n\nbody\n"]);
        sh(&dir, &["tag", "-a", "empty", "-m", ""]);
        commit(&dir, "a.txt", "two\n");
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        let tag = |n: &str| git::refs(&dir).unwrap().into_iter().find(|r| r.name == n);
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();

        // Moving keeps the annotation (and lightweight stays lightweight); undo moves it back.
        let before = git::snapshot(&dir).unwrap();
        let obj = |n: &str| sh(&dir, &["cat-file", "-p", n]).split_once("\n\n").unwrap().1.to_owned();
        let old = [obj("v2"), obj("empty")];
        for n in ["v1", "light", "v2", "empty"] {
            exec(&dir, &move_tag(&dir, &tag(n).unwrap(), &head)).unwrap();
        }
        assert_eq!([obj("v2"), obj("empty")], old);
        assert_eq!(sh(&dir, &["cat-file", "-t", "empty"]), "tag\n");
        let (v1, light) = (tag("v1").unwrap(), tag("light").unwrap());
        assert_eq!((v1.target.as_str(), v1.message.as_str()), (head.as_str(), "Release one\n\n## Notes"));
        assert_eq!((light.target.as_str(), light.message.as_str()), (head.as_str(), ""));
        git::restore(&dir, &before, "--keep").unwrap();
        assert_ne!(tag("v1").unwrap().target, head);

        exec(&dir, &push_all("origin")).unwrap();
        assert_eq!(sh(&remote, &["tag"]), "empty\nlight\nv1\nv2\n");
        exec(&dir, &delete_everywhere(&tag("v1").unwrap(), "origin")).unwrap();
        assert!(tag("v1").is_none());
        assert_eq!(sh(&remote, &["tag"]), "empty\nlight\nv2\n");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// The message is kept byte for byte, and an empty annotation is still an annotated tag.
    #[test]
    fn move_tag_keeps_message_and_type() {
        let dir = scratch("tags-msg");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let msg = "Release 2.0\nwrapped line\n\nbody";
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "t1", "-m", msg]);
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "t2", "-m", ""]);
        commit(&dir, "a.txt", "two\n");
        let tag = |n: &str| git::refs(&dir).unwrap().into_iter().find(|r| r.name == n).unwrap();
        for t in ["t1", "t2"] {
            exec(&dir, &move_tag(&dir, &tag(t), "HEAD")).unwrap();
            assert_eq!(sh(&dir, &["cat-file", "-t", t]), "tag\n");
        }
        assert_eq!(sh(&dir, &["tag", "-l", "--format=%(contents)", "t1"]).trim_end(), msg);
        assert_eq!(tag("t1").message, msg);
        assert_eq!(sh(&dir, &["rev-parse", "t2^{commit}"]), sh(&dir, &["rev-parse", "HEAD"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
