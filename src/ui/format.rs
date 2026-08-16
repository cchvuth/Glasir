//! Small text formatters shared by the graph, inspector, history and diff views.

pub fn initials(name: &str) -> String {
    let mut w = name.split_whitespace().filter_map(|w| w.chars().next());
    let a = w.next().unwrap_or('?');
    match w.last() {
        Some(b) => format!("{a}{b}"),
        None => a.to_string(),
    }
    .to_uppercase()
}

fn age(t: i64) -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64) - t
}

/// "5 mins ago" … "3 years ago".
pub fn ago(t: i64) -> String {
    let f = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match age(t) {
        d if d < 60 => "just now".into(),
        d if d < 3600 => f(d / 60, "min"),
        d if d < 86400 => f(d / 3600, "hour"),
        d if d < 30 * 86400 => f(d / 86400, "day"),
        d if d < 365 * 86400 => f(d / (30 * 86400), "month"),
        d => f(d / (365 * 86400), "year"),
    }
}

/// Relative for the last week, then `fallback` (an absolute date).
pub fn relative_time(t: i64, fallback: &str) -> String {
    if age(t) < 7 * 86400 { ago(t) } else { fallback.into() }
}

/// "3 modified, 1 added, 2 deleted".
pub fn change_summary(files: &[crate::git::Change]) -> String {
    let kinds: [(&[char], &str); 6] = [(&['M'], "modified"), (&['A', '?'], "added"), (&['D'], "deleted"), (&['R'], "renamed"), (&['C'], "copied"), (&['T'], "type changed")];
    let parts: Vec<_> = kinds.iter().map(|(cs, w)| (files.iter().filter(|c| cs.contains(&c.code)).count(), w)).filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
    parts.join(", ")
}

/// 512 -> "512 B", 1572864 -> "1.5 MB".
pub fn bytes(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 {
            return if unit == "B" { format!("{n} B") } else { format!("{v:.1} {unit}") };
        }
        v /= 1024.0;
    }
    format!("{v:.1} TB")
}

/// Visual lines `chars` monospace characters take in `cols` columns (`usize::MAX`: no wrapping).
/// `chars` is lazy: counting every line's characters each frame is too slow when not wrapping.
pub fn wrapped(chars: impl FnOnce() -> usize, cols: usize) -> usize {
    if cols == usize::MAX { 1 } else { chars().div_ceil(cols.max(1)).max(1) }
}

/// Tops of consecutive rows of `lines` visual lines of height `h` each, then the total.
pub fn tops(h: f32, lines: impl Iterator<Item = usize>) -> Vec<f32> {
    let mut y = 0.0;
    std::iter::once(0.0).chain(lines.map(|n| { y += n as f32 * h; y })).collect()
}

pub fn visible(tops: &[f32], y0: f32, y1: f32) -> std::ops::Range<usize> {
    let n = tops.len() - 1;
    tops.partition_point(|&t| t <= y0).saturating_sub(1).min(n)..tops.partition_point(|&t| t < y1).min(n)
}

pub fn text_job(text: &str, font: egui::FontId, color: egui::Color32, h: f32, cols: usize, cw: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(text, 0.0, egui::TextFormat { line_height: Some(h), ..egui::TextFormat::simple(font, color) });
    wrap_job(&mut job, cols, cw);
    job
}

/// Wraps `job` at `cols` columns (`usize::MAX`: none).
pub fn wrap_job(job: &mut egui::text::LayoutJob, cols: usize, cw: f32) {
    if cols != usize::MAX {
        job.wrap.max_width = (cols as f32 + 0.5) * cw;
        job.wrap.break_anywhere = true;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wrapped_rows() {
        let w = |n: usize, cols| super::wrapped(|| n, cols);
        assert_eq!((w(0, 10), w(10, 10), w(11, 10), super::wrapped(|| unreachable!(), usize::MAX)), (1, 1, 2, 1));
        let t = super::tops(10.0, [1, 3, 1].into_iter());
        assert_eq!(t, [0.0, 10.0, 40.0, 50.0]);
        assert_eq!(super::visible(&t, 0.0, 5.0), 0..1);
        assert_eq!(super::visible(&t, 15.0, 45.0), 1..3);
        assert_eq!(super::visible(&t, 40.0, 100.0), 2..3);
        assert_eq!(super::visible(&super::tops(10.0, std::iter::empty()), 0.0, 100.0), 0..0);
    }

    #[test]
    fn summary_counts() {
        let c = |code| crate::git::Change { path: "p".into(), old_path: None, code };
        assert_eq!(super::change_summary(&[c('M'), c('A'), c('M'), c('D'), c('D'), c('M'), c('?')]), "3 modified, 2 added, 2 deleted");
        assert_eq!(super::change_summary(&[]), "");
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(super::bytes(512), "512 B");
        assert_eq!(super::bytes(1572864), "1.5 MB");
        assert_eq!(super::bytes(3 << 40), "3.0 TB");
    }
}
