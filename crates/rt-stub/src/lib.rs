//! `rt-stub` — a naive malloc-backed implementation of every `rt-abi` entry point:
//! correct semantics, no arenas, no real scheduler. Lets the backend and interpreter
//! produce running binaries before the real runtime exists. Bootstrap-only; the real
//! `runtime` crate replaces it without changing any signature.
//!
//! Stub in Step 0 (compiles empty).
