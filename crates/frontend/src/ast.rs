//! `frontend::ast` — the surface abstract syntax tree.
//!
//! This module is the **second public contract of the frontend crate** (alongside
//! [`crate::compile`]): the interpreter (`interp -> frontend`), the formatter (`fmt`),
//! and the LSP (`lsp`) all consume this tree. It is a faithful, one-to-one image of the
//! frozen surface grammar in `spec/grammar.ebnf` (v0.1.1) — every production there has a
//! node here — and it holds *pure data only* (no parsing, typing, or lowering logic).
//!
//! ## Stability contract (Step 3 fan-out)
//! Treat this shape like `midir`/`rt-abi`: during the fan-out it changes **additively
//! only** (new variants/fields), and any breaking change is a coordinated note to the
//! `interp`/`fmt`/`lsp` owners, never a silent edit. The parser (`crate::parser`) is the
//! sole producer of these nodes; downstream crates are consumers.
//!
//! Grammar cross-references (`§Sn` / `§amend n`) are cited on each node.

/// A half-open byte range `[start, end)` into the original source, for diagnostics.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Declaration visibility. Absence of `flex` means [`Vis::Hush`] (module-private) — the
/// default settled in plan-amendment-01 §2.8.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Vis {
    /// `flex` — exported from the module.
    Flex,
    /// `hush` — module-private (the default).
    Hush,
}

// ============================================================================
// §S1 — Program & top-level items
// ============================================================================

/// A whole compilation unit: `{ topItem }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub items: Vec<Item>,
}

/// One top-level item (`topItem = pull | topDecl`). `vis` is meaningful only for the
/// declaration kinds; it is [`Vis::Hush`] for `Import`/`Extern`.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub vis: Vis,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    /// `pull "spill"` — import a module (§S1).
    Import(String),
    /// `finna ...` — function or method (§S2).
    Fn(FnDecl),
    /// `drip Name { ... }` — struct (§S2).
    Drip(DripDecl),
    /// `moods Name { ... }` — sum type (§S2, §amend 2.1).
    Moods(MoodsDecl),
    /// `crib name [: T]` — arena declaration (§S2).
    Crib(CribDecl),
    /// `facts NAME [: T] = expr` — constant (§S2).
    Const(ConstDecl),
    /// `lowkey a, b [: T] = ...` — binding (§S2).
    Var(VarDecl),
    /// `extern "C" finna f(...) -> T` — FFI declaration (§S1, §amend 2.6).
    Extern(ExternDecl),
}

// ============================================================================
// §S2 — Declarations
// ============================================================================

/// `finna [receiver] name [generics] (params) [-> ret] block`.
#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: String,
    /// Method receiver `(p: Player)`, if any (§amend 2.8).
    pub receiver: Option<Param>,
    /// Monomorphized generic parameters `[T, U]` (§amend 2.2).
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    /// Return types: empty = `void`, one = single, many = multi-value return (§6).
    pub ret: Vec<Type>,
    pub body: Block,
}

/// `ident ":" type` — a parameter or receiver binding.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// `drip Name [generics] { field* }`.
#[derive(Clone, Debug, PartialEq)]
pub struct DripDecl {
    pub name: String,
    pub generics: Vec<String>,
    pub fields: Vec<FieldDecl>,
}

/// `[flex|hush] ident ":" type` — a struct field.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    pub vis: Vis,
}

/// `moods Name [generics] { variant, ... }` (§amend 2.1).
#[derive(Clone, Debug, PartialEq)]
pub struct MoodsDecl {
    pub name: String,
    pub generics: Vec<String>,
    pub variants: Vec<Variant>,
}

/// `ident [ "(" type, ... ")" ]` — a sum-type variant with an optional payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: String,
    pub payload: Vec<Type>,
}

/// `crib ident [: type]` — untyped bump arena or typed slab (§7.2).
#[derive(Clone, Debug, PartialEq)]
pub struct CribDecl {
    pub name: String,
    pub ty: Option<Type>,
}

