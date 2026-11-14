# Isonomic Inference: status

What is actually built, newest first, with every deviation and deferral.
The spec is [spec.md](spec.md).

## worker: the reusable lifecycle (#7)

Done after the port passed its tests, as the brief requires.

- The lifecycle (spawn, probe, recover, idle unload, gate, environment,
  process group, guardian) moved from `llama` into `worker`, parameterized
  by a `WorkerSpec`: the binary, the arguments for a launch, and a
  `HealthProbe`. `HttpProbe` is the probe the llama.cpp server uses.
- `llama` is now its configuration, the translation into a spec, and a thin
  `LlamaSupervisor` that dereferences to the worker supervisor. Its public
  names are unchanged, and its 17 lifecycle and hardening tests pass
  untouched.
- New tests run a worker that is not the llama.cpp server (other arguments,
  another secret variable, a probe that is not HTTP) through the same
  supervisor.
- `speech` and `image` are reserved in the README only.

Gate for Milestone 0, and the test that proves each item:

1. First chat request starts the worker; tokens arrive as a stream:
   `provider/tests/local.rs`,
   `the_first_chat_request_starts_the_worker_and_tokens_arrive_as_a_stream`.
2. Idle timeout stops it; the next request starts it again:
   `after_the_idle_timeout_the_worker_stops_and_the_next_request_starts_it_again`
   (provider) and `idle_unload_stops_without_counting_a_crash_and_restarts_lazily`
   (llama).
3. A killed worker restarts; repeated crashes trip the breaker and keep the
   logs: `kill_9_restarts_on_the_backoff_schedule` and
   `breaker_trips_to_degraded_with_logs_after_repeated_crashes` (llama).
4. A model too large for the gate is refused with numbers and a reason, and
   nothing starts: `a_model_too_large_for_the_gate_is_refused_and_nothing_starts`
   (llama) and `a_model_too_large_for_the_gate_is_refused_with_numbers_and_a_reason`
   (provider).
5. A wrong hash is rejected and leaves no usable file; a cached file is
   re-verified: `rejects_a_checksum_mismatch`,
   `a_corrupt_cached_file_is_reverified_discarded_and_replaced`,
   `a_corrupt_cached_file_is_never_handed_back_when_the_download_fails`
   (library).
6. The worker's environment contains only allow-listed variables:
   `the_worker_environment_contains_only_allow_listed_variables` (llama).

Smoke: passed against a real server with a synthetic model (see the entry
below). A real-model run on the Intel test machine is open.

## xtask: smoke, models, fetch (#6)

- `cargo xtask smoke --server <path> --model <gguf>` starts the real server
  through the same supervisor and provider the engine will use, runs one
  chat completion and one embedding (a second launch, in embedding mode),
  checks that a request without the secret is refused, and prints a report:
  versions, load time, time to first token, tokens per second, and the last
  log lines on failure.
- `cargo xtask models` lists the manifest. `cargo xtask fetch <id>` downloads
  and verifies one model. These two are additions to the brief; `fetch` is
  how a draft hash gets confirmed and how a model for the smoke test is
  obtained.

Smoke run, 2026-11-14, in the build sandbox (Linux x86_64): PASS.

```
server version     version: 1.0.0-dev (build 1, commit 32bdc3a) | built with GNU 13.3.0 for Linux x86_64
chat       load 0.26 s · secret enforced: yes (request without it: 401) · 32 tokens · finish: length
embedding  load 0.25 s · 1 × 64 · ok
```

What that run proves and what it does not:

- The server was a real llama.cpp server, built from source at commit
  32bdc3a. The command line, the API key in the environment, the 401
  without it, `/health`, the event stream and embedding mode all behave as
  this repo assumes.
- **The model was synthetic**: a 17 MB llama-architecture file with random
  weights and a real vocabulary, because the sandbox cannot reach the model
  host. Its speed and its output mean nothing. A run with a real model on
  the Intel test machine is still needed for a tokens-per-second number.

Deviation:

- **Tokens per second counts stream chunks.** The frozen chunk type carries
  no usage figures. The server sends one token per chunk in practice.

## provider (#5)

- The provider trait is carried over unchanged, with `ChatStream` and
  `GatewayError` next to it. The request and response types it speaks are in
  inference-core.
- `LocalLlama` is the earlier local provider, ported with its test and six
  new ones: lazy start on the first request, streamed tokens, embeddings,
  listing, health, zero cost, typed errors.
