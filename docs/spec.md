# Isonomic Inference: spec

What this repo is supposed to do. What is actually built, with every
deviation, is in [status.md](status.md).

## Purpose

Isonomic is a native Rust desktop platform. A background engine runs the
user's databases, files, keys and models; a window shows apps as tabs. This
repo is the part that runs AI models on the user's own machine. It is a Rust
library plus worker processes, with no UI, and everything in it is testable
without a Mac and without a GPU.

The engine depends on this repo, never the reverse. The engine supplies three
things this repo does not know about: the path to the model server binary,
the real memory gate, and the directory models are stored in.

## Crates

### inference-core

Types every other crate agrees on. No process, network or file-system code.

- `ModelRef`, `ModelMetadata`, `License`, `Capability` (chat or embeddings).
- The chat and embedding wire types: `ChatRequest`, `ChatMessage`,
  `ChatChunk`, `EmbedRequest`, `Embeddings`, `ModelInfo`, `ProviderHealth`,
  `ProviderId`, `CostEstimate`. Their shape is frozen.
- `RamGate`: the memory gate trait, asked before a worker is started.
  `RamDenial`: required bytes, available bytes, a readable reason.
  `FixedGate`: a fixed-budget gate for tests and tools. The real gate is the
  engine's.
- `Clock`: a monotonic clock that can also wait. `SystemClock` for
  production, `ManualClock` for tests.

### library

- A manifest of known models, compiled in from `crates/library/models.json`:
  name, source URL, SHA-256, size, license, capabilities, context length.
- Every entry records its license. An entry whose license does not allow
  commercial use is rejected when the manifest is loaded.
- An entry is a draft until its hash has been confirmed against a real
  download. The hash is enforced either way.
- Download with streaming hash verification. Bytes go to a `.part` file that
  is renamed only after the hash matches. A cached file is hashed again
  before it is reused; a corrupt one is discarded and fetched again.
- Progress events: started, received, verifying, cached, done.

### worker

The lifecycle of one external program that serves one model, parameterized
by a `WorkerSpec`: binary, arguments and health probe.

State machine: `Stopped → Starting → Healthy`, and `Degraded` when the
breaker trips.

- **Lazy start.** Nothing runs until the first request calls
  `ensure_running`.
- **Memory gate.** Asked with the model file's size before every start: the
  first lazy start and every crash restart. A refusal carries required
  bytes, available bytes and a reason, and nothing is started. A refused
  restart leaves the worker `Stopped`; the next request asks again.
- **Health.** A port check, then a protocol probe. Every 5 s while starting
  (bounded by the start timeout, 120 s), every 30 s after.
- **Hung worker.** Two failed steady-state probes with the process alive:
  kill it and let the crash path restart it.
- **Crash recovery.** Backoff from 1 s doubling to 60 s. More than five
  restarts in ten minutes trips the breaker: the worker is `Degraded`, its
  last 50 log lines are kept, and `reset` allows another attempt.
- **Idle unload.** After the idle timeout with no activity the worker is
  stopped on purpose. This is not counted as a crash, and the next request
  starts it again. Activity (`touch`, and every streamed token) keeps the
  model loaded.
- **Stop.** SIGTERM to the process group, a 5 s grace, then SIGKILL. Returns
  once the worker is gone.

How a worker is started:

- **Environment.** Built from an allow-list. Nothing is inherited from the
  parent process. A variable off the list is refused before anything starts.
- **Exposure.** Loopback only, on a port chosen at every launch, never
  hardcoded. A random secret per launch, handed to the worker through its
  environment and sent on every request.
- **Process group.** The worker leads its own process group and dies with
  its parent, including when the parent is killed outright.
- **Arguments.** The supervisor owns where to listen, what to load, and the
  secret. Those arguments are refused in the caller's extra arguments.

### llama

The llama.cpp server as a worker: its command line (`--host`, `--port`,
`-m`), its health endpoint (`/health`: 503 while loading, 200 when ready),
its API key variable (`LLAMA_API_KEY`).

**Launch settings the supervisor owns:** where to listen, what to load,
the mode, the context window (`-c`, 8,192 tokens by default), the number of
request slots (`-np 1`) and, for chat, whether the model thinks before it
answers (`-rea`, off by default). That is the launch default; a request can
override it (see provider).

**One server, one job.** The llama.cpp server restricts an instance to
embeddings when started with `--embeddings` and refuses embeddings otherwise.
So an instance is started for chat or for embeddings (`ServerMode`), and
serving both takes two instances.

### provider

- The frozen provider trait: `id`, `models`, `health`, `chat`, `embeddings`,
  `cost`. One trait, N providers, OpenAI-compatible wire format end to end.
