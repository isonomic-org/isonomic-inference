//! The messages format: system and turns of typed content blocks, tools
//! with input schemas, and a stream of typed events.

#[cfg(feature = "server")]
mod accept;
mod decode;
mod emit;
#[cfg(feature = "server")]
mod encode;
pub mod types;

pub use decode::MessagesDecoder;
#[cfg(feature = "server")]
pub use encode::MessagesEncoder;

use crate::conversation::Conversation;
use crate::diag::{Diag, HubError, ProtocolError};
use crate::emit::EmitOptions;
use crate::limits::Limits;
use crate::sse::SseFrame;
use crate::WireClient;

/// The extension key and the format's name in diagnostics.
pub const NAME: &str = "messages";

/// The messages format.
pub struct Messages;

impl WireClient for Messages {
    const NAME: &'static str = NAME;
    type Request = types::Request;
    type Response = types::Response;
    type Event = types::Event;
    type Decoder = MessagesDecoder;

    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<(types::Request, Vec<Diag>), ProtocolError> {
        emit::emit(conv, opts).map(|e| (e.request, e.diags))
    }

    fn parse(resp: types::Response) -> Result<(Conversation, Vec<Diag>), ProtocolError> {
        decode::decode_response(resp).map(|reply| reply.into_conversation())
    }

    fn decoder(limits: Limits) -> MessagesDecoder {
        MessagesDecoder::new(limits)
    }

    fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<types::Event, ProtocolError> {
        decode::parse_event(frame, limits)
    }

    fn decode_error(status: Option<u16>, body: &[u8]) -> HubError {
        decode::decode_error(status, body)
    }
}

#[cfg(feature = "server")]
impl crate::WireServer for Messages {
    type Encoder = MessagesEncoder;

    fn accept(
        req: types::Request,
        limits: &Limits,
    ) -> Result<crate::accept::Accepted, ProtocolError> {
        accept::accept(req, limits)
    }

    fn encoder(opts: crate::encode::EncodeOptions) -> MessagesEncoder {
        MessagesEncoder::new(opts)
    }

    fn encode_response(
        conv: &Conversation,
        opts: &crate::encode::EncodeOptions,
    ) -> types::Response {
        encode::encode_response(&crate::parse::Reply::of(conv), opts)
    }

    fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
        encode::encode_error(error)
    }
}
