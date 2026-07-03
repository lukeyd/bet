//! `frontend::ast` — the surface AST for `bet`.
//!
//! This is the **contract** the parser produces and downstream consumers (the interpreter,
//! the lowering pass, tooling) read. It mirrors `spec/grammar.ebnf` (FROZEN v0.1.1) node for
//! node; every syntactic production there has a corresponding node here. Keep changes to this
//! module ADDITIVE — the interpreter builds against this shape.
//!
//! Every node that spans source carries a byte-offset [`Span`] so diagnostics and later passes
//! can point back at the surface text.

/// A half-open byte range `[start, end)` into the original source string.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// The dummy span used for synthesized nodes with no source text.
    pub const DUMMY: Span = Span { start: 0, end: 0 };
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Compact form keeps AST snapshots readable: `12..17` rather than `Span { .. }`.
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Declaration / field visibility. Absence of `flex` means `hush` (module-private) by default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vis {
    /// `flex` — exported.
    Flex,
    /// `hush` — module-private (the default).
    Hush,
}

// ---------------------------------------------------------------------------
// Program & top-level items.
// ---------------------------------------------------------------------------

/// A whole compilation unit: a sequence of top-level items.
#[derive(Clone, PartialEq, Debug)]
pub struct Program {
    pub items: Vec<Item>,
}

/// A top-level item (`spec/grammar.ebnf` §S1).
#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    Pull(Pull),
    Func(FnDecl),
    Drip(DripDecl),
    Moods(MoodsDecl),
    Crib(CribDecl),
    Const(ConstDecl),
    Var(VarDecl),
    Extern(ExternDecl),
}

/// `pull "module"` — an import.
#[derive(Clone, PartialEq, Debug)]
pub struct Pull {
    pub module: String,
    pub span: Span,
}

/// `[flex] finna [receiver] name[generics](params) [-> ret] block`.
#[derive(Clone, PartialEq, Debug)]
pub struct FnDecl {
    pub vis: Vis,
    pub receiver: Option<Receiver>,
    pub name: String,
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    pub ret: RetType,
    pub body: Block,
    pub span: Span,
}