/// `facts ident [: type] = expr` — a compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub name: String,
    pub ty: Option<Type>,
    pub value: Expr,
}

/// `lowkey a, b [: type] = e1, e2` — a (possibly multi-value) binding.
#[derive(Clone, Debug, PartialEq)]
pub struct VarDecl {
    pub names: Vec<String>,
    pub ty: Option<Type>,
    pub values: Vec<Expr>,
}

/// `extern "abi" finna name(params) [-> ret]` (§amend 2.6).
#[derive(Clone, Debug, PartialEq)]
pub struct ExternDecl {
    pub abi: String,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
}

// ============================================================================
// §S3 — Types
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// `[]T` — slice.
    Slice(Box<Type>),
    /// `T[N]` — fixed-size array (grammar restricts the element to a name type).
    Array { elem: Box<Type>, len: u64 },
    /// `tag T` — a generational handle (8 bytes).
    Tag(Box<Type>),
    /// `crib T` — a crib reference (parameter/field position).
    Crib(Box<Type>),
    /// `finna(A, B) -> R` — a first-class function type (§amend 2.5).
    Fn { params: Vec<Type>, ret: Box<Type> },
    /// `rawptr` — the FFI-only raw pointer (§amend 2.6).
    RawPtr,
    /// `int`, `str`, `Enemy`, `stash[K, V]`, ... — a named type with optional generic args.
    /// Predeclared names (`void`, the numeric tower, `bool`, `str`, `yikes`, ...) are
    /// represented here with an empty arg list; resolution happens in typecheck.
    Name { name: String, args: Vec<Type> },
}

// ============================================================================
// §S4 — Statements
// ============================================================================

/// `{ stmt* }` — a brace-delimited block.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Var(VarDecl),
    Const(ConstDecl),
    Crib(CribDecl),
    /// `fr cond { } naw fr cond { } naw { }` — if / else-if / else.
    Fr(FrStmt),
    /// `vibin cond { }` — while loop.
    Vibin(VibinStmt),
    /// `squad x in iter { }` — for-in loop.
    Squad(SquadStmt),
    /// `vibe e { arm* naw { } }` — pattern match (§amend 2.1).
    Vibe(VibeStmt),
    /// `holla x = handle in arena { } ghosted { }` — checked tag deref (§7.4).
    Holla(HollaStmt),
    /// `sheesh { } [naw ident { }]` — recover boundary (§6; binding form provisional).
    Sheesh(SheeshStmt),
    /// `evict crib` — O(1) mass free (§7.2).
    Evict(Expr),
    /// `slide task` — spawn a task (§8).
    Slide(Expr),
    /// `bet [e1, e2]` — return 0..n values (§6).
    Bet(Vec<Expr>),
    /// `bounce err` — early error return (§amend 2.8).
    Bounce(Expr),
    /// `yeet(e)` — panic (§6).
    Yeet(Expr),
    /// `dip` — break.
    Dip,
    /// `skip` — continue.
    Skip,
    /// `a, b op= e1, e2` — assignment.
    Assign(AssignStmt),
    /// A bare expression evaluated for effect (call / method / cop).
    Expr(Expr),
}

/// `fr cond { } { naw fr cond { } } [ naw { } ]`.
#[derive(Clone, Debug, PartialEq)]
pub struct FrStmt {
    pub cond: Expr,
    pub then: Block,
    pub elifs: Vec<ElseIf>,
    pub els: Option<Block>,
}

