//! Read-only view over a component's `translate` block.

use serde::Deserialize;

use crate::prng::Range;

#[derive(Deserialize, Default)]
pub(crate) struct ComponentTranslate {
    x: Option<Range>,
    y: Option<Range>,
}

impl ComponentTranslate {
    pub(crate) fn x(&self) -> Option<Range> {
        self.x
    }

    pub(crate) fn y(&self) -> Option<Range> {
        self.y
    }
}