- The local provider, `LocalLlama`:
  - streaming chat: the server's event stream is parsed into chunks as it
    arrives, without buffering the full response; a model's reasoning
    arrives in `ChatChunk::reasoning`, apart from the answer in `delta`;
  - per-request reasoning: `ChatRequest::reasoning` (off or on) is sent as
    the chat template's `enable_thinking`, over the launch default; unset
    keeps the launch default. `ReasoningPolicy::AlwaysOff` keeps reasoning
    off for every request; it is the default on Intel Macs;
  - embeddings;
  - model listing with the capability of the server instance;
  - health from the worker's state;
  - a cost of zero for local inference;
  - typed errors a caller can render directly: model missing, insufficient
    RAM, server crashed.
- Requests to the local server never go through a proxy, whatever the
  environment says.

Not here: the router, remote providers and the HTTP endpoint. They belong to
the engine's model gateway.

### protocol

`isonomic-model-protocol`: one internal vocabulary for a conversation with a
model, and translation between it and the wire formats programs use to talk
to models. Three consumers: the engine's local model endpoint, its opt-in
remote providers, and Chat, which needs reasoning streamed apart from the
answer.

- **Pure.** No network, files, processes or clocks; no HTTP client or
  server. It depends only on serde, serde_json and the pure crates on
  `crates/protocol/deps-allow.txt`, and on no other crate in this repo.
- **One hub.** `Conversation` holds system instructions; turns of blocks
  (text, reasoning, image, file, tool use, tool result, citation); tool
  specs and tool choice; a response format (JSON Schema or an opaque
  constraint); reasoning effort and budget; sampling limits; metadata. Fields
  a format has and the hub lacks live in per-format extensions and are
  written back on the way out.
- **Lossless, or it says so.** Every lossy step returns a diagnostic.
- **Images and files are references.** A blob is resolved by the caller;
  inline data is size-capped.
- **One effort table** maps reasoning levels and budgets between formats.
- **Streams decode in constant memory**: only the current line and event are
  held, each under a limit.
- **Limits** on body size, JSON depth, turns, blocks, tools, schema size,
  inline blobs, stream lines, stream events and streamed tool calls.

Formats, each a `Wire` implementation:

| Format | Emit (hub → request) | Decode (stream, response, error) | Accept (request → hub) | Encode (deltas → stream) |
| --- | --- | --- | --- | --- |
| messages | yes | yes | yes | yes |
| chat | yes | yes | yes | yes |
| responses | yes | yes | yes | yes |

Fixtures in `crates/protocol/fixtures` cover every format × {text, single
tool call, parallel tool calls, reasoning, image input, structured output,
error, stop mid-stream}. Each fixture emits, decodes, accepts and encodes to
the expected values, and translates across formats with only the listed
losses. Accept turns inline media into blobs (`Accepted::blobs`) that the
engine stores; emit reads blobs back through `BlobStore`. Encoders need a
creation time and fallback names from the engine (`EncodeOptions`); the
crate reads no clock. Fuzz targets for the decoders and accept live in
`crates/protocol/fuzz`, outside the workspace.

### Reserved

`speech` and `image` will be workers on top of `worker`. They are not built.

## Verification

- **Hermetic tests are the default.** Stand-in binaries, a fake model
  server, an injected clock. No test may depend on the machine's free
  memory, real timers, a GPU or the network.
- **One real smoke test, opt-in.** `cargo xtask smoke` takes a path to a real
  model server binary and a small model, runs one chat completion and one
  embedding, and prints a block to paste back: versions, load time, tokens
  per second, and the last log lines on failure.
- Before pushing: `cargo fmt --all`,
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.

## The gate for Milestone 0

The hermetic suite proves all of these:

1. The first chat request starts the worker; tokens arrive as a stream.
2. After the idle timeout the worker stops; the next request starts it again.
3. Killing the worker makes it restart on its own; repeated crashes trip the
   breaker and keep the log lines.
4. A model too large for the gate is refused with required bytes, available
   bytes and a readable reason, and the worker is not started.
5. A download with a wrong hash is rejected and leaves no usable file; a
   cached file is re-verified before use.
6. The worker's environment contains only allow-listed variables.

And the smoke test has run once against a real server and model.

Which test proves which item is listed in the worker entry of
[status-milestone-0.md](status-milestone-0.md).

## Out of scope

The router, remote providers, API keys for remote services, the HTTP
endpoint (the protocol crate translates; the engine serves and calls), the
hardware profile, the real memory gate, binary bundling, speech, image and
video models, and anything with a UI.
