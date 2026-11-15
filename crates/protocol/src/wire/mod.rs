//! The wire formats. Each implements [`crate::Wire`].

pub mod chat;
pub mod messages;
pub mod responses;

use crate::diag::{ErrorKind, HubError};

/// An error body that is not the format's error shape.
pub(crate) fn unparsed_error(status: Option<u16>, body: &[u8]) -> HubError {
    let text = String::from_utf8_lossy(body);
    HubError {
        kind: status.map_or(ErrorKind::Other, ErrorKind::from_status),
        message: text.chars().take(1000).collect(),
        raw_type: None,
    }
}
