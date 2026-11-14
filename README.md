# Isonomic Inference

Runs AI models on the user's own machine. A Rust library plus worker
processes. No UI.

Isonomic Desktop's engine uses this repo to serve local models, and the first
app, Chat, cannot work without it. The engine depends on this repo. This repo
depends on nothing else in Isonomic.

## Layout

| Directory | Package | What it holds |
| --- | --- | --- |
| `crates/inference-core` | `isonomic-inference-core` | Model references and metadata, capabilities, chat and embedding types, the memory gate trait and its denial type, injectable clocks |
| `crates/library` | `isonomic-model-library` | The manifest of known models and checksum-verified downloads |
| `crates/worker` | `isonomic-worker` | The lifecycle of a model worker process, for any kind of worker |
| `crates/llama` | `isonomic-llama` | The llama.cpp server as a worker |
| `crates/protocol` | `isonomic-model-protocol` | One vocabulary for a conversation with a model, and pure translation to and from the messages and chat wire formats |
| `crates/provider` | `isonomic-provider` | The frozen provider trait and the local provider |
| `crates/testkit` | `isonomic-inference-testkit` | Test support: a fake model server, stand-in binaries, clock helpers |
| `xtask` | `xtask` | Repository tasks, including the smoke test |

Reserved crate names, not built yet: `speech` and `image`. Both will be
workers on top of `crates/worker`.

What exists today is in [docs/status.md](docs/status.md). What the repo is
meant to do is in [docs/spec.md](docs/spec.md). Where work stands, issue by
issue, is in [docs/PROGRESS.md](docs/PROGRESS.md).

## Verify

Everything is testable without a Mac, a GPU or the network.

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The tests are hermetic: a stand-in binary takes the place of the model
server, a fake server speaks its HTTP surface, and time is a clock the tests
move by hand.

## Smoke test

One test is not hermetic, and it is opt-in. It runs one chat completion and
one embedding against a real llama.cpp server binary and a real model, and
prints a report to paste back:

```sh
cargo xtask smoke --server /path/to/llama-server --model /path/to/model.gguf
```

Add `--embedding-model /path/to/embed.gguf` to use a dedicated embedding
model. Without it, the chat model is reused with mean pooling.

To get a model from the manifest, hash-verified:

```sh
cargo xtask models
cargo xtask fetch phi-3-mini-4k-instruct-q4 --dir models
```

## Boundaries

- This repo must not depend on `isonomic-capsule`, `isonomic-sdk` or any UI
  crate.
- `crates/protocol` is pure: no network, files, processes or clocks, and
  only the dependencies on `crates/protocol/deps-allow.txt`. CI enforces the
  list.
- The path to the model server binary is an input. This repo knows nothing
  about bundles.
- The memory gate trait is defined here because this repo is its consumer.
  The real, hardware-aware gate lives in the engine and is injected. Only a
  fixed-budget gate for tests and tools ships here.
- The router, remote providers, API keys for remote services, the HTTP
  endpoint, the hardware profile and binary bundling are out of scope.

## Working here

Read [CLAUDE.md](CLAUDE.md) first. It is binding.
