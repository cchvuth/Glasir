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

/// Append `/<path>` to the repo's .gitignore (refuses to write through a symlink).
pub fn append_gitignore(dir: &Path, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    let p = dir.join(".gitignore");
    if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other(".gitignore is a symlink; edit it manually"));
    }
    let existing = std::fs::read_to_string(&p).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(f)?;
    }
    writeln!(f, "/{path}")
}
