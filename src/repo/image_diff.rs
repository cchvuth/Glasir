//! Image diffs: both sides of a changed image, read with `git cat-file` (or
//! from disk) and decoded into textures off the UI thread.

use super::action::DiffSrc;
use crate::git::{self, Res};
use std::path::Path;

const MAX_BYTES: u64 = 50 << 20;
/// Bigger images are scaled down for the texture (GPUs cap texture sizes).
const MAX_SIDE: u32 = 4096;

pub struct Side {
    pub tex: egui::TextureHandle,
    /// Original width and height in pixels.
    pub size: [u32; 2],
    pub bytes: u64,
}

/// `None`: the side doesn't exist (added / deleted); `Err`: too big or not decodable.
pub struct ImageDiff {
    pub old: Option<Res<Side>>,
    pub new: Option<Res<Side>>,
}

pub fn is_image(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |e| e.1).to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
}

/// Where a side's bytes are: a `git cat-file` spec (`rev:path`, `:path` = index), or `None` for the working tree.
fn sides(src: &DiffSrc) -> [Option<Option<String>>; 2] {
    let c = src.change();
    let old_path = c.old_path.as_deref().unwrap_or(&c.path);
    let old = |rev: &str| (!matches!(c.code, 'A' | '?')).then(|| Some(format!("{rev}:{old_path}")));
    let new = |spec: Option<String>| (c.code != 'D').then_some(spec);
    match src {
        DiffSrc::Worktree { staged: true, .. } => [old("HEAD"), new(Some(format!(":{}", c.path)))],
        DiffSrc::Worktree { staged: false, .. } => [old(""), new(None)],
        DiffSrc::Commit { id, parent, .. } => [parent.as_deref().and_then(old), new(Some(format!("{id}:{}", c.path)))],
    }
}

fn load_side(ctx: &egui::Context, dir: &Path, path: &str, spec: Option<String>) -> Res<Side> {
    let too_big = |n: u64| if n > MAX_BYTES { Err(format!("Too large to preview ({} MB)", n >> 20)) } else { Ok(()) };
    let bytes = match &spec {
        Some(spec) => {
            too_big(git::run(dir, &["cat-file", "-s", spec])?.trim().parse().unwrap_or(0))?;
            git::run_raw(dir, &["cat-file", "blob", spec], None)?
        }
        None => {
            too_big(std::fs::metadata(dir.join(path)).map_err(|e| e.to_string())?.len())?;
            std::fs::read(dir.join(path)).map_err(|e| e.to_string())?
        }
    };
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Can't decode image: {e}"))?;
    let size = [img.width(), img.height()];
    let img = if size[0].max(size[1]) > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img }.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    let tex = ctx.load_texture(format!("{path}@{}", spec.as_deref().unwrap_or("")), color, egui::TextureOptions::LINEAR);
    Ok(Side { tex, size, bytes: bytes.len() as u64 })
}

pub fn load(ctx: &egui::Context, dir: &Path, src: &DiffSrc) -> ImageDiff {
    let [old, new] = sides(src);
    let c = src.change();
    let old_path = c.old_path.as_deref().unwrap_or(&c.path);
    ImageDiff { old: old.map(|s| load_side(ctx, dir, old_path, s)), new: new.map(|s| load_side(ctx, dir, &c.path, s)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Change;
    use crate::repo::testutil::{identity, scratch, sh};

    #[test]
    fn image_sides_load_from_git_and_disk() {
        let dir = scratch("imgdiff");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let png = |w, h| {
            let mut b = vec![];
            image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
            b
        };
        std::fs::write(dir.join("a.png"), png(4, 3)).unwrap();
        sh(&dir, &["add", "a.png"]);
        sh(&dir, &["commit", "-q", "-m", "add"]);
        std::fs::write(dir.join("a.png"), png(8, 2)).unwrap();
        let ctx = egui::Context::default();
        let change = Change { path: "a.png".into(), old_path: None, code: 'M' };
        let dims = |d: &ImageDiff| [&d.old, &d.new].map(|s| s.as_ref().map(|s| s.as_ref().unwrap().size));

        let d = load(&ctx, &dir, &DiffSrc::Worktree { change: change.clone(), staged: false });
        assert_eq!(dims(&d), [Some([4, 3]), Some([8, 2])]);
        sh(&dir, &["add", "a.png"]);
        let d = load(&ctx, &dir, &DiffSrc::Worktree { change: change.clone(), staged: true });
        assert_eq!(dims(&d), [Some([4, 3]), Some([8, 2])]);
        // A root commit's added image has no old side.
        let root = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let d = load(&ctx, &dir, &DiffSrc::Commit { id: root, parent: None, change: Change { code: 'A', ..change } });
        assert_eq!(dims(&d), [None, Some([4, 3])]);
        assert!(is_image("x/Y.JPG") && !is_image("a.svg") && !is_image("png"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
