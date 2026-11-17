// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The server direction on every fixture: accept then emit gives back the
//! request with no diagnostics; encoding the expected deltas, reply and
//! error and decoding them again gives back the same values. Built with the
//! `server` feature.
#![cfg(feature = "server")]

mod common;

use std::path::{Path, PathBuf};

use isonomic_model_protocol::{
    Chat, Delta, EmitOptions, EncodeOptions, Encoder, HubError, Limits, Messages, Responses,
    StreamDecoder, WireServer,
};
use serde_json::Value;

const SCENARIOS: [&str; 8] = [
    "text",
    "tool_call",
    "parallel_tool_calls",
    "reasoning",
    "image_input",
    "structured_output",
    "error",
    "stop_mid_stream",
];

fn fixture(format: &str, scenario: &str) -> Value {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format)
        .join(format!("{scenario}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn opts() -> EncodeOptions {
    EncodeOptions {
        created: 1,
        stream_usage: true,
        fallback_id: "r".into(),
        fallback_model: "m".into(),
    }
}

/// Encode deltas to a stream, then decode it again.
pub fn reencode<W: WireServer>(deltas: &[Delta], keep_alive: bool) -> (Vec<Delta>, bool) {
    let mut encoder = W::encoder(opts());
    let mut stream = String::new();
    for delta in deltas {
        stream.push_str(&encoder.encode(delta).unwrap());
        if keep_alive {
            stream.push_str(&encoder.keep_alive());
        }
    }
    stream.push_str(&encoder.finish());
    let mut decoder = StreamDecoder::<W>::new(Limits::default());
    let out = decoder.push(stream.as_bytes()).unwrap();
    (out, decoder.finish().is_ok())
}

fn check<W: WireServer>() {
    for scenario in SCENARIOS {
        let fixture = fixture(W::NAME, scenario);
        let label = format!("{}/{scenario}", W::NAME);

        // Accept, then emit: the same request, with nothing lost.
        let body = serde_json::to_vec(&fixture["request"]).unwrap();
        let accepted = W::accept_body(&body, &Limits::default())
            .unwrap_or_else(|e| panic!("{label}: accept: {e}"));
        assert!(accepted.diags.is_empty(), "{label}: {:?}", accepted.diags);
        let (request, diags) = W::emit(
            &accepted.conversation,
            &EmitOptions {
                stream: accepted.stream,
                blobs: &accepted.blobs,
                ..EmitOptions::default()
            },
        )
        .unwrap();
        assert!(diags.is_empty(), "{label}: {diags:?}");
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            fixture["request"],
            "{label}: accept then emit"
        );

        // Encode the deltas, decode them again; keep-alives change nothing.
        let deltas: Vec<Delta> = serde_json::from_value(fixture["deltas"].clone()).unwrap();
        let mut expected = deltas.clone();
        if W::NAME == "responses" && fixture["end"] == "truncated" {
            // The format must close with a status: an interrupted stream
            // comes back as incomplete.
            expected.push(Delta::Stop {
                reason: isonomic_model_protocol::StopReason::Other("interrupted".into()),
            });
        }
        for keep_alive in [false, true] {
            let (again, closed) = reencode::<W>(&deltas, keep_alive);
            assert_eq!(again, expected, "{label}: encode then decode");
            assert!(closed, "{label}: the encoded stream is closed");
        }

        if let Some(reply) = fixture.get("reply") {
            let conv = common::conversation_of_reply(reply);
            let response = serde_json::to_vec(&W::encode_response(&conv, &opts())).unwrap();
            let (again, diags) = W::parse_body(&response, &Limits::default()).unwrap();
            assert_eq!(
                common::reply_json(&again, &diags),
                *reply,
                "{label}: response"
            );
        }
        if let Some(error) = fixture.get("error") {
            let error: HubError = serde_json::from_value(error.clone()).unwrap();
            let (status, body) = W::encode_error(&error);
            assert_eq!(Some(u64::from(status)), fixture["error_status"].as_u64());
            assert_eq!(
                W::decode_error(Some(status), &body),
                error,
                "{label}: error"
            );
        }
    }
}

#[test]
fn messages_server_direction() {
    check::<Messages>();
}

#[test]
fn chat_server_direction() {
    check::<Chat>();
}

#[test]
fn an_encoder_refuses_what_its_format_cannot_carry() {
    let start = |i: usize| Delta::ToolCallStart {
        index: i,
        id: None,
        name: "t".into(),
    };
    let args = |i: usize| Delta::ToolCallArgs {
        index: i,
        fragment: "{}".into(),
    };
    // The messages format streams tool calls one at a time.
    let mut encoder = Messages::encoder(opts());
    for delta in [start(0), start(1), args(0), args(1)] {
        encoder.encode(&delta).unwrap();
    }
    assert!(encoder.encode(&args(0)).is_err());
    // The chat format interleaves them freely.
    let mut encoder = Chat::encoder(opts());
    for delta in [start(0), start(1), args(0), args(1), args(0)] {
        encoder.encode(&delta).unwrap();
    }
    // Nothing after the end.
    encoder.finish();
    assert!(encoder.encode(&args(0)).is_err());
}

#[test]
fn responses_server_direction() {
    check::<Responses>();
}
