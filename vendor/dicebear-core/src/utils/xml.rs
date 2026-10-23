//! Minimal XML escaping for SVG/XML text and attribute content.

/// Returns `value` with the five XML predefined entities escaped.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());

    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_the_five_entities() {
        assert_eq!(
            escape(r#"<a b="c" d='e' & f>"#),
            "&lt;a b=&quot;c&quot; d=&apos;e&apos; &amp; f&gt;"
        );
    }

    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(escape("hello world"), "hello world");
    }
}
