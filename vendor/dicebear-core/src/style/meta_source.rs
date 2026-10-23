//! Read-only view over a style definition's `meta.source` block.

use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(crate) struct MetaSource {
    name: Option<String>,
    url: Option<String>,
}

impl MetaSource {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}