/// A method receiver: `(name: Type)` before the function name.
#[derive(Clone, PartialEq, Debug)]
pub struct Receiver {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// A `name: Type` parameter.
#[derive(Clone, PartialEq, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// A function's declared return: nothing, one type, or a parenthesized multi-value list.
#[derive(Clone, PartialEq, Debug)]
pub enum RetType {
    None,
    Single(Type),
    Multi(Vec<Type>),
}

/// `[flex] drip Name[generics] { field* }`.
#[derive(Clone, PartialEq, Debug)]
pub struct DripDecl {
    pub vis: Vis,
    pub name: String,
    pub generics: Vec<String>,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

/// A struct field: `[flex|hush] name: Type`. `vis: None` means the default (`hush`).
#[derive(Clone, PartialEq, Debug)]
pub struct FieldDecl {
    pub vis: Option<Vis>,
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// `[flex] moods Name[generics] { variant, ... }`.
#[derive(Clone, PartialEq, Debug)]
pub struct MoodsDecl {
    pub vis: Vis,
    pub name: String,
    pub generics: Vec<String>,
    pub variants: Vec<Variant>,
    pub span: Span,
}

/// A sum-type variant: `Name` or `Name(Type, ...)`.
#[derive(Clone, PartialEq, Debug)]
pub struct Variant {
    pub name: String,
    pub payload: Vec<Type>,
    pub span: Span,
}

/// `[flex] crib name [: Type]` — an arena declaration (typed slab or untyped bump).
#[derive(Clone, PartialEq, Debug)]
pub struct CribDecl {
    pub vis: Vis,
    pub name: String,
    pub ty: Option<Type>,
    pub span: Span,
}

/// `[flex] facts NAME [: Type] = expr` — a constant.
#[derive(Clone, PartialEq, Debug)]
pub struct ConstDecl {
    pub vis: Vis,
    pub name: String,
    pub ty: Option<Type>,
    pub value: Expr,
    pub span: Span,
}

/// `[flex] lowkey a, b [: Type] = expr, ...` — a (possibly multi-value) binding.
#[derive(Clone, PartialEq, Debug)]
pub struct VarDecl {
    pub vis: Vis,
    pub targets: Vec<String>,
    pub ty: Option<Type>,
    pub values: Vec<Expr>,
    pub span: Span,
}

/// `extern "ABI" finna name(params) [-> Type]` — an FFI import.
#[derive(Clone, PartialEq, Debug)]
pub struct ExternDecl {
    pub abi: String,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: RetType,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Types (§S3).
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum TypeKind {
    /// `[]T`.
    Slice(Box<Type>),
    /// `T[N]` — a fixed-size array.
    Array(Box<Type>, u64),
    /// `tag T`.
    Tag(Box<Type>),
    /// `crib T`.
    Crib(Box<Type>),
    /// `finna(params) -> ret` — a function-pointer type.
    Fn(Vec<Type>, Box<Type>),
    /// `rawptr`.
    RawPtr,
    /// `Name` or `Name[T, ...]` — a named/generic type (`int`, `stash[str, i64]`, `Enemy`).
    Named(String, Vec<Type>),
}

// ---------------------------------------------------------------------------
// Statements (§S4).
// ---------------------------------------------------------------------------

/// A `{ ... }` block of statements.
#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum StmtKind {
    Var(VarDecl),
    Const(ConstDecl),
    Crib(CribDecl),
    /// `fr cond { .. } naw fr cond { .. } naw { .. }`.
    Fr(FrStmt),
    /// `vibin cond { .. }` — a while loop.
    Vibin {
        cond: Expr,
        body: Block,
    },
    /// `squad name in iter { .. }` — a for-each loop.
    Squad {
        var: String,
        iter: Expr,
        body: Block,
    },
    /// `vibe scrutinee { arm* naw { .. } }` — a pattern match.
    Vibe {
        scrutinee: Expr,
        arms: Vec<MatchArm>,
        default: Option<Block>,
    },
    /// `holla binding = tag in crib { live } ghosted { ghosted }`.
    Holla {
        binding: String,
        tag: Expr,
        crib: Expr,
        live: Block,
        ghosted: Block,
    },
    /// `sheesh { .. } [naw name { .. }]` — a panic-recovery boundary.
    Sheesh {
        body: Block,
        recover: Option<(String, Block)>,
    },
    /// `evict crib`.
    Evict(Expr),
    /// `slide call()` — spawn a task.
    Slide(Expr),
    /// `bet v, ...` — return zero or more values.
    Bet(Vec<Expr>),
    /// `bounce err` — early-return-on-error sugar.
    Bounce(Expr),
    /// `yeet(msg)` — panic.
    Yeet(Expr),
    /// `dip` — break.
    Dip,
    /// `skip` — continue.
    Skip,
    /// `lvalue, ... op expr, ...` — assignment / compound-assignment.
    Assign {
        targets: Vec<Expr>,
        op: AssignOp,
        values: Vec<Expr>,
    },
    /// A bare expression evaluated for effect (a call, `cop`, etc.).
    Expr(Expr),
}

/// `fr` / `naw fr` / `naw` chain.
#[derive(Clone, PartialEq, Debug)]
pub struct FrStmt {
    pub cond: Expr,
    pub then: Block,
    /// Zero or more `naw fr cond { .. }` else-if arms.
    pub elifs: Vec<(Expr, Block)>,
    /// An optional trailing `naw { .. }` else arm.
    pub els: Option<Block>,
}

/// A `vibe` arm: `Variant(bind, ...) { .. }`.
#[derive(Clone, PartialEq, Debug)]
pub struct MatchArm {
    pub variant: String,
    pub bindings: Vec<String>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

// ---------------------------------------------------------------------------
// Expressions (§S5 / E1).
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ExprKind {
    /// An integer literal (decimal / hex / binary), value normalized to `i128`.
    Int(i128),
    /// A floating-point literal.
    Float(f64),
    /// A UTF-8 string literal (escapes already decoded).
    Str(String),
    /// A single-byte (`u8`) literal (`'A'`).
    Byte(u8),
    /// `nocap` / `cap`.
    Bool(bool),
    /// `ghosted` — the nil / no-error literal.
    Ghosted,
    /// A name reference, optionally with a generic-argument list (`pickFirst[int]`).
    Name { name: String, generics: Vec<Type> },
    /// `op operand` — `!`, `~`, or unary `-`.
    Unary(UnOp, Box<Expr>),
    /// `lhs op rhs`.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `expr as Type`.
    Cast(Box<Expr>, Type),
    /// `base.name[generics]` — field access (or generic-instantiated member).
    Field {
        base: Box<Expr>,
        name: String,
        generics: Vec<Type>,
    },
    /// `receiver.method[generics](args)` — a method call.
    Method {
        receiver: Box<Expr>,
        method: String,
        generics: Vec<Type>,
        args: Vec<Arg>,
    },
    /// `callee(args)` — a function call.
    Call { callee: Box<Expr>, args: Vec<Arg> },
    /// `base[index]` — indexing.
    Index { base: Box<Expr>, index: Box<Expr> },
    /// `tag.trust() in crib` — unchecked tag resolution.
    Trust { tag: Box<Expr>, crib: Box<Expr> },
    /// `Name{ field: expr, ... }` — a struct literal.
    Struct(StructLit),
    /// `[a, b, c]` — an array/slice literal.
    Array(Vec<Expr>),
    /// `cop init in crib` — allocate into a crib.
    Cop { init: Box<CopInit>, crib: Box<Expr> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    /// `-x`.
    Neg,
    /// `!b`.
    Not,
    /// `~x`.
    BitNot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// A call argument, with an optional `label:`.
#[derive(Clone, PartialEq, Debug)]
pub struct Arg {
    pub label: Option<String>,
    pub value: Expr,
}

/// `Name[generics]{ field: expr, ... }`.
#[derive(Clone, PartialEq, Debug)]
pub struct StructLit {
    pub name: String,
    pub generics: Vec<Type>,
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// The initializer of a `cop` (§S5 `copInit`).
#[derive(Clone, PartialEq, Debug)]
pub enum CopInit {
    /// `Player{ hp: 100 }`.
    Struct(StructLit),
    /// `Add(l, r)` or a nullary `Dot` — a `moods` variant constructor.
    Variant { name: String, args: Vec<Arg> },
}
