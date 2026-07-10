//! `bet` — the driver CLI users invoke. `bet build <input> [-o out]` threads a program through
//! the pipeline to a native executable (`.mir` input goes straight to the backend; `.bet` input
//! through the frontend first), linking against the bootstrap `rt-stub` archive (see `link`).
//! `bet run <input.bet>` parses a program and executes it on the tree-walking interpreter,
//! writing its output to stdout — no LLVM required. `bet fmt <input.bet>` prints the program
//! in canonical form (`--check` just verifies it, exiting non-zero if it differs).

mod link;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
bet — the bet compiler driver

USAGE:
    bet build [--release] <input.bet|input.mir> [-o <output>]
    bet build [--release] --emit asm <input.bet> [-o <output.s>]
    bet build --emit <tokens|ast|mir> <input.bet>
    bet run   <input.bet>
    bet fmt   [--check] <input.bet>

`build` compiles a program to a native executable, linking it against the bootstrap runtime.
Requires a codegen-enabled build (`--features llvm`); without it, `build` reports that no code
generator is present. `--release` (alias `-O2`) runs the LLVM `default<O2>` optimization pipeline
— inlining, SROA, and the loop/SLP vectorizers — so abstractions become zero-cost and `soa` loops
auto-vectorize; the default is unoptimized `-O0` (fastest to build).

`build --emit asm` writes the target assembly (`.s`) instead of linking — honoring `--release`, so
`--release --emit asm` shows the optimized (vectorized) code. `build --emit <tokens|ast|mir>`
instead prints a canonical textual dump of a frontend intermediate and stops — no backend, so those
work in the default build. The dumps are the differential-testing surface for the self-hosted frontend.

`run` parses a `.bet` program and executes it on the tree-walking interpreter, writing its
output to stdout. No codegen or LLVM is required.

