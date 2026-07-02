//! `backend` — bet's code generator: lowers `midir` to LLVM IR (via `inkwell`, behind
//! the non-default `llvm` feature), runs the pass pipeline, and links with `lld` for
//! all six target triples. Knows only the IR and the runtime ABI, never the frontend.
//!
//! Stub in Step 0 (compiles empty; LLVM not built by default).
