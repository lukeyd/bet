# `selfhost` — self-hosted compiler (empty until trigger)

**Do not start work here yet.** This directory stays empty until **Milestone 7 (language
freeze for stage 1)**: all remaining §12 open questions closed and the corpus stable for
N weeks (plan-amendment-01.md §7).

Rationale: a self-hosted compiler makes every language change ~2× more expensive; we do
not pay that tax while the language is still moving.

**Milestone 7.1 — Self-host stage 1:** the frontend (lexer → parser → typecheck → midir
emission) rewritten in bet, compiled by the Rust compiler. The backend and runtime stay
Rust **permanently** ("self-hosted" means the frontend/middle-end, per convention — Go's
runtime is still substantially not-Go).

**Milestone 8 — Self-host fixpoint:** the self-compiled compiler recompiles its own source;
stage-2 and stage-3 binaries must be functionally identical across the full corpus.

The self-hosted compiler is one of the two permanent design oracles (amendment §1): a
brutally honest general-purpose test (graphs, symbol tables, string processing, sum-typed
trees). Any v1 design decision that makes it unwritable or miserable is wrong.
