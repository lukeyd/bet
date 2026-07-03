//! The mid-level IR data model.
//!
//! Shape: **MIR-style** (à la Rust MIR / Swift SIL). A [`Func`] is a list of typed
//! [`Local`]s and a control-flow graph of [`Block`]s; each block is a run of [`Stmt`]s
//! ending in exactly one [`Terminator`]. Values live in locals (assignable more than
//! once) addressed through [`Place`]s (projections); there are **no phi nodes** — the
//! backend lowers each local to an LLVM `alloca` and lets `mem2reg`/SROA build SSA.
//!
//! Generics arrive **already monomorphized**: the IR never sees type parameters.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Ids — dense u32 indices into the module's / function's tables.
// ---------------------------------------------------------------------------

macro_rules! define_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub u32);

        impl $name {
            /// This id as a `usize` index.
            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

define_id!(/// Index into the module's interned type table.
    TyId);
define_id!(/// Index into the module's interned signature table.
    SigId);
define_id!(/// Index into the module's `struct` (`drip`) table.
    StructId);
define_id!(/// Index into the module's `sum` (`moods`) table.
    SumId);
define_id!(/// Index into the module's function table.
    FuncId);
define_id!(/// Index into the module's global-constant (`facts`) table.
    GlobalId);
define_id!(/// Index into the module's module-level `crib` (global arena) table.
    CribGlobalId);
define_id!(/// Index into the module's `extern` (FFI import) table.
    ExternId);
define_id!(/// Index of a local within a [`Func`].
    LocalId);
define_id!(/// Index of a block within a [`Func`].
    BlockId);

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Width of an integer type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IntWidth {
    W8,
    W16,
    W32,
    W64,
}

impl IntWidth {
    /// The width in bits.
    pub fn bits(self) -> u32 {
        match self {
            IntWidth::W8 => 8,
            IntWidth::W16 => 16,
            IntWidth::W32 => 32,
            IntWidth::W64 => 64,
        }
    }
}

/// A structural type. Interned in the [`Module`]; refer to one by [`TyId`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
    Bool,
    Int {
        width: IntWidth,
        signed: bool,
    },
    F32,
    F64,
    /// `str` — a UTF-8 string value.
    Str,
    /// The unit / no-value type (a function returning nothing).
    Void,
    /// An FFI raw pointer (`rawptr`); only meaningful at the `extern` boundary.
    RawPtr,
    Struct(StructId),
    Sum(SumId),
    /// `[]T` — a slice.
    Slice(TyId),
    /// `T[N]` — a fixed-size array.
    Array(TyId, u64),
    /// `tag T` — an 8-byte generational handle into a typed crib.
    Tag(TyId),
    /// `crib T` — a crib (arena) handle, as a parameter or field type.
    Crib(TyId),
    /// A live reference into a crib element, produced by a `holla` check or `trust`.
    /// Distinct from [`TyKind::Tag`]: a tag is a checkable handle, a ref is resolved.
    Ref(TyId),
    /// `map[K]V` — an opaque, runtime-backed hash map handle.
    Map(TyId, TyId),
    /// `vec[T]` — an opaque, runtime-backed growable-array handle.
    Vec(TyId),
    /// A function-pointer value; the pointee signature is interned.
    FnPtr(SigId),
    /// An anonymous tuple, used to carry multi-value returns as one value.
    Tuple(Vec<TyId>),
}

/// A function signature (for interned function-pointer types).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Sig {
    pub params: Vec<TyId>,
    pub rets: Vec<TyId>,
}

// ---------------------------------------------------------------------------
// Aggregate definitions.
// ---------------------------------------------------------------------------

/// Field / declaration visibility (`flex` = exported, `hush` = module-private).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vis {
    Flex,
    Hush,
}

/// A `drip` (struct) definition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    pub name: String,
    pub ty: TyId,
    pub vis: Vis,
}

