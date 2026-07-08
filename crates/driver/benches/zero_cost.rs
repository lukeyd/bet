//! Zero-cost-abstraction *runtime* benchmark: does `soa []Cell` (ergonomic `cs[i].a` access) run as
//! fast as hand-rolling the same two parallel `[]u32` arrays? We compile both `tests/bench/*.bet`
//! kernels once at `--release` (the `-O2` pipeline) and criterion-time executing each. If `soa` is
//! zero-cost — as `tests/zero_cost.rs` proves at the instruction level — the two times match.
//!
//! Run with codegen: `cargo bench -p driver --features llvm`. Without `--features llvm` the `bet`
//! binary can't compile the kernels, so the benchmark skips cleanly (prints a notice, times nothing)
//! — it is not part of the default LLVM-free gate, matching `tests/bench/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// `bet build` links against `librt_stub.a`, which cargo only stages next to the `bet` binary when
/// the `rt-stub` *package* is built (an rlib dep does not emit the staticlib). Benches run in the
/// release profile, so the `bet` binary (and thus the archive it looks for beside itself) lives in
/// `target/release/` — build rt-stub `--release` to stage it there. Build it once.
fn ensure_rt_stub() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = Command::new(env!("CARGO"))
            .args(["build", "-p", "rt-stub", "--release", "--locked"])
            .status();
    });
}

/// Compile a kernel at `--release` to a native executable. Returns `None` (skip) if `bet build`
/// fails — e.g. this bench binary was built without `--features llvm`, so `bet` has no codegen.
fn build_kernel(kernel_rel: &str, out_name: &str) -> Option<PathBuf> {
    let bet = env!("CARGO_BIN_EXE_bet");
    let out = std::env::temp_dir().join(format!("{out_name}{}", std::env::consts::EXE_SUFFIX));
    let status = Command::new(bet)
        .args(["build", "--release"])
        .arg(repo_path(kernel_rel))
        .arg("-o")
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

fn run(exe: &Path) {
    let status = Command::new(exe).output().expect("running kernel").status;
    assert!(status.success(), "kernel exited with {status}");
}

fn bench_zero_cost(c: &mut Criterion) {
    ensure_rt_stub();
    let soa = build_kernel("tests/bench/soa_kernel.bet", "bet_bench_soa");
    let aos = build_kernel("tests/bench/aos_kernel.bet", "bet_bench_aos");
    let (Some(soa), Some(aos)) = (soa, aos) else {
        eprintln!(
            "zero_cost bench skipped: `bet build --release` produced no binary \
             (build with `cargo bench -p driver --features llvm` for codegen)."
        );
        return;
    };

    let mut group = c.benchmark_group("zero_cost");
    group.bench_function("soa_abstraction", |b| b.iter(|| run(black_box(&soa))));
    group.bench_function("aos_hand_written", |b| b.iter(|| run(black_box(&aos))));
    group.finish();

    let _ = std::fs::remove_file(&soa);
    let _ = std::fs::remove_file(&aos);
}

criterion_group!(benches, bench_zero_cost);
criterion_main!(benches);
