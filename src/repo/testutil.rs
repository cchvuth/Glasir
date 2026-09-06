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

/// Polls until the running op and the reload after it are done; returns the toasts.
pub(super) fn settle(ctx: &egui::Context, t: &mut super::RepoTab) -> Vec<super::Msg> {
    let mut msgs = vec![];
    for _ in 0..1000 {
        t.poll(ctx, &mut msgs);
        if t.running.is_none() && !t.loading() && t.data.is_some() && t.drop_task.is_none() {
            return msgs;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("op didn't finish");
}

/// Tests: runs `op` with its backup, as the app does; the files it changed.
pub(super) fn run_backed(dir: &Path, op: &super::Op) -> git::Res<Option<super::undo::Files>> {
    let before = op.backup.as_ref().map(|b| super::undo::backup(dir, b, &op.label)).transpose()?.flatten();
    super::op::exec(dir, op)?;
    before.map_or(Ok(None), |s| super::undo::backed_up(dir, s, &op.label))
}

/// Tests: the undo of those files alone; its note.
pub(super) fn undo_files(dir: &Path, f: &super::undo::Files) -> git::Res<Option<String>> {
    super::undo::transition(dir, "Undo", None, &[(f.after.clone(), f.before.clone(), f.before.paths.clone())]).map(|r| r.0)
}

/// Tests: a ref step's undo, from where the refs are now back to `to`.
pub(super) fn move_back(dir: &Path, to: &git::Snapshot, mode: super::Undo) -> git::Res<Option<String>> {
    super::undo::transition(dir, "Undo", Some((git::snapshot(dir)?, to.clone(), mode)), &[]).map(|r| r.0)
}
