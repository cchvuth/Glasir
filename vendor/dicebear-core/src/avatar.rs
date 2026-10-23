//! Top-level entry point for rendering an avatar from a style and options.

use std::fmt;

use serde_json::{json, Value};

use crate::error::Error;
use crate::options::Options;
use crate::renderer::Renderer;
use crate::resolver::Resolver;
use crate::style::Style;

/// A rendered avatar. Construction immediately resolves and renders the SVG;
/// the accessor methods return different serializations of that result.
pub struct Avatar {
    svg: String,
    resolved_options: Value,
}

impl Avatar {
    /// Validates `options`, then resolves and renders an avatar for `style`.
    /// `null` options are treated as empty; any other non-object value fails
    /// validation. Returns an error on invalid options or a circular color
    /// reference.
    pub fn new(style: &Style, options: Value) -> Result<Self, Error> {
        let options = if options.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            options
        };

        crate::validate::options(&options)?;

        let options = Options::new(options);
        let resolver = Resolver::new(style, &options);
        let svg = Renderer::new(style, &resolver).render()?;
        let resolved_options = resolver.resolved();

        Ok(Self {
            svg,
            resolved_options,
        })
    }

    /// Returns the rendered SVG markup.
    pub fn to_svg(&self) -> &str {
        &self.svg
    }

    /// Returns `{ "svg", "options" }` — the SVG and the resolved options used to
    /// render it. The raw seed is excluded from the resolved options.
    pub fn to_json(&self) -> Value {
        json!({ "svg": self.svg, "options": self.resolved_options })
    }

    /// Returns the SVG encoded as a `data:image/svg+xml` URI.
    pub fn to_data_uri(&self) -> String {
        format!(
            "data:image/svg+xml;charset=utf-8,{}",
            encode_uri_component(&self.svg)
        )
    }
}

impl fmt::Display for Avatar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.svg)
    }
}

/// Percent-encodes `s` exactly like JavaScript's `encodeURIComponent`: every
/// byte is escaped except the unreserved set `A-Za-z0-9-_.!~*'()`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());

    for byte in s.bytes() {
        let ch = byte as char;

        if ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }

    out
}
