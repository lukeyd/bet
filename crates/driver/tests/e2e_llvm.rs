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

/// `bet build <input> -o <tmp>`, run the result, and return its stdout.
fn build_and_run(input_rel: &str, out_name: &str) -> String {
    ensure_runtime_staticlib();
    let bet = env!("CARGO_BIN_EXE_bet");
    // Include the platform exe suffix so `bet build -o <out>` writes exactly this path.
    let out = std::env::temp_dir().join(format!("{out_name}{}", std::env::consts::EXE_SUFFIX));
    let input = repo_path(input_rel);

    let build = Command::new(bet)
        .arg("build")
        .arg(&input)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("failed to spawn `bet build`");
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
