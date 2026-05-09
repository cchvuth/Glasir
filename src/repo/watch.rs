//! File watcher: live refresh when the working tree or `.git` changes elsewhere.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

/// Watches the working tree and `.git` so edits made elsewhere show up live.
/// Sends `true` when refs/HEAD changed (full reload), `false` for status-only.
pub(super) fn watch(ctx: &egui::Context, dir: &Path) -> Option<(notify::RecommendedWatcher, Receiver<bool>)> {
    use notify::Watcher;
    let (tx, rx) = channel();
    let (ctx, root) = (ctx.clone(), dir.to_path_buf());
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(e) = res else { return };
        if matches!(e.kind, notify::EventKind::Access(_)) {
            return; // our own git reads (inotify IN_OPEN/CLOSE) are not changes
        }
        for p in &e.paths {
            let Ok(rel) = p.strip_prefix(&root) else { continue };
            let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
            let kind = match parts.next().as_deref() {
                Some(".git") => match parts.next().as_deref() {
                    Some("HEAD" | "refs" | "packed-refs" | "MERGE_HEAD" | "rebase-merge" | "rebase-apply" | "CHERRY_PICK_HEAD") => Some(true),
                    Some("index") => Some(false),
                    _ => None, // objects, logs, locks: noise
                },
                Some(_) => Some(false),
                None => None,
            };
            if let Some(full) = kind {
                let _ = tx.send(full);
                ctx.request_repaint_after(std::time::Duration::from_millis(450));
            }
        }
    })
    .ok()?;
    w.watch(dir, notify::RecursiveMode::Recursive).ok()?;
    Some((w, rx))
}