/// A `moods` (sum / tagged-union) definition. Variant order is the discriminant order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SumDef {
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Variant {
    pub name: String,
    /// Payload field types (empty for a nullary variant).
    pub payload: Vec<TyId>,
}

/// A global constant (`facts`).
#[derive(Clone, PartialEq, Debug)]
pub struct Global {
    pub name: String,
    pub ty: TyId,
    pub value: Const,
}

/// A module-level `crib` (a global arena). Unlike a `fact`, this is runtime-initialized
/// mutable storage: the backend reserves a global holding the [`CribHandle`] and initializes
/// it at startup. Referenced from a function body by [`Rvalue::CribGlobal`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CribGlobal {
    pub name: String,
    /// The element type (`void` for an untyped bump crib).
    pub elem: TyId,
    /// Slot count (typed) or byte reserve (bump); 0 = default.
    pub capacity: u32,
}

/// An `extern "C"` function declaration (FFI import).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Extern {
    pub name: String,
    pub abi: String,
    pub sig: Sig,
}

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// A compile-time constant operand.
#[derive(Clone, PartialEq, Debug)]
pub enum Const {
    /// An integer literal carrying its (interned) integer type.
    Int(i128, TyId),
    /// A float literal carrying its (interned) float type (`F32` or `F64`).
    Float(f64, TyId),
    Bool(bool),
    Str(String),
    /// `ghosted` — the nil / no-error / null-tag literal.
    Ghosted,
    /// A first-class function value (a code pointer), e.g. a `think` field.
    FnRef(FuncId),
}

// ---------------------------------------------------------------------------
// Functions, locals, blocks.
// ---------------------------------------------------------------------------

/// A function: a signature, typed locals, and a CFG of blocks.
///
/// The first `params.len()` locals are the parameters (in order); the rest are
/// temporaries. Return values are named directly by the [`Terminator::Return`] operand
/// list rather than through a dedicated return local.
#[derive(Clone, PartialEq, Debug)]
pub struct Func {
    pub name: String,
    pub params: Vec<TyId>,
    pub rets: Vec<TyId>,
    pub locals: Vec<Local>,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
}

impl Func {
    /// The declared type of a local.
    pub fn local_ty(&self, l: LocalId) -> TyId {
        self.locals[l.index()].ty
    }

