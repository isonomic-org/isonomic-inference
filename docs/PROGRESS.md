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

## Model protocol and reasoning

| Order | Issue | Task | State |
| --- | --- | --- | --- |
| 1 | #27 | Protocol crate: hub types, plus emit and decode for messages and chat | done |
| 2 | #28 | Reasoning on streamed chunks, with a per-request control | done |
| 3 | #29 | Commit Cargo.lock | waiting: a deploy key, to push with git from the Mac |
| 4 | #32 | Protocol: accept and encode for messages and chat; cross-format tests; fuzz targets | done |
| 5 | #33 | Protocol: the responses format, both directions | done |

Work the issues in this order. Each one is a brief: Context, Acceptance
criteria, Files in scope, Out of scope.

## Open

- The protocol crate is complete for the brief's Milestone P0: three
  formats, both directions, fixtures, cross-format tests and fuzzing. Its
  status is in `docs/status-protocol.md`.
- The local provider still parses the server's stream itself; it does not
  use the protocol crate yet.
- Fuzzing has run without a sanitizer only; a run under the address
  sanitizer needs a nightly toolchain.

## Next three issues

1. The engine's local endpoint (in the engine repo) on top of `accept`,
   `encoder` and `encode_error`: the protocol crate is ready for it.
2. Move the local provider's stream parsing onto the protocol crate's chat
   decoder, and run the fuzz targets under the address sanitizer.
3. Benchmark every chat model on the Intel test machine while it is idle,
   and record which sizes each hardware profile should default to.

## How to resume

1. Read `CLAUDE.md`, then `docs/spec.md`, then the newest entry in
   `docs/status.md`.
2. Run the three commands under "Before pushing" in `CLAUDE.md`. They must
   pass before any change.
3. Take the first task above that is not merged.
