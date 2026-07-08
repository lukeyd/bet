//! `backend` — bet's code generator: lowers `midir` to LLVM IR (via `inkwell`, behind the
//! non-default `llvm` feature) and emits a native object file. Knows only the IR and the
//! runtime ABI, never the frontend.
//!
//! The public surface here is **LLVM-free** — `inkwell` types never appear in a signature —
//! so `driver` (and the default `cargo build`) compile without LLVM. Real code generation
//! lives in the [`codegen`] module, compiled only under `--features llvm`; without it,
//! [`compile_to_object`] returns [`BackendError::NoCodegen`].
//!
//! Step 2 (tracer bullet) scope: enough of the IR to thread `spill.it("hi")` — externs,
//! string-literal data pointers/lengths, direct calls, and a synthesized C `main` entry —
//! through to a linked, running binary. Broader lowering lands with the backend fan-out.

#[cfg(feature = "llvm")]
mod codegen;

/// How to emit an object from a module.
#[derive(Clone, Debug)]
pub struct EmitOptions {
    /// Target triple to emit for; `None` uses the host triple.
    pub target: Option<String>,
    /// If set, synthesize a C-ABI `main` that initializes the runtime, calls the named bet
    /// function, shuts the runtime down, and returns 0. `None` emits no entry wrapper.
    pub entry: Option<String>,
    /// Optimization level.
    pub opt: OptLevel,
    /// What kind of artifact to emit — native object code (the default) or textual assembly.
    pub emit: EmitKind,
}

impl Default for EmitOptions {
    fn default() -> Self {
        EmitOptions {
            target: None,
            entry: None,
            opt: OptLevel::O0,
            emit: EmitKind::Object,
        }
    }
}

/// Optimization level for object emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    /// No optimization (allocas left for the register allocator; fastest to emit).
    O0,
    /// Moderate optimization: runs the LLVM `default<O2>` mid-level pipeline (inliner, mem2reg,
    /// SROA, loop + SLP vectorizers) before codegen. This is what makes zero-cost abstractions
    /// actually free and lets `soa` loops auto-vectorize.
    O2,
}

/// What kind of artifact [`compile_to_object`] / [`compile_mir_source`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmitKind {
    /// Native object code (`.o`) — the default; the driver links it into an executable.
    #[default]
    Object,
    /// Textual target assembly (`.s`), for inspection and SIMD demos. Not linked.
    Assembly,
}

/// Anything that can go wrong turning a module into an object.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// This build was compiled without the `llvm` feature, so no code generator exists.
    #[error("backend was built without codegen; rebuild with `--features llvm`")]
    NoCodegen,
    /// The `.mir` source failed to parse (only from [`compile_mir_source`]).
    #[error("parse error: {0}")]
    Parse(String),
    /// The module failed validation.
    #[error("invalid module: {0}")]
    Validate(String),
    /// An IR construct isn't supported by the current (minimal) code generator.
    #[error("cannot lower: {0}")]
    Lower(String),
    /// A target/target-machine or object-emission failure.
    #[error("target error: {0}")]
    Target(String),
}

/// Compile a validated [`midir::Module`] to a native object file's bytes.
///
/// Without the `llvm` feature this always returns [`BackendError::NoCodegen`]; the signature
/// is identical in both builds so callers need no `cfg`.
#[cfg(feature = "llvm")]
pub fn compile_to_object(
    module: &midir::Module,
    opts: &EmitOptions,
) -> Result<Vec<u8>, BackendError> {
    codegen::compile(module, opts)
}

/// See [`compile_to_object`]. This is the no-LLVM stub.
#[cfg(not(feature = "llvm"))]
pub fn compile_to_object(
    _module: &midir::Module,
    _opts: &EmitOptions,
) -> Result<Vec<u8>, BackendError> {
    Err(BackendError::NoCodegen)
}

/// Parse `.mir` text, validate it, and compile it to an object. Lets a caller feed the
/// textual IR without depending on `midir` directly.
pub fn compile_mir_source(src: &str, opts: &EmitOptions) -> Result<Vec<u8>, BackendError> {
    let module = midir::parse(src).map_err(|e| BackendError::Parse(e.to_string()))?;
    midir::validate(&module).map_err(|errs| {
        let joined = errs
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        BackendError::Validate(joined)
    })?;
    compile_to_object(&module, opts)
}
