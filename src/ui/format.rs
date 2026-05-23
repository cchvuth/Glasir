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

#[cfg(test)]
mod tests {
    #[test]
    fn byte_sizes() {
        assert_eq!(super::bytes(512), "512 B");
        assert_eq!(super::bytes(1572864), "1.5 MB");
        assert_eq!(super::bytes(3 << 40), "3.0 TB");
    }
}
