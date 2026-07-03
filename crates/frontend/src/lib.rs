//! `frontend` — bet's surface-language front end: lexer → parser → (typecheck) → `midir`.
//! Reused by the driver, the interpreter, the formatter, and the LSP.
//!
//! Two public entry points:
//! - [`parse`] — lex + parse arbitrary `bet` source to a full [`ast::Program`] covering the
//!   frozen surface grammar (`spec/grammar.ebnf` v0.1.1). This is the contract the interpreter
//!   consumes.
//! - [`compile`] — the Step-2 tracer-bullet pipeline: parse, then lower the `spill.it("…")`
//!   subset to a validated `midir` module (used by the `driver`'s LLVM path). Lowering of the
//!   wider grammar to `midir` is still to come; `parse` already accepts it.

pub mod ast;
mod lexer;
mod lower;
mod parser;

/// Re-exported so callers (e.g. the `driver`) can hold the produced module without naming
/// the `midir` crate directly.
pub use midir::Module;

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

/// A failure anywhere in the front-end pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    Lex(String),
    Parse(String),
    Lower(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Lex(m) => write!(f, "lex error: {m}"),
            CompileError::Parse(m) => write!(f, "parse error: {m}"),
            CompileError::Lower(m) => write!(f, "lowering error: {m}"),
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
    let module = lower::lower(&program).map_err(CompileError::Lower)?;
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
