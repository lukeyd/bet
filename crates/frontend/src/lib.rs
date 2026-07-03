//! `frontend` — bet's surface-language front end: lexer → parser → (typecheck) → `midir`.
//! Reused by the driver, the interpreter, the formatter, and the LSP.
//!
//! Step-2 (tracer bullet) scope: exactly enough to lower `tests/corpus/01-basics/hello.bet`
//! (`pull "spill"` + `finna main() { spill.it("hi") }`) to a validated `midir` module. The
//! real lexer/parser/typechecker — the full frozen grammar — lands with the frontend fan-out.

pub mod ast;

mod lexer;
mod lower;
mod parser;

/// Re-exported so callers (e.g. the `driver`) can hold the produced module without naming
/// the `midir` crate directly.
pub use midir::Module;

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
pub fn compile(src: &str) -> Result<Module, CompileError> {
    let tokens = lexer::tokenize(src).map_err(CompileError::Lex)?;
    let program = parser::parse(&tokens).map_err(CompileError::Parse)?;
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
    fn rejects_non_string_print() {
        // `42` isn't in this minimal frontend's token/statement set — it must be rejected
        // (as a lex error today), not silently mislowered.
        let src = "finna main() {\n    spill.it(42)\n}\n";
        assert!(compile(src).is_err());
    }
}
