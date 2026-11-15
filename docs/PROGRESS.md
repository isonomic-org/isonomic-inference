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
| 5 | #33 | Protocol: the responses format, both directions | done, behind the `server` feature |
| 6 | #36 | Protocol: align with the P1a brief (client trait, parse, server code behind a feature) | ready |

Work the issues in this order. Each one is a brief: Context, Acceptance
criteria, Files in scope, Out of scope.

## Open

- The P1a gate is met; item by item in `docs/status-protocol.md`.
- P1b code (server direction, responses format) is in main behind the
  `server` feature, merged early in #35. The master session decides whether
  to keep it or revert it.
- Merge rule: merge your own pull request once every check is green;
  shared-contract changes get `needs-master` and wait. Open issues labelled
  `master` come first.
- Cargo.lock (#29) is waiting on the deploy key for git pushes from the Mac.

## Next three issues

1. The engine's model gateway calls models through this crate: it needs
   `Messages`/`Chat::emit` for remote providers, `StreamDecoder` for their
   streams, `parse_body` for whole replies and `decode_error` for failures,
   plus a `BlobStore` over the engine's content store.
2. Move the local provider's stream parsing onto the protocol crate's chat
   decoder (the brief's follow-up after the gate).
3. Run the fuzz targets under the address sanitizer once a nightly
   toolchain is available, and benchmark every chat model on the Intel test
   machine while it is idle.

## How to resume

1. Read `CLAUDE.md`, then `docs/spec.md`, then the newest entry in
   `docs/status.md`.
2. Run the three commands under "Before pushing" in `CLAUDE.md`. They must
   pass before any change.
3. Take the first task above that is not merged.
