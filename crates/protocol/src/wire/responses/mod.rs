//! The responses format: instructions and a list of input items (messages,
//! function calls and their output, reasoning), and a stream of typed,
//! numbered events ending in the whole response.

mod accept;
mod decode;
mod emit;
mod encode;
mod reply;
pub mod types;

pub use decode::ResponsesDecoder;
pub use encode::ResponsesEncoder;

use crate::conversation::Conversation;
use crate::diag::{Diag, HubError, ProtocolError};
use crate::emit::EmitOptions;
use crate::limits::Limits;
use crate::sse::SseFrame;
use crate::WireClient;

/// The extension key and the format's name in diagnostics.
pub const NAME: &str = "responses";

/// The responses format.
pub struct Responses;

impl WireClient for Responses {
    const NAME: &'static str = NAME;
    type Request = types::Request;
    type Response = types::Response;
    type Event = types::Event;
    type Decoder = ResponsesDecoder;

    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<(types::Request, Vec<Diag>), ProtocolError> {
        emit::emit(conv, opts).map(|e| (e.request, e.diags))
    }

    fn parse(resp: types::Response) -> Result<(Conversation, Vec<Diag>), ProtocolError> {
        decode::decode_response(resp).map(|reply| reply.into_conversation())
    }

    fn decoder(limits: Limits) -> ResponsesDecoder {
        ResponsesDecoder::new(limits)
    }

    fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<types::Event, ProtocolError> {
        decode::parse_event(frame, limits)
    }

    fn decode_error(status: Option<u16>, body: &[u8]) -> HubError {
        decode::decode_error(status, body)
    }
}

#[cfg(feature = "server")]
impl crate::WireServer for Responses {
    type Encoder = ResponsesEncoder;

    fn accept(
        req: types::Request,
        limits: &Limits,
    ) -> Result<crate::accept::Accepted, ProtocolError> {
        accept::accept(req, limits)
    }

    fn encoder(opts: crate::encode::EncodeOptions) -> ResponsesEncoder {
        ResponsesEncoder::new(opts)
    }

    fn encode_response(
        conv: &Conversation,
        opts: &crate::encode::EncodeOptions,
    ) -> types::Response {
        reply::encode_response(&crate::parse::Reply::of(conv), opts)
    }

    fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
        reply::encode_error(error)
    }
}
