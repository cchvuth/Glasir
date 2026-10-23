//! Lazily-constructed view over a style definition's `meta` block.

use serde::Deserialize;

use super::meta_creator::MetaCreator;
use super::meta_license::MetaLicense;
use super::meta_source::MetaSource;

#[derive(Deserialize, Default)]
pub(crate) struct Meta {
    #[serde(default)]
    license: MetaLicense,
    #[serde(default)]
    creator: MetaCreator,
    #[serde(default)]
    source: MetaSource,
}

impl Meta {
    pub(crate) fn license(&self) -> &MetaLicense {
        &self.license
    }

    pub(crate) fn creator(&self) -> &MetaCreator {
        &self.creator
    }

    pub(crate) fn source(&self) -> &MetaSource {
        &self.source
    }
}
