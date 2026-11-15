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

use crate::accept::Accepted;
use crate::conversation::Conversation;
use crate::decode::Reply;
use crate::diag::{HubError, ProtocolError};
use crate::emit::{EmitOptions, Emitted};
use crate::encode::EncodeOptions;
use crate::limits::Limits;
use crate::sse::SseFrame;
use crate::Wire;

/// The extension key and the format's name in diagnostics.
pub const NAME: &str = "responses";

/// The responses format.
pub struct Responses;

impl Wire for Responses {
    const NAME: &'static str = NAME;
    type Request = types::Request;
    type Response = types::Response;
    type Event = types::Event;
    type Decoder = ResponsesDecoder;
    type Encoder = ResponsesEncoder;

    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<Emitted<types::Request>, ProtocolError> {
        emit::emit(conv, opts)
    }

    fn decoder(limits: Limits) -> ResponsesDecoder {
        ResponsesDecoder::new(limits)
    }

    fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<types::Event, ProtocolError> {
        decode::parse_event(frame, limits)
    }

    fn decode_response(resp: types::Response) -> Result<Reply, ProtocolError> {
        decode::decode_response(resp)
    }

    fn decode_error(status: Option<u16>, body: &[u8]) -> HubError {
        decode::decode_error(status, body)
    }

    fn accept(req: types::Request, limits: &Limits) -> Result<Accepted, ProtocolError> {
        accept::accept(req, limits)
    }

    fn encoder(opts: EncodeOptions) -> ResponsesEncoder {
        ResponsesEncoder::new(opts)
    }

    fn encode_response(reply: &Reply, opts: &EncodeOptions) -> types::Response {
        reply::encode_response(reply, opts)
    }

    fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
        reply::encode_error(error)
    }
}
