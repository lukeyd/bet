//! The tracer-bullet end-to-end test: `bet build <input>` → a native binary whose stdout is
//! exactly `hi\n`. Covers both the `.mir` path (backend + link) and the full `.bet` source
//! path (frontend → backend + link). Gated behind `--features llvm`, so it compiles out of
//! the default LLVM-free build (and `nextest --no-tests=pass` covers the empty set).

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// `bet build <input> -o <tmp>`, run the result, and return its stdout.
fn build_and_run(input_rel: &str, out_name: &str) -> String {
    let bet = env!("CARGO_BIN_EXE_bet");
    let out = std::env::temp_dir().join(out_name);
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
