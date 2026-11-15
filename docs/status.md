# Isonomic Inference: status

What is actually built, newest first, with every deviation and deferral.
The spec is [spec.md](spec.md).

## Chat models benchmarked on the Intel test machine (#39)

Run on 2026-11-15 on the 2020 Intel MacBook Pro (i7-8850H, 6 cores, 16 GB),
llama.cpp b12387 for Intel macOS (CPU only), `llama-bench -p 128 -n 64
-t 6 -r 3`, every chat model in the manifest that fits the machine. Tokens
per second, mean ± standard deviation over three runs:

| Model | Size | Prompt (`pp128`) | Generation (`tg64`) |
| --- | --- | --- | --- |
| Qwen3.5 0.8B Q4_K_M | 0.5 GB | 51.3 ± 7.5 | 16.0 ± 5.9 |
| Qwen3.5 2B Q4_K_M | 1.2 GB | 39.9 ± 7.2 | 4.2 ± 2.2 |
| SmolLM3 3B Q4_K_M | 1.8 GB | 22.5 ± 0.4 | 4.1 ± 0.6 |
| Phi-3 mini 4k Q4 | 2.2 GB | 20.9 ± 0.4 | 7.4 ± 0.3 |
| Qwen3.5 4B Q4_K_M | 2.8 GB | 5.7 ± 2.7 | 1.8 ± 0.9 |
| Gemma 4 E4B Q4_0 | 4.8 GB | 6.6 ± 3.6 | 1.9 ± 0.9 |
| Qwen3.5 9B Q4_K_M | 5.3 GB | 2.5 ± 2.0 | 1.5 ± 0.7 |

- **Not a clean idle run.** No one was using the machine, but the load
  average stayed between 42 and 360 and swap was almost full (16.9 of
  17.4 GB) from other apps. The numbers are lower bounds, and the larger
  models suffer most: Qwen3.5 4B measured 3.2 tokens per second in the
  gate run (#23) against 1.8 here. The ranking is still clear.
- **Recommended chat default per hardware profile** (the engine owns the
  profile and makes the choice):
  - Intel Macs, CPU only: **Qwen3.5 0.8B**, the only model here above 10
    tokens per second. Qwen3.5 2B or Phi-3 mini as an opt-in for better
    answers at 4 to 7 tokens per second. Nothing at 4B or above for
    interactive use.
  - Every other profile: Qwen3.5 4B stays the default, as in the manifest.
- Qwen3.6 35B-A3B was not run: it does not fit this machine.

## Reasoning on streamed chunks, with a per-request control (#28)

Decision 4 is approved. Two additions to the frozen chat types, nothing
else:

- `ChatChunk::reasoning: Option<String>`: a reasoning delta, streamed apart
  from the answer in `delta`. The local provider fills it from the server's
  `reasoning_content` field (and from `reasoning`, which some servers use).
- `ChatRequest::reasoning: Option<ReasoningChoice>` (off or on), layered on
  the launch default from #21. The local provider sends it as the chat
  template's `enable_thinking`; unset sends nothing and keeps the launch
  default. It is not serialized as is.
- `ReasoningPolicy` on `LocalLlama`: `PerRequest`, or `AlwaysOff`, which
  sends reasoning off on every request whatever it asks. The default is
  `AlwaysOff` on Intel Macs (a build for macOS on x86-64, which includes an
  Intel build running under translation on Apple silicon) and `PerRequest`
  elsewhere. `with_reasoning_policy` overrides it.
- The fake server answers a request with reasoning on by streaming a
  reasoning delta first, as the real server does.
- Tests: `a_request_can_turn_reasoning_on_and_it_streams_apart_from_the_answer`,
  `an_unset_choice_keeps_the_launch_default`,
  `on_intel_macs_reasoning_stays_off_whatever_the_request_asks`, and two SSE
  parsing tests.

Deviations from the draft brief: it asked for the reasoning field on the
streamed chunk "and on the final message". The frozen surface has no final
message type, only the stream, so the field is on the chunk only. The
per-request control was asked for by Omar with the approval of Decision 4;
it is the second addition. The local provider does not use the protocol
crate yet; it parses the stream as before.

## Protocol crate (#27, #32, #33, #36)

The protocol crate's status, lossy paths and deviations are in
[status-protocol.md](status-protocol.md), newest first.

## Small Qwen3.5 models; manifest hashes confirmed (#25)

- Added Qwen3.5 0.8B (0.53 GB) and Qwen3.5 2B (1.28 GB), Apache-2.0, from
  the Unsloth GGUF builds. On the Intel test machine the 4B model generates
  about 3 tokens per second, so Intel Macs need these smaller sizes for
  interactive use. The engine picks the size per hardware profile.
- Downloaded on the Intel test machine on 2026-11-14 with
  `cargo xtask fetch`, each hash checked while streaming: Qwen3.5 0.8B, 2B
  and 9B, SmolLM3 3B, Gemma 4 E4B, Qwen3 Embedding 0.6B. All matched, so
  their draft flags are cleared.
- Every manifest entry is now confirmed except Qwen3.6 35B-A3B (22.4 GB),
  which was not downloaded: it does not fit the test machine.

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
