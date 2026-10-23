//! Read-only view over an entry in a style definition's `colors` block.

use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Color {
    values: Vec<String>,
    #[serde(rename = "notEqualTo", default)]
    not_equal_to: Vec<String>,
    #[serde(rename = "contrastTo")]
    contrast_to: Option<String>,
}

impl Color {
    pub(crate) fn values(&self) -> &[String] {
        &self.values
    }

    pub(crate) fn not_equal_to(&self) -> &[String] {
        &self.not_equal_to
    }

    pub(crate) fn contrast_to(&self) -> Option<&str> {
        self.contrast_to.as_deref()
    }
}