- Every request carries the secret of the current launch.

Deviations:

- **One instance, one job.** The earlier provider advertised chat and
  embeddings from one server. The real server does one or the other, so the
  provider reports the capability of its instance and answers a request for
  the other with `Unavailable`, without starting anything. Serving both
  means two providers. This is a behavior change to a ported module; it was
  made because the earlier behavior cannot work against the real server.
- **Streams are decoded line by line.** The earlier code decoded each
  network read on its own, which corrupts a character split across two
  reads. Bytes are now decoded only once a whole line has arrived.
- **No proxy.** The HTTP client for the local server ignores proxy variables
  in the environment.
- **Routing was not ported.** The earlier gateway's `route` function and its
  inputs are routing policy; they go to the engine with the router.

## llama: the model server supervisor (#4)

Ported from the earlier build with its tests, split from one 31 KB file into
`config`, `state`, `health`, `process`, `supervisor` and `tasks`. The seven
earlier tests are kept; ten new ones cover activity, a hung server, the start
deadline, stop, the environment, exposure and the death of the parent. A test support crate, `testkit`,
holds the fake model server, the stand-in scripts and the clock helpers.

Carried over unchanged in behavior: lazy start, port check then `GET
/health`, steady-state probing with hung-server detection, restart with
backoff (1 s doubling to 60 s), the breaker (more than 5 restarts in 10
minutes, last 50 log lines kept, `reset`), idle unload that is not a crash,
`touch`, the memory gate.

Required changes, all made:

- **Environment.** The worker's environment is built from an allow-list
  (`LLAMA_API_KEY`, `HOME`, `TMPDIR`, `LANG`) and nothing is inherited. A
  variable off the list is refused before anything starts.
- **Exposure.** Loopback only. A free port is chosen at every launch. A
  random 32-byte secret is generated for every launch and sent on every
  request, the health probe included.
- **Process group.** The worker leads its own group and dies with its
  parent. A guardian (`/bin/sh`) holds a pipe whose only write end is in
  this process; when this process dies, even by SIGKILL, the pipe closes and
  the guardian kills the group. The guardian is a member of the group, which
  keeps the group id reserved while it lives, so a group signal cannot reach
  an unrelated process.
- **No inherited prefixes.** Every name and message was renamed.
- **Injected clock.** Every wait goes through `Clock`.

What the llama.cpp server documentation says (tools/server/README.md at
commit 32bdc3a, 2026-11-14), and what was done with it:

- `--api-key KEY` exists, and the key can be given in the environment as
  `LLAMA_API_KEY`. The key is passed that way, so it never appears in the
  process list. Clients send `Authorization: Bearer <key>`.
- `GET /health` is public (no key check): 503 while the model loads, 200
  when ready. The probe sends the key anyway.
- `--embeddings` restricts an instance to embeddings; an instance without it
  refuses embeddings. So a launch has a `ServerMode`: chat or embedding.
- The server reads `LLAMA_ARG_*` variables. With the allow-listed
  environment the user's shell cannot inject them.

Deviations from the earlier behavior, each small and deliberate:

- **A new port per launch.** The earlier build chose a port once and reused
  it across restarts. The brief requires a port chosen at start.
  `ensure_running` now returns an `Endpoint` (address and secret) instead of
  an address.
- **`stop` returns once the worker is gone.** The earlier one returned after
  sending the last signal. The grace period is a config field (5 s).
- **The pid is cleared when the worker exits.** The earlier build kept a
  stale pid during the backoff.
- **A failed spawn returns to `Stopped`.** The earlier build left the state
  at `Starting`.
- **The start deadline kills the whole group**, not only the leader.
- **Reserved arguments are refused** in `extra_args` (host, port, model, API
  key, mode), so a caller cannot move the server off loopback.
- `ServiceState::tray_label` was not ported: it is UI text.

Open question for Omar:

- **The gate is asked on lazy starts, not on crash restarts.** That is what
  the earlier code did, and it is kept. The brief says "before every start".
  Asking again on a restart would be a behavior change with a new failure
  state, so it was not made without asking.

Not done:

- Windows: process groups and the guardian are Unix-only. On other
  platforms those calls are no-ops.

## library (#3)

- The download is the earlier one, carried over with its tests: streaming
  SHA-256, a `.part` file renamed only after the hash matches, a cached file
  hashed again before reuse, progress events.
