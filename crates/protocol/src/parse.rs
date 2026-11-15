//! Parse: a format's whole reply → conversation.
//!
//! Each format's decoder module builds a [`Reply`]; parse turns it into the
//! hub's terms: a conversation holding the reply as one assistant turn,
//! with the model, usage and stop reason.

use crate::conversation::{Conversation, StopReason, Turn, Usage};
use crate::diag::Diag;

/// A whole reply, as the format decoders build it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reply {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) turn: Turn,
    pub(crate) stop: Option<StopReason>,
    pub(crate) usage: Option<Usage>,
    pub(crate) diags: Vec<Diag>,
}

impl Reply {
    pub(crate) fn into_conversation(self) -> (Conversation, Vec<Diag>) {
        let mut turn = self.turn;
        turn.id = self.id;
        let conversation = Conversation {
            model: self.model,
            turns: vec![turn],
            usage: self.usage,
            stop_reason: self.stop,
            ..Conversation::default()
        };
        (conversation, self.diags)
    }

    /// The reply a conversation ends with: its last assistant turn, with the
    /// conversation's model, usage and stop reason.
    #[cfg(feature = "server")]
    pub(crate) fn of(conversation: &Conversation) -> Reply {
        use crate::conversation::Role;
        let turn = conversation
            .turns
            .iter()
            .rev()
            .find(|t| t.role == Role::Assistant)
            .cloned()
            .unwrap_or(Turn {
                id: None,
                role: Role::Assistant,
                blocks: Vec::new(),
            });
        Reply {
            id: turn.id.clone(),
            model: conversation.model.clone(),
            turn,
            stop: conversation.stop_reason.clone(),
            usage: conversation.usage,
            diags: Vec::new(),
        }
    }
}
