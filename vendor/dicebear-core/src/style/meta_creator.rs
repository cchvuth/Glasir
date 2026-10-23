//! Read-only view over a style definition's `meta.creator` block.

use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(crate) struct MetaCreator {
    name: Option<String>,
    url: Option<String>,
}

impl MetaCreator {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[allow(dead_code)]
    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}
