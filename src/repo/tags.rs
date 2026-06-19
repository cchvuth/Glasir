//! Tag ops beyond create: move a tag (keeping its annotation), delete it
//! locally and on a remote in one go, push every tag.

use super::{Op, Undo};
use crate::git::Ref;

/// Point tag `r` at `target`; an annotated tag is re-created with its message.
pub fn move_tag(r: &Ref, target: &str) -> Op {
    let annotated = !r.message.is_empty();
    let mut args = vec!["tag", "-f"];
    if annotated {
        args.extend(["-a", "--cleanup=verbatim", "-F", "-"]);
    }
    args.extend([r.name.as_str(), target]);
    let op = Op::new(format!("Move tag {}", r.name), &args).undo(Undo::Keep);
    if annotated { op.input(r.message.clone()) } else { op }
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
        commit(&dir, "a.txt", "two\n");
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        let tag = |n: &str| git::refs(&dir).unwrap().into_iter().find(|r| r.name == n);
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();

        // Moving keeps the annotation (and lightweight stays lightweight); undo moves it back.
        let before = git::snapshot(&dir).unwrap();
        exec(&dir, &move_tag(&tag("v1").unwrap(), &head)).unwrap();
        exec(&dir, &move_tag(&tag("light").unwrap(), &head)).unwrap();
        let (v1, light) = (tag("v1").unwrap(), tag("light").unwrap());
        assert_eq!((v1.target.as_str(), v1.message.as_str()), (head.as_str(), "Release one\n\n## Notes"));
        assert_eq!((light.target.as_str(), light.message.as_str()), (head.as_str(), ""));
        git::restore(&dir, &before, "--keep").unwrap();
        assert_ne!(tag("v1").unwrap().target, head);

        exec(&dir, &push_all("origin")).unwrap();
        assert_eq!(sh(&remote, &["tag"]), "light\nv1\n");
        exec(&dir, &delete_everywhere(&tag("v1").unwrap(), "origin")).unwrap();
        assert!(tag("v1").is_none());
        assert_eq!(sh(&remote, &["tag"]), "light\n");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}
