//! `interp` — a tree-walking interpreter over the frontend AST. Races ahead on the
//! golden corpus to validate that the slang feels good before codegen exists, calls
//! `rt-abi` for memory ops (keeping semantics aligned with the compiled path), and is
//! the differential-testing partner. Later becomes the REPL.
//!
//! Stub in Step 0 (compiles empty).
