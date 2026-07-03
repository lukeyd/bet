//! The tracer-bullet end-to-end test: `bet build <input>` → a native binary whose stdout is
//! exactly `hi\n`. Covers both the `.mir` path (backend + link) and the full `.bet` source
//! path (frontend → backend + link). Gated behind `--features llvm`, so it compiles out of
//! the default LLVM-free build (and `nextest --no-tests=pass` covers the empty set).

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Ensure `librt_stub.a` exists next to the `bet` binary. A Rust `staticlib` crate-type is
/// *not* produced when `rt-stub` is pulled only as an rlib dependency (as it is when nextest
/// builds `-p driver`), so we build the `rt-stub` package explicitly. Runs once per test
/// process; concurrent invocations serialize on cargo's own build lock.
fn ensure_runtime_staticlib() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "rt-stub", "--locked"])
            .status()
            .expect("failed to spawn cargo to build the rt-stub staticlib");
        assert!(status.success(), "building the rt-stub staticlib failed");
    });
}

/// Ensure `libruntime.a` (the real runtime archive) exists next to the `bet` binary. Same
/// rationale as [`ensure_runtime_staticlib`], for the `--runtime real` link path.
fn ensure_real_runtime_staticlib() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "runtime", "--locked"])
            .status()
            .expect("failed to spawn cargo to build the runtime staticlib");
        assert!(status.success(), "building the runtime staticlib failed");
    });
}

/// `bet build <input> -o <tmp>`, run the result, and return its stdout.
fn build_and_run(input_rel: &str, out_name: &str) -> String {
    build_and_run_impl(input_rel, out_name, false)
}

/// As [`build_and_run`], but link against the real `runtime` archive (`--runtime real`).
fn build_and_run_real(input_rel: &str, out_name: &str) -> String {
    build_and_run_impl(input_rel, out_name, true)
}

fn build_and_run_impl(input_rel: &str, out_name: &str, real_runtime: bool) -> String {
    ensure_runtime_staticlib();
    if real_runtime {
        ensure_real_runtime_staticlib();
    }
    let bet = env!("CARGO_BIN_EXE_bet");
    // Include the platform exe suffix so `bet build -o <out>` writes exactly this path.
    let out = std::env::temp_dir().join(format!("{out_name}{}", std::env::consts::EXE_SUFFIX));
    let input = repo_path(input_rel);

    let mut build_cmd = Command::new(bet);
    build_cmd.arg("build").arg(&input).arg("-o").arg(&out);
    if real_runtime {
        build_cmd.args(["--runtime", "real"]);
    }
    let build = build_cmd.output().expect("failed to spawn `bet build`");
    assert!(
        build.status.success(),
        "`bet build {input_rel}` failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("failed to run the compiled program");
    let _ = std::fs::remove_file(&out);
    assert!(
        run.status.success(),
        "compiled program exited with {}",
        run.status
    );
    String::from_utf8(run.stdout).expect("stdout should be UTF-8")
}

#[test]
fn hello_mir_prints_hi() {
    assert_eq!(
        build_and_run("tests/mir/hello.mir", "bet_e2e_hello_mir"),
        "hi\n"
    );
}

#[test]
fn hello_bet_prints_hi() {
    assert_eq!(
        build_and_run("tests/corpus/01-basics/hello.bet", "bet_e2e_hello_bet"),
        "hi\n"
    );
}

#[test]
fn hello_bet_prints_hi_on_real_runtime() {
    // The same program linked against the real `runtime` archive (`--runtime real`) instead of
    // the headless `rt-stub` — the runtime swap is a drop-in.
    assert_eq!(
        build_and_run_real("tests/corpus/01-basics/hello.bet", "bet_e2e_hello_bet_real"),
        "hi\n"
    );
}

#[test]
fn memory_model_on_real_runtime() {
    // A generational-safety program (cop/evict/holla, reused slot at a newer generation) run
    // against the real runtime's growable arenas.
    assert_eq!(
        build_and_run_real(
            "tests/corpus/08-memory/generation-reuse.bet",
            "bet_e2e_genreuse_real"
        ),
        "7\n42\n-1\n"
    );
}
