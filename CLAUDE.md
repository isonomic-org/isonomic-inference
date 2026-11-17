# Isonomic Inference: rules for anyone working here

These rules are binding. Read [docs/spec.md](docs/spec.md) for what the repo
must do, [docs/status.md](docs/status.md) for what is built, and
[docs/PROGRESS.md](docs/PROGRESS.md) to resume.

## Identity and names

- All work is authored as **Omar Qadmiri** (@QadmiriOmar), commit email
  omar@isonomic.com. No other account name appears anywhere.
- Never write an older repo name or an older product name in code, comments,
  commit messages, branch names, issues or docs. CI scans for this.
- Environment variables that belong to Isonomic use the `ISONOMIC_` prefix.
  `LLAMA_API_KEY` is the llama.cpp server's own variable, not ours.

## Process

- One task per branch and pull request. Issues are briefs: Context,
  Acceptance criteria, Files in scope, Out of scope.
- End every pull request with ready, blocked: …, failed: … or waiting: …,
  and merge it yourself once every check is green. Exception: a change to a
  shared contract (the session event schema once frozen, the SDK wire
  interface, or isonomic-org/.github) gets the label `needs-master` and
  waits for the master session to merge it. Merge the master session's CI
  pull request in this repo once it is green.
- Before each new task, handle any open issue labelled `master` first: that
  is how the master session reaches this repo.
- CI is the organization's shared Rust workflow plus this repo's protocol
  dependency allow-list job.
- A pull request is ready only when all checks are green.
- Docs change in the same pull request as the behavior they describe.
  `docs/status.md` is newest first and states every deviation and deferral
  honestly. Update `docs/PROGRESS.md` at every issue boundary.
- Do not create, rename or delete repositories.

## Code

- Keep every source and doc file under 20 KB. Split by responsibility.
  Larger files have failed to write through the tooling used here.
  Lockfiles are exempt: `Cargo.lock` is committed, and pushes that carry it
  go through git.
- Hermetic tests are the default: stand-in binaries, the fake model server
  and an injected clock (`ManualClock`). No test may depend on the machine's
  free memory, real timers, a GPU or the network. Tests move time with
  `advance`; real time appears only as a safety limit on waiting for the
  operating system.
- Every policy wait in production code goes through a `Clock`. Do not call
  `tokio::time` directly outside `SystemClock`.
- A worker is started with an environment built from an allow-list, on
  loopback only, with a port and a secret chosen at launch, in its own
  process group, with a guardian. Do not add a path around any of these.
- The provider trait in `crates/provider/src/provider.rs` is frozen, and so
  are the types it speaks in `crates/inference-core/src/chat.rs`. Ask before
  changing either. The one approved change so far (Decision 4, #28):
  `ChatChunk::reasoning` and the per-request `ChatRequest::reasoning`.
- This repo must not depend on `isonomic-capsule`, `isonomic-sdk` or any UI
  crate.
- `crates/protocol` is pure: no network, files, processes or clocks. Its
  dependencies are on `crates/protocol/deps-allow.txt`, and CI checks them.
  Call the wire formats messages, chat and responses; name no vendor.

## Before pushing

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Stop and ask Omar when

- you cannot push or build;
- the frozen provider trait would need to change;
- a ported module needs its behavior changed rather than renamed;
- the boundary with the engine is unclear.

Otherwise decide, record the decision in the pull request and in
`docs/status.md`, and continue.

## Licence

- Repos stay private. This repository is licensed GPL-3.0-or-later: keep LICENSE at the root unmodified, keep `license = "GPL-3.0-or-later"` in every Cargo.toml, and start every new source file with `Copyright 2026 Anud Inc.` and `SPDX-License-Identifier: GPL-3.0-or-later`. Never remove an existing copyright or licence notice. Never rename or delete a repo.
