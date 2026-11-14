//! The messages format: system and turns of typed content blocks, tools
//! with input schemas, and a stream of typed events.

mod decode;
mod emit;
pub mod types;

pub use decode::MessagesDecoder;

use crate::conversation::Conversation;
use crate::decode::Reply;
use crate::diag::{HubError, ProtocolError};
use crate::emit::{EmitOptions, Emitted};
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
}
