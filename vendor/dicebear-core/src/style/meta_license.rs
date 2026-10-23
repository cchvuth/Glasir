//! Read-only view over a style definition's `meta.license` block.

use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(crate) struct MetaLicense {
    name: Option<String>,
    url: Option<String>,
    text: Option<String>,
}

impl MetaLicense {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    #[allow(dead_code)]
    pub(crate) fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}
