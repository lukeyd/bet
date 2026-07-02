//! `runtime` — bet's real runtime (the differentiator): crib arenas, slot generations,
//! per-frame scratch, allocator context, the `slide` scheduler, and the per-OS syscall
//! layer. Implements `rt-abi` for real; swaps in for `rt-stub` with no signature changes.
//!
//! Stub in Step 0 (compiles empty).
