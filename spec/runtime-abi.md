# bet — runtime ABI (design rationale)

> **The code in `crates/rt-abi` is normative.** This document is rationale only.

The runtime ABI is the contract between generated code and the runtime: every
`extern "C"` entry point — allocation, crib push/evict, `holla` check intrinsics, task
spawn/yield (`slide`) — plus the **platform layer** (framebuffer present, audio ring
buffer, input events, hi-res timing) added by plan-amendment-01 §6.1. LLVM handles
per-platform calling conventions, keeping this surface small.

Delivered as `crates/rt-abi` (signatures + shared types) with a naive malloc-backed
`crates/rt-stub` implementation so the backend and interpreter produce running binaries
before the real `crates/runtime` exists. The runtime team's job becomes "replace the stub
without changing the signatures" — cleanly parallel (bootstrap-plan.md §1b).

**The platform-layer entry points must be decided in Phase 0, not retrofitted** — they
shape the ABI the same way allocation intrinsics do (amendment §2.6).

Stub in Step 0 (`crates/rt-abi` compiles empty); real signatures land in Step 1b.
