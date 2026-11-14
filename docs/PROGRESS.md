# Progress

Updated at every issue boundary. A successor session resumes from here.

## Milestone 0

| Order | Issue | Task | State |
| --- | --- | --- | --- |
| 1 | #1 | Workspace scaffold, docs and CI | done |
| 2 | #2 | inference-core: shared types, the memory gate and clocks | done |
| 3 | #3 | library: model manifest and verified download | done |
| 4 | #4 | llama: port the model server supervisor | done |
| 5 | #5 | provider: the frozen trait and the local provider | done |
| 6 | #6 | xtask: smoke test against a real server | done |
| 7 | #7 | worker: extract the reusable lifecycle | done |

Work the issues in this order. Each one is a brief: Context, Acceptance
criteria, Files in scope, Out of scope.

## Open

- Smoke test with a real model on the Intel test machine:
  `cargo xtask fetch phi-3-mini-4k-instruct-q4 --dir models`, then
  `cargo xtask smoke --server <llama-server> --model models/Phi-3-mini-4k-instruct-q4.gguf`.
  Paste the report into `docs/status.md`.
- Decide whether the memory gate is asked again on a crash restart (see the
  llama entry in `docs/status.md`).
- Decide the default chat model once a real tokens-per-second number exists.

## Next three issues

1. Confirm the manifest hashes with real downloads, clear the draft flags,
   and record the real-model smoke report.
2. Switch `ci.yml` to the organization's shared workflow once it exists.
3. Engine integration: the engine injects its memory gate and the server
   binary path, and its model gateway routes to `LocalLlama` (one provider
   for chat, one for embeddings).

## How to resume

1. Read `CLAUDE.md`, then `docs/spec.md`, then the newest entry in
   `docs/status.md`.
2. Run the three commands under "Before pushing" in `CLAUDE.md`. They must
   pass before any change.
3. Take the first task above that is not merged.
