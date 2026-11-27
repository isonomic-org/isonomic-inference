// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parsing of the server's event stream, one line at a time.

use isonomic_inference_core::{ChatChunk, ToolCallDelta};

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
    let tool_calls = parse_tool_call_deltas(&delta_obj["tool_calls"])?;
    Ok(SseEvent::Chunk(ChatChunk {
        delta,
        reasoning,
        finish_reason,
        tool_calls,
    }))
}

/// Tool-call fragments from `choices[0].delta.tool_calls`. The first
/// fragment of a call carries its id and name; later ones carry only its
/// index and more argument text. An empty id or name counts as absent, as
/// some servers repeat them empty on later fragments. Join them with
/// `ToolCallAssembler`, which refuses fragments that contradict each other.
fn parse_tool_call_deltas(calls: &serde_json::Value) -> Result<Vec<ToolCallDelta>, GatewayError> {
    let Some(calls) = calls.as_array() else {
        return Ok(Vec::new());
    };
    let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    calls
        .iter()
        .enumerate()
        .map(|(position, call)| {
            let index = match &call["index"] {
                // Without an index, only a call's first fragment (with its
                // id) can be placed: by its position. A later fragment
                // without one could belong to any call, so it is refused.
                serde_json::Value::Null if text(&call["id"]).is_some() => position as u64,
                serde_json::Value::Null => {
                    return Err(GatewayError::Protocol(
                        "a tool call fragment has neither an index nor an id".into(),
                    ))
                }
                v => v
                    .as_u64()
                    .ok_or_else(|| GatewayError::Protocol(format!("bad tool call index: {v}")))?,
            };
            let index = u32::try_from(index).map_err(|_| {
                GatewayError::Protocol(format!("tool call index {index} too large"))
            })?;
            Ok(ToolCallDelta {
                index,
                id: text(&call["id"]),
                name: text(&call["function"]["name"]),
                arguments: call["function"]["arguments"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
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

    fn chunk(line: &str) -> ChatChunk {
        match parse_sse_line(line).unwrap() {
            SseEvent::Chunk(c) => c,
            other => panic!("expected chunk, got {other:?}"),
        }
    }

    #[test]
    fn two_streamed_tool_calls_with_interleaved_arguments_reassemble() {
        // Two calls, as a server streams them: each first fragment names the
        // call, then argument text arrives for either call in any order.
        let lines = [
            r#"data: {"choices":[{"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"list_dir","arguments":"{\"dir"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"","function":{"name":"","arguments":"\":\".\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        let chunks: Vec<ChatChunk> = lines.iter().map(|l| chunk(l)).collect();
        assert_eq!(chunks[0].delta, "", "null content is no text");
        assert_eq!(
            chunks[0].tool_calls,
            [ToolCallDelta {
                index: 0,
                id: Some("call_a".to_string()),
                name: Some("read_file".to_string()),
                arguments: String::new(),
            }]
        );
        assert_eq!(
            chunks[1].tool_calls,
            [ToolCallDelta {
                index: 0,
                arguments: "{\"pa".to_string(),
                ..ToolCallDelta::default()
            }]
        );
        assert_eq!(chunks[4].tool_calls[0].id, None, "empty id is absent");
        assert_eq!(chunks[4].tool_calls[0].name, None, "empty name is absent");

        // Reassemble as a caller would: by index, appending arguments.
        let mut calls: Vec<(String, String, String)> = Vec::new();
        for delta in chunks.iter().flat_map(|c| &c.tool_calls) {
            let i = delta.index as usize;
            if calls.len() <= i {
                calls.resize(i + 1, Default::default());
            }
            if let Some(id) = &delta.id {
                calls[i].0 = id.clone();
            }
            if let Some(name) = &delta.name {
                calls[i].1 = name.clone();
            }
            calls[i].2.push_str(&delta.arguments);
        }
        assert_eq!(
            calls,
            [
                (
                    "call_a".into(),
                    "read_file".into(),
                    r#"{"path":"a.txt"}"#.into()
                ),
                ("call_b".into(), "list_dir".into(), r#"{"dir":"."}"#.into()),
            ]
        );
        let last = chunks.last().unwrap();
        assert_eq!(last.finish_reason.as_deref(), Some("tool_calls"));
        assert!(last.tool_calls.is_empty());
        assert!(chunks.iter().take(5).all(|c| c.finish_reason.is_none()));
    }

    #[test]
    fn several_calls_in_one_event_and_a_missing_index_take_their_position() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"name":"f"}},{"id":"y","function":{"name":"g"}}]},"finish_reason":null}]}"#;
        let c = chunk(line);
        assert_eq!(c.tool_calls.len(), 2);
        assert_eq!((c.tool_calls[0].index, c.tool_calls[1].index), (0, 1));
        assert_eq!(c.tool_calls[1].id.as_deref(), Some("y"));
        assert_eq!(c.tool_calls[1].arguments, "");
    }

    #[test]
    fn a_fragment_with_neither_index_nor_id_is_refused() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{}"}}]},"finish_reason":null}]}"#;
        assert!(parse_sse_line(line).is_err());
    }

    #[test]
    fn a_bad_tool_call_index_is_a_protocol_error() {
        for index in ["-1", "\"0\"", "4294967296"] {
            let line = format!(
                r#"data: {{"choices":[{{"delta":{{"tool_calls":[{{"index":{index}}}]}}}}]}}"#
            );
            assert!(parse_sse_line(&line).is_err(), "index {index}");
        }
    }

    #[test]
    fn a_plain_text_chunk_has_no_tool_calls() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
        assert!(chunk(line).tool_calls.is_empty());
    }

    #[test]
    fn ignores_keepalives_and_rejects_bad_json() {
        assert_eq!(parse_sse_line("").unwrap(), SseEvent::Ignore);
        assert_eq!(parse_sse_line(": keep-alive").unwrap(), SseEvent::Ignore);
        assert_eq!(parse_sse_line("event: message").unwrap(), SseEvent::Ignore);
        assert!(parse_sse_line("data: {not json").is_err());
    }
}
