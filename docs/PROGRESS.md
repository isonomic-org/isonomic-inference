# Progress

Updated at every issue boundary. A successor session resumes from here.

## Milestone 0

| Order | Issue | Task | State |
| --- | --- | --- | --- |
| 1 | #1 | Workspace scaffold, docs and CI | done |
| 2 | #2 | inference-core: shared types, the memory gate and clocks | done |
| 3 | #3 | library: model manifest and verified download | not started |
| 4 | #4 | llama: port the model server supervisor | not started |
| 5 | #5 | provider: the frozen trait and the local provider | not started |
| 6 | #6 | xtask: smoke test against a real server | not started |
| 7 | #7 | worker: extract the reusable lifecycle | not started |

Work the issues in this order. Each one is a brief: Context, Acceptance
criteria, Files in scope, Out of scope.

## Next

1. library: model manifest and verified download (#3)
2. llama: port the model server supervisor (#4)
3. provider: the frozen trait and the local provider (#5)

## How to resume

1. Read `CLAUDE.md`, then `docs/spec.md`, then the newest entry in
   `docs/status.md`.
2. Run the three commands under "Before pushing" in `CLAUDE.md`. They must
   pass before any change.
3. Take the first task above that is not merged.