    /// The block with the given id (blocks are stored in id order).
    pub fn block(&self, b: BlockId) -> &Block {
        &self.blocks[b.index()]
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Local {
    pub ty: TyId,
    pub name: Option<String>,
    pub kind: LocalKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalKind {
    Param,
    Temp,
}

/// A basic block: a straight-line run of statements ending in one terminator.
#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub id: BlockId,
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
}

// ---------------------------------------------------------------------------
// Places & operands.
// ---------------------------------------------------------------------------

/// An lvalue: a local reached through zero or more projections.
#[derive(Clone, PartialEq, Debug)]
pub struct Place {
    pub local: LocalId,
    pub proj: Vec<Proj>,
}

impl Place {
    /// A bare local with no projections.
    pub fn local(l: LocalId) -> Place {
        Place {
            local: l,
            proj: Vec::new(),
        }
    }
}

/// A single projection step off a base value.
#[derive(Clone, PartialEq, Debug)]
pub enum Proj {
    /// `.field(i)` on a struct or the current sum-variant downcast.
    Field(u32),
    /// `[i]` on a slice or array.
    Index(Operand),
    /// Dereference a `ref`.
    Deref,
    /// Interpret a sum value as a specific variant (precedes its payload `Field`s).
    Downcast(u32),
}

/// A read of a value: either a literal, or a copy/move out of a place.
#[derive(Clone, PartialEq, Debug)]
pub enum Operand {
    Const(Const),
    Copy(Place),
    Move(Place),
}

// ---------------------------------------------------------------------------
// Statements & rvalues.
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub enum Stmt {
    /// `place = rvalue`.
    Assign(Place, Rvalue),
    /// `evict crib` — O(1) mass-free that bumps every slot generation.
    Evict(Operand),
    Nop,
}

/// A value-producing computation (the right-hand side of an assignment).
#[derive(Clone, PartialEq, Debug)]
pub enum Rvalue {
    Use(Operand),
    BinOp(BinOp, Operand, Operand, ArithMode),
    UnOp(UnOp, Operand),
    Cast(Operand, TyId, CastKind),
    /// A direct or indirect call. Multi-value results have tuple type; the callee
    /// destructures via `Field` projections.
    Call(Callee, Vec<Operand>),
    /// Build a value-typed aggregate (a `drip` value or a tuple) from its fields.
    Aggregate(AggKind, Vec<Operand>),
    /// Read the discriminant of a sum value (for a `Switch`).
    Discriminant(Operand),
    /// `cop init in crib` — allocate into a crib. Yields `tag T` for a typed crib,
    /// or `rawptr` for an untyped bump crib.
    Cop(Operand, CopInit),
    /// `tag.trust() in crib` — unchecked resolve to a `ref` (backend picks
    /// checked-in-debug vs. raw-load-in-release).
    Trust(Operand, Operand),
    /// The data pointer of a `str` value, as a `rawptr`. For a literal, the backend
    /// interns a private byte-array global and yields its address. This is one of the
    /// two projections of the (eventual) fat `str` `{ ptr, len }`, so it applies to any
    /// `str` operand, not just literals.
    StrPtr(Operand),
    /// The byte length of a `str` value, as `u64` — the other `str` projection.
    StrLen(Operand),
    /// The address of a place's storage, as a `rawptr` (the data pointer for a fat slice,
    /// or an FFI-boundary raw address). The place must be memory-backed (every local is).
    AddrOf(Place),
    /// Build a fat `[]elem` slice value from a data pointer and an element count. The two
    /// projections of the fat `{ ptr, len }` are `AddrOf`-style reads / `Index`.
    MakeSlice {
        data: Operand,
        len: Operand,
        elem: TyId,
    },
    /// `crib name: T[N]` / `crib name` — create a fresh crib, yielding a `crib elem` handle.
    /// A typed crib (`elem` non-void) is a slab of `capacity` fixed-size slots; a bump crib
    /// (`elem` = void) is an untyped arena where `capacity` is a byte reserve (0 = default).
    /// The backend supplies element size/alignment from the target data layout.
    CribNew {
        elem: TyId,
        capacity: u32,
    },
    /// The handle of a module-level `crib` (loads the backing global). Yields `crib elem`.
    CribGlobal(CribGlobalId),
    /// The size in bytes of a type, as `u64` — the target-layout store size. Lets the frontend
    /// pass value/key sizes to runtime primitives (e.g. `bet_map_new`) without knowing the ABI.
    SizeOf(TyId),
    /// Build a fat `str` value from a data pointer and a byte length — the str counterpart of
    /// [`Rvalue::MakeSlice`] (a `str` shares the `{ ptr, len }` layout).
    MakeStr {
        data: Operand,
        len: Operand,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub enum Callee {
    Direct(FuncId),
    Indirect(Operand),
    /// A direct call to an `extern "C"` FFI import (e.g. an `rt-abi` entry point such as
    /// `bet_print`). Resolved through the module's `extern` table, not the `func` table.
    Extern(ExternId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AggKind {
    Struct(StructId),
    Tuple,
    /// A fixed `[elem; N]` array value built from `N` operands (`N` is the operand count).
    Array(TyId),
    /// A by-value `moods` (sum) value: the given variant with its payload operands. Sets
    /// the discriminant and stores the payload without allocating into a crib (the `cop`
    /// path is [`CopInit::SumVariant`]).
    Sum {
        sum: SumId,
        variant: u32,
    },
}

/// How a `cop` initializes the freshly allocated slot.
#[derive(Clone, PartialEq, Debug)]
pub enum CopInit {
    /// `Player{ hp: 100 }` — a struct literal (field index → value).
    StructLit(StructId, Vec<(u32, Operand)>),
    /// `Add(l, r)` — a sum variant with its payload operands.
    SumVariant(SumId, u32, Vec<Operand>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    /// True for the comparison operators, whose result type is `bool`.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    /// Arithmetic negation (`-x`).
    Neg,
    /// Logical not (`!b`) on a bool.
    Not,
    /// Bitwise not (`~x`) on an integer.
    BitNot,
}

/// Overflow behavior of an integer [`BinOp`] (amendment §2.4). `Na` for float/bool ops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArithMode {
    /// Wrap on overflow (unsigned arithmetic; explicit `math.lap` wrapping ops).
    Wrap,
    /// Trap on overflow in debug, wrap in release (signed arithmetic).
    Trap,
    /// Not applicable (float, bool, comparison, or bitwise result).
    Na,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CastKind {
    /// Zero-extend a narrower integer to a wider one.
    IntZext,
    /// Sign-extend a narrower integer to a wider one.
    IntSext,
    /// Truncate a wider integer to a narrower one.
    IntTrunc,
    IntToFloat,
    FloatToInt,
    /// `f32 <-> f64`.
    FloatResize,
    /// Reinterpret the bits (same size); FFI / `bytes.cast`.
    Bitcast,
}

// ---------------------------------------------------------------------------
// Terminators.
// ---------------------------------------------------------------------------

/// The single control-flow instruction that ends a [`Block`].
#[derive(Clone, PartialEq, Debug)]
pub enum Terminator {
    Goto(BlockId),
    /// Two-way branch on a `bool` operand.
    Branch {
        cond: Operand,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    /// Multi-way branch on an integer scrutinee (a raw int or a `Discriminant`).
    /// Backs `vibe` matching and multi-arm `fr`/`naw` chains.
    Switch {
        scrutinee: Operand,
        cases: Vec<(u64, BlockId)>,
        default: BlockId,
    },
    /// `holla resolved = tag in crib { .. } ghosted { .. }` — the checked generational
    /// access. On the live edge, `resolved` is bound to the element ref before jumping
    /// to `live`; otherwise control goes to `ghosted`.
    HollaCheck {
        tag: Operand,
        crib: Operand,
        resolved: Place,
        live: BlockId,
        ghosted: BlockId,
    },
    /// Return zero or more values (must match the function's `rets`).
    Return(Vec<Operand>),
    /// `yeet(msg)` — panic.
    Panic(Operand),
    /// Statically unreachable (e.g. right after a `Panic`).
    Unreachable,
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

/// A whole compilation unit: interned types & signatures, aggregate/extern/global
/// definitions, and functions.
#[derive(Default)]
pub struct Module {
    types: Vec<TyKind>,
    type_dedup: HashMap<TyKind, TyId>,
    sigs: Vec<Sig>,
    sig_dedup: HashMap<Sig, SigId>,
    structs: Vec<StructDef>,
    sums: Vec<SumDef>,
    externs: Vec<Extern>,
    globals: Vec<Global>,
    crib_globals: Vec<CribGlobal>,
    funcs: Vec<Func>,
}

impl Module {
    pub fn new() -> Module {
        Module::default()
    }

    // --- type interning ---

    /// Intern a type, returning its (deduplicated) id.
    pub fn intern_ty(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.type_dedup.get(&kind) {
            return id;
        }
        let id = TyId(self.types.len() as u32);
        self.types.push(kind.clone());
        self.type_dedup.insert(kind, id);
        id
    }

    pub fn ty(&self, id: TyId) -> &TyKind {
        &self.types[id.index()]
    }

    pub fn types(&self) -> &[TyKind] {
        &self.types
    }

    // --- signature interning ---

    pub fn intern_sig(&mut self, sig: Sig) -> SigId {
        if let Some(&id) = self.sig_dedup.get(&sig) {
            return id;
        }
        let id = SigId(self.sigs.len() as u32);
        self.sigs.push(sig.clone());
        self.sig_dedup.insert(sig, id);
        id
    }

    pub fn sig(&self, id: SigId) -> &Sig {
        &self.sigs[id.index()]
    }

    pub fn sigs(&self) -> &[Sig] {
        &self.sigs
    }

    // --- aggregates / externs / globals ---

    pub fn add_struct(&mut self, def: StructDef) -> StructId {
        let id = StructId(self.structs.len() as u32);
        self.structs.push(def);
        id
    }

    pub fn struct_def(&self, id: StructId) -> &StructDef {
        &self.structs[id.index()]
    }

    pub fn structs(&self) -> &[StructDef] {
        &self.structs
    }

    pub fn add_sum(&mut self, def: SumDef) -> SumId {
        let id = SumId(self.sums.len() as u32);
        self.sums.push(def);
        id
    }

    pub fn sum_def(&self, id: SumId) -> &SumDef {
        &self.sums[id.index()]
    }

    pub fn sums(&self) -> &[SumDef] {
        &self.sums
    }

    pub fn add_extern(&mut self, ext: Extern) -> ExternId {
        let id = ExternId(self.externs.len() as u32);
        self.externs.push(ext);
        id
    }

    pub fn extern_def(&self, id: ExternId) -> &Extern {
        &self.externs[id.index()]
    }

    pub fn externs(&self) -> &[Extern] {
        &self.externs
    }

    pub fn add_global(&mut self, global: Global) -> GlobalId {
        let id = GlobalId(self.globals.len() as u32);
        self.globals.push(global);
        id
    }

    pub fn globals(&self) -> &[Global] {
        &self.globals
    }

    pub fn add_crib_global(&mut self, crib: CribGlobal) -> CribGlobalId {
        let id = CribGlobalId(self.crib_globals.len() as u32);
        self.crib_globals.push(crib);
        id
    }

    pub fn crib_global(&self, id: CribGlobalId) -> &CribGlobal {
        &self.crib_globals[id.index()]
    }

    pub fn crib_globals(&self) -> &[CribGlobal] {
        &self.crib_globals
    }

    // --- functions ---

    pub fn add_func(&mut self, func: Func) -> FuncId {
        let id = FuncId(self.funcs.len() as u32);
        self.funcs.push(func);
        id
    }

    pub fn func(&self, id: FuncId) -> &Func {
        &self.funcs[id.index()]
    }

    pub fn funcs(&self) -> &[Func] {
        &self.funcs
    }

    // --- common-type conveniences (used heavily by the builder & tests) ---

    pub fn t_bool(&mut self) -> TyId {
        self.intern_ty(TyKind::Bool)
    }

    pub fn t_int(&mut self, width: IntWidth, signed: bool) -> TyId {
        self.intern_ty(TyKind::Int { width, signed })
    }

    pub fn t_i64(&mut self) -> TyId {
        self.t_int(IntWidth::W64, true)
    }

    pub fn t_u32(&mut self) -> TyId {
        self.t_int(IntWidth::W32, false)
    }

    pub fn t_str(&mut self) -> TyId {
        self.intern_ty(TyKind::Str)
    }

    pub fn t_void(&mut self) -> TyId {
        self.intern_ty(TyKind::Void)
    }

    pub fn t_tag(&mut self, elem: TyId) -> TyId {
        self.intern_ty(TyKind::Tag(elem))
    }

    pub fn t_ref(&mut self, elem: TyId) -> TyId {
        self.intern_ty(TyKind::Ref(elem))
    }

    pub fn t_crib(&mut self, elem: TyId) -> TyId {
        self.intern_ty(TyKind::Crib(elem))
    }
}
