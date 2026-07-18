//! `frontend` — bet's surface-language front end: lexer → parser → (typecheck) → `midir`.
//! Reused by the driver, the interpreter, the formatter, and the LSP.
//!
//! Two public entry points:
//! - [`parse`] — lex + parse arbitrary `bet` source to a full [`ast::Program`] covering the
//!   frozen surface grammar (`spec/grammar.ebnf` v0.1.1). This is the contract the interpreter
//!   consumes.
//! - [`compile`] — the full pipeline: parse, resolve, then lower to a validated `midir`
//!   module (used by the `driver`'s LLVM path). Lowering covers the whole frozen grammar —
//!   it is what compiles the DOOM port and the self-hosted frontend (`selfhost/betfe.bet`).

pub mod ast;
pub mod dump;
mod lexer;
mod loader;
mod lower;
mod parser;
mod resolve;

pub use loader::load;

/// Re-exported so tooling (the formatter) can name the recovered comment trivia.
pub use lexer::{Comment, CommentKind};

/// Re-exported so callers (e.g. the `driver`) can hold the produced module without naming
/// the `midir` crate directly.
pub use midir::Module;

/// Lexical trivia recovered alongside a parse — currently the source's comments, span-sorted.
///
/// The surface [`ast`] deliberately carries no comments (they are structurally irrelevant), so a
/// tool that must preserve them — the formatter — gets them here instead and re-interleaves them
/// by span. The compiler pipeline uses [`parse`] and never sees this.
#[derive(Clone, Debug, Default)]
pub struct Trivia {
    pub comments: Vec<Comment>,
}

/// Parse `bet` source into a surface [`ast::Program`].
///
/// This is the front end's primary entry point: it runs the full lexer (numeric tower,
/// string/byte literals, every operator, Go-style ASI) and the recursive-descent parser over
/// the frozen grammar. Downstream consumers (the interpreter, lowering, tooling) build on the
/// returned tree.
pub fn parse(src: &str) -> Result<ast::Program, CompileError> {
    let tokens = lexer::tokenize(src).map_err(CompileError::Lex)?;
    parser::parse(&tokens).map_err(CompileError::Parse)
}

/// Parse `bet` source like [`parse`], additionally returning the source's lexical [`Trivia`]
/// (its comments), for tooling that must round-trip them.
///
/// The parsed [`ast::Program`] is identical to what [`parse`] returns — the recovered comments
/// ride alongside in the token scan and never affect parsing. Only the formatter needs this; the
/// compiler pipeline stays on [`parse`].
pub fn parse_with_trivia(src: &str) -> Result<(ast::Program, Trivia), CompileError> {
    let (tokens, comments) = lexer::tokenize_with_trivia(src).map_err(CompileError::Lex)?;
    let program = parser::parse(&tokens).map_err(CompileError::Parse)?;
    Ok((program, Trivia { comments }))
}

/// A failure anywhere in the front-end pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    Lex(String),
    Parse(String),
    Lower(String),
    /// Module-graph resolution failure: a missing imported file, an import cycle, a namespace
    /// collision, or a `flex`/`hush` visibility violation (see [`load`]).
    Load(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Lex(m) => write!(f, "lex error: {m}"),
            CompileError::Parse(m) => write!(f, "parse error: {m}"),
            CompileError::Lower(m) => write!(f, "lowering error: {m}"),
            CompileError::Load(m) => write!(f, "load error: {m}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile bet source to a validated `midir` module.
///
/// Tracer-bullet scope: lowers the `spill.it("…")` print subset (see [`lower`]). Broader
/// programs parse via [`parse`] but return a lowering [`CompileError::Lower`] here until the
/// full AST→IR pass lands.
pub fn compile(src: &str) -> Result<Module, CompileError> {
    let program = parse(src)?;
    compile_program(&program)
}

/// Lower an already-parsed (and, for multi-file programs, [`load`]-resolved) [`ast::Program`] to
/// a validated `midir` module. This is the lowering tail of [`compile`], split out so the driver
/// can feed it a merged multi-file program.
pub fn compile_program(program: &ast::Program) -> Result<Module, CompileError> {
    let module = lower::lower(program).map_err(CompileError::Lower)?;
    midir::validate(&module).map_err(|errs| {
        CompileError::Lower(
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_hello() {
        let src = "pull \"spill\"\nfinna main() {\n    spill.it(\"hi\")\n}\n";
        let m = compile(src).expect("hello should compile");
        // One extern (bet_print) and one function (main).
        assert_eq!(m.externs().len(), 1);
        assert_eq!(m.funcs().len(), 1);
        assert_eq!(m.funcs()[0].name, "main");
    }

    #[test]
    fn prints_computed_scalar() {
        // A computed integer now lowers to a typed runtime print (`bet_print_i64`); this used
        // to be a clean lowering error before the `spill` value-print pass landed.
        let src = "finna main() {\n    spill.it(42)\n}\n";
        let m = compile(src).expect("spill.it(<int>) should lower now");
        assert!(
            m.externs().iter().any(|e| e.name == "bet_print_i64"),
            "expected a synthesized bet_print_i64 extern"
        );
    }
}
