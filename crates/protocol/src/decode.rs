//! Decoding replies: streamed deltas, whole responses, and the stream driver.

use serde::{Deserialize, Serialize};

use crate::conversation::{Citation, StopReason, Usage};
use crate::diag::{Diag, HubError, ProtocolError};
use crate::limits::Limits;
use crate::sse::SseParser;
use crate::WireClient;

/// One step of a streamed reply, in the hub's terms. Every format decodes to
/// the same deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    Start {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ReasoningSignature {
        signature: String,
    },
    ReasoningRedacted {
        data: String,
    },
    /// A tool call begins. `index` counts tool calls in this reply from 0.
    ToolCallStart {
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
    },
    /// A fragment of a tool call's JSON arguments.
    ToolCallArgs {
        index: usize,
        fragment: String,
    },
    Citation {
        citation: Citation,
    },
    Refusal {
        text: String,
    },
    Usage {
        usage: Usage,
    },
    Stop {
        reason: StopReason,
    },
    /// The server reported an error inside the stream.
    Error {
        error: HubError,
    },
    /// Something the hub has no name for; see the diagnostic.
    Notice {
        diag: Diag,
    },
}

/// Turns a format's stream events into deltas. A decoder holds only the
/// state it needs to map indexes; it never buffers a reply.
pub trait Decoder {
    type Event;
    /// After the end, a closing frame yields nothing and any other event is
    /// `ProtocolError::AfterEnd`.
    fn decode(&mut self, event: Self::Event) -> Result<Vec<Delta>, ProtocolError>;
    /// Whether the format's closing frame (or an in-stream error) has arrived.
    fn is_done(&self) -> bool;
}

/// Bytes of a server-sent-events stream in, deltas out.
pub struct StreamDecoder<W: WireClient> {
    parser: SseParser,
    decoder: W::Decoder,
    limits: Limits,
}

impl<W: WireClient> StreamDecoder<W> {
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: SseParser::new(limits),
            decoder: W::decoder(limits),
            limits,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Delta>, ProtocolError> {
        let mut deltas = Vec::new();
        for frame in self.parser.push(bytes)? {
            // Decoders accept a repeated closing frame after the end (some
            // servers send one after an in-stream error) and reject anything
            // else with `ProtocolError::AfterEnd`.
            let event = W::parse_event(&frame, &self.limits)?;
            deltas.extend(self.decoder.decode(event)?);
        }
        Ok(deltas)
    }

    /// Call when the byte stream ends. Fails if the closing frame never came.
    pub fn finish(&self) -> Result<(), ProtocolError> {
        if self.decoder.is_done() {
            Ok(())
        } else {
            Err(ProtocolError::Truncated)
        }
    }

    pub fn is_done(&self) -> bool {
        self.decoder.is_done()
    }

    /// Bytes held between pushes; bounded by the limits.
    pub fn buffered_bytes(&self) -> usize {
        self.parser.buffered_bytes()
    }
}
