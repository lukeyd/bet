# bet — mid-level IR (design rationale)

> **The code in `crates/midir` is normative.** This document is rationale only.

The mid-level IR (à la Rust MIR / Swift SIL) sits between the frontend and LLVM. It hosts
bet-specific passes — arena lifetime analysis, allocator inlining, hoisting `holla`
generation checks out of loops, fusing repeated `holla`s, SoA crib layout — and isolates
the frontend from LLVM API churn (making Cranelift pluggable). Delivered as a real crate,
not a doc (bootstrap-plan.md §1a): Rust types + builder API + textual `.mir` format
(parser + printer) + validator.

Per plan-amendment-01 §6.1, `midir` must represent:
- `moods` construction and `vibe` dispatch (switch-on-tag),
- monomorphized generic instantiations (post-expansion — the IR never sees type params),
- function-pointer values and indirect calls,
- the full sized-integer tower with explicit wrap/trap arithmetic ops.

Stub in Step 0 (`crates/midir` compiles empty); real IR lands in Step 1a.
