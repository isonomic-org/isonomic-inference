//! Translating across formats preserves meaning on the whole fixture set.
//! Requests go messages → chat → messages and chat → messages → chat;
//! streams decode in one format, encode in the other and decode again.
//! The lossy steps on the way are exactly the expected ones.

use std::collections::BTreeMap;
use std::path::Path;

use isonomic_model_protocol::{
    Chat, Delta, DiagKind, EmitOptions, EncodeOptions, Encoder, Limits, Messages, Responses,
    StopReason, StreamDecoder, Wire,
};
use serde_json::Value;

const SCENARIOS: [&str; 6] = [
    "text",
    "tool_call",
    "parallel_tool_calls",
    "reasoning",
    "image_input",
    "structured_output",
];

fn read(format: &str, file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format)
        .join(file);
    std::fs::read_to_string(path).unwrap()
}

/// Request in format A → hub → format B → hub → format A.
fn round_trip<A: Wire, B: Wire>(request: &Value) -> (Value, Vec<(DiagKind, String)>) {
    let limits = Limits::default();
    let mut diags = Vec::new();
    let first = A::accept_body(&serde_json::to_vec(request).unwrap(), &limits).unwrap();
    let opts = |blobs| EmitOptions {
        stream: true,
        blobs,
        ..EmitOptions::default()
    };
    let there = B::emit(&first.conversation, &opts(&first.blobs)).unwrap();
    let body = serde_json::to_vec(&there.request).unwrap();
    let second = B::accept_body(&body, &limits).unwrap();
    let back = A::emit(&second.conversation, &opts(&second.blobs)).unwrap();
    for diag in first
        .diags
        .iter()
        .chain(&there.diags)
        .chain(&second.diags)
        .chain(&back.diags)
    {
        diags.push((diag.kind, diag.at.clone()));
    }
    (serde_json::to_value(&back.request).unwrap(), diags)
}

/// Request in format A → hub → format B → hub → format A, against the
/// fixture, allowing exactly the expected losses. Only the messages format
/// loses anything on this set: it has no schema name or strict flag, and
/// can only turn parallel calls off.
fn check_requests<A: Wire, B: Wire>() {
    let schema = |a: &str| -> &'static str {
        match a {
            "responses" => "/text/format",
            _ => "/response_format/json_schema",
        }
    };
    for scenario in SCENARIOS {
        let label = format!("{} -> {} -> {}: {scenario}", A::NAME, B::NAME, A::NAME);
        let fixture: Value =
            serde_json::from_str(&read(A::NAME, &format!("{scenario}.json"))).unwrap();
        let (back, diags) = round_trip::<A, B>(&fixture["request"]);
        let mut expected = fixture["request"].clone();
        let mut expected_diags: Vec<(DiagKind, String)> = Vec::new();
        let through_messages = B::NAME == "messages";
        match scenario {
            "structured_output" if through_messages => {
                let format = expected.pointer_mut(schema(A::NAME)).unwrap();
                format["name"] = "response".into();
                format.as_object_mut().unwrap().remove("strict");
                expected_diags.push((DiagKind::Dropped, "response_format".into()));
                expected_diags.push((DiagKind::Defaulted, "response_format.name".into()));
            }
            "structured_output" if A::NAME == "messages" => {
                expected_diags.push((DiagKind::Defaulted, "response_format.name".into()));
                expected_diags.push((DiagKind::Dropped, "response_format".into()));
            }
            "parallel_tool_calls" if through_messages => {
                expected
                    .as_object_mut()
                    .unwrap()
                    .remove("parallel_tool_calls");
                expected_diags.push((DiagKind::Approximated, "parallel_tool_calls".into()));
            }
            _ => {}
        }
        assert_eq!(back, expected, "{label}");
        assert_eq!(diags, expected_diags, "{label}");
    }
}

#[test]
fn requests_survive_a_trip_through_another_format() {
    check_requests::<Messages, Chat>();
    check_requests::<Messages, Responses>();
    check_requests::<Chat, Messages>();
    check_requests::<Chat, Responses>();
    check_requests::<Responses, Messages>();
    check_requests::<Responses, Chat>();
}

/// What a reply means: its text, reasoning, tool calls, stop and errors.
#[derive(Debug, PartialEq, Default)]
struct Meaning {
    text: String,
    reasoning: String,
    tools: BTreeMap<usize, (String, String)>,
    stop: Option<StopReason>,
    errors: usize,
}

fn meaning(deltas: &[Delta]) -> Meaning {
    let mut m = Meaning::default();
    for delta in deltas {
        match delta {
            Delta::Text { text } => m.text.push_str(text),
            Delta::Reasoning { text } => m.reasoning.push_str(text),
            Delta::ToolCallStart { index, name, .. } => {
                m.tools.entry(*index).or_default().0 = name.clone();
            }
            Delta::ToolCallArgs { index, fragment } => {
                m.tools.entry(*index).or_default().1.push_str(fragment);
            }
            Delta::Stop { reason } => m.stop = Some(reason.clone()),
            Delta::Error { .. } => m.errors += 1,
            _ => {}
        }
    }
    m
}

fn decode<W: Wire>(stream: &str) -> Vec<Delta> {
    StreamDecoder::<W>::new(Limits::default())
        .push(stream.as_bytes())
        .unwrap()
}

fn encode<W: Wire>(deltas: &[Delta]) -> String {
    let mut encoder = W::encoder(EncodeOptions {
        stream_usage: true,
        ..EncodeOptions::default()
    });
    let mut out: String = deltas.iter().map(|d| encoder.encode(d).unwrap()).collect();
    out.push_str(&encoder.finish());
    out
}

fn check_streams<A: Wire, B: Wire>() {
    for scenario in SCENARIOS.iter().chain(&["error"]) {
        let original = decode::<A>(&read(A::NAME, &format!("{scenario}.sse")));
        let crossed = decode::<B>(&encode::<B>(&original));
        assert_eq!(
            meaning(&crossed),
            meaning(&original),
            "{} -> {}: {scenario}",
            A::NAME,
            B::NAME
        );
    }
}

#[test]
fn streams_keep_their_meaning_in_another_format() {
    check_streams::<Messages, Chat>();
    check_streams::<Messages, Responses>();
    check_streams::<Chat, Messages>();
    check_streams::<Chat, Responses>();
    check_streams::<Responses, Messages>();
    check_streams::<Responses, Chat>();
}
