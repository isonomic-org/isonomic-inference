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
| 8 | #15 | CI: never cancel a run on main | done |
| 9 | #17 | Ask the memory gate before every start | done |
| 10 | #19 | Model library refresh, November 2026 | done |
| 11 | #21 | Server launch defaults: context, slots, reasoning | done |
| 12 | #23 | Milestone 0 gate: real-model smoke on the Intel test machine | done |
| 13 | #25 | Small Qwen3.5 models; manifest hashes confirmed | done |

## Model protocol, client direction

| Order | Issue | Task | State |
| --- | --- | --- | --- |
| 1 | #27 | Protocol crate: hub types, plus emit and decode for messages and chat | in review |

Work the issues in this order. Each one is a brief: Context, Acceptance
criteria, Files in scope, Out of scope.

## Open

- Milestone 0 is complete: every gate item, and the real-model smoke test
  on the Intel test machine.
- Omar approves every merge. Pull requests stop at review.
- The server direction of the protocol crate (accept, encode) and the
  responses format wait for Omar's go-ahead.

## Next three issues

1. Protocol crate, server direction: accept and encode for messages and
   chat, which the engine's local endpoint needs; then cross-format tests
   and fuzz targets.
2. Benchmark every chat model on the Intel test machine while it is idle,
   and record which sizes each hardware profile should default to.
3. Engine integration: the engine injects its memory gate, the server
   binary path and a context size per hardware profile, and its model
   gateway routes to one `LocalLlama` for chat and one for embeddings.

## How to resume

1. Read `CLAUDE.md`, then `docs/spec.md`, then the newest entry in
   `docs/status.md`.
2. Run the three commands under "Before pushing" in `CLAUDE.md`. They must
   pass before any change.
3. Take the first task above that is not merged.
