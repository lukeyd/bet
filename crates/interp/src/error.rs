//! [`RunError`] — every way a `bet` program can fail to run under the interpreter.
//!
//! The interpreter has no static typechecker in front of it (that lands with the frontend
//! fan-out), so a number of these are conditions a fully-typed program would never hit; we
//! still report them cleanly rather than panicking.

use crate::value::Value;

/// A recoverable failure while interpreting a program.
#[derive(Clone, Debug, PartialEq)]
pub enum RunError {
    /// The program has no `finna main()` entry point.
    NoMain,
    /// A name was used that resolves to no binding, function, or variant.
    Undefined(String),
    /// A value was called that is not callable (not a function or variant constructor).
    NotCallable(String),
    /// A call (or variant constructor) got the wrong number of arguments.
    Arity {
        what: String,
        expected: usize,
        got: usize,
    },
    /// A multi-value binding expected N values but the right-hand side produced M.
    Destructure { expected: usize, got: usize },
    /// An operation was applied to a value of the wrong shape (e.g. `.field` on an int).
    Type(String),
    /// Integer division or remainder by zero.
    DivByZero,
    /// A signed integer operation overflowed (the debug-build "trap"; amendment §2.4).
    Overflow(String),
    /// Field access named a field the struct does not have.
    UnknownField { ty: String, field: String },
    /// A `vibe` match had no arm covering the scrutinee (and no `naw` wildcard).
    NonExhaustive(String),
    /// A `spill.f` format string was malformed or under-supplied with arguments.
    BadFormat(String),
    /// `yeet(e)` — an explicit panic; carries the yeeted value.
    Yeet(Value),
    /// A grammatically valid construct the interpreter does not yet evaluate.
    Unsupported(String),
    /// Evaluation nested deeper than the interpreter's recursion cap (issue #38) — reported
    /// instead of overflowing the native stack. Fires on unbounded `finna` recursion (a missing
    /// base case) and on evaluating a pathologically deep AST such as the left-nested `Binary`
    /// tree from `1 + 1 + 1 + …`. `depth` is the cap that was hit.
    RecursionLimit { depth: u32 },
    /// A program-controlled allocation size exceeded the interpreter's cap (issue #40) — reported
    /// *before* allocating, so a hostile size (`mem.slab[int](1 << 40)`, an `int[1000000000]`
    /// field, or a `gg` framebuffer whose `w * h` overflows) can't OOM the host or panic on `Vec`
    /// capacity overflow. `requested` is the element count asked for; `cap` is the ceiling.
    AllocLimit { requested: u128, cap: usize },
    /// Writing captured output to the sink failed.
    Io(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NoMain => write!(f, "no `finna main()` to run"),
            RunError::Undefined(n) => write!(f, "undefined name `{n}`"),
            RunError::NotCallable(n) => write!(f, "`{n}` is not callable"),
            RunError::Arity {
                what,
                expected,
                got,
            } => write!(f, "`{what}` expects {expected} argument(s), got {got}"),
            RunError::Destructure { expected, got } => {
                write!(f, "cannot bind {got} value(s) to {expected} name(s)")
            }
            RunError::Type(m) => write!(f, "type error: {m}"),
            RunError::DivByZero => write!(f, "division by zero"),
            RunError::Overflow(m) => write!(f, "integer overflow in {m}"),
            RunError::UnknownField { ty, field } => {
                write!(f, "`{ty}` has no field `{field}`")
            }
            RunError::NonExhaustive(m) => write!(f, "non-exhaustive `vibe` over {m}"),
            RunError::BadFormat(m) => write!(f, "bad format string: {m}"),
            RunError::Yeet(v) => write!(f, "yeet: {}", crate::value::display(v)),
            RunError::Unsupported(m) => write!(f, "unsupported construct: {m}"),
            RunError::RecursionLimit { depth } => write!(
                f,
                "recursion limit exceeded (depth {depth}); likely unbounded recursion \
                 (a missing base case) or an over-deep expression"
            ),
            RunError::AllocLimit { requested, cap } => write!(
                f,
                "allocation too large: requested {requested} element(s), cap is {cap}"
            ),
            RunError::Io(m) => write!(f, "output error: {m}"),
        }
    }
}

impl std::error::Error for RunError {}
