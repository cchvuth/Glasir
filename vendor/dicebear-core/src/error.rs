//! The error type returned when a style definition or options object is
//! malformed, fails schema validation, or contains a circular color reference.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The input could not be parsed as JSON or deserialized into the model.
    Parse(serde_json::Error),
    /// The input parsed but failed schema or alias validation.
    Validation(String),
    /// A color references itself, directly or indirectly. The vector holds the
    /// resolution path that closed the cycle.
    CircularColorReference(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "parse error: {e}"),
            Error::Validation(message) => write!(f, "validation error: {message}"),
            Error::CircularColorReference(chain) => {
                write!(f, "Circular color reference: {}", chain.join(" \u{2192} "))
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Parse(error)
    }
}
