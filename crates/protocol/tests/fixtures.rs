//! Every hand-written fixture, for every format: emit matches the expected
//! request, the stream decodes to the expected deltas however it is split,
//! and whole responses and error bodies decode to the expected hub values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isonomic_model_protocol::{
    Chat, Conversation, EmitOptions, Limits, Messages, ProtocolError, StreamDecoder, Wire,
};
use serde_json::{json, Value};

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

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json_file(path: PathBuf) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

fn blobs() -> BTreeMap<String, Vec<u8>> {
    // The first four bytes of a PNG file.
    BTreeMap::from([("img1".to_string(), vec![0x89, b'P', b'N', b'G'])])
}

fn decode_all<W: Wire>(sse: &[u8], step: usize) -> (Vec<Value>, Result<(), ProtocolError>) {
    let mut decoder = StreamDecoder::<W>::new(Limits::default());
    let mut deltas = Vec::new();
    for piece in sse.chunks(step) {
        for delta in decoder.push(piece).expect("stream decodes") {
            deltas.push(serde_json::to_value(delta).unwrap());
        }
    }
    (deltas, decoder.finish())
}

fn check<W: Wire>() {
    let blobs = blobs();
    for scenario in SCENARIOS {
        let dir = root().join(W::NAME);
        let fixture = json_file(dir.join(format!("{scenario}.json")));
        let label = format!("{}/{scenario}", W::NAME);

        // Emit.
        let conv_name = fixture["conversation"].as_str().unwrap();
        let conv: Conversation = serde_json::from_value(json_file(
            root().join(format!("conversations/{conv_name}.json")),
        ))
        .unwrap_or_else(|e| panic!("{label}: conversation: {e}"));
        let opts = EmitOptions {
            stream: true,
            blobs: &blobs,
            ..EmitOptions::default()
        };
        let emitted = W::emit(&conv, &opts).unwrap_or_else(|e| panic!("{label}: emit: {e}"));
        let request = serde_json::to_value(&emitted.request).unwrap();
        assert_eq!(request, fixture["request"], "{label}: request");
        assert_eq!(
            serde_json::to_value(&emitted.diags).unwrap(),
            fixture["diags"],
            "{label}: diagnostics"
        );
        // The wire request type reads back what it wrote.
        let back: W::Request = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(back).unwrap(),
            request,
            "{label}: wire round trip"
        );

        // Decode the stream, whole and in every small split.
        let sse = read(dir.join(format!("{scenario}.sse")));
        for step in [sse.len().max(1), 1, 2, 7, 64] {
            let (deltas, end) = decode_all::<W>(sse.as_bytes(), step);
            assert_eq!(
                Value::from(deltas),
                fixture["deltas"],
                "{label}: deltas, step {step}"
            );
            match fixture["end"].as_str().unwrap() {
                "done" => assert_eq!(end, Ok(()), "{label}: end"),
                "truncated" => assert_eq!(end, Err(ProtocolError::Truncated), "{label}: end"),
                other => panic!("{label}: unknown end {other}"),
            }
        }

        // Whole responses.
        if let Some(response) = fixture.get("response") {
            let body = serde_json::to_vec(response).unwrap();
            let reply = W::decode_response_body(&body, &Limits::default())
                .unwrap_or_else(|e| panic!("{label}: response: {e}"));
            assert_eq!(
                serde_json::to_value(reply).unwrap(),
                fixture["reply"],
                "{label}: reply"
            );
        }

        // Error bodies.
        if let Some(body) = fixture.get("error_body") {
            let status = fixture["error_status"].as_u64().map(|s| s as u16);
            let error = W::decode_error(status, &serde_json::to_vec(body).unwrap());
            assert_eq!(
                serde_json::to_value(error).unwrap(),
                fixture["error"],
                "{label}: error"
            );
        }
    }
}

#[test]
fn messages_fixtures() {
    check::<Messages>();
}

#[test]
fn chat_fixtures() {
    check::<Chat>();
}

#[test]
fn an_error_body_in_no_known_shape_falls_back_to_the_status() {
    let error = Chat::decode_error(Some(503), b"<html>busy</html>");
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!({ "kind": "overloaded", "message": "<html>busy</html>" })
    );
}
