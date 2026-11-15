//! The messages format: system and turns of typed content blocks, tools
//! with input schemas, and a stream of typed events.

mod accept;
mod decode;
mod emit;
mod encode;
pub mod types;

pub use decode::MessagesDecoder;
pub use encode::MessagesEncoder;

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
pub const NAME: &str = "messages";

/// The messages format.
pub struct Messages;

impl Wire for Messages {
    const NAME: &'static str = NAME;
    type Request = types::Request;
    type Response = types::Response;
    type Event = types::Event;
    type Decoder = MessagesDecoder;
    type Encoder = MessagesEncoder;

    fn emit(
        conv: &Conversation,
        opts: &EmitOptions<'_>,
    ) -> Result<Emitted<types::Request>, ProtocolError> {
        emit::emit(conv, opts)
    }

    fn decoder(limits: Limits) -> MessagesDecoder {
        MessagesDecoder::new(limits)
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

    fn encoder(opts: EncodeOptions) -> MessagesEncoder {
        MessagesEncoder::new(opts)
    }

    fn encode_response(reply: &Reply, opts: &EncodeOptions) -> types::Response {
        encode::encode_response(reply, opts)
    }

    fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
        encode::encode_error(error)
    }
}
