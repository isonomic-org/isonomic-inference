//! Isonomic model protocol: one vocabulary for a conversation with a model
//! ([`Conversation`]) and pure translation between it and the wire formats
//! programs use to talk to models.
//!
//! This crate does no I/O: no network, files, processes or clocks. The
//! engine owns those and hands this crate bytes and values.
//!
//! Built so far, the client direction for two formats:
//! - **emit**: [`Conversation`] → a format's request ([`Wire::emit`]);
//! - **decode**: a format's stream or response → hub deltas and replies
//!   ([`StreamDecoder`], [`Wire::decode_response`], [`Wire::decode_error`]).
//!
//! The server direction (accept, encode) and the responses format come next.

pub mod conversation;
pub mod decode;
pub mod diag;
pub mod emit;
pub mod limits;
pub mod reasoning;
pub mod sse;
pub mod wire;

pub use conversation::*;
pub use decode::{Decoder, Delta, Reply, StreamDecoder};
pub use diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
pub use emit::{BlobStore, EmitOptions, Emitted, NoBlobs};
pub use limits::Limits;
pub use wire::chat::Chat;
pub use wire::messages::Messages;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// One wire format, client direction.
pub trait Wire {
    /// The format's name: its key in [`Extensions`] and in diagnostics.
    const NAME: &'static str;
    type Request: Serialize + DeserializeOwned;
    type Response: DeserializeOwned;
    type Event;
    type Decoder: Decoder<Event = Self::Event>;

    /// Conversation → wire request, with a diagnostic for every lossy step.
    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<Emitted<Self::Request>, ProtocolError>;

    fn decoder(limits: Limits) -> Self::Decoder;

    /// One server-sent event → the format's typed event.
    fn parse_event(frame: &sse::SseFrame, limits: &Limits) -> Result<Self::Event, ProtocolError>;

    fn decode_response(resp: Self::Response) -> Result<Reply, ProtocolError>;

    /// An error body (with its HTTP status, if known) → the hub's error.
    fn decode_error(status: Option<u16>, body: &[u8]) -> HubError;

    /// A whole response body → reply, under the depth and size limits.
    fn decode_response_body(body: &[u8], limits: &Limits) -> Result<Reply, ProtocolError> {
        if body.len() > limits.max_body_bytes {
            return Err(ProtocolError::Limit {
                what: "body bytes",
                limit: limits.max_body_bytes,
            });
        }
        let text = std::str::from_utf8(body)
            .map_err(|_| ProtocolError::Invalid("body is not UTF-8".into()))?;
        limits.check_text_depth(text)?;
        Self::decode_response(serde_json::from_str(text)?)
    }
}
