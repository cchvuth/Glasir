//! Git LFS: is git-lfs installed, does the repo use it, which files it tracks,
//! and pointer-file parsing. Content goes through git's own filter process.

use super::{FileDiff, read_small, run, run_input};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Debug, Default)]
pub struct Lfs {
    /// `git lfs` works (checked once per process).
    pub installed: bool,
    /// The top-level .gitattributes routes some path through `filter=lfs`.
    pub used: bool,
    /// Index and untracked paths with `filter=lfs`.
    pub files: HashSet<String>,
}

fn lfs_installed() -> bool {
    static ONCE: OnceLock<bool> = OnceLock::new();
    *ONCE.get_or_init(|| run(&std::env::temp_dir(), &["lfs", "version"]).is_ok())
}

/// Two git calls per full load (ls-files | check-attr), none for repos without LFS.
/// (`ls-files ':(attr:filter=lfs)'` would be one, but it skips new untracked folders.)
// Only the top-level .gitattributes is checked; nested ones with filter=lfs go unnoticed.
pub fn lfs(dir: &Path) -> Lfs {
    let used = read_small(&dir.join(".gitattributes")).is_some_and(|a| String::from_utf8_lossy(&a).contains("filter=lfs"));
    let files = if used {
        let all = run(dir, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).unwrap_or_default();
        lfs_paths(&run_input(dir, &["check-attr", "--stdin", "-z", "filter"], Some(all.as_bytes())).unwrap_or_default())
    } else {
        HashSet::new()
    };
    Lfs { installed: lfs_installed(), used, files }
}

/// `git check-attr -z` output: `<path>\0filter\0<value>\0` records.
fn lfs_paths(out: &str) -> HashSet<String> {
    let f: Vec<&str> = out.split('\0').collect();
    f.chunks_exact(3).filter(|r| r[2] == "lfs").map(|r| r[0].to_owned()).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct LfsPointer {
    /// `sha256:<hex>`
    pub oid: String,
    pub size: u64,
}

/// A pointer file's text: `version https://git-lfs...`, `oid sha256:...`, `size N`.
pub fn parse_pointer(text: &str) -> Option<LfsPointer> {
    let mut lines = text.lines();
    lines.next()?.strip_prefix("version https://git-lfs.github.com/spec/")?;
    let (mut oid, mut size) = (None, None);
    for l in lines {
        if let Some(o) = l.strip_prefix("oid ") {
            oid = Some(o.to_owned());
        } else if let Some(s) = l.strip_prefix("size ") {
            size = s.trim().parse().ok();
        }
    }
    Some(LfsPointer { oid: oid?, size: size? })
}

/// Both sides of a diff of a pointer file; `None` when neither side is a pointer.
pub fn pointer_diff(f: &FileDiff) -> Option<(Option<LfsPointer>, Option<LfsPointer>)> {
    let side = |skip: u8| {
        let text: Vec<&str> = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind != skip && l.kind != b'\\').map(|l| l.text.as_str()).collect();
        parse_pointer(&text.join("\n"))
    };
    let (old, new) = (side(b'+'), side(b'-'));
    (old.is_some() || new.is_some()).then_some((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_diff;

    #[test]
    fn pointer_diffs() {
        let d = "diff --git a/h.psd b/h.psd\nindex 1..2 100644\n--- a/h.psd\n+++ b/h.psd\n@@ -1,3 +1,3 @@\n version https://git-lfs.github.com/spec/v1\n-oid sha256:aa\n-size 10\n+oid sha256:bb\n+size 2048\n";
        let (old, new) = pointer_diff(&parse_diff(d)[0]).unwrap();
        assert_eq!(old, Some(LfsPointer { oid: "sha256:aa".into(), size: 10 }));
        assert_eq!(new, Some(LfsPointer { oid: "sha256:bb".into(), size: 2048 }));
        // A new pointer file: nothing on the old side.
        let d = "diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,3 @@\n+version https://git-lfs.github.com/spec/v1\n+oid sha256:cc\n+size 1\n";
        assert_eq!(pointer_diff(&parse_diff(d)[0]), Some((None, Some(LfsPointer { oid: "sha256:cc".into(), size: 1 }))));
        assert!(pointer_diff(&parse_diff("diff --git a/f b/f\n@@ -1 +1 @@\n-a\n+b\n")[0]).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_gitattributes_symlink_to_an_endless_file_is_ignored() {
        let dir = std::env::temp_dir().join(format!("glasir-lfs-zero-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("/dev/zero", dir.join(".gitattributes")).unwrap();
        assert!(!lfs(&dir).used);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lfs_files_from_attributes() {
        let dir = std::env::temp_dir().join(format!("glasir-lfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        run(&dir, &["init", "-q"]).unwrap();
        assert!(!lfs(&dir).used);
        std::fs::write(dir.join(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        for f in ["a.psd", "assets/b.psd", "c.txt"] {
            std::fs::write(dir.join(f), "x\n").unwrap();
        }
        run(&dir, &["add", "a.psd"]).unwrap();
        let l = lfs(&dir);
        let mut files: Vec<_> = l.files.into_iter().collect();
        files.sort();
        assert!(l.used);
        assert_eq!(files, ["a.psd", "assets/b.psd"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
