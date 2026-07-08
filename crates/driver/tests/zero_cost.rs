//! Zero-cost-abstraction proof for the `soa` layout keyword. `tests/bench/soa_kernel.bet` uses the
//! ergonomic `soa []Cell` abstraction (`cs[i].a`); `tests/bench/aos_kernel.bet` hand-rolls the same
//! two parallel `[]u32` arrays. If `soa` is genuinely zero-cost, the two compile to *equivalent*
//! optimized machine code — so we assert that at `--release` both kernels (a) vectorize to NEON at
//! all and (b) contain the *same* number of vector instructions. Gated behind `--features llvm`, so
//! it compiles out of the default LLVM-free build (`nextest --no-tests=pass` covers the empty set).
//!
//! This is the deterministic backstop for the runtime numbers in `benches/zero_cost.rs`: timing is
//! noisy, but "same instructions" is the literal definition of zero-cost and never flakes.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// `bet build --release --emit asm <kernel>` → the target assembly as text.
fn emit_release_asm(kernel_rel: &str, out_name: &str) -> String {
    let bet = env!("CARGO_BIN_EXE_bet");
    let out = std::env::temp_dir().join(out_name);
    let input = repo_path(kernel_rel);

    let build = Command::new(bet)
        .args(["build", "--release", "--emit", "asm"])
        .arg(&input)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("failed to spawn `bet build --release --emit asm`");
    assert!(
        build.status.success(),
        "`bet build --release --emit asm {kernel_rel}` failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let asm = std::fs::read_to_string(&out).expect("reading emitted .s");
    let _ = std::fs::remove_file(&out);
    asm
}

/// Count AArch64 NEON vector instructions in an assembly listing — the fixed-width lane suffixes
/// (`.4s`/`.2d`/`.16b`) plus the vector load/store mnemonics (`ld1`/`st1`) the loop vectorizer
/// emits. Zero means the loop stayed scalar.
fn count_neon_ops(asm: &str) -> usize {
    asm.lines()
        .filter(|l| {
            l.contains(".4s")
                || l.contains(".2d")
                || l.contains(".16b")
                || l.contains("ld1")
                || l.contains("st1")
        })
        .count()
}

#[test]
fn soa_is_zero_cost_vs_hand_written_aos() {
    let soa = count_neon_ops(&emit_release_asm(
        "tests/bench/soa_kernel.bet",
        "bet_zerocost_soa.s",
    ));
    let aos = count_neon_ops(&emit_release_asm(
        "tests/bench/aos_kernel.bet",
        "bet_zerocost_aos.s",
    ));

    // Both must actually vectorize (guards against the optimizer silently regressing to scalar).
    assert!(
        soa > 0,
        "soa_kernel did not vectorize at --release (0 NEON ops) — the -O2 pipeline is not running"
    );
    assert!(
        aos > 0,
        "aos_kernel did not vectorize at --release (0 NEON ops)"
    );

    // The zero-cost claim: the `soa` abstraction emits the *same* vector code as hand-written
    // parallel arrays. A difference would mean the abstraction costs something.
    assert_eq!(
        soa, aos,
        "`soa` is not zero-cost: soa_kernel has {soa} NEON ops but hand-written aos_kernel has {aos}"
    );
}
