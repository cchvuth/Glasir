//! Stateless helpers shared across the core. Only [`color`](crate::color) is
//! part of the public API (re-exported at the crate root, mirroring the JS and
//! Python `Color`); the rest are crate-internal.

pub mod color;
pub(crate) mod initials;
pub(crate) mod license;
pub(crate) mod number;
pub(crate) mod xml;
