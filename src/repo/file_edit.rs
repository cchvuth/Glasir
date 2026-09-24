//! Editing a changed working-tree file in place (state + save; the window is
//! `ui/file_edit.rs`). Saving is a backed-up op, so ⌘Z restores the previous content.

use super::{Action, Backup, Op};
use crate::git::Res;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

/// `FILE_SAVE path fingerprint` (new bytes on stdin): writes the file unless it no longer
/// has the content the editor loaded.
pub(super) const FILE_SAVE: &str = "!file-save";
/// The save op's label (`poll_op` closes the editor when it succeeds).
pub(super) const SAVE: &str = "Save file";
/// Bigger files are not edited here.
const MAX: u64 = 2 << 20;
/// Syntax colors are recomputed on every edit: only for files up to this size.
pub const HIGHLIGHT_MAX: usize = 100_000;

pub struct FileEdit {
    pub path: String,
    /// The text as edited, with `\n` line ends (put back to CRLF on save).
    pub text: String,
    base: String,
    crlf: bool,
    /// Fingerprint of the file's bytes when loaded (or when the user chose to overwrite).
    original: u64,
    seen: (Option<SystemTime>, u64),
    /// The file changed on disk since it was loaded: Reload / Overwrite / Keep editing.
    pub conflict: bool,
    pub saving: bool,
    /// Unsaved-changes dialog: what the user was about to do (`Some(None)`: just leaving).
    pub ask: Option<Option<Action>>,
    /// Run once the save succeeds.
    pub then: Option<Action>,
    /// Syntax colors of `text` (key: its fingerprint and the theme).
    pub hl: Option<((u64, bool), crate::highlight::Spans)>,
}

