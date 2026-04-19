//! Scratch-repo helpers shared by the repo tests.

use crate::git;
use std::path::{Path, PathBuf};

/// A fresh scratch dir under the system temp dir.
pub(super) fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glasir-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn sh(dir: &Path, args: &[&str]) -> String {
    git::run(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

pub(super) fn identity(dir: &Path) {
    sh(dir, &["config", "user.email", "t@t"]);
    sh(dir, &["config", "user.name", "T"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
}

pub(super) fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-q", "-m", text]);
}
