//! Syntax highlighting for diffs (syntect, pure-Rust regex engine).

use crate::git::FileDiff;
use egui::Color32;
use std::ops::Range;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Colored byte ranges of one (tab-expanded) line.
pub type Spans = Vec<(Color32, Range<usize>)>;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

/// Tabs expanded, trailing CR removed, very long lines (minified code) cut:
/// exactly what the diff view paints.
pub fn display(t: &str) -> String {
    let s = t.replace('\t', "    ");
    let s = s.trim_end_matches('\r');
    match s.char_indices().nth(2000) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Characters of [`display`]`(t)`.
pub fn display_len(t: &str) -> usize {
    t.trim_end_matches('\r').chars().take(2001).map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>().min(2001)
}

/// `[hunk][line]` spans, or empty when the language is unknown or the diff is huge.
pub fn highlight(path: &str, f: &FileDiff, dark: bool) -> Vec<Vec<Spans>> {
    let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
    if lines > 20_000 || f.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text.len() > 20_000) {
        return vec![];
    }
    let ss = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    let first = f.hunks.first().and_then(|h| h.lines.first()).map_or("", |l| l.text.as_str());
    let Some(syntax) = ss.find_syntax_by_extension(ext).or_else(|| ss.find_syntax_by_first_line(first)) else {
        return vec![];
    };
    let ts = THEMES.get_or_init(ThemeSet::load_defaults);
    let theme = &ts.themes[if dark { "base16-ocean.dark" } else { "InspiredGitHub" }];
    f.hunks
        .iter()
        .map(|h| {
            let mut hl = HighlightLines::new(syntax, theme);
            h.lines
                .iter()
                .map(|l| {
                    let line = format!("{}\n", display(&l.text));
                    let mut pos = 0;
                    hl.highlight_line(&line, ss)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(style, s)| {
                            let r = pos..(pos + s.len()).min(line.len() - 1);
                            pos += s.len();
                            let c = style.foreground;
                            (Color32::from_rgb(c.r, c.g, c.b), r)
                        })
                        .filter(|(_, r)| !r.is_empty())
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Colored byte ranges covering all of `text` (the editor's raw text), or empty when the
/// language is unknown. Callers bound the size: this runs on every edit.
pub fn text(path: &str, text: &str, dark: bool) -> Spans {
    let ss = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(syntax) = ss.find_syntax_by_extension(ext).or_else(|| ss.find_syntax_by_first_line(text.lines().next().unwrap_or(""))) else {
        return vec![];
    };
    let ts = THEMES.get_or_init(ThemeSet::load_defaults);
    let mut hl = HighlightLines::new(syntax, &ts.themes[if dark { "base16-ocean.dark" } else { "InspiredGitHub" }]);
    let (mut out, mut pos) = (vec![], 0);
    for line in syntect::util::LinesWithEndings::from(text) {
        let spans = hl.highlight_line(line, ss).unwrap_or_else(|_| vec![(Default::default(), line)]);
        for (style, s) in spans {
            let c = style.foreground;
            out.push((Color32::from_rgb(c.r, c.g, c.b), pos..pos + s.len()));
            pos += s.len();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_diff;

    #[test]
    fn rust_diff_gets_colors() {
        let d = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn b() { let x = \"s\"; }\n";
        let f = &parse_diff(d)[0];
        let hl = highlight("x.rs", f, true);
        assert_eq!(hl.len(), 1);
        let spans = &hl[0][1];
        let text = display(&f.hunks[0].lines[1].text);
        assert!(spans.len() > 3, "{spans:?}");
        assert_eq!(spans.last().unwrap().1.end, text.len()); // covers the line, no trailing newline
        let colors: std::collections::HashSet<_> = spans.iter().map(|s| s.0).collect();
        assert!(colors.len() > 1);
    }

    #[test]
    fn text_spans_cover_everything() {
        let t = "fn a() {}\nlet x = \"s\";\n";
        let spans = text("x.rs", t, true);
        assert!(spans.len() > 3);
        assert_eq!(spans.first().unwrap().1.start, 0);
        assert_eq!(spans.last().unwrap().1.end, t.len());
        assert!(spans.windows(2).all(|w| w[0].1.end == w[1].1.start));
        assert!(text("x.unknownext", t, true).is_empty());
    }

    #[test]
    fn display_len_matches_display() {
        for t in ["", "a\tb\r", &"x".repeat(1999), &"x".repeat(2001), &"\t".repeat(600), &"y".repeat(5_000_000)] {
            assert_eq!(display_len(t), display(t).chars().count(), "{}", t.len());
        }
    }
}
