//! `sys.peep()` end-to-end through the `bet run` interpreter path: a real line piped on stdin is
//! read back (newline stripped), and a closed stdin reads as the empty string at EOF. This drives
//! the actual `bet` binary as a subprocess, so it exercises stdin plumbing the corpus runner (which
//! always closes stdin) can't reach. No LLVM / codegen — the interpreter path only.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A tiny program that reads one stdin line and reports its length and content. Written to a temp
/// file per call so each test is hermetic and nothing is shared on disk.
const PROG: &str = r#"pull "spill"
pull "sys"

finna main() {
    lowkey s = sys.peep()
    spill.f("len={}\n", str.len(s))
    spill.f("line=[{}]\n", s)
}
"#;

/// Write `PROG` to a unique path under the system temp dir. The `tag` and the process id keep the
/// name distinct across tests and across concurrent test binaries.
fn write_prog(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bet_stdin_read_line_{tag}_{}.bet",
        std::process::id()
    ));
    std::fs::write(&path, PROG).expect("write temp .bet program");
    path
}

/// Spawn `bet run <prog>`, feed `stdin` to it (or close stdin immediately when `None`), and return
/// its captured stdout as a String.
fn run_with_stdin(prog: &std::path::Path, stdin: Option<&[u8]>) -> String {
    let bet = env!("CARGO_BIN_EXE_bet");
    let mut child = Command::new(bet)
        .arg("run")
        .arg(prog)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `bet run`");

    // Write the input (if any), then drop the handle so the child sees EOF.
    {
        let mut sink = child.stdin.take().expect("child stdin is piped");
        if let Some(bytes) = stdin {
            sink.write_all(bytes).expect("write to child stdin");
        }
    }

    let out = child.wait_with_output().expect("wait for `bet run`");
    assert!(
        out.status.success(),
        "`bet run` exited with {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout should be UTF-8")
}

#[test]
fn peep_reads_a_piped_line() {
    let prog = write_prog("line");
    let stdout = run_with_stdin(&prog, Some(b"hello trail\n"));
    let _ = std::fs::remove_file(&prog);

    // The trailing newline is stripped, so the echoed content is exactly "hello trail" (11 chars).
    assert!(
        stdout.contains("hello trail"),
        "expected the piped line echoed back, got:\n{stdout}"
    );
    assert!(
        stdout.contains("len=11"),
        "expected len=11 for \"hello trail\", got:\n{stdout}"
    );
    assert!(
        stdout.contains("line=[hello trail]"),
        "expected the bracketed echo, got:\n{stdout}"
    );
}

#[test]
fn peep_at_eof_is_empty() {
    let prog = write_prog("eof");
    // Closed stdin (no bytes) -> immediate EOF -> the empty string.
    let stdout = run_with_stdin(&prog, None);
    let _ = std::fs::remove_file(&prog);

    assert!(
        stdout.contains("len=0"),
        "expected len=0 at EOF, got:\n{stdout}"
    );
    assert!(
        stdout.contains("line=[]"),
        "expected an empty bracketed echo at EOF, got:\n{stdout}"
    );
}
