# Isonomic Inference: status

What is actually built, newest first, with every deviation and deferral.
The spec is [spec.md](spec.md).

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
