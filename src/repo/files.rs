//! Working-tree file helpers: reveal in file manager, append to .gitignore.

use std::path::Path;

/// Show a file in the system file manager. Never "opens" it: a repo file
/// could be a script or executable.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = open::that_detached(path.parent().unwrap_or(path));
}

/// Append `/<path>` to the repo's .gitignore (refuses to write through a symlink),
/// escaped so it matches only that file, not a glob of others.
pub fn append_gitignore(dir: &Path, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    if path.contains(['\n', '\r']) {
        return Err(std::io::Error::other("the file name contains a line break; edit .gitignore manually"));
    }
    let p = dir.join(".gitignore");
    if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other(".gitignore is a symlink; edit it manually"));
    }
    let mut pattern = String::new();
    for c in path.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '?') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    // git strips unescaped trailing spaces.
    if pattern.ends_with(' ') {
        pattern.insert(pattern.len() - 1, '\\');
    }
    let existing = std::fs::read(&p).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    if existing.last().is_some_and(|&b| b != b'\n') {
        writeln!(f)?;
    }
    writeln!(f, "/{pattern}")
}

#[cfg(test)]
mod tests {
    use crate::repo::testutil::{scratch, sh};

    #[test]
    fn gitignore_entries_match_only_that_file() {
        let dir = scratch("gitignore");
        sh(&dir, &["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), b"\xff").unwrap(); // not UTF-8, no final newline
        for p in ["pages/[id].tsx", "a*b", "notes "] {
            super::append_gitignore(&dir, p).unwrap();
        }
        assert!(super::append_gitignore(&dir, "x\ny").is_err());
        let ignored = |p: &str| crate::git::run(&dir, &["check-ignore", "--no-index", "-q", p]).is_ok();
        assert!(ignored("pages/[id].tsx") && ignored("a*b") && ignored("notes "));
        assert!(!ignored("pages/i.tsx") && !ignored("aXb") && !ignored("notes"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