- Added tests: a corrupt cached file is re-verified, discarded and replaced;
  when the replacement download fails, the corrupt file is gone and nothing
  is handed back.
- The manifest is `crates/library/models.json`: five models, each with
  source URL, SHA-256, size, context length, capabilities and license.
  Loading rejects an entry whose license does not allow commercial use, a
  malformed hash, a source that is not https, a duplicate id, and a default
  that cannot do its job.
- Hashes and sizes were read on 2026-11-14 from the publishers' own file
  listings on Hugging Face.

Deviations and deferrals:

- **Every manifest entry is a draft.** None of the pinned hashes has been
  confirmed by a real download from this repo yet. `cargo xtask fetch <id>`
  (added with the smoke task) does that; clear the flag when it passes.
- **The default chat model is unchanged**, Phi-3 mini 4k instruct Q4 (MIT).
  It is not the model I would keep: its 4,096-token context is small for a
  chat app and it is from early 2024. Qwen3 4B Q4_K_M (Apache-2.0, official
  GGUF, 32k context, about the same size) is pinned in the manifest as the
  candidate. The default should change only on a real tokens-per-second
  number from the Intel test machine.
- `ensure_model_with` takes a caller-supplied HTTP client; `ensure_model`
  uses a default one. Tests use a client that ignores proxy variables.
- The earlier `default_model()` constructor is gone; the default is a
  manifest field. The 4 GB ceiling for the default is kept as a test.

## inference-core (#2)

- `ModelRef`, `ModelMetadata`, `License`, `Capability`.
- The chat and embedding types the provider trait speaks, carried over
  unchanged: `ChatRequest`, `ChatMessage`, `ChatChunk`, `EmbedRequest`,
  `Embeddings`, `ModelInfo`, `ProviderHealth`, `ProviderId`, `CostEstimate`.
- The memory gate: `RamGate` and `RamDenial` (required bytes, available
  bytes, readable reason), taken from the earlier build as is, plus
  `FixedGate`, a fixed-budget gate for tests and tools. The earlier
  hardware-aware gate and the hardware profile are not here; the engine
  owns them.
- Clocks: the `Clock` trait, `SystemClock`, and `ManualClock`, which only
  moves when a test advances it.

Decisions:

- **The gate keeps its earlier names**, `RamGate` and `RamDenial`, so the
  engine's port of the real gate implements this trait without renaming.
- **The provider's error type is not in this crate.** The brief lists
  "errors" under inference-core. The frozen error enum wraps the HTTP
  client's error type, so moving it here would either change its shape or
  make the types crate depend on an HTTP client. It stays next to the trait
  in the provider crate. The denial type is here.

## Workspace scaffold, docs and CI (#1)

- A Cargo workspace with shared dependency versions, `cargo xtask` wired as
  an alias, and the docs this repo is run by: `README.md`, `CLAUDE.md`,
  `docs/spec.md`, this file and `docs/PROGRESS.md`.
- CI on every pull request and on `main`: format, clippy with warnings
  denied, and the tests, on Linux and on macOS; a scan that fails if an
  older name appears in any file or in the commit being built; a check that
  source and doc files stay under 20 KB.

Step 0 results, recorded once:

- The repo did not exist and the GitHub connector could not create it (403).
  It was created in the browser, signed in as @QadmiriOmar, private, on
  Omar's explicit instruction.
- Baseline: the earlier build's supervisor and gateway crates build and pass
  their tests in the sandbox on Linux (66 and 13 tests).
- The earlier model server supervisor is one 31 KB file. It is split by
  responsibility as it is ported.

Deviations:

- **CI is self-contained, not the shared workflow.** The brief says `ci.yml`
  should call the shared workflow in the organization's `.github` repo. That
  repo does not exist yet. The jobs are written so that `check` becomes one
  `uses:` line when it does.
- **`Cargo.lock` is not committed.** It is well over the size the write
  tooling used here can handle, and a later session could not update it. CI
  resolves dependency versions on each run. The engine, which is the
  application, will carry its own lockfile.
- **Package names are prefixed.** Directories are named as the brief names
  the crates (`inference-core`, `library`, `llama`, `provider`). Packages
  are `isonomic-inference-core`, `isonomic-model-library`, `isonomic-llama`,
  `isonomic-provider`, so the engine does not import a package called
  `library` or `provider`.
