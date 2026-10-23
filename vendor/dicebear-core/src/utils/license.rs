//! Builds attribution strings and embedded RDF/Dublin Core metadata from a
//! style's `meta` block.

use crate::style::Meta;
use crate::utils::xml::escape;

/// Builds an embedded `<metadata>` block with Dublin Core terms, or an empty
/// string when no metadata fields are populated.
pub(crate) fn xml(meta: Option<&Meta>) -> String {
    let rights = text(meta);

    let Some(meta) = meta else {
        return String::new();
    };

    let title = non_empty(meta.source().name());
    let creator_name = non_empty(meta.creator().name());
    let source_url = non_empty(meta.source().url());
    let license_url = non_empty(meta.license().url());

    if title.is_none()
        && creator_name.is_none()
        && source_url.is_none()
        && license_url.is_none()
        && rights.is_empty()
    {
        return String::new();
    }

    let mut fields = String::new();

    if let Some(title) = title {
        fields.push_str(&format!("<dc:title>{}</dc:title>", escape(title)));
    }
    if let Some(creator_name) = creator_name {
        fields.push_str(&format!(
            "<dc:creator>{}</dc:creator>",
            escape(creator_name)
        ));
    }
    if let Some(source_url) = source_url {
        fields.push_str(&format!(
            "<dc:source xsi:type=\"dcterms:URI\">{}</dc:source>",
            escape(source_url)
        ));
    }
    if let Some(license_url) = license_url {
        fields.push_str(&format!(
            "<dcterms:license xsi:type=\"dcterms:URI\">{}</dcterms:license>",
            escape(license_url)
        ));
    }
    if !rights.is_empty() {
        fields.push_str(&format!("<dc:rights>{}</dc:rights>", escape(&rights)));
    }

    format!(
        "<metadata xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
xmlns:dcterms=\"http://purl.org/dc/terms/\">\
<rdf:RDF><rdf:Description>{fields}</rdf:Description></rdf:RDF></metadata>"
    )
}

/// Returns a single-line attribution string, or an empty string when no
/// attribution data is available.
fn text(meta: Option<&Meta>) -> String {
    let Some(meta) = meta else {
        return String::new();
    };

    let source_name = non_empty(meta.source().name());
    let source_url = non_empty(meta.source().url());
    let creator_name_raw = meta.creator().name();
    let creator_name = non_empty(creator_name_raw);
    let license_name = non_empty(meta.license().name());
    let license_url = non_empty(meta.license().url());

    if source_name.is_none() && creator_name.is_none() && license_name.is_none() {
        return String::new();
    }

    let mut title = match source_name {
        Some(s) => format!("\u{201c}{s}\u{201d}"),
        None => "Design".to_string(),
    };

    if let Some(url) = source_url {
        title.push_str(&format!(" ({url})"));
    }

    // JS uses `creatorName ?? 'Unknown'` (nullish): an empty creator name stays
    // empty; only a missing one becomes "Unknown".
    let creator = format!("\u{201c}{}\u{201d}", creator_name_raw.unwrap_or("Unknown"));

    let mut result = String::new();

    // Skip the "Remix of" prefix for MIT-licensed or DiceBear-original styles.
    if license_name != Some("MIT") && creator_name != Some("DiceBear") && source_name.is_some() {
        result.push_str("Remix of ");
    }

    result.push_str(&format!("{title} by {creator}"));

    if let Some(license_name) = license_name {
        result.push_str(&format!(", licensed under \u{201c}{license_name}\u{201d}"));

        if let Some(license_url) = license_url {
            result.push_str(&format!(" ({license_url})"));
        }
    }

    result
}

/// Treats an empty string as absent, mirroring the JS falsy checks (`!sourceName`
/// is true for both a missing field and `""`).
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}
