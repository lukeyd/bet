//! `rt-abi` — bet's runtime ABI: every `extern "C"` entry point (allocation, crib
//! push/evict, holla check intrinsics, task spawn/yield) plus the platform layer
//! (framebuffer, audio, input, timing — plan-amendment-01 §6.1), and shared ABI types.
//!
//! Contract crate ★. Stub in Step 0 (compiles empty); real signatures land in Step 1b.