`fmt` parses a `.bet` program and prints its canonical formatting to stdout. With `--check` it
prints nothing and instead exits non-zero when the file is not already canonically formatted.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bet: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("build") => build(&args[1..]),
        Some("run") => run_program(&args[1..]),
        Some("fmt") => fmt_source(&args[1..]),
        Some("-h") | Some("--help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// `bet run <input.bet>` — parse a program and execute it on the interpreter. Uses
/// [`frontend::parse`] (the full surface grammar), not `frontend::compile` (which only lowers
/// the print subset for the LLVM path), so the whole corpus can run without codegen.
fn run_program(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    for arg in args {
        if arg.starts_with('-') {
            return Err(format!("unknown flag `{arg}`"));
        }
        if input.is_some() {
            return Err("more than one input file given".into());
        }
        input = Some(PathBuf::from(arg));
    }
    let input = input.ok_or("`bet run` needs an input file")?;
    // `frontend::load` resolves the entry file's `pull` imports across files into one program.
    let program = frontend::load(&input).map_err(|e| e.to_string())?;
    interp::run(&program).map_err(|e| e.to_string())
}

/// `bet fmt [--check] <input.bet>` — pretty-print a program in its one canonical form.
///
/// Without `--check` the formatted source is written to stdout. With `--check` nothing is
/// printed on success; if the on-disk file differs from its canonical formatting the command
/// returns an error (a non-zero process exit), matching `gofmt -l`'s "is this formatted?" gate.
fn fmt_source(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
            _ => {
                if input.is_some() {
                    return Err("more than one input file given".into());
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }
    let input = input.ok_or("`bet fmt` needs an input file")?;
    let src =
        std::fs::read_to_string(&input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    let formatted = fmt::format_source(&src)?;
    if check {
        if src != formatted {
            return Err(format!(
                "{} is not formatted (run `bet fmt` to rewrite it)",
                input.display()
            ));
        }
    } else {
        print!("{formatted}");
    }
    Ok(())
}

fn build(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut runtime = link::Runtime::Stub;
    let mut emit: Option<String> = None;
    let mut opt = backend::OptLevel::O0;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                let path = args.get(i).ok_or("`-o` needs a path")?;
                output = Some(PathBuf::from(path));
            }
            "--emit" => {
                i += 1;
                emit = Some(args.get(i).ok_or("`--emit` needs a kind")?.clone());
            }
            "--runtime" => {
                i += 1;
                runtime = match args.get(i).map(String::as_str) {
                    Some("stub") => link::Runtime::Stub,
                    Some("real") => link::Runtime::Real,
                    _ => return Err("`--runtime` needs `stub` or `real`".into()),
                };
            }
            "--release" | "-O2" => opt = backend::OptLevel::O2,
            "-O0" => opt = backend::OptLevel::O0,
            flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
            path => {
                if input.is_some() {
                    return Err("more than one input file given".into());
                }
                input = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }
    let input = input.ok_or("`bet build` needs an input file")?;

    // `--emit=<kind>` short-circuits the native link. `tokens`/`ast`/`mir` print a canonical
    // textual dump of a frontend intermediate (for differential-testing the self-hosted frontend)
    // and never touch the backend, so they work in the default LLVM-free build. `asm` instead runs
    // the full backend (honoring `--release`) and writes the target assembly to the output file
    // (default `<stem>.s`) without linking — for inspection and SIMD demos; it needs codegen.
    if let Some(kind) = emit {
        if kind == "asm" {
            let output = output.unwrap_or_else(|| default_output(&input).with_extension("s"));
            let opts = backend::EmitOptions {
                entry: Some("main".into()),
                opt,
                emit: backend::EmitKind::Assembly,
                ..Default::default()
            };
            let asm = compile_object(&input, &opts)?;
            std::fs::write(&output, &asm)
                .map_err(|e| format!("writing {}: {e}", output.display()))?;
            return Ok(());
        }
        return emit_dump(&input, &kind);
    }

    let output = output.unwrap_or_else(|| default_output(&input));

    let opts = backend::EmitOptions {
        entry: Some("main".into()),
        opt,
        ..Default::default()
    };
    let object = compile_object(&input, &opts)?;
    link::link_executable(&object, &output, runtime)?;
    Ok(())
}

/// Produce object bytes for `input`, dispatching on extension: `.mir` straight to the backend,
/// `.bet` through the frontend first (resolving `pull` imports across files via
/// [`frontend::load`], then lowering the merged program with [`frontend::compile_program`]).
fn compile_object(input: &Path, opts: &backend::EmitOptions) -> Result<Vec<u8>, String> {
    match input.extension().and_then(|e| e.to_str()) {
        Some("mir") => {
            let src = std::fs::read_to_string(input)
                .map_err(|e| format!("reading {}: {e}", input.display()))?;
            backend::compile_mir_source(&src, opts).map_err(|e| e.to_string())
        }
        Some("bet") | None => {
            let program = frontend::load(input).map_err(|e| e.to_string())?;
            let module = frontend::compile_program(&program).map_err(|e| e.to_string())?;
            backend::compile_to_object(&module, opts).map_err(|e| e.to_string())
        }
        Some(ext) => Err(format!(
            "unknown input extension `.{ext}` (expected `.bet` or `.mir`)"
        )),
    }
}

/// `bet build --emit=<tokens|ast|mir> <input.bet>` — print a canonical textual dump of a
/// frontend intermediate to stdout. The dumps are byte-for-byte reproducible by the future
/// self-hosted `bet` frontend, so `diff`ing the two localizes a port bug to a single layer.
fn emit_dump(input: &Path, kind: &str) -> Result<(), String> {
    match input.extension().and_then(|e| e.to_str()) {
        Some("bet") | None => {}
        Some(ext) => return Err(format!("`--emit` expects a `.bet` input, got `.{ext}`")),
    }
    // `mir` dumps a whole *program*: it resolves the entry's `pull` imports across files (like
    // `build`/`run`) and prints the merged, resolved, mangled `.mir`, so multi-file programs have a
    // reference dump. `tokens`/`ast` stay single-file — they are pure per-file lexer/parser dumps.
    let dump = match kind {
        "mir" => frontend::dump::mir_program(input),
        "tokens" | "ast" => {
            let src = std::fs::read_to_string(input)
                .map_err(|e| format!("reading {}: {e}", input.display()))?;
            match kind {
                "tokens" => frontend::dump::tokens(&src),
                _ => frontend::dump::ast(&src),
            }
        }
        other => {
            return Err(format!(
                "unknown --emit kind `{other}` (expected `tokens`, `ast`, or `mir`)"
            ));
        }
    }
    .map_err(|e| e.to_string())?;
    print!("{dump}");
    Ok(())
}

fn default_output(input: &Path) -> PathBuf {
    input
        .file_stem()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("a.out"))
}
