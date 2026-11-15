//! Isonomic model protocol: one vocabulary for a conversation with a model
//! ([`Conversation`]) and pure translation between it and the wire formats
//! programs use to talk to models.
//!
//! This crate does no I/O: no network, files, processes or clocks. The
//! engine owns those and hands this crate bytes and values.
//!
//! Both directions, for each format:
//! - **emit**: [`Conversation`] → a format's request ([`Wire::emit`]), for
//!   calling a model;
//! - **decode**: a format's stream or response → hub deltas and replies
//!   ([`StreamDecoder`], [`Wire::decode_response`], [`Wire::decode_error`]);
//! - **accept**: a format's request → [`Conversation`] ([`Wire::accept_body`]),
//!   for serving one;
//! - **encode**: hub deltas and replies → a format's stream and response
//!   ([`Wire::encoder`], [`Wire::encode_response`], [`Wire::encode_error`]).

pub mod accept;
pub mod conversation;
pub mod decode;
pub mod diag;
pub mod emit;
pub mod encode;
pub mod limits;
pub mod reasoning;
pub mod sse;
pub mod wire;

pub use accept::{Accepted, Blob};
pub use conversation::*;
pub use decode::{Decoder, Delta, Reply, StreamDecoder};
pub use diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
pub use emit::{BlobStore, EmitOptions, Emitted, NoBlobs};
pub use encode::{status_of, EncodeOptions, Encoder};
pub use limits::Limits;
pub use wire::chat::Chat;
pub use wire::messages::Messages;
pub use wire::responses::Responses;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// One wire format, both directions.
pub trait Wire {
    /// The format's name: its key in [`Extensions`] and in diagnostics.
    const NAME: &'static str;
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;
    type Event;
    type Decoder: Decoder<Event = Self::Event>;
    type Encoder: Encoder;

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

    /// Wire request → conversation, with a diagnostic for every lossy step.
    /// Inline media becomes blobs in [`Accepted::blobs`].
    fn accept(req: Self::Request, limits: &Limits) -> Result<Accepted, ProtocolError>;

    fn encoder(opts: EncodeOptions) -> Self::Encoder;

    fn encode_response(reply: &Reply, opts: &EncodeOptions) -> Self::Response;

    /// The hub's error → (HTTP status, body in the format's error shape).
    fn encode_error(error: &HubError) -> (u16, Vec<u8>);

    /// A whole request body → conversation, under every limit.
    fn accept_body(body: &[u8], limits: &Limits) -> Result<Accepted, ProtocolError> {
        Self::accept(Self::parse_body(body, limits)?, limits)
    }

    /// Parse a body under the size and depth limits.
    fn parse_body<T: DeserializeOwned>(body: &[u8], limits: &Limits) -> Result<T, ProtocolError> {
        if body.len() > limits.max_body_bytes {
            return Err(ProtocolError::Limit {
                what: "body bytes",
                limit: limits.max_body_bytes,
            });
        }
        let text = std::str::from_utf8(body)
            .map_err(|_| ProtocolError::Invalid("body is not UTF-8".into()))?;
        limits.check_text_depth(text)?;
        Ok(serde_json::from_str(text)?)
    }

    /// A whole response body → reply, under the depth and size limits.
    fn decode_response_body(body: &[u8], limits: &Limits) -> Result<Reply, ProtocolError> {
        Self::decode_response(Self::parse_body(body, limits)?)
    }
}