/// One `naw fr cond { }` arm of an [`FrStmt`].
#[derive(Clone, Debug, PartialEq)]
pub struct ElseIf {
    pub cond: Expr,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VibinStmt {
    pub cond: Expr,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SquadStmt {
    pub binder: String,
    pub iter: Expr,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VibeStmt {
    pub scrutinee: Expr,
    /// Arms in source order; the `naw` wildcard (if present) is the arm whose pattern is
    /// [`Pattern::Wildcard`].
    pub arms: Vec<VibeArm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VibeArm {
    pub pat: Pattern,
    pub body: Block,
}

/// A `vibe` arm pattern. v1 patterns are a variant tag plus flat payload bindings, or the
/// `naw` wildcard — no nested/guarded patterns yet (§amend 2.1).
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// `Variant(a, b)` or `Variant` — binds the payload to the given names.
    Variant { name: String, binds: Vec<String> },
    /// `naw` — the exhaustiveness wildcard.
    Wildcard,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HollaStmt {
    pub binder: String,
    pub handle: Expr,
    pub arena: Expr,
    pub body: Block,
    pub ghosted: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SheeshStmt {
    pub body: Block,
    pub recover: Option<Recover>,
}

/// The `naw ident { }` recover arm of a [`SheeshStmt`].
#[derive(Clone, Debug, PartialEq)]
pub struct Recover {
    pub binder: String,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssignStmt {
    pub targets: Vec<Expr>,
    pub op: AssignOp,
    pub values: Vec<Expr>,
}

/// `=` and the compound-assignment operators (§S4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Eq,
    AddEq,
    SubEq,
    MulEq,
    DivEq,
    RemEq,
    AndEq,
    OrEq,
    XorEq,
    ShlEq,
    ShrEq,
}

// ============================================================================
// §S5 / E1 — Expressions
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    // --- literals (§L3) ---
    /// An integer literal's raw magnitude; width/signedness resolved in typecheck. A `u64`
    /// holds every `intLit` (dec/hex/bin); negatives come from a [`UnOp::Neg`] wrapper.
    Int(u64),
    Float(f64),
    Str(String),
    Byte(u8),
    Bool(bool),
    /// `ghosted` — nil / no-error literal.
    Ghosted,

    // --- names & access ---
    /// `x` or a generic target `foo[T, U]`.
    Name {
        name: String,
        args: Vec<Type>,
    },
    /// `recv.field` or `recv.method[T]` (a following [`ExprKind::Call`] makes it a call).
    Field {
        recv: Box<Expr>,
        name: String,
        generics: Vec<Type>,
    },
    /// `recv[index]`.
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
    },
    /// `callee(args)`.
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },

    // --- operators (§E1) ---
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `e as T` — explicit cast (§amend 2.4).
    Cast {
        expr: Box<Expr>,
        ty: Type,
    },

    // --- memory (§7) ---
    /// `cop init in crib` — allocate into an arena (§7.2/§7.3).
    Cop {
        init: CopInit,
        crib: Box<Expr>,
    },
    /// `handle.trust() in arena` — unchecked tag deref (§7.5).
    Trust {
        handle: Box<Expr>,
        arena: Box<Expr>,
    },

    // --- composite literals ---
    Struct(StructLit),
    /// `[e1, e2, ...]` — array literal; element type inferred (§S5).
    Array(Vec<Expr>),
}

/// The thing being allocated by a `cop` expression: a struct literal (`Player{...}`) or a
/// moods-variant constructor (`Lit(2)`, `Dot`).
#[derive(Clone, Debug, PartialEq)]
pub enum CopInit {
    Struct(StructLit),
    Variant { name: String, args: Vec<Expr> },
}

/// `Name{ field: expr, ... }` — a struct literal (§S5). `ty` is always a name type.
#[derive(Clone, Debug, PartialEq)]
pub struct StructLit {
    pub ty: Type,
    pub fields: Vec<FieldInit>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
}

/// A call argument with an optional label (`in: crib`).
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub label: Option<String>,
    pub value: Expr,
}

/// Prefix unary operators `! ~ -` (§E1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `!` logical not.
    Not,
    /// `~` bitwise not.
    BitNot,
    /// `-` arithmetic negation.
    Neg,
}

/// Binary operators (§E1). Precedence/associativity live in the parser, not here.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
