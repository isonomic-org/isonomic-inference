// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parsing of the server's event stream, one line at a time.

use isonomic_inference_core::ChatChunk;

use crate::provider::GatewayError;

/// One parsed server-sent-events line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SseEvent {
    Chunk(ChatChunk),
    Done,
    Ignore,
}

/// Parse a single SSE line from an OpenAI-compatible streaming response.
pub(crate) fn parse_sse_line(line: &str) -> Result<SseEvent, GatewayError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(SseEvent::Ignore);
    }
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(SseEvent::Ignore);
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(SseEvent::Done);
    }
    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| GatewayError::Protocol(format!("bad SSE json: {e}")))?;
    let delta = value["choices"][0]["delta"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // The server names the field `reasoning_content`; some others name it
    // `reasoning`.
    let delta_obj = &value["choices"][0]["delta"];
    let reasoning = delta_obj["reasoning_content"]
        .as_str()
        .or_else(|| delta_obj["reasoning"].as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let finish_reason = value["choices"][0]["finish_reason"]
        .as_str()
        .map(|s| s.to_string());
    Ok(SseEvent::Chunk(ChatChunk {
        delta,
        reasoning,
        finish_reason,
    }))
}

/// Splits a byte stream into complete lines. Bytes are only decoded once a
/// whole line has arrived, so a character split across two network reads is
/// never corrupted.
#[derive(Debug, Default)]
pub(crate) struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    /// Add bytes; returns every line they completed, without the newline.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            lines.push(String::from_utf8_lossy(&line[..pos]).into_owned());
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_emitted_as_they_complete() {
        let mut buf = LineBuffer::default();
        assert!(buf.push(b"data: a").is_empty());
        assert_eq!(buf.push(b"b\n\ndata: c\n"), ["data: ab", "", "data: c"]);
        assert!(buf.push(b"tail").is_empty());
    }

    #[test]
    fn a_character_split_across_two_reads_survives() {
        let text = "data: caf\u{e9} \u{1f600}\n".as_bytes();
        // Cut inside the two-byte and inside the four-byte character.
        for cut in [10, 14] {
            let mut buf = LineBuffer::default();
            assert!(buf.push(&text[..cut]).is_empty());
            assert_eq!(buf.push(&text[cut..]), ["data: caf\u{e9} \u{1f600}"]);
        }
    }

    #[test]
    fn parses_a_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
        match parse_sse_line(line).unwrap() {
            SseEvent::Chunk(c) => {
                assert_eq!(c.delta, "Hi");
                assert_eq!(c.finish_reason, None);
            }
            other => panic!("expected chunk, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_reasoning_delta_apart_from_the_answer() {
        for field in ["reasoning_content", "reasoning"] {
            let line = format!(
                r#"data: {{"choices":[{{"delta":{{"{field}":"Let me think."}},"finish_reason":null}}]}}"#
            );
            match parse_sse_line(&line).unwrap() {
                SseEvent::Chunk(c) => {
                    assert_eq!(c.delta, "");
                    assert_eq!(c.reasoning.as_deref(), Some("Let me think."));
                }
                other => panic!("expected chunk, got {other:?}"),
            }
        }
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
        match parse_sse_line(line).unwrap() {
            SseEvent::Chunk(c) => assert_eq!(c.reasoning, None),
            other => panic!("expected chunk, got {other:?}"),
        }
    }

    #[test]
    fn parses_finish_reason_and_done_marker() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        match parse_sse_line(line).unwrap() {
            SseEvent::Chunk(c) => {
                assert_eq!(c.delta, "");
                assert_eq!(c.finish_reason.as_deref(), Some("stop"));
            }
            other => panic!("expected chunk, got {other:?}"),
        }
        assert_eq!(parse_sse_line("data: [DONE]").unwrap(), SseEvent::Done);
    }

    #[test]
    fn ignores_keepalives_and_rejects_bad_json() {
        assert_eq!(parse_sse_line("").unwrap(), SseEvent::Ignore);
        assert_eq!(parse_sse_line(": keep-alive").unwrap(), SseEvent::Ignore);
        assert_eq!(parse_sse_line("event: message").unwrap(), SseEvent::Ignore);
        assert!(parse_sse_line("data: {not json").is_err());
    }
}
