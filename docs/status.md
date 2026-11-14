# Isonomic Inference: status

What is actually built, newest first, with every deviation and deferral.
The spec is [spec.md](spec.md).

## Milestone 0 gate: real-model smoke on the Intel test machine (#23)

Run on 2026-11-14 on the 2020 Intel MacBook Pro (i7-8850H, 6 cores, 16 GB,
macOS 15.7.9), with the official llama.cpp build b12387 for Intel macOS
(CPU only; that build ships no Metal backend). The machine was also
compiling another repository during the runs, so speeds are lower bounds.

| | Qwen3.5 4B Q4_K_M | Phi-3 mini 4k Q4 |
| --- | --- | --- |
| Smoke result | PASS | PASS |
| Load time | 13.0 s | 23.0 s |
| Time to first token | 2.3 s | 1.0 s |
| Prompt speed (`llama-bench pp128`) | 12.6 tok/s | 6.8 tok/s |
| Generation speed (`llama-bench tg64`) | 3.2 ± 0.3 tok/s | 3.7 ± 4.5 tok/s |
| Context | 262K (8K by default) | 4K |
| Secret enforced (401 without it) | yes | yes |

Embeddings, Nomic Embed v1.5: 0.8 s load, 1 × 768 vector, PASS.

- **Gate passed.** All six hermetic items and the real-model smoke test.
- **Hashes confirmed by real downloads:** Qwen3.5 4B, Phi-3 mini, Nomic
  Embed v1.5. Their draft flags are cleared.
- **Default chat model is now Qwen3.5 4B.** Generation speed is the same
  as Phi-3 within the noise, prompt processing is about twice as fast, it
  loads faster, its context is 64 times larger, and it is Apache-2.0.
- **Honest limit:** about 3 tokens per second on this machine is usable
  for short answers only. Intel Macs should get the smallest models
  (Qwen3.5 2B or 0.8B) for anything interactive; that is the engine's
  per-profile choice.
- The first run, before #21, failed: see that entry.

## Server launch defaults: context, slots, reasoning (#21)

Found by the first real-model smoke run on the Intel test machine:

- **Context.** With no `-c`, the server reserves the model's full trained
  context. For Qwen3.5 4B that is 262,144 tokens, and loading took 127 s.
  Every launch now passes `-c`, 8,192 tokens by default
  (`DEFAULT_CONTEXT_SIZE`); the engine sets the real value per profile.
  Load time with the default: 13 s.
- **Slots.** The server picked 4 request slots on its own, each reserving
  memory. Every launch now passes `-np 1`: one person uses a desktop at a
  time.
- **Reasoning.** Qwen3.5 thinks before it answers, and the server streams
  the thoughts outside the answer text. The frozen chat types have no
  reasoning channel, so all 64 requested tokens went into thinking and the
  caller received no text at all. Chat launches now pass `-rea off` by
  default (`Reasoning::Off`; `On` and `Auto` are available).
- `-c`, `--ctx-size`, `-np`, `--parallel`, `-rea` and `--reasoning` are now
  reserved: the supervisor owns them.

Open question for Omar: showing a model's reasoning in Chat needs a
reasoning field on the streamed chunk, which is a change to the frozen
provider types. Until then reasoning stays off.

## Model library refresh, November 2026 (#19)

Chat models added, each with its file, size and SHA-256 read on 2026-11-14
from the publisher's or a well-known quantizer's Hugging Face listing:

| Id | Size | License | Fits |
| --- | --- | --- | --- |
| `qwen3.5-4b-q4-k-m` | 3.0 GB | Apache-2.0 | every profile, CPU-only included |
| `qwen3.5-9b-q4-k-m` | 5.7 GB | Apache-2.0 | 16 GB machines |
| `gemma-4-e4b-it-qat-q4-0` | 5.2 GB | Apache-2.0 | 16 GB machines |
| `qwen3.6-35b-a3b-ud-q4-k-xl` | 22.4 GB | Apache-2.0 | High profile only (32 GB and up) |

- Qwen3 4B is removed: Qwen3.5 4B supersedes it at about the same size.
- All new entries are drafts until a real download confirms their hash.
- These models are text and vision models. Only text is served for now;
  vision needs a second projector file per model and is not wired up.
- **Kimi is not added.** Every current Kimi model is server-class: K3 has
  about 2.8 trillion parameters and even its smallest community quantization
  is hundreds of gigabytes. Its custom license also adds commercial
  conditions above a revenue threshold. It belongs with remote providers in
  the engine, not in the local library.
- **Default unchanged.** Qwen3.5 4B is the recommended replacement for
  Phi-3 mini as the default chat model (262K context against 4K). The switch
  waits on a tokens-per-second number from the Intel test machine.

## The memory gate is asked before every start (#17)

- The gate is now asked before a crash restart as well as before a lazy
  start. This settles the open question in the llama entry below, following
  the brief's wording ("the memory gate consulted before every start").
- A refused restart does not start anything: the state returns to
  `Stopped`, the refusal is in the last log lines, and the restart is not
  counted. The next request's lazy start asks again and returns the refusal
  to its caller with required bytes, available bytes and the reason.
- This changes the behavior of a ported module. Before, a worker that
  crashed was always restarted, even if memory had run out meanwhile.
- Tests: `crates/worker/tests/gate.rs`, a refused restart and an admitted one.

Older entries, for the Milestone 0 build: [status-milestone-0.md](status-milestone-0.md).
