# `tests/bench` — performance benchmarks

## Zero-cost abstraction (`soa`)

`soa_kernel.bet` uses the ergonomic `soa []Cell` abstraction (`cs[i].a`); `aos_kernel.bet` hand-rolls
the same two parallel `[]u32` arrays. They do identical work, so if the `soa` keyword is genuinely
zero-cost the two compile to equivalent optimized code and run in the same time. Two harnesses check
this (both require codegen — build with LLVM 18; see `selfhost/README.md`):

- **Deterministic (CI-gated):** `crates/driver/tests/zero_cost.rs` compiles both kernels with
  `bet build --release --emit asm` and asserts both vectorize to NEON *and* emit the same number of
  vector instructions — the instruction-level definition of zero-cost, never flaky. Runs under
  `cargo nextest run -p driver --features llvm` (and in `.github/workflows/backend-llvm.yml`).
- **Runtime headline:** `cargo bench -p driver --features llvm` (criterion,
  `crates/driver/benches/zero_cost.rs`) times executing both compiled kernels; the `soa`/`aos` ratio
  should be ≈ 1.0. Without `--features llvm` the kernels don't compile and the bench skips cleanly.

These kernels are compiled-only (large N × rounds), so they are **not** corpus entries — the
tree-walking interpreter would time out on them, and they carry no `.expected` golden.

## Pause-time & allocation (planned)

Criterion benchmarks that catch memory regressions (pause time, allocation counts —
language-spec.md §11.4). Run on main-branch merges with regression thresholds, post-milestone 3.
Not part of the Step-0 CI gate.
