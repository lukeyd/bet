//! `playground` — a `wasm-bindgen` shim that runs `bet` source in the browser.
//!
//! Built for `wasm32-unknown-unknown` and post-processed with `wasm-bindgen --target web`
//! (see `cargo xtask wasm`). The generated `pkg/` is a plain ES module: load it from any static
//! host with a bare `import()` — no bundler or Vite plugin required.
//!
//! Exposes ONE entry point, [`run_bet`], mirroring the in-memory `bet run` path
//! (`frontend::parse` → `interp::run_to_string`). Both compiler error types are rendered through
//! their `Display` impls into the thrown `JsError`'s message.

use wasm_bindgen::prelude::*;

/// Parse and run `bet` source, returning its captured output on success.
///
/// On failure the `Err(JsError)` carries the front-end (`CompileError`) or runtime (`RunError`)
/// message; in JS this surfaces as a thrown `Error` whose `.message` is that text.
#[wasm_bindgen]
pub fn run_bet(src: &str) -> Result<String, JsError> {
    let program = frontend::parse(src).map_err(|e| JsError::new(&e.to_string()))?;
    interp::run_to_string(&program).map_err(|e| JsError::new(&e.to_string()))
}
