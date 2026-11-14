# Isonomic Inference: status

What is actually built, newest first, with every deviation and deferral.
The spec is [spec.md](spec.md).

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
