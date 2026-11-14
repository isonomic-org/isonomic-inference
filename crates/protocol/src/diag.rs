//! Diagnostics, protocol errors, and the hub's error kinds.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A note that a translation was not exact. Nothing is dropped silently:
/// every lossy step returns one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diag {
    pub kind: DiagKind,
    /// Where in the conversation, e.g. `turns[2].blocks[0]`.
    pub at: String,
    pub detail: String,
}

impl Diag {
    pub fn new(kind: DiagKind, at: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            at: at.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagKind {
    /// The format has no place for this; it was left out.
    Dropped,
    /// Translated to the nearest value the format has.
    Approximated,
    /// Moved to another place the format allows.
    Relocated,
    /// The format requires a value the conversation did not give.
    Defaulted,
    /// An identifier was rewritten to fit the format's shape.
    IdMapped,
    /// Something in a stream or response the hub has no name for.
    Unrecognized,
}

/// A request or stream this crate cannot translate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Malformed JSON, or JSON of the wrong shape.
    Json(String),
    /// A limit in [`crate::Limits`] was exceeded.
    Limit { what: &'static str, limit: usize },
    /// A blob the conversation refers to was not given to emit.
    MissingBlob(String),
    /// Valid JSON the format does not allow here.
    Invalid(String),
    /// The stream ended before the format's closing frame.
    Truncated,
    /// Data arrived after the closing frame.
    AfterEnd,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Json(e) => write!(f, "bad json: {e}"),
            ProtocolError::Limit { what, limit } => write!(f, "{what} over the limit of {limit}"),
            ProtocolError::MissingBlob(id) => write!(f, "blob {id} was not provided"),
            ProtocolError::Invalid(e) => write!(f, "invalid: {e}"),
            ProtocolError::Truncated => f.write_str("stream ended before its closing frame"),
            ProtocolError::AfterEnd => f.write_str("data after the closing frame"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        ProtocolError::Json(e.to_string())
    }
}

/// An error a model server reported, in the hub's terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubError {
    pub kind: ErrorKind,
    pub message: String,
    /// The format's own error type string, kept for lossless round trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    Authentication,
    PermissionDenied,
    NotFound,
    RequestTooLarge,
    RateLimited,
    Overloaded,
    Timeout,
    Server,
    Other,
}

impl ErrorKind {
    /// The kind an HTTP status implies, for errors whose body names none.
    pub fn from_status(status: u16) -> Self {
        match status {
            400 | 422 => ErrorKind::InvalidRequest,
            401 => ErrorKind::Authentication,
            403 => ErrorKind::PermissionDenied,
            404 => ErrorKind::NotFound,
            408 | 504 => ErrorKind::Timeout,
            413 => ErrorKind::RequestTooLarge,
            429 => ErrorKind::RateLimited,
            503 | 529 => ErrorKind::Overloaded,
            500..=599 => ErrorKind::Server,
            _ => ErrorKind::Other,
        }
    }
}
