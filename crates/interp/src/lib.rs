//! `interp` — a tree-walking interpreter over the frontend AST. Races ahead on the
//! golden corpus to validate that the slang feels good before codegen exists, calls
//! `rt-abi` for memory ops (keeping semantics aligned with the compiled path), and is
//! the differential-testing partner. Later becomes the REPL.
//!
//! # What runs today (Step 3 slice)
//! A meaningful, well-tested subset of the frozen surface grammar, driven directly off
//! [`frontend::ast`]:
//!
//! * literals — `int`, `float`, `byte`, `bool` (`nocap`/`cap`), `str`, `ghosted`;
//! * the full operator set — arithmetic, bitwise, comparison, and short-circuit logical
//!   (`&&`/`||`); precedence is already encoded in the AST tree shape;
//! * bindings — `lowkey`, `facts`, multi-value binds, compound assignment, `.field` stores;
//! * control flow — `fr`/`naw` (if/elif/else), `vibin` (while), `squad` (for-in), `dip`,
//!   `skip`;
//! * `finna` functions with parameters, methods with receivers, first-class function values,
//!   monomorphized generic calls, and multi-value `bet` returns;
//! * `drip` struct construction, field access, and value-copy semantics;
//! * `moods` sum types with `vibe` pattern matching (including the `naw` wildcard);
//! * integer casts with two's-complement wrapping (`300 as u8 == 44`) and float→int
//!   truncation, plus overflow-trapping signed arithmetic (amendment §2.4);
//! * the `spill.it` / `spill.f` output builtins and a minimal `str` module (`glow`, `slaps`).
//!
//! Memory-model constructs (`crib`/`cop`/`evict`, `tag`/`holla`/`trust`), error handling
//! (`sheesh`/`bounce`), and `slide` concurrency parse into the AST but evaluate to a clean
//! [`RunError::Unsupported`] for now — they land once `rt-abi`/`rt-stub` back the value model.
//!
//! # API
//! ```
//! use frontend::ast::*;
//! // A program whose `main` prints "hi\n".
//! let program = Program {
//!     items: vec![Item::Func(FnDecl {
//!         vis: Vis::Hush,
//!         receiver: None,
//!         name: "main".into(),
//!         generics: vec![],
//!         params: vec![],
//!         ret: RetType::None,
//!         body: Block {
//!             stmts: vec![Stmt {
//!                 span: Span::DUMMY,
//!                 kind: StmtKind::Expr(Expr {
//!                     span: Span::DUMMY,
//!                     kind: ExprKind::Method {
//!                         receiver: Box::new(Expr {
//!                             span: Span::DUMMY,
//!                             kind: ExprKind::Name { name: "spill".into(), generics: vec![] },
//!                         }),
//!                         method: "it".into(),
//!                         generics: vec![],
//!                         args: vec![Arg {
//!                             label: None,
//!                             value: Expr {
//!                                 span: Span::DUMMY,
//!                                 kind: ExprKind::Str("hi".into()),
//!                             },
//!                         }],
//!                     },
//!                 }),
//!             }],
//!             span: Span::DUMMY,
//!         },
//!         span: Span::DUMMY,
//!     })],
//! };
//! assert_eq!(interp::run_to_string(&program).unwrap(), "hi\n");
//! ```

mod error;
mod interp;
mod value;

pub use error::RunError;
pub use interp::Interp;
pub use value::{Value, display};

use frontend::ast::Program;

/// Execute `program`'s `finna main()`, writing its output to stdout.
pub fn run(program: &Program) -> Result<(), RunError> {
    use std::io::Write;
    let mut vm = Interp::new(program)?;
    vm.exec_main()?;
    std::io::stdout()
        .write_all(vm.output())
        .map_err(|e| RunError::Io(e.to_string()))
}

/// Execute `program`'s `finna main()`, capturing its output as a string.
///
/// This is the testable twin of [`run`]: unit tests build [`frontend::ast`] values by hand
/// and assert the captured output against the corpus `.expected` strings.
pub fn run_to_string(program: &Program) -> Result<String, RunError> {
    let mut vm = Interp::new(program)?;
    vm.exec_main()?;
    vm.into_output_string()
}