fn fingerprint(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

pub fn text_key(t: &str) -> u64 {
    fingerprint(t.as_bytes())
}

fn stat(p: &Path) -> (Option<SystemTime>, u64) {
    std::fs::metadata(p).map_or((None, 0), |m| (m.modified().ok(), m.len()))
}

impl FileEdit {
    /// Opens `path` for editing, or says why it can't be (the note is shown as is).
    pub fn load(dir: &Path, path: &str) -> Res<FileEdit> {
        let no = |why: &str| Err(format!("Can't edit {path}: {why}."));
        if !Path::new(path).components().all(|c| matches!(c, std::path::Component::Normal(_))) {
            return no("the path is outside the repository");
        }
        let p = dir.join(path);
        let Some(m) = std::fs::symlink_metadata(&p).ok().filter(|m| m.is_file()) else { return no("it is not a regular file (missing, symlink or submodule)") };
        if m.len() > MAX {
            return no("it is larger than 2 MB, so it is read-only here (use Open in editor)");
        }
        let bytes = std::fs::read(&p).map_err(|e| format!("Can't edit {path}: {e}"))?;
        if bytes.contains(&0) {
            return no("it is a binary file");
        }
        let Ok(raw) = std::str::from_utf8(&bytes) else { return no("it is not UTF-8 text") };
        if raw.starts_with("version https://git-lfs.github.com/spec") {
            return no("it is a Git LFS pointer");
        }
        let (crlf_n, lf_n) = (raw.matches("\r\n").count(), raw.matches('\n').count());
        if crlf_n > 0 && crlf_n != lf_n {
            return no("it mixes CRLF and LF line endings");
        }
        let text = if crlf_n > 0 { raw.replace("\r\n", "\n") } else { raw.to_owned() };
        if text.contains('\r') {
            return no("it has stray carriage returns");
        }
        Ok(FileEdit {
            path: path.to_owned(),
            base: text.clone(),
            text,
            crlf: crlf_n > 0,
            original: fingerprint(&bytes),
            seen: stat(&p),
            conflict: false,
            saving: false,
            ask: None,
            then: None,
            hl: None,
        })
    }

    pub fn dirty(&self) -> bool {
        self.text != self.base
    }

    pub fn crlf(&self) -> bool {
        self.crlf
    }

    /// Flags `conflict` when the file on disk is no longer what was loaded (checked when
    /// its size or mtime moved).
    pub fn check(&mut self, dir: &Path) {
        let now = stat(&dir.join(&self.path));
        if now != self.seen {
            self.seen = now;
            self.conflict = !self.on_disk(dir);
        }
    }

    fn on_disk(&self, dir: &Path) -> bool {
        std::fs::read(dir.join(&self.path)).is_ok_and(|b| fingerprint(&b) == self.original)
    }

    /// "Overwrite": keep the edits and take the file as it is now as the base to replace.
    pub fn overwrite(&mut self, dir: &Path) {
        let p = dir.join(&self.path);
        self.original = fingerprint(&std::fs::read(&p).unwrap_or_default());
        self.seen = stat(&p);
        self.conflict = false;
    }

    /// The save as an op, or None while the file on disk has changed (`conflict` is then set).
    pub fn save_op(&mut self, dir: &Path) -> Option<Op> {
        self.seen = (None, 0); // force the disk check
        self.check(dir);
        if self.conflict {
            return None;
        }
        // Pasted text may carry its own \r\n: normalize before putting the file's line ends back.
        let lf = self.text.replace('\r', "");
        let bytes = if self.crlf { lf.replace('\n', "\r\n") } else { lf };
        Some(Op::new(SAVE, &[FILE_SAVE, &self.path, &self.original.to_string()]).input(bytes).light().backup(Backup::Paths(vec![self.path.clone()])))
    }
}

/// The save command: re-checks (off the UI thread, right before writing) that the file is
/// still the one loaded, then overwrites it in place (mode and ownership stay).
pub(super) fn save(dir: &Path, a: &[&str], input: &[u8]) -> Res<String> {
    let [_, path, want] = a else { return Err("bad save".into()) };
    let p = dir.join(path);
    match std::fs::symlink_metadata(&p) {
        Ok(m) if !m.is_file() => return Err(format!("{path} is not a regular file.")),
        _ => {}
    }
    if fingerprint(&std::fs::read(&p).unwrap_or_default()).to_string() != *want {
        return Err(format!("{path} changed on disk since the editor loaded it, so it wasn't saved. Open it again."));
    }
    // Temp file + rename: a failure mid-write leaves the original untouched (mode is copied).
    let tmp = p.with_file_name(format!(".{}.glasir-tmp", p.file_name().unwrap_or_default().to_string_lossy()));
    let w = || -> std::io::Result<()> {
        std::fs::write(&tmp, input)?;
        std::fs::set_permissions(&tmp, std::fs::metadata(&p)?.permissions())?;
        std::fs::rename(&tmp, &p)
    };
    w().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not write {path}: {e}")
    })?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::op::exec;
    use crate::repo::testutil::{commit, identity, run_backed, scratch, sh, undo_files};

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        dir
    }

    #[test]
    fn save_and_undo_restore_the_previous_content() {
        let dir = repo("fe_save");
        commit(&dir, "a.txt", "one\ntwo\n");
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut ed = FileEdit::load(&dir, "a.txt").unwrap();
        assert!(!ed.dirty());
        ed.text.push_str("four\n");
        assert!(ed.dirty());
        let op = ed.save_op(&dir).unwrap();
        assert!(op.backup.is_some());
        let saved = run_backed(&dir, &op).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\nthree\nfour\n");
        undo_files(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crlf_missing_final_newline_and_mode_survive() {
        let dir = repo("fe_crlf");
        std::fs::write(dir.join("w.txt"), "a\r\nb\r\nc").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("w.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut ed = FileEdit::load(&dir, "w.txt").unwrap();
        assert!(ed.crlf() && ed.text == "a\nb\nc");
        ed.text = "a\nB\nc\nd".into();
        exec(&dir, &ed.save_op(&dir).unwrap()).unwrap();
        assert_eq!(std::fs::read(dir.join("w.txt")).unwrap(), b"a\r\nB\r\nc\r\nd");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("w.txt")).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_what_it_cannot_round_trip() {
        let dir = repo("fe_refuse");
        std::fs::write(dir.join("bin"), b"a\0b").unwrap();
        std::fs::write(dir.join("latin"), b"caf\xe9\n").unwrap();
        std::fs::write(dir.join("mixed"), "a\r\nb\nc\n").unwrap();
        std::fs::write(dir.join("lfs"), "version https://git-lfs.github.com/spec/v1\noid sha256:aa\nsize 1\n").unwrap();
        std::fs::write(dir.join("big"), vec![b'x'; (MAX + 1) as usize]).unwrap();
        for f in ["bin", "latin", "mixed", "lfs", "big", "missing", "../x"] {
            let e = FileEdit::load(&dir, f).err().unwrap_or_default();
            assert!(e.starts_with(&format!("Can't edit {f}:")), "{f}: {e}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_changed_on_disk_is_not_overwritten() {
        let dir = repo("fe_changed");
        std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
        let mut ed = FileEdit::load(&dir, "a.txt").unwrap();
        ed.text = "edited\n".into();
        let stale = ed.save_op(&dir).unwrap(); // prepared before the outside change
        std::fs::write(dir.join("a.txt"), "theirs!\n").unwrap();
        // The op itself refuses ...
        assert!(exec(&dir, &stale).unwrap_err().contains("changed on disk"));
        // ... and so does preparing a new one: the banner shows instead.
        assert!(ed.save_op(&dir).is_none() && ed.conflict);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "theirs!\n");
        // Overwrite takes the current file as the base and goes through.
        ed.overwrite(&dir);
        exec(&dir, &ed.save_op(&dir).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "edited\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pasted_crlf_is_not_doubled() {
        let dir = repo("fe_paste");
        std::fs::write(dir.join("w.txt"), "a\r\nb\r\n").unwrap();
        let mut ed = FileEdit::load(&dir, "w.txt").unwrap();
        ed.text = "a\r\nb\n".into();
        exec(&dir, &ed.save_op(&dir).unwrap()).unwrap();
        assert_eq!(std::fs::read(dir.join("w.txt")).unwrap(), b"a\r\nb\r\n");
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("glasir-tmp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsaved_state_follows_the_text() {
        let dir = repo("fe_dirty");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut ed = FileEdit::load(&dir, "a.txt").unwrap();
        ed.text.push('y');
        assert!(ed.dirty());
        ed.text.pop();
        assert!(!ed.dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
