// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Isonomic model protocol: one vocabulary for a conversation with a model
//! ([`Conversation`]) and pure translation between it and the wire formats
//! programs use to talk to models.
//!
//! This crate does no I/O: no network, files, processes or clocks. The
//! engine owns those and hands this crate bytes and values.
//!
//! **Client direction** ([`WireClient`]), for the messages and chat formats:
//! - **emit**: [`Conversation`] → a format's request;
//! - **parse**: a format's whole reply → [`Conversation`];
//! - **decode**: a format's event stream → [`Delta`]s ([`StreamDecoder`]),
//!   and its error bodies → [`HubError`].
//!
//! **Server direction** ([`WireServer`]: accept and encode) and the
//! **responses** format are behind the `server` feature, off by default.
//! They belong to a later milestone and are not part of the default build.

#[cfg(feature = "server")]
pub mod accept;
pub mod conversation;
pub mod decode;
pub mod diag;
pub mod emit;
#[cfg(feature = "server")]
pub mod encode;
pub mod limits;
mod parse;
pub mod reasoning;
pub mod sse;
pub mod wire;

#[cfg(feature = "server")]
pub use accept::{Accepted, Blob};
pub use conversation::*;
pub use decode::{Decoder, Delta, StreamDecoder};
pub use diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
pub use emit::{BlobStore, EmitOptions, NoBlobs};
#[cfg(feature = "server")]
pub use encode::{status_of, EncodeOptions, Encoder};
pub use limits::Limits;
pub use wire::chat::Chat;
pub use wire::messages::Messages;
#[cfg(feature = "server")]
pub use wire::responses::Responses;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// One wire format, client direction: calling a model.
pub trait WireClient {
    /// The format's name: its key in [`Extensions`] and in diagnostics.
    const NAME: &'static str;
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;
    type Event;
    type Decoder: Decoder<Event = Self::Event>;

    /// Conversation → wire request, with a diagnostic for every lossy step.
    /// Fails only on a limit or a blob the caller did not provide.
    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<(Self::Request, Vec<Diag>), ProtocolError>;

    /// Whole reply → a conversation holding it as one assistant turn, with
    /// the model, usage and stop reason.
    fn parse(resp: Self::Response) -> Result<(Conversation, Vec<Diag>), ProtocolError>;

    fn decoder(limits: Limits) -> Self::Decoder;

    /// One server-sent event → the format's typed event.
    fn parse_event(frame: &sse::SseFrame, limits: &Limits) -> Result<Self::Event, ProtocolError>;

    /// An error body (with its HTTP status, if known) → the hub's error.
    fn decode_error(status: Option<u16>, body: &[u8]) -> HubError;

    /// A whole response body → conversation, under the size and depth
    /// limits.
    fn parse_body(
        body: &[u8],
        limits: &Limits,
    ) -> Result<(Conversation, Vec<Diag>), ProtocolError> {
        Self::parse(read_json(body, limits)?)
    }
}

/// One wire format, server direction: serving a model. Behind the `server`
/// feature.
#[cfg(feature = "server")]
pub trait WireServer: WireClient {
    type Encoder: Encoder;

    /// Wire request → conversation, with a diagnostic for every lossy step.
    /// Inline media becomes blobs in [`Accepted::blobs`].
    fn accept(req: Self::Request, limits: &Limits) -> Result<Accepted, ProtocolError>;

    fn encoder(opts: EncodeOptions) -> Self::Encoder;

    /// The conversation's last assistant turn, model, usage and stop reason
    /// → a whole reply.
    fn encode_response(conv: &Conversation, opts: &EncodeOptions) -> Self::Response;

    /// The hub's error → (HTTP status, body in the format's error shape).
    fn encode_error(error: &HubError) -> (u16, Vec<u8>);

    /// A whole request body → conversation, under every limit.
    fn accept_body(body: &[u8], limits: &Limits) -> Result<Accepted, ProtocolError> {
        Self::accept(read_json(body, limits)?, limits)
    }
}

/// Parse JSON under the size and depth limits.
pub fn read_json<T: DeserializeOwned>(body: &[u8], limits: &Limits) -> Result<T, ProtocolError> {
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
