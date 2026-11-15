# Isonomic Inference: protocol crate status

What `crates/protocol` actually does, newest first, with every lossy path,
deviation and deferral. The spec is the protocol section of
[spec.md](spec.md). The rest of the repo is in [status.md](status.md).

## P1a: client trait, parse, and P1b behind a feature (#36)

The approved brief "Kickoff: Isonomic Inference, model protocol crate (P1a)"
replaces the draft and scopes P1a to the client direction for messages and
chat. This entry brings the crate in line with it.

- **`WireClient`**: `emit` (conversation → request and diagnostics),
  `parse` and `parse_body` (whole reply → conversation), `decoder` with
  `StreamDecoder`, and `decode_error`.
- **The hub** gains `usage` and `stop_reason` on `Conversation`, and an
  optional server `id` on `Turn`. All three are additive, so P1b needs no hub
  change.
- **P1b behind `server`**: `WireServer: WireClient` (accept, encode) and the
  responses format compile only with the `server` feature, which is off by
  default. The shared CI runs clippy and the tests with `--all-features`, so
  they stay built and tested.

The gate, item by item:

| # | Gate item | Proof | State |
| --- | --- | --- | --- |
| 1 | Fixtures for messages and chat × the eight scenarios; each emits, parses or decodes and matches | `tests/fixtures.rs` | met |
| 2 | Emit, then parse the matching response; meaning preserved, every diagnostic listed | `tests/client.rs`, `a_parsed_reply_feeds_the_next_turn` | met |
| 3 | A conversation emitted as messages and as chat carries the same meaning; every lossy path listed | `tests/client.rs`, `a_conversation_means_the_same_as_messages_and_as_chat`; losses below | met |
| 4 | Decoders and parse each fuzzed ten minutes with no panic; each limit has a test | fuzz targets `decode` and `parse` (results below); `tests/limits.rs` | met |
| 5 | Decoding a 1 MB stream uses constant memory | `tests/stream_memory.rs` | met |
| 6 | Dependency allow-list passes: no I/O crates | CI job `protocol dependency allow-list` | met |

Losses for gate items 2 and 3:

- Feeding a parsed reply back as the next turn: the chat format sends
  earlier reasoning back only with `chat_reasoning_history`, so the
  reasoning is dropped, with a diagnostic. Messages loses nothing on the
  fixture set.
- The same conversation as messages and as chat: the messages format has no
  schema name or strict flag (dropped), and can only turn parallel calls
  off, so an explicit "allowed" is approximated. Chat loses nothing on the
  fixture set.
- Every other lossy path is in the tables below.

Fuzzing round 3 (this build; no sanitizer, stable with `RUSTC_BOOTSTRAP=1`):
`decode` 7,406,472 runs and `parse` 28,846,979 runs, 601 s each, no crash.

Deviations from the P1a brief:

- **P1b code is already in main.** #35 merged the server direction and the
  responses format before the P1a brief replaced the draft. This session
  merged #35 itself, which the brief now forbids. The code stays, behind the
  `server` feature, so P1b adds nothing to the hub. If the master session
  would rather not carry it, reverting #35 and this feature gate removes it.
- `emit` returns `Result<(Request, Vec<Diag>), ProtocolError>`, not a bare
  pair: a limit or a missing blob is an error, not a diagnostic.
- `decoder` takes the limits. `parse_event`, `decode_error` and
  `parse_body` are on the trait too, so a caller needs no per-format code.
- Files: `emit.rs`, `parse.rs`, `decode.rs`, `limits.rs`, `diag.rs` as the
  brief lists, plus `sse.rs` (event-stream framing) and `reasoning.rs` (the
  effort table).

## Responses format, all four directions (#33)

- `wire/responses`: the request (instructions and input items: messages,
  function calls, function output, reasoning), the response object, stream
  events and errors, written from the format's public reference
  documentation. Emit, decode, accept and encode, like the other two.
- Fixtures for the eight scenarios in `crates/protocol/fixtures/responses`,
  run through the same fixture, server-direction, noise and limit tests.
  `tests/cross.rs` now checks all six ordered pairs of formats.
- The encoder keeps the reply's items: the format's closing events repeat
  each item in full and its last event carries the whole response. That is
  the format's design; decoders still keep nothing.
- The encoder names items by output index (`msg_0`, `rs_1`, `fc_2`). A
  reasoning signature from this format is a JSON object holding the item ID
  and its encrypted content, opaque to the hub, so reasoning round-trips.
