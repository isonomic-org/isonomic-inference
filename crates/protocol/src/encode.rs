//! Shared pieces of encode: the encoder trait, its options, and the HTTP
//! status each error kind maps to.

use crate::decode::Delta;
use crate::diag::{ErrorKind, ProtocolError};

/// Turns hub deltas into a format's server-sent-events stream, frame by
/// frame. Each call returns the text to write; it may be empty.
pub trait Encoder {
    fn encode(&mut self, delta: &Delta) -> Result<String, ProtocolError>;
    /// Closing frames, if the stream has not been closed by a stop or an
    /// error. Call once, when the deltas end.
    fn finish(&mut self) -> String;
    /// A frame that keeps an idle connection open without changing the
    /// reply.
    fn keep_alive(&self) -> String;
}

/// What an encoder needs that the deltas do not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Creation time, in seconds since the Unix epoch, for formats that
    /// stamp one. The engine supplies it; this crate reads no clock.
    pub created: u64,
    /// Send token usage at the end of a chat-format stream.
    pub stream_usage: bool,
    /// Used when the deltas carry no reply ID.
    pub fallback_id: String,
    /// Used when the deltas carry no model name.
    pub fallback_model: String,
}

/// The HTTP status an error kind is served with.
pub fn status_of(kind: ErrorKind) -> u16 {
    match kind {
        ErrorKind::InvalidRequest => 400,
        ErrorKind::Authentication => 401,
        ErrorKind::PermissionDenied => 403,
        ErrorKind::NotFound => 404,
        ErrorKind::RequestTooLarge => 413,
        ErrorKind::RateLimited => 429,
        ErrorKind::Overloaded => 503,
        ErrorKind::Timeout => 504,
        ErrorKind::Server | ErrorKind::Other => 500,
    }
}

/// One server-sent event.
pub(crate) fn frame(event: Option<&str>, data: &str) -> String {
    match event {
        Some(event) => format!("event: {event}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// Tool calls in a reply, as a format that streams them one after another
/// needs them: a fragment for a call that is no longer the current one
/// cannot be placed.
pub(crate) fn interleaved(index: usize) -> ProtocolError {
    ProtocolError::Invalid(format!(
        "arguments for tool call {index} arrived after a later call began; \
         this format streams tool calls one at a time"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::chat::Chat;
    use crate::wire::messages::Messages;
    use crate::{WireClient, WireServer};

    #[test]
    fn every_kind_survives_encode_then_decode() {
        let kinds = [
            ErrorKind::InvalidRequest,
            ErrorKind::Authentication,
            ErrorKind::PermissionDenied,
            ErrorKind::NotFound,
            ErrorKind::RequestTooLarge,
            ErrorKind::RateLimited,
            ErrorKind::Overloaded,
            ErrorKind::Timeout,
            ErrorKind::Server,
        ];
        for kind in kinds {
            let error = crate::HubError {
                kind,
                message: "m".into(),
                raw_type: None,
            };
            let (status, body) = Messages::encode_error(&error);
            assert_eq!(Messages::decode_error(Some(status), &body).kind, kind);
            let (status, body) = Chat::encode_error(&error);
            assert_eq!(Chat::decode_error(Some(status), &body).kind, kind);
        }
    }
}
