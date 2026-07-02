# `tests/conformance` — differential test harness

Runs the golden corpus through the interpreter AND the compiled path and diffs the
results (`cargo xtask corpus`). Once self-host lands, a third path is added:
**interp vs. Rust-compiled vs. self-compiled** must agree on the entire corpus
(amendment §6.3). Becomes a member crate / xtask target post-Step-0. Empty until Step 2.