- A stream that ends before the model stopped is closed as incomplete,
  reason `interrupted`: the format must end with a status.

Lossy paths, each with a test:

| Where | What happens | Diagnostic |
| --- | --- | --- |
| emit | several system instructions: joined with blank lines | approximated |
| emit | stop sequences (the format has none) | dropped |
| emit | the error flag on tool output | dropped |
| emit | tool output ahead of other content in the same turn | relocated |
| emit | citations without the format's own annotation; reasoning without this format's signature (it needs the item ID) | dropped |
| emit | no schema name: `response` sent; a budget without an effort: nearest effort | defaulted, approximated |
| accept | item IDs other than reasoning; item types other than messages, calls, output and reasoning; parts by file ID | dropped |
| accept | tools that are not functions; text settings other than the format; reasoning settings other than the effort; an unknown effort | dropped |
| accept | system or developer messages after the first turn | relocated |
| accept | a tool choice of another shape: kept in the extensions | none |
| decode | progress and closing events (they repeat the deltas); unknown events | skipped; notice |
| decode | the stop reason, derived from the status and what the reply held; a stop sequence reads as end of turn | none (the format does not say) |
| encode | redacted reasoning, and signatures from other formats | none on the wire |

Fuzzing, no sanitizer, stable toolchain with `RUSTC_BOOTSTRAP=1` (the
nightly toolchain could not be downloaded in the build environment), seeded
with the fixtures, inputs up to 64 KB:

| Round | Formats | `decode` | `accept` | Result |
| --- | --- | --- | --- | --- |
| 1 | messages, chat | 19,349,422 runs in 601 s | 36,983,222 runs in 601 s | no crash |
| 2 | messages, chat, responses | 8,701,400 runs in 601 s | 21,074,245 runs in 601 s | no crash |

Deviation: #32 and #33 ship in one pull request. The responses format
reuses every test the server direction added, and splitting them would have
meant landing those tests twice.

## Protocol crate, server direction: accept and encode (#32)

- `Wire` gains `accept` / `accept_body` (request → conversation, under every
  limit), `encoder` (deltas → event stream), `encode_response` and
  `encode_error`. Both formats implement them.
- Accept turns inline media (base64 blocks, `data:` URLs) into numbered
  blobs in `Accepted::blobs`, under the inline limit; emit reads them back,
  so accept then emit reproduces the request byte for byte.
- Encoders emit each format's sequence: message start, content blocks one at
  a time and message stop for messages; a role chunk, delta chunks, a finish
  chunk, an optional usage chunk and the done marker for chat. Keep-alives:
  a ping event and a comment line. In-stream errors close the stream.
- `tests/server.rs`: for every fixture, accept then emit gives back the
  request with no diagnostics; the expected deltas, reply and error encode
  and decode back to themselves, with and without keep-alives.
- `tests/cross.rs`: requests go messages → chat → messages and chat →
  messages → chat; streams decode in one format, encode in the other and
  decode again. Only the expected losses appear.
- Fuzz targets `decode` and `accept` in `crates/protocol/fuzz` (outside the
  workspace; run with `cargo fuzz`). The noise test also feeds accept.

More lossy paths, each with a test:

