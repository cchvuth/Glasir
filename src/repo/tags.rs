//! Tag ops beyond create: move a tag (keeping its annotation), delete it
//! locally and on a remote in one go, push every tag.

use super::{Op, Undo};
use crate::git::{self, Ref};
use std::path::Path;

/// Point tag `r` at `target`; an annotated tag is re-created with its exact message bytes,
/// read from the tag object. A signature can't be kept, so a signed tag asks first.
pub fn move_tag(dir: &Path, r: &Ref, target: &str) -> Result<Op, String> {
    let mut args = vec!["tag", "-f"];
    let mut msg = None;
    if r.annotated {
        let obj = git::run_raw(dir, &["cat-file", "tag", &r.full], None).map_err(|e| format!("Can't read tag {}: {e}", r.name))?;
        msg = Some(tag_message(&obj));
        args.extend(["-a", "--cleanup=verbatim", "-F", "-"]);
    } else {
        args.push("--no-sign"); // tag.gpgSign=true would otherwise demand a message
    }
    args.extend(["--end-of-options", r.name.as_str(), target]);
    let mut op = Op::new(format!("Move tag {}", r.name), &args).undo(Undo::Keep);
    if let Some((m, signed)) = msg {
        if signed {
            op = op.confirm(format!("Tag {} is signed. The moved tag will be unsigned (undo restores the signed one). Move it?", r.name));
        }
        op = op.input(m);
    }
    Ok(op)
}

/// A tag object's message without its signature (which wouldn't match the moved tag), and
/// whether it had one.
fn tag_message(obj: &[u8]) -> (Vec<u8>, bool) {
    let msg = obj.windows(2).position(|w| w == b"\n\n").map_or(&[][..], |p| &obj[p + 2..]);
    let sig = ["PGP SIGNATURE", "PGP MESSAGE", "SIGNED MESSAGE", "SSH SIGNATURE"].iter().filter_map(|k| {
        let line = format!("-----BEGIN {k}-----");
        if msg.starts_with(line.as_bytes()) { Some(0) } else { msg.windows(line.len() + 1).rposition(|w| w[0] == b'\n' && &w[1..] == line.as_bytes()).map(|p| p + 1) }
    });
    match sig.max() {
        Some(end) => (msg[..end].to_vec(), true),
        None => (msg.to_vec(), false),
    }
}

/// Delete tag `r` here, then on `remote` (only the local half can be undone).
pub fn delete_everywhere(r: &Ref, remote: &str) -> Op {
    Op::new(format!("Delete tag {}", r.name), &["tag", "-d", "--end-of-options", &r.name])
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
    fn moving_a_lightweight_tag_with_gpgsign_on() {
        let dir = scratch("tag-gpgsign");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        sh(&dir, &["tag", "lw"]);
        commit(&dir, "a.txt", "two\n");
        sh(&dir, &["config", "tag.gpgSign", "true"]);
        let r = git::refs(&dir).unwrap().into_iter().find(|r| r.name == "lw").unwrap();
        exec(&dir, &move_tag(&dir, &r, "HEAD").unwrap()).unwrap();
        assert_eq!(sh(&dir, &["cat-file", "-t", "lw"]), "commit\n");
    }

    #[test]
    fn a_tag_named_like_an_option_is_not_read_as_one() {
        let dir = scratch("tag-option-name");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "secret.txt", "SECRET\n");
        sh(&dir, &["update-ref", "refs/tags/-Fsecret.txt", "HEAD"]);
        let r = git::refs(&dir).unwrap().into_iter().find(|r| r.name == "-Fsecret.txt").unwrap();
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let _ = exec(&dir, &move_tag(&dir, &r, &head).unwrap());
        assert_eq!(sh(&dir, &["tag", "-l"]), "-Fsecret.txt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            exec(&dir, &move_tag(&dir, &tag(n).unwrap(), &head).unwrap()).unwrap();
        }
        assert_eq!([obj("v2"), obj("empty")], old);
        assert_eq!(sh(&dir, &["cat-file", "-t", "empty"]), "tag\n");
        let (v1, light) = (tag("v1").unwrap(), tag("light").unwrap());
        assert_eq!((v1.target.as_str(), v1.message.as_str()), (head.as_str(), "Release one\n\n## Notes"));
        assert_eq!((light.target.as_str(), light.message.as_str()), (head.as_str(), ""));
        crate::repo::testutil::move_back(&dir, &before, crate::repo::Undo::Keep).unwrap();
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
            exec(&dir, &move_tag(&dir, &tag(t), "HEAD").unwrap()).unwrap();
            assert_eq!(sh(&dir, &["cat-file", "-t", t]), "tag\n");
        }
        assert_eq!(sh(&dir, &["tag", "-l", "--format=%(contents)", "t1"]).trim_end(), msg);
        assert_eq!(tag("t1").message, msg);
        assert_eq!(sh(&dir, &["rev-parse", "t2^{commit}"]), sh(&dir, &["rev-parse", "HEAD"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Non-UTF-8 bytes survive; a signed tag asks first (it comes back unsigned); an unreadable one is refused.
    #[test]
    fn move_tag_raw_bytes_signature_and_missing() {
        let dir = scratch("tags-raw");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let f = dir.join("msg");
        std::fs::write(&f, b"caf\xe9\n\n").unwrap();
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "latin", "-F", &f.to_string_lossy()]);
        std::fs::write(&f, "Signed\n-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n").unwrap();
        sh(&dir, &["tag", "--cleanup=verbatim", "-a", "signed", "-F", &f.to_string_lossy()]);
        commit(&dir, "a.txt", "two\n");
        let tag = |n: &str| git::refs(&dir).unwrap().into_iter().find(|r| r.name == n).unwrap();
        let body = |n: &str| {
            let o = git::run_raw(&dir, &["cat-file", "tag", n], None).unwrap();
            o[o.windows(2).position(|w| w == b"\n\n").unwrap() + 2..].to_vec()
        };

        let op = move_tag(&dir, &tag("latin"), "HEAD").unwrap();
        assert!(op.confirm.is_none());
        exec(&dir, &op).unwrap();
        assert_eq!(body("latin"), b"caf\xe9\n\n");

        let op = move_tag(&dir, &tag("signed"), "HEAD").unwrap();
        assert!(op.confirm.as_deref().is_some_and(|c| c.contains("unsigned")));
        exec(&dir, &op).unwrap();
        assert_eq!(body("signed"), b"Signed\n");

        let mut gone = tag("latin");
        gone.full = "refs/tags/missing".into();
        assert!(move_tag(&dir, &gone, "HEAD").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
