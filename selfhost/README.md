# `selfhost` — self-hosted compiler (Phase C, unblocked)

**The Milestone-7 language freeze is ratified (language-spec §12) — work may begin here.**
Every critical-path §12 question is resolved, the self-host language surface is complete
(`bytes-io` + string builder + allocator-context), and the corpus has stayed green (interp ==
compiled) across every increment. The gate that kept this directory empty is now open.

Rationale for the gate (kept for the record): a self-hosted compiler makes every language
change ~2× more expensive; we did not pay that tax while the language was still moving. It has
now settled, so Phase C begins.

## Layout (Phase C — see `timelog/tasks.toml` `selfhost-*`)

- `betfe.bet` — the self-hosted frontend's `main`: read → lex → parse → lower → emit → write
  `.mir`. **C0 status:** a tracer bullet — it emits a fixed program's `.mir` (via the A2 string
  builder) to stdout, proving the pipeline `betfe (Rust-compiled) → .mir → backend → binary` runs
  end-to-end (`cargo xtask selfhost`). C1–C5 replace the fixed emission with real lexing/parsing/
  lowering, mirroring `crates/frontend/src/{lexer,parser,ast,lower}.rs`.

**Milestone 7.1 — Self-host stage 1:** the frontend (lexer → parser → typecheck → midir
emission) rewritten in bet, compiled by the Rust compiler. The backend and runtime stay
Rust **permanently** ("self-hosted" means the frontend/middle-end, per convention — Go's
runtime is still substantially not-Go).

**Milestone 8 — Self-host fixpoint:** the self-compiled compiler recompiles its own source;
stage-2 and stage-3 binaries must be functionally identical across the full corpus.

The self-hosted compiler is one of the two permanent design oracles (amendment §1): a
brutally honest general-purpose test (graphs, symbol tables, string processing, sum-typed
trees). Any v1 design decision that makes it unwritable or miserable is wrong.