| Where | What happens | Diagnostic |
| --- | --- | --- |
| messages, emit | `parallel_tool_calls: true` (the format's default; it can only turn them off) | approximated |
| messages, accept | citations on system text; system blocks that are not text; an effort level the hub does not know | dropped |
| messages, accept | an effort without thinking: sent back with adaptive thinking | approximated |
| chat, accept | system or developer text after the first turn: joined to the system instructions | relocated |
| chat, accept | non-text parts in system, developer, tool or assistant content; a file by ID; tools that are not functions | dropped |
| chat, accept | tool arguments that are not JSON: kept as a string | approximated |
| messages, encode | reasoning signatures, refusals as text; tool arguments for a call after a later call began are refused (the format streams calls one at a time) | error, not silent |
| chat, encode | reasoning signatures and redacted reasoning (no place in the format) | none on the wire; the deltas carry them |

Cross-format losses on the fixture set, asserted in `tests/cross.rs`: the
structured-output schema name and strict flag (the messages format has
neither), and an explicit `parallel_tool_calls: true`.

Deviations from the draft brief:

- `accept` returns an `Accepted` value (conversation, stream flags, blobs,
  diagnostics) rather than a pair, because the endpoint needs the stream
  flags and the blobs.
- Encoders return the text of their frames rather than typed events; the
  engine writes it to the connection as is.
- Fuzzing ran without a sanitizer, on the stable toolchain with
  `RUSTC_BOOTSTRAP=1`: the nightly toolchain could not be downloaded in the
  build environment. Results are in the responses entry above.

## Protocol crate, client direction: messages and chat (#27)

New crate `crates/protocol` (`isonomic-model-protocol`). Decision 3 is
approved; the code is written from each format's public reference
documentation. Scope set by Omar: the hub types plus emit and decode for the
messages and chat formats, with no I/O. Accept, encode and the responses
format wait.

- **Hub types** in `conversation.rs`; one effort table in `reasoning.rs`;
  limits in `limits.rs`; event-stream framing in `sse.rs`; diagnostics and
  error kinds in `diag.rs`.
- **Emit** for both formats, with a diagnostic for every lossy step.
- **Decode** for both formats: streams to deltas, whole responses to a
  reply, error bodies to the hub's error kinds.
- **Fixtures**: 8 scenarios × 2 formats, hand-written, in
  `crates/protocol/fixtures`. Each emits the expected request, decodes its
  stream to the expected deltas whole and split at 1, 2, 7 and 64 bytes,
  and, where present, decodes its response and error body.
- **Constant memory**: a 1 MB and a 4 MB stream hold the same peak buffer,
  no larger than one event (`tests/stream_memory.rs`).
- **Limits**: every limit has a test (`tests/limits.rs`).
- **Stream edges**: a leading byte-order mark is skipped; after the end (the
  closing frame or an in-stream error) a repeated closing frame is accepted
  and any other event is an error.
- **Dependencies**: a CI job checks the crate's dependency closure against
  `crates/protocol/deps-allow.txt`.

Lossy paths, each with a test in `tests/lossy.rs`:

| Where | What happens | Diagnostic |
| --- | --- | --- |
| both, emit | no model named: sent empty | defaulted |
| messages, emit | no output limit: `default_max_tokens` (4096) sent | defaulted |
| messages, emit | citation without the format's own location, or with no text before it | dropped |
| messages, emit | reasoning in a user turn; reasoning without a signature | dropped |
| messages, emit | tool-result content other than text and images | dropped |
| messages, emit | call ID outside `[A-Za-z0-9_-]`: rewritten, the same way for call and result | id mapped |
| messages, emit | effort `minimal` sent as `low`, `max` as `high` | approximated |
| messages, emit | response-format name and strict flag | dropped |
| messages, emit | metadata keys other than `user_id` | dropped |
| both, emit | an extension field that collides with a field the hub sets | dropped |
| chat, emit | reasoning in an assistant turn (sent as `reasoning_content` only when `chat_reasoning_history` is on; the signature is always lost) | dropped |
| chat, emit | citations, images and files in an assistant turn; tool use, reasoning and citations in a user turn | dropped |
| chat, emit | a file by URL (the format takes files inline) | dropped |
| chat, emit | an image in a tool result: moved to the user message after it | relocated |
| chat, emit | tool results ahead of other content in the same turn | relocated |
| chat, emit | several text blocks in a tool result or assistant turn: joined | approximated |
| chat, emit | the error flag on a tool result | dropped |
| chat, emit | no schema name: `response` sent | defaulted |
| chat, emit | a budget without an effort: nearest effort; a budget beside an effort: dropped | approximated, dropped |
| messages, decode | block, delta and event types the hub does not know | notice |
| messages, reply | reply blocks other than text, reasoning and tool use | unrecognized |
| chat, decode and reply | choices after the first | notice, dropped |
| chat, reply | a refusal: kept as text, stop reason refusal | approximated |
| chat, reply | tool arguments that are not JSON: kept as a string | approximated |
| chat, decode | finish reason `stop` covers both end of turn and a stop sequence; decoded as end of turn | none (the format does not say) |

Deviations from the draft brief:

- `emit` returns the request with its diagnostics (`Emitted`), not the bare
  request, so that design rule 2 (lossless or it says so) holds.
- The `Wire` trait has the client half only. `accept` and `encoder` are
  added with the server direction.
- A citation is a block that annotates the text block before it, matching
  how both formats attach citations to text.
- Extensions are kept per format at the conversation level. Per-turn and
  per-block extensions come with accept, where a round trip needs them.
- Fuzzing: a deterministic noise test (`tests/noise.rs`) mutates the
  fixture streams and feeds random bytes, 4,000 rounds per format, with no
  panic. Fuzz targets need a nightly toolchain; they come with accept.
- Cross-format translation tests need accept, so they wait.
