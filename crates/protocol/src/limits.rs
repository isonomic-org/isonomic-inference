// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Limits that keep a hostile or broken peer to bounded time and memory.

use serde_json::Value;

use crate::conversation::{Block, Conversation, ResponseFormat};
use crate::diag::ProtocolError;

/// Every limit this crate enforces. The defaults are generous for real use
/// and small enough that one request cannot exhaust the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Size of one serialized request body.
    pub max_body_bytes: usize,
    /// Nesting depth of any JSON value: tool inputs, schemas, stream events.
    pub max_depth: usize,
    /// Turns in one conversation.
    pub max_turns: usize,
    /// Blocks in one turn, counting tool-result content.
    pub max_blocks_per_turn: usize,
    pub max_tools: usize,
    /// Serialized size of one tool's input schema or the response schema.
    pub max_schema_bytes: usize,
    /// Size of one inlined blob, before encoding.
    pub max_inline_bytes: usize,
    /// Length of one line of a server-sent-events stream.
    pub max_line_bytes: usize,
    /// Size of one server-sent event's data, summed over its lines.
    pub max_event_bytes: usize,
    /// Tool calls in one streamed reply.
    pub max_stream_tool_calls: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 32 << 20,
            max_depth: 64,
            max_turns: 4096,
            max_blocks_per_turn: 1024,
            max_tools: 256,
            max_schema_bytes: 256 << 10,
            max_inline_bytes: 20 << 20,
            max_line_bytes: 1 << 20,
            max_event_bytes: 4 << 20,
            max_stream_tool_calls: 512,
        }
    }
}

fn over(what: &'static str, limit: usize) -> ProtocolError {
    ProtocolError::Limit { what, limit }
}

impl Limits {
    /// Check a conversation before it is emitted.
    pub fn check_conversation(&self, conv: &Conversation) -> Result<(), ProtocolError> {
        if conv.turns.len() > self.max_turns {
            return Err(over("turns", self.max_turns));
        }
        if conv.tools.len() > self.max_tools {
            return Err(over("tools", self.max_tools));
        }
        for tool in &conv.tools {
            self.check_schema(&tool.input_schema)?;
        }
        if let Some(ResponseFormat::JsonSchema { schema, .. }) = &conv.response_format {
            self.check_schema(schema)?;
        }
        for turn in &conv.turns {
            let mut count = 0;
            for block in &turn.blocks {
                count += 1;
                match block {
                    Block::ToolUse { input, .. } => self.check_depth(input)?,
                    Block::ToolResult { content, .. } => count += content.len(),
                    _ => {}
                }
            }
            if count > self.max_blocks_per_turn {
                return Err(over("blocks in a turn", self.max_blocks_per_turn));
            }
        }
        Ok(())
    }

    pub fn check_schema(&self, schema: &Value) -> Result<(), ProtocolError> {
        self.check_depth(schema)?;
        let size = serde_json::to_vec(schema)?.len();
        if size > self.max_schema_bytes {
            return Err(over("schema bytes", self.max_schema_bytes));
        }
        Ok(())
    }

    /// Reject JSON nested deeper than `max_depth`, without recursion.
    pub fn check_depth(&self, value: &Value) -> Result<(), ProtocolError> {
        let mut stack = vec![(value, 1usize)];
        while let Some((value, depth)) = stack.pop() {
            if depth > self.max_depth {
                return Err(over("json depth", self.max_depth));
            }
            match value {
                Value::Array(items) => stack.extend(items.iter().map(|v| (v, depth + 1))),
                Value::Object(map) => stack.extend(map.values().map(|v| (v, depth + 1))),
                _ => {}
            }
        }
        Ok(())
    }

    /// Reject raw JSON text nested deeper than `max_depth`, before parsing,
    /// so a hostile event never reaches the parser's own recursion.
    pub fn check_text_depth(&self, text: &str) -> Result<(), ProtocolError> {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for byte in text.bytes() {
            if in_string {
                match (escaped, byte) {
                    (true, _) => escaped = false,
                    (false, b'\\') => escaped = true,
                    (false, b'"') => in_string = false,
                    _ => {}
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > self.max_depth {
                        return Err(over("json depth", self.max_depth));
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        Ok(())
    }

    /// Serialize a request, enforcing `max_body_bytes`.
    pub fn to_body<T: serde::Serialize>(&self, request: &T) -> Result<Vec<u8>, ProtocolError> {
        let body = serde_json::to_vec(request)?;
        if body.len() > self.max_body_bytes {
            return Err(over("body bytes", self.max_body_bytes));
        }
        Ok(body)
    }
}
