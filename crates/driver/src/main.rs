//! `bet` — the driver CLI users invoke. `bet build <input> [-o out]` threads a program through
//! the pipeline to a native executable (`.mir` input goes straight to the backend; `.bet` input
//! through the frontend first), linking against the bootstrap `rt-stub` archive (see `link`).
//! `bet run <input.bet>` parses a program and executes it on the tree-walking interpreter,
//! writing its output to stdout — no LLVM required. `fmt` lands later.

mod link;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
bet — the bet compiler driver

USAGE:
    bet build <input.bet|input.mir> [-o <output>]
    bet run   <input.bet>

`build` compiles a program to a native executable, linking it against the bootstrap runtime.
Requires a codegen-enabled build (`--features llvm`); without it, `build` reports that no code
generator is present.

`run` parses a `.bet` program and executes it on the tree-walking interpreter, writing its
output to stdout. No codegen or LLVM is required.";

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
    let src =
        std::fs::read_to_string(&input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    let program = frontend::parse(&src).map_err(|e| e.to_string())?;
    interp::run(&program).map_err(|e| e.to_string())
}

fn build(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                let path = args.get(i).ok_or("`-o` needs a path")?;
                output = Some(PathBuf::from(path));
            }
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
    let output = output.unwrap_or_else(|| default_output(&input));

    let opts = backend::EmitOptions {
        entry: Some("main".into()),
        ..Default::default()
    };
    let object = compile_object(&input, &opts)?;
    link::link_executable(&object, &output)?;
    Ok(())
}

/// Produce object bytes for `input`, dispatching on extension: `.mir` straight to the backend,
/// `.bet` through the frontend first.
fn compile_object(input: &Path, opts: &backend::EmitOptions) -> Result<Vec<u8>, String> {
    let src =
        std::fs::read_to_string(input).map_err(|e| format!("reading {}: {e}", input.display()))?;
    match input.extension().and_then(|e| e.to_str()) {
        Some("mir") => backend::compile_mir_source(&src, opts).map_err(|e| e.to_string()),
        Some("bet") | None => {
            let module = frontend::compile(&src).map_err(|e| e.to_string())?;
            backend::compile_to_object(&module, opts).map_err(|e| e.to_string())
        }
        Some(ext) => Err(format!(
            "unknown input extension `.{ext}` (expected `.bet` or `.mir`)"
        )),
    }
}

fn default_output(input: &Path) -> PathBuf {
    input
        .file_stem()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("a.out"))
}
