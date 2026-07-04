//! Patches: commits as `git format-patch` mbox text (copied or saved to a
//! file), and applying a patch file (`git am` for mbox, `git apply` for plain diffs).

use super::{Msg, Op, RepoTab, Task, Undo};
use crate::git::{self, Res};
use std::path::{Path, PathBuf};

/// The commits (newest first, as selected) as one mbox, oldest first.
pub(super) fn format_patch(dir: &Path, ids: &[String]) -> Res<Vec<u8>> {
    if ids.is_empty() {
        return Err("No commits to export.".into());
    }
    // `format-patch -1 <merge>` silently exports the first non-merge commit below it.
    let merges = git::run(dir, &[&["rev-list", "--no-walk", "--min-parents=2"][..], &ids.iter().map(String::as_str).collect::<Vec<_>>()].concat())?;
    if !merges.trim().is_empty() {
        return Err("Merge commits can't be exported as patches.".into());
    }
    let mut out = vec![];
    for id in ids.iter().rev() {
        out.extend(git::run_raw(dir, &["format-patch", "-1", "--stdout", id], None)?);
    }
    Ok(out)
}

/// `git am` for a mailbox (format-patch output), `git apply` for a plain diff.
pub(super) fn apply_op(path: &Path, text: &[u8]) -> Op {
    let p = path.to_string_lossy();
    if text.starts_with(b"From ") {
        Op::new("Apply patch as commits", &["am", "--3way", &p]).undo(Undo::Keep)
    } else {
        Op::new("Apply patch", &["apply", "--3way", &p])
    }
}

impl RepoTab {
    /// Copy the commits as a patch, or save it to a file the user picks.
    pub(super) fn export_patch(&mut self, ctx: &egui::Context, ids: Vec<String>, save: bool) {
        let Some(newest) = ids.first() else { return };
        let what = if ids.len() == 1 { newest[..newest.len().min(7)].to_owned() } else { format!("{} commits", ids.len()) };
        let path = match save {
            true => match rfd::FileDialog::new().set_title("Save patch").set_file_name(format!("{}.patch", what.replace(' ', "-"))).save_file() {
                Some(p) => Some(p),
                None => return,
            },
            false => None,
        };
        let dir = self.dir.clone();
        self.patch_task = Some(Task::spawn(ctx, move || {
            let bytes = format_patch(&dir, &ids)?;
            match path {
                Some(p) => std::fs::write(&p, bytes).map(|_| (None, format!("Saved {what} to {}", p.display()))).map_err(|e| e.to_string()),
                // The clipboard takes text only: lossy conversion would corrupt non-UTF-8 lines.
                None => match String::from_utf8(bytes) {
                    Ok(s) => Ok((Some(s), format!("Copied {what} as a patch"))),
                    Err(_) => Err("This patch contains non-UTF-8 content, so it can't be copied as text. Use Save as patch… instead.".into()),
                },
            }
        }));
    }

    pub(super) fn poll_patch(&mut self, ctx: &egui::Context) {
        match Task::poll(&mut self.patch_task) {
            Some(Ok((copy, text))) => {
                if let Some(c) = copy {
                    ctx.copy_text(c);
                }
                self.notes.push(Msg { text, error: false });
            }
            Some(Err(e)) => self.notes.push(Msg { text: format!("Patch failed:\n{e}"), error: true }),
            None => {}
        }
    }

    pub(super) fn apply_patch(&mut self, ctx: &egui::Context) {
        let Some(p): Option<PathBuf> = rfd::FileDialog::new().set_title("Apply patch").pick_file() else { return };
        match std::fs::read(&p) {
            Ok(text) => self.run(ctx, apply_op(&p, &text)),
            Err(e) => self.notes.push(Msg { text: format!("Can't read {}: {e}", p.display()), error: true }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::op::exec;
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn patches_round_trip() {
        let dir = scratch("patch");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "a.txt", "base\nfeat 1\n");
        commit(&dir, "b.txt", "feat 2\n");
        let ids: Vec<String> = sh(&dir, &["rev-list", "-2", "HEAD"]).lines().map(String::from).collect(); // newest first
        let mbox = format_patch(&dir, &ids).unwrap();
        let text = String::from_utf8_lossy(&mbox);
        assert!(text.find("feat 1").unwrap() < text.find("feat 2").unwrap(), "oldest first");
        let files = scratch("patch-files");
        std::fs::write(files.join("p.mbox"), &mbox).unwrap();
        let plain = git::run_raw(&dir, &["diff", "main", "feat"], None).unwrap();
        std::fs::write(files.join("p.diff"), &plain).unwrap();

        // A mailbox becomes commits.
        sh(&dir, &["checkout", "-q", "-b", "applied", "main"]);
        let op = apply_op(&files.join("p.mbox"), &mbox);
        assert_eq!(op.cmds[0][0], "am");
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-list", "--count", "main..HEAD"]), "2\n");
        assert_eq!(sh(&dir, &["diff", "feat", "HEAD"]), "");
        // A plain diff lands in the working tree (and index, with --3way).
        sh(&dir, &["checkout", "-q", "-b", "plain", "main"]);
        exec(&dir, &apply_op(&files.join("p.diff"), &plain)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "feat 2\n");
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), sh(&dir, &["rev-parse", "main"]));
        assert!(format_patch(&dir, &[]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&files);
    }

    /// A merge would export a parent instead; non-UTF-8 lines can't go on the clipboard intact.
    #[test]
    fn merges_and_non_utf8_are_refused() {
        let dir = scratch("patch-merge");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "side"]);
        commit(&dir, "s", "side\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "m", "m\n");
        sh(&dir, &["merge", "-q", "--no-edit", "side"]);
        let merge = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let side = sh(&dir, &["rev-parse", "side"]).trim().to_owned();
        assert!(format_patch(&dir, &[merge.clone()]).is_err());
        assert!(format_patch(&dir, &[merge, side.clone()]).is_err());
        assert!(format_patch(&dir, &[side]).is_ok());

        std::fs::write(dir.join("l1"), b"caf\xe9\n").unwrap();
        sh(&dir, &["add", "l1"]);
        sh(&dir, &["commit", "-q", "-m", "latin-1"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, crate::repo::View::default());
        t.export_patch(&ctx, vec![sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned()], false);
        while t.patch_task.is_some() {
            t.poll_patch(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(t.notes.last().is_some_and(|m| m.error && m.text.contains("non-UTF-8")), "{:?}", t.notes.last().map(|m| &m.text));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
