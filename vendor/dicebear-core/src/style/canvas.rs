//! Read-only view over a style definition's `canvas` block.

use serde::Deserialize;

use super::element::Element;

#[derive(Deserialize)]
pub(crate) struct Canvas {
    width: f64,
    height: f64,
    elements: Vec<Element>,
}

impl Canvas {
    pub(crate) fn width(&self) -> f64 {
        self.width
    }

    pub(crate) fn height(&self) -> f64 {
        self.height
    }

    pub(crate) fn elements(&self) -> &[Element] {
        &self.elements
    }
}
