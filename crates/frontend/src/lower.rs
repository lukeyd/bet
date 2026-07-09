//! Lower the surface [`ast`] to `midir`.
//!
//! This is the real AST→IR pass (superseding the Step-2 `spill.it("literal")` tracer bullet).
//! It lowers the *ready subset* of the surface language — the part the backend already
//! codegens — and returns a clean "not yet lowered" [`Err`] for everything still on the
//! frontier (dynamic strings, array/map literals, `squad`, `sheesh`, `slide`, generics,
//! module-level cribs). The design keeps the Rust boring: a [`LowerCtx`] resolves names and
//! interns types against the module; a per-function walker emits into a [`FuncBuilder`].
//!
//! What lowers today:
//! * **Functions** — parameters, single- and multi-value (`Tuple`/`Return(vec)`) returns, and
//!   receiver methods (the receiver is a leading parameter). `extern` FFI imports.
//! * **Expressions** — int/float/bool/byte/string/`ghosted` literals; local reads; `facts`
//!   constants inlined at the use site (the IR has no global-read operand); function values
//!   (`Const::FnRef`); `BinOp` with the right [`ArithMode`] (Trap for signed, Wrap for
//!   unsigned, per amendment §2.4); `UnOp`; short-circuit `&&`/`||` via control flow; `Cast`;
//!   direct/indirect/extern calls; struct field access; struct construction (`Aggregate`); the
//!   memory model (`cop`, `trust`).
//! * **Statements** — `lowkey` (incl. multi-value destructuring), local `facts`, assignment and
//!   compound-assignment, `fr`/`naw` chains, `vibin` while-loops, `dip`/`skip`, `bet`, `yeet`,
//!   `vibe` matches (`Discriminant`+`Switch`+`Downcast`), `crib` decls, `holla`, and `evict`.
//!
//! Verification is IR-level: every function this pass emits passes `midir::validate`, and the
//! `.mir` text is snapshotted in the frontend tests. `spill.it` / `spill.f` now lower for every
//! scalar type — ints (via `bet_print_i64`/`bet_print_u64`), floats (`bet_print_f64`), `bool`
//! (a `nocap`/`cap` branch), `ghosted`, and string literals (`bet_print`) — matching the
//! interpreter's `display`. Computed (non-literal) strings remain a backend gap and return a
//! clean `Err`.

use crate::ast::{self, Expr, ExprKind, Item, Stmt, StmtKind};
use midir::*;
use std::collections::HashMap;

/// Lower a whole program to a validated-shape `midir` module.
pub fn lower(prog: &ast::Program) -> Result<Module, String> {
    let mut cx = LowerCtx::new();
    cx.collect(prog)?;
    cx.lower_items(prog)?;
    Ok(cx.m)
}

/// The built-in stdlib "module" names — the `spill.it` / `math.lap` intrinsic namespaces. These
/// are compiler intrinsics, not source files: `pull "spill"` is a no-op and a `spill.method(…)`
/// receiver is dispatched here rather than resolved to a user function. The module loader
/// (`crate::loader`) consults this so `pull "spill"` is never treated as a file import, and so a
/// user namespace can't shadow a built-in. This is the single source of truth for the set.
pub(crate) fn is_builtin_module(name: &str) -> bool {
    matches!(
        name,
        "spill"
            | "str"
            | "math"
            | "mem"
            | "bytes"
            | "fmt"
            | "stash"
            | "vec"
            | "yikes"
            | "fs"
            | "sys"
            | "gg"
            // Stdlib groupings that `pull` names but that aren't dispatched as `module.method`
            // (squadops ops are collection receiver-methods; time/net are reserved). Listed so the
            // loader treats `pull "squadops"` as a no-op instead of a self-referential file import.
            // See language-spec.md §9.2.
            | "squadops"
            | "time"
            | "net"
    )
}

/// A resolved `facts` constant: the literal value plus its interned type.
#[derive(Clone)]
struct ConstVal {
    value: Const,
    ty: TyId,
}

/// A reserved-but-not-yet-lowered monomorphized function instance.
#[derive(Clone)]
struct MonoFnJob {
    /// The generic function's base name.
    name: String,
    /// The concrete type arguments (already resolved to interned ids).
    args: Vec<TyId>,
    /// The `FuncId` reserved for this instance (must match its eventual `add_func`).
    id: FuncId,
}

/// A collected user-function signature, keyed by name for call/`@f` resolution.
#[derive(Clone)]
struct FuncSig {
    id: FuncId,
    params: Vec<TyId>,
    rets: Vec<TyId>,
    /// True if the first `params` entry is a method receiver.
    has_receiver: bool,
    /// False when the signature could not be interned (e.g. a generic function); calling it
    /// is then a clean lowering error rather than a panic.
    ok: bool,
}

/// The whole lowering state: the module under construction, module-scoped name resolution
/// (types, functions, externs, globals), and — while a function body is being walked — the
/// per-function builder, scope stack, and control-flow bookkeeping.
struct LowerCtx {
    m: Module,

    // Module-scope name resolution.
    structs: HashMap<String, StructId>,
    sums: HashMap<String, SumId>,
    /// Variant name → every `(sum, variant-index)` that declares it (usually one).
    variants: HashMap<String, Vec<(SumId, u32)>>,
    funcs: HashMap<String, FuncSig>,
    externs: HashMap<String, (ExternId, Vec<TyId>, Vec<TyId>)>,
    globals: HashMap<String, ConstVal>,
    /// Module-level `crib` name → its `(CribGlobalId, crib T)` handle type.
    crib_globals: HashMap<String, (CribGlobalId, TyId)>,

    // --- generics / monomorphization ---
    /// Generic function/struct definitions, kept by name for on-demand instantiation.
    generic_funcs: HashMap<String, ast::FnDecl>,
    generic_structs: HashMap<String, ast::DripDecl>,
    /// Instantiation caches: `(name, concrete type args)` → the monomorphized id.
    mono_structs: HashMap<(String, Vec<TyId>), StructId>,
    mono_funcs: HashMap<(String, Vec<TyId>), FuncId>,
    /// The monomorphic `(params, rets)` of each reserved function instance, by id.
    mono_sigs: HashMap<FuncId, (Vec<TyId>, Vec<TyId>)>,
    /// Reserved-but-not-yet-lowered function instances (drained after the concrete funcs).
    mono_worklist: Vec<MonoFnJob>,
    /// The next `FuncId` to hand a monomorphized instance (starts past the concrete funcs).
    next_mono_fid: u32,
    /// The active type-parameter substitution while lowering a generic instance.
    subst: HashMap<String, TyId>,
    /// The synthesized `__yikes { present: bool, msg: str }` struct backing the `yikes` type in
    /// the compiled path (created lazily on first use of `yikes`).
    yikes_sid: Option<StructId>,
    /// The synthesized `__gg_FrameBuffer { pixels: rawptr, width, height, stride: u32 }` struct
    /// matching `rt-abi`'s `#[repr(C)] FrameBuffer` (created lazily on first `gg.blit`).
    frame_sid: Option<StructId>,
    /// The synthesized `__gg_Event { kind, code: u32, x, y: i32 }` struct matching `rt-abi`'s
    /// `#[repr(C)] Event` (created lazily on first `gg.poll`).
    event_sid: Option<StructId>,
    /// `bet_print(rawptr, u64) -> void` — the stdout entry point (always present).
    print_extern: ExternId,
    /// Deduped externs synthesized on demand, keyed by `(name, ret-type)`.
    /// Keyed by the full signature `(name, params, rets)`, not just the name: one C symbol can
    /// appear under several midir signatures (e.g. `bet_vec_push` per `vec[T]` element type —
    /// all erase to the same `ptr fn(ptr, ptr)` at the ABI, and the backend dedups the LLVM
    /// declaration by name). Keying on the name alone would alias distinct element types onto
    /// the first one's parameter types, tripping the validator.
    extern_cache: HashMap<(String, Vec<TyId>, Vec<TyId>), ExternId>,

    // Per-function state (valid only while lowering a body).
    fb: Option<FuncBuilder>,
    local_tys: Vec<TyId>,
    scopes: Vec<HashMap<String, LocalId>>,
    local_consts: HashMap<String, ConstVal>,
    /// `(header, exit)` per enclosing loop, for `skip`/`dip`.
    loops: Vec<(BlockId, BlockId)>,
    cur_rets: Vec<TyId>,
    /// The logical current block and whether it already has a terminator.
    cur: BlockId,
    done: bool,
}

impl LowerCtx {
    fn new() -> LowerCtx {
        let mut m = Module::new();
        let rawptr = m.intern_ty(TyKind::RawPtr);
        let u64t = m.t_int(IntWidth::W64, false);
        let print_extern = m.add_extern(Extern {
            name: "bet_print".into(),
            abi: "C".into(),
            sig: Sig {
                params: vec![rawptr, u64t],
                rets: vec![],
            },
        });
        LowerCtx {
            m,
            structs: HashMap::new(),
            sums: HashMap::new(),
            variants: HashMap::new(),
            funcs: HashMap::new(),
            externs: HashMap::new(),
            globals: HashMap::new(),
            crib_globals: HashMap::new(),
            generic_funcs: HashMap::new(),
            generic_structs: HashMap::new(),
            mono_structs: HashMap::new(),
            mono_funcs: HashMap::new(),
            mono_sigs: HashMap::new(),
            mono_worklist: Vec::new(),
            next_mono_fid: 0,
            subst: HashMap::new(),
            yikes_sid: None,
            frame_sid: None,
            event_sid: None,
            print_extern,
            extern_cache: HashMap::new(),
            fb: None,
            local_tys: Vec::new(),
            scopes: Vec::new(),
            local_consts: HashMap::new(),
            loops: Vec::new(),
            cur_rets: Vec::new(),
            cur: BlockId(0),
            done: false,
        }
    }

    // === collection pass =====================================================

    /// Pre-register every type, function, extern, and global name so bodies may refer to
    /// declarations that appear later (and to each other). Ids follow appearance order per
    /// kind, matching the order the lowering pass adds them in.
    fn collect(&mut self, prog: &ast::Program) -> Result<(), String> {
        // 1a. Assign struct/sum ids by appearance order. Generic aggregates get no id here;
        // they are stored for on-demand monomorphization and their instances appended later.
        let (mut sn, mut un) = (0u32, 0u32);
        for item in &prog.items {
            match item {
                Item::Drip(d) if !d.generics.is_empty() => {
                    self.generic_structs.insert(d.name.clone(), d.clone());
                }
                Item::Drip(d) => {
                    self.structs.insert(d.name.clone(), StructId(sn));
                    sn += 1;
                }
                Item::Moods(md) => {
                    self.sums.insert(md.name.clone(), SumId(un));
                    un += 1;
                }
                _ => {}
            }
        }
        // 1b. Build field/variant types (names now resolvable) and add the defs in order.
        for item in &prog.items {
            match item {
                Item::Drip(d) => {
                    if !d.generics.is_empty() {
                        continue; // generic — monomorphized on demand
                    }
                    let mut fields = Vec::with_capacity(d.fields.len());
                    for f in &d.fields {
                        let ty = self.map_type(&f.ty)?;
                        fields.push(Field {
                            name: f.name.clone(),
                            ty,
                            vis: match f.vis {
                                Some(ast::Vis::Flex) => Vis::Flex,
                                _ => Vis::Hush,
                            },
                        });
                    }
                    self.m.add_struct(StructDef {
                        name: d.name.clone(),
                        fields,
                    });
                }
                Item::Moods(md) => {
                    let mut variants = Vec::with_capacity(md.variants.len());
                    for v in &md.variants {
                        let mut payload = Vec::with_capacity(v.payload.len());
                        for t in &v.payload {
                            payload.push(self.map_type(t)?);
                        }
                        variants.push(Variant {
                            name: v.name.clone(),
                            payload,
                        });
                    }
                    self.m.add_sum(SumDef {
                        name: md.name.clone(),
                        variants,
                    });
                }
                _ => {}
            }
        }

        // 1c. Index every variant name for constructor resolution.
        for (name, &sid) in &self.sums {
            let _ = name;
            for (i, v) in self.m.sum_def(sid).variants.iter().enumerate() {
                self.variants
                    .entry(v.name.clone())
                    .or_default()
                    .push((sid, i as u32));
            }
        }

        // 2. Externs (FFI imports).
        for item in &prog.items {
            if let Item::Extern(e) = item {
                let mut params = Vec::with_capacity(e.params.len());
                for p in &e.params {
                    params.push(self.map_type(&p.ty)?);
                }
                let rets = self.ret_types(&e.ret)?;
                let id = self.m.add_extern(Extern {
                    name: e.name.clone(),
                    abi: e.abi.clone(),
                    sig: Sig {
                        params: params.clone(),
                        rets: rets.clone(),
                    },
                });
                self.externs.insert(e.name.clone(), (id, params, rets));
            }
        }

        // 3. Function signatures (id by concrete-function order — the same order lowering adds).
        // Generic functions get no id; they are stored for on-demand monomorphization, and their
        // instances are appended after all concrete functions (starting at `next_mono_fid`).
        let mut fid = 0u32;
        for item in &prog.items {
            if let Item::Func(f) = item {
                if f.generics.is_empty() {
                    let sig = self.collect_fn_sig(f, FuncId(fid));
                    self.funcs.insert(f.name.clone(), sig);
                    fid += 1;
                } else {
                    self.generic_funcs.insert(f.name.clone(), f.clone());
                }
            }
        }
        self.next_mono_fid = fid;

        // 4. Module-level `facts` constants.
        for item in &prog.items {
            if let Item::Const(c) = item {
                let cv = self.eval_const(c)?;
                self.globals.insert(c.name.clone(), cv);
            }
        }
        Ok(())
    }

    fn collect_fn_sig(&mut self, f: &ast::FnDecl, id: FuncId) -> FuncSig {
        if !f.generics.is_empty() {
            return FuncSig {
                id,
                params: Vec::new(),
                rets: Vec::new(),
                has_receiver: f.receiver.is_some(),
                ok: false,
            };
        }
        let mut params = Vec::new();
        let mut ok = true;
        if let Some(r) = &f.receiver {
            match self.map_type(&r.ty) {
                Ok(t) => params.push(t),
                Err(_) => ok = false,
            }
        }
        for p in &f.params {
            match self.map_type(&p.ty) {
                Ok(t) => params.push(t),
                Err(_) => ok = false,
            }
        }
        let rets = self.ret_types(&f.ret).unwrap_or_else(|_| {
            ok = false;
            Vec::new()
        });
        FuncSig {
            id,
            params,
            rets,
            has_receiver: f.receiver.is_some(),
            ok,
        }
    }

    fn ret_types(&mut self, ret: &ast::RetType) -> Result<Vec<TyId>, String> {
        match ret {
            ast::RetType::None => Ok(vec![]),
            ast::RetType::Single(t) => Ok(vec![self.map_type(t)?]),
            ast::RetType::Multi(ts) => ts.iter().map(|t| self.map_type(t)).collect(),
        }
    }

    /// Evaluate a (module- or function-level) `facts` initializer to a constant. Only literal
    /// forms are supported — enough for the corpus; anything else is a clean error.
    fn eval_const(&mut self, c: &ast::ConstDecl) -> Result<ConstVal, String> {
        let hint = match &c.ty {
            Some(t) => Some(self.map_type(t)?),
            None => None,
        };
        let (value, ty) = self.const_literal(&c.value, hint)?;
        Ok(ConstVal { value, ty })
    }

    /// A restricted constant evaluator for `facts` values: literals and their unary negation.
    fn const_literal(&mut self, e: &Expr, hint: Option<TyId>) -> Result<(Const, TyId), String> {
        match &e.kind {
            ExprKind::Int(v) => {
                let ty = self.int_hint_or_default(hint);
                Ok((Const::Int(*v, ty), ty))
            }
            ExprKind::Float(v) => {
                let ty = self.float_hint_or_default(hint);
                Ok((Const::Float(*v, ty), ty))
            }
            ExprKind::Bool(b) => Ok((Const::Bool(*b), self.m.t_bool())),
            ExprKind::Str(s) => Ok((Const::Str(s.clone()), self.m.t_str())),
            ExprKind::Byte(b) => {
                let ty = self.m.t_int(IntWidth::W8, false);
                Ok((Const::Int(*b as i128, ty), ty))
            }
            ExprKind::Unary(ast::UnOp::Neg, inner) => match &inner.kind {
                ExprKind::Int(v) => {
                    let ty = self.int_hint_or_default(hint);
                    Ok((Const::Int(-*v, ty), ty))
                }
                ExprKind::Float(v) => {
                    let ty = self.float_hint_or_default(hint);
                    Ok((Const::Float(-*v, ty), ty))
                }
                _ => Err("`facts` initializer must be a literal".into()),
            },
            _ => Err("`facts` initializer must be a literal (constant folding is minimal)".into()),
        }
    }

    // === type mapping ========================================================

    /// Map a surface [`ast::Type`] to an interned [`TyId`].
    fn map_type(&mut self, t: &ast::Type) -> Result<TyId, String> {
        match &t.kind {
            ast::TypeKind::Slice(e) => {
                let e = self.map_type(e)?;
                Ok(self.m.intern_ty(TyKind::Slice(e)))
            }
            ast::TypeKind::Array(e, n) => {
                let e = self.map_type(e)?;
                Ok(self.m.intern_ty(TyKind::Array(e, *n)))
            }
            ast::TypeKind::Tag(e) => {
                let e = self.map_type(e)?;
                Ok(self.m.intern_ty(TyKind::Tag(e)))
            }
            ast::TypeKind::Crib(e) => {
                let e = self.map_type(e)?;
                Ok(self.m.intern_ty(TyKind::Crib(e)))
            }
            ast::TypeKind::Soa(inner) => {
                let inner_id = self.map_type(inner)?;
                // `soa` wraps a container (array / slice / vec) of a `drip` element.
                let elem = match self.m.ty(inner_id) {
                    TyKind::Array(e, _) | TyKind::Slice(e) | TyKind::Vec(e) => *e,
                    _ => {
                        return Err("nah — soa only wraps a container of a drip: \
                             `soa Enemy[N]`, `soa []Enemy`, or `soa vec[Enemy]`."
                            .into());
                    }
                };
                let sid = match self.m.ty(elem) {
                    TyKind::Struct(sid) => *sid,
                    _ => {
                        return Err("soa only vibes with a drip (struct) element — \
                             that container's element ain't a drip."
                            .into());
                    }
                };
                // No soa inside soa — the transpose only goes one level deep.
                let nested = self
                    .m
                    .struct_def(sid)
                    .fields
                    .iter()
                    .find(|f| matches!(self.m.ty(f.ty), TyKind::Soa(_)))
                    .map(|f| f.name.clone());
                if let Some(fname) = nested {
                    let sname = self.m.struct_def(sid).name.clone();
                    return Err(format!(
                        "no soa inside soa, fam — drip `{sname}` has a nested soa field `{fname}`."
                    ));
                }
                Ok(self.m.intern_ty(TyKind::Soa(inner_id)))
            }
            ast::TypeKind::RawPtr => Ok(self.m.intern_ty(TyKind::RawPtr)),
            ast::TypeKind::Fn(params, ret) => {
                let params: Vec<TyId> = params
                    .iter()
                    .map(|p| self.map_type(p))
                    .collect::<Result<_, _>>()?;
                let rets = match &ret.kind {
                    // A `finna(..) -> void`/`nada` pointer returns nothing.
                    ast::TypeKind::Named(n, _) if n == "void" || n == "nada" => vec![],
                    _ => vec![self.map_type(ret)?],
                };
                let sig = self.m.intern_sig(Sig { params, rets });
                Ok(self.m.intern_ty(TyKind::FnPtr(sig)))
            }
            ast::TypeKind::Named(name, generics) => {
                if generics.is_empty() {
                    self.named_type(name)
                } else if name == "stash" {
                    // `stash[K, V]` — a hash map handle.
                    if generics.len() != 2 {
                        return Err("`stash` takes two type arguments `[K, V]`".into());
                    }
                    let k = self.map_type(&generics[0])?;
                    let v = self.map_type(&generics[1])?;
                    Ok(self.m.intern_ty(TyKind::Map(k, v)))
                } else if name == "vec" {
                    // `vec[T]` — a growable-array handle.
                    if generics.len() != 1 {
                        return Err("`vec` takes one type argument `[T]`".into());
                    }
                    let e = self.map_type(&generics[0])?;
                    Ok(self.m.intern_ty(TyKind::Vec(e)))
                } else {
                    // Generic aggregate instantiation, e.g. `Pair[int]` → the mono struct.
                    let sid = self.mono_struct(name, generics)?;
                    Ok(self.m.intern_ty(TyKind::Struct(sid)))
                }
            }
        }
    }

    /// The (lazily-created) `__yikes { present: bool, msg: str }` struct id. A `yikes` value is
    /// this struct: `present` distinguishes a live error from `ghosted`, `msg` carries the text.
    fn yikes_struct(&mut self) -> StructId {
        if let Some(sid) = self.yikes_sid {
            return sid;
        }
        let boolt = self.m.t_bool();
        let strt = self.m.t_str();
        let sid = self.m.add_struct(StructDef {
            name: "__yikes".into(),
            fields: vec![
                Field {
                    name: "present".into(),
                    ty: boolt,
                    vis: Vis::Hush,
                },
                Field {
                    name: "msg".into(),
                    ty: strt,
                    vis: Vis::Hush,
                },
            ],
        });
        self.yikes_sid = Some(sid);
        sid
    }

    /// True if `ty` is the synthesized `yikes` struct.
    fn is_yikes(&self, ty: TyId) -> bool {
        matches!(self.m.ty(ty), TyKind::Struct(s) if Some(*s) == self.yikes_sid)
    }

    /// Build a `yikes` value: `present` set for a live error, plus its message operand.
    fn build_yikes(&mut self, present: bool, msg: Operand) -> (Operand, TyId) {
        let sid = self.yikes_struct();
        let sty = self.m.intern_ty(TyKind::Struct(sid));
        let tmp = self.new_local(sty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Aggregate(
                AggKind::Struct(sid),
                vec![Operand::Const(Const::Bool(present)), msg],
            ),
        );
        (Operand::Copy(Place::local(tmp)), sty)
    }

    /// The (lazily-created) `__gg_FrameBuffer` struct id. Its field layout matches `rt-abi`'s
    /// `#[repr(C)] FrameBuffer { pixels: *mut u32, width: u32, height: u32, stride: u32 }`: a
    /// leading `rawptr` (forcing 8-byte alignment) followed by three `u32`s.
    fn frame_struct(&mut self) -> StructId {
        if let Some(sid) = self.frame_sid {
            return sid;
        }
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let u32t = self.m.t_u32();
        let sid = self.m.add_struct(StructDef {
            name: "__gg_FrameBuffer".into(),
            fields: vec![
                Field {
                    name: "pixels".into(),
                    ty: rawptr,
                    vis: Vis::Hush,
                },
                Field {
                    name: "width".into(),
                    ty: u32t,
                    vis: Vis::Hush,
                },
                Field {
                    name: "height".into(),
                    ty: u32t,
                    vis: Vis::Hush,
                },
                Field {
                    name: "stride".into(),
                    ty: u32t,
                    vis: Vis::Hush,
                },
            ],
        });
        self.frame_sid = Some(sid);
        sid
    }

    /// The (lazily-created) `__gg_Event` struct id, matching `rt-abi`'s
    /// `#[repr(C)] Event { kind: u32, code: u32, x: i32, y: i32 }`.
    fn event_struct(&mut self) -> StructId {
        if let Some(sid) = self.event_sid {
            return sid;
        }
        let u32t = self.m.t_u32();
        let i32t = self.m.t_int(IntWidth::W32, true);
        let sid = self.m.add_struct(StructDef {
            name: "__gg_Event".into(),
            fields: vec![
                Field {
                    name: "kind".into(),
                    ty: u32t,
                    vis: Vis::Hush,
                },
                Field {
                    name: "code".into(),
                    ty: u32t,
                    vis: Vis::Hush,
                },
                Field {
                    name: "x".into(),
                    ty: i32t,
                    vis: Vis::Hush,
                },
                Field {
                    name: "y".into(),
                    ty: i32t,
                    vis: Vis::Hush,
                },
            ],
        });
        self.event_sid = Some(sid);
        sid
    }

    /// The zero value of ANY field-shaped type, for zero-defaulting omitted fields in a
    /// struct literal / `cop T{}` (spec §5). Scalars are their literal zeros; a `tag` is the
    /// null tag (always ghosted); handle-shaped types (fn values, `vec`/`stash`/`rng`, raw
    /// pointers) are the null pointer — safe to hold and overwrite, a crash only on use; a
    /// slice is the empty `{ null, 0 }` fat value; nested drips and fixed arrays recurse.
    /// May emit statements (aggregate temps) into the current block.
    fn zero_value(&mut self, ty: TyId) -> Result<Operand, String> {
        Ok(match self.m.ty(ty).clone() {
            TyKind::Int { .. } => Operand::Const(Const::Int(0, ty)),
            TyKind::F32 | TyKind::F64 => Operand::Const(Const::Float(0.0, ty)),
            TyKind::Bool => Operand::Const(Const::Bool(false)),
            TyKind::Str => Operand::Const(Const::Str(String::new())),
            TyKind::Tag(_) => Operand::Const(Const::Ghosted),
            TyKind::FnPtr(_)
            | TyKind::Map(_, _)
            | TyKind::Vec(_)
            | TyKind::Rng
            | TyKind::RawPtr
            | TyKind::Crib(_) => Operand::Const(Const::NullPtr),
            TyKind::Slice(elem) => {
                let tmp = self.new_local(ty);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::MakeSlice {
                        data: Operand::Const(Const::NullPtr),
                        len: Operand::Const(Const::Int(0, usize_t)),
                        elem,
                    },
                );
                Operand::Copy(Place::local(tmp))
            }
            TyKind::Struct(sid) => {
                let field_tys: Vec<TyId> =
                    self.m.struct_def(sid).fields.iter().map(|f| f.ty).collect();
                let mut ops = Vec::with_capacity(field_tys.len());
                for fty in field_tys {
                    ops.push(self.zero_value(fty)?);
                }
                let tmp = self.new_local(ty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Aggregate(AggKind::Struct(sid), ops),
                );
                Operand::Copy(Place::local(tmp))
            }
            TyKind::Array(elem, n) => {
                let z = self.zero_value(elem)?;
                let ops = vec![z; n as usize];
                let tmp = self.new_local(ty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Aggregate(AggKind::Array(elem), ops),
                );
                Operand::Copy(Place::local(tmp))
            }
            TyKind::Simd { elem, lanes } => {
                let z = self.zero_value(elem)?;
                let ops = vec![z; lanes as usize];
                let tmp = self.new_local(ty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Aggregate(AggKind::Simd(elem), ops),
                );
                Operand::Copy(Place::local(tmp))
            }
            other => {
                return Err(format!(
                    "a field of type {other:?} has no zero value; initialize it explicitly"
                ));
            }
        })
    }

    /// The zero value of a scalar/`str` type, for the leading slots of a `bounce` early return.
    fn zero_operand(&mut self, ty: TyId) -> Result<Operand, String> {
        Ok(match self.m.ty(ty) {
            TyKind::Int { .. } => Operand::Const(Const::Int(0, ty)),
            TyKind::F32 | TyKind::F64 => Operand::Const(Const::Float(0.0, ty)),
            TyKind::Bool => Operand::Const(Const::Bool(false)),
            TyKind::Str => Operand::Const(Const::Str(String::new())),
            other => {
                return Err(format!(
                    "`bounce` cannot synthesize a zero value for {other:?}"
                ));
            }
        })
    }

    fn named_type(&mut self, name: &str) -> Result<TyId, String> {
        // A type-parameter name in the active substitution resolves to its concrete argument.
        if let Some(&t) = self.subst.get(name) {
            return Ok(t);
        }
        // `yikes` is the compiled path's error type — a `{ present, msg }` struct.
        if name == "yikes" {
            let sid = self.yikes_struct();
            return Ok(self.m.intern_ty(TyKind::Struct(sid)));
        }
        let int = |w, s| TyKind::Int {
            width: w,
            signed: s,
        };
        let kind = match name {
            "int" => int(IntWidth::W64, true),
            "i8" => int(IntWidth::W8, true),
            "i16" => int(IntWidth::W16, true),
            "i32" => int(IntWidth::W32, true),
            "i64" => int(IntWidth::W64, true),
            "uint" => int(IntWidth::W64, false),
            "u8" => int(IntWidth::W8, false),
            "u16" => int(IntWidth::W16, false),
            "u32" => int(IntWidth::W32, false),
            "u64" => int(IntWidth::W64, false),
            "bool" => TyKind::Bool,
            "float" | "f64" => TyKind::F64,
            "f32" => TyKind::F32,
            "str" => TyKind::Str,
            "void" | "nada" => TyKind::Void,
            "rawptr" => TyKind::RawPtr,
            // `rng` — an opaque seeded-PRNG handle (`math.cook`); no type arguments.
            "rng" => TyKind::Rng,
            _ => {
                // First-class SIMD vector types: `<elem>x<N>` (e.g. `f32x4`, `i64x2`) and the
                // `vec2`/`vec3`/`vec4` float aliases. Resolved by name like `rng`, no lexer change.
                if let Some((elem_name, lanes)) = simd_type_name(name) {
                    let elem = self.named_type(elem_name)?;
                    return Ok(self.m.intern_ty(TyKind::Simd { elem, lanes }));
                }
                if let Some(&s) = self.structs.get(name) {
                    return Ok(self.m.intern_ty(TyKind::Struct(s)));
                }
                if let Some(&s) = self.sums.get(name) {
                    return Ok(self.m.intern_ty(TyKind::Sum(s)));
                }
                return Err(format!("unknown type `{name}`"));
            }
        };
        Ok(self.m.intern_ty(kind))
    }

    // === monomorphization ====================================================

    /// Build a `type-param → concrete arg` substitution from a generic def's params and args.
    fn build_subst(&self, params: &[String], args: &[TyId]) -> HashMap<String, TyId> {
        params.iter().cloned().zip(args.iter().copied()).collect()
    }

    /// Instantiate a generic `drip` at concrete type args, returning the monomorphized
    /// `StructId` (creating and caching it on first use). Field types are resolved under a
    /// fresh substitution; the caller's substitution is saved and restored.
    fn mono_struct(&mut self, name: &str, args: &[ast::Type]) -> Result<StructId, String> {
        let arg_tys: Vec<TyId> = args
            .iter()
            .map(|t| self.map_type(t))
            .collect::<Result<_, _>>()?;
        let key = (name.to_string(), arg_tys.clone());
        if let Some(&sid) = self.mono_structs.get(&key) {
            return Ok(sid);
        }
        let decl = self
            .generic_structs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown generic struct `{name}`"))?;
        if decl.generics.len() != arg_tys.len() {
            return Err(format!(
                "generic struct `{name}` expects {} type arg(s), got {}",
                decl.generics.len(),
                arg_tys.len()
            ));
        }
        let mangled = self.mangle(name, &arg_tys);
        let new_subst = self.build_subst(&decl.generics, &arg_tys);
        let saved = std::mem::replace(&mut self.subst, new_subst);
        let mut fields = Vec::with_capacity(decl.fields.len());
        let mut err = None;
        for f in &decl.fields {
            match self.map_type(&f.ty) {
                Ok(ty) => fields.push(Field {
                    name: f.name.clone(),
                    ty,
                    vis: match f.vis {
                        Some(ast::Vis::Flex) => Vis::Flex,
                        _ => Vis::Hush,
                    },
                }),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        self.subst = saved;
        if let Some(e) = err {
            return Err(e);
        }
        let sid = self.m.add_struct(StructDef {
            name: mangled,
            fields,
        });
        self.mono_structs.insert(key, sid);
        Ok(sid)
    }

    /// Reserve (and, on first use, sign) a generic function instance at concrete type args,
    /// returning its `(FuncId, params, rets)`. The body is lowered later from the work-list.
    fn mono_fn(
        &mut self,
        name: &str,
        args: &[ast::Type],
    ) -> Result<(FuncId, Vec<TyId>, Vec<TyId>), String> {
        let decl = self
            .generic_funcs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown generic function `{name}`"))?;
        let arg_tys: Vec<TyId> = args
            .iter()
            .map(|t| self.map_type(t))
            .collect::<Result<_, _>>()?;
        if decl.generics.len() != arg_tys.len() {
            return Err(format!(
                "generic function `{name}` expects {} type arg(s), got {}",
                decl.generics.len(),
                arg_tys.len()
            ));
        }
        let key = (name.to_string(), arg_tys.clone());
        if let Some(&id) = self.mono_funcs.get(&key) {
            let (p, r) = self.mono_sigs[&id].clone();
            return Ok((id, p, r));
        }
        // Resolve the instance signature under the substitution.
        let new_subst = self.build_subst(&decl.generics, &arg_tys);
        let saved = std::mem::replace(&mut self.subst, new_subst);
        let mut params = Vec::new();
        let sig_res = (|this: &mut Self| -> Result<Vec<TyId>, String> {
            if let Some(r) = &decl.receiver {
                params.push(this.map_type(&r.ty)?);
            }
            for p in &decl.params {
                params.push(this.map_type(&p.ty)?);
            }
            this.ret_types(&decl.ret)
        })(self);
        self.subst = saved;
        let rets = sig_res?;
        let id = FuncId(self.next_mono_fid);
        self.next_mono_fid += 1;
        self.mono_funcs.insert(key, id);
        self.mono_sigs.insert(id, (params.clone(), rets.clone()));
        self.mono_worklist.push(MonoFnJob {
            name: name.to_string(),
            args: arg_tys,
            id,
        });
        Ok((id, params, rets))
    }

    /// Lower one queued monomorphized function instance. Its `FuncId` must equal the id
    /// reserved when it was signed (it is appended after all concrete + prior-instance funcs).
    fn lower_mono_fn(&mut self, job: &MonoFnJob) -> Result<(), String> {
        let decl = self.generic_funcs[&job.name].clone();
        let (params, rets) = self.mono_sigs[&job.id].clone();
        let mangled = self.mangle(&job.name, &job.args);
        let new_subst = self.build_subst(&decl.generics, &job.args);
        let saved = std::mem::replace(&mut self.subst, new_subst);
        let res = self.lower_fn_core(
            mangled,
            params,
            rets,
            decl.receiver.as_ref(),
            &decl.params,
            &decl.body,
        );
        self.subst = saved;
        res?;
        debug_assert_eq!(FuncId(self.m.funcs().len() as u32 - 1), job.id);
        Ok(())
    }

    /// A unique, readable symbol suffix for an instantiation: `pickFirst$i64`, `Pair$str`.
    fn mangle(&self, base: &str, args: &[TyId]) -> String {
        let parts: Vec<String> = args.iter().map(|&t| self.mangle_ty(t)).collect();
        format!("{base}${}", parts.join("$"))
    }

    fn mangle_ty(&self, t: TyId) -> String {
        match self.m.ty(t) {
            TyKind::Bool => "bool".into(),
            TyKind::Int { width, signed } => {
                format!("{}{}", if *signed { "i" } else { "u" }, width.bits())
            }
            TyKind::F32 => "f32".into(),
            TyKind::F64 => "f64".into(),
            TyKind::Str => "str".into(),
            TyKind::Void => "void".into(),
            TyKind::RawPtr => "rawptr".into(),
            TyKind::Struct(s) => self.m.struct_def(*s).name.clone(),
            TyKind::Sum(s) => self.m.sum_def(*s).name.clone(),
            TyKind::Slice(e) => format!("slice_{}", self.mangle_ty(*e)),
            TyKind::Array(e, n) => format!("arr{n}_{}", self.mangle_ty(*e)),
            TyKind::Tag(e) => format!("tag_{}", self.mangle_ty(*e)),
            TyKind::Crib(e) => format!("crib_{}", self.mangle_ty(*e)),
            TyKind::Ref(e) => format!("ref_{}", self.mangle_ty(*e)),
            _ => format!("t{}", t.0),
        }
    }

    fn int_hint_or_default(&mut self, hint: Option<TyId>) -> TyId {
        match hint {
            Some(t) if matches!(self.m.ty(t), TyKind::Int { .. }) => t,
            _ => self.m.t_int(IntWidth::W64, true),
        }
    }

    fn float_hint_or_default(&mut self, hint: Option<TyId>) -> TyId {
        match hint {
            Some(t) if matches!(self.m.ty(t), TyKind::F32 | TyKind::F64) => t,
            _ => self.m.intern_ty(TyKind::F64),
        }
    }

    // === lowering: items & functions =========================================

    fn lower_items(&mut self, prog: &ast::Program) -> Result<(), String> {
        // Pass 1: register module-level cribs (global arenas) so any function may reference one.
        for item in &prog.items {
            if let Item::Crib(c) = item {
                let (elem, capacity) = self.crib_elem_and_cap(c.ty.as_ref())?;
                let crib_ty = self.m.intern_ty(TyKind::Crib(elem));
                let id = self.m.add_crib_global(CribGlobal {
                    name: c.name.clone(),
                    elem,
                    capacity,
                });
                self.crib_globals.insert(c.name.clone(), (id, crib_ty));
            }
        }
        // Pass 2: lower concrete function bodies (generic ones are instantiated on demand).
        for item in &prog.items {
            match item {
                Item::Pull(_) | Item::Extern(_) | Item::Drip(_) | Item::Moods(_) => {}
                // Module-level facts already collected; nothing to emit (inlined at use).
                Item::Const(_) => {}
                Item::Crib(_) => {} // registered in pass 1
                Item::Func(f) if f.generics.is_empty() => self.lower_func(f)?,
                Item::Func(_) => {} // generic — lowered from the work-list below
                Item::Var(_) => {
                    return Err("module-level `lowkey` is not yet lowered".into());
                }
            }
        }
        // Pass 3: drain the monomorphization work-list. Instances may enqueue more (generic
        // functions calling other generic instances); the index walk picks those up too.
        let mut i = 0;
        while i < self.mono_worklist.len() {
            let job = self.mono_worklist[i].clone();
            i += 1;
            self.lower_mono_fn(&job)?;
        }
        Ok(())
    }

    fn lower_func(&mut self, f: &ast::FnDecl) -> Result<(), String> {
        let sig = self
            .funcs
            .get(&f.name)
            .ok_or_else(|| format!("function `{}` was not collected", f.name))?
            .clone();
        if !sig.ok {
            return Err(format!(
                "function `{}` has a signature that is not yet lowerable",
                f.name
            ));
        }
        self.lower_fn_core(
            f.name.clone(),
            sig.params,
            sig.rets,
            f.receiver.as_ref(),
            &f.params,
            &f.body,
        )
    }

    /// The shared body-lowering machinery for a concrete or monomorphized function: sets up
    /// fresh per-function state, binds the (receiver +) parameters, walks the body, adds the
    /// finished [`Func`] to the module. The active `self.subst` (if any) is left untouched.
    fn lower_fn_core(
        &mut self,
        name: String,
        params: Vec<TyId>,
        rets: Vec<TyId>,
        receiver: Option<&ast::Receiver>,
        fn_params: &[ast::Param],
        body: &ast::Block,
    ) -> Result<(), String> {
        let fb = FuncBuilder::new(name, params.clone(), rets.clone());
        self.fb = Some(fb);
        self.local_tys = params;
        self.scopes = vec![HashMap::new()];
        self.local_consts = HashMap::new();
        self.loops = Vec::new();
        self.cur_rets = rets;

        // The entry block, then param bindings (receiver first, if any).
        let entry = self.new_block();
        debug_assert_eq!(entry, BlockId(0));
        let mut pi = 0usize;
        if let Some(r) = receiver {
            self.bind(&r.name, LocalId(pi as u32));
            pi += 1;
        }
        for p in fn_params {
            self.bind(&p.name, LocalId(pi as u32));
            pi += 1;
        }

        self.lower_block(body)?;

        // Fall-through epilogue: void functions return; value functions that reach here
        // without a `bet` are structurally unreachable.
        if !self.done {
            if self.cur_rets.is_empty() {
                self.fb().ret(vec![]);
            } else {
                self.fb().unreachable();
            }
        }

        let func = self.fb.take().unwrap().finish();
        self.m.add_func(func);
        Ok(())
    }

    // === lowering: statements ================================================

    fn lower_block(&mut self, b: &ast::Block) -> Result<(), String> {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            if self.done {
                break; // subsequent statements are unreachable
            }
            self.lower_stmt(s)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match &s.kind {
            StmtKind::Var(v) => self.lower_var(v),
            StmtKind::Const(c) => {
                let cv = self.eval_const(c)?;
                self.local_consts.insert(c.name.clone(), cv);
                Ok(())
            }
            StmtKind::Crib(c) => self.lower_crib_decl(c),
            StmtKind::Fr(fr) => self.lower_fr(fr),
            StmtKind::Vibin { cond, body } => self.lower_vibin(cond, body),
            StmtKind::Vibe {
                scrutinee,
                arms,
                default,
            } => self.lower_vibe(scrutinee, arms, default.as_ref()),
            StmtKind::Holla {
                binding,
                tag,
                crib,
                live,
                ghosted,
            } => self.lower_holla(binding, tag, crib, live, ghosted),
            StmtKind::Evict { crib, tag } => {
                let (crib_op, crib_ty) = self.lower_expr(crib, None)?;
                match tag {
                    // `evict crib` — whole-crib mass free.
                    None => self.fb().evict(crib_op),
                    // `evict tag in crib` — free one slot (rt-abi `bet_evict_slot`).
                    Some(t) => {
                        if !matches!(self.m.ty(crib_ty), TyKind::Crib(_)) {
                            return Err("`evict .. in ..` needs a crib after `in`".to_string());
                        }
                        let (tag_op, tag_ty) = self.lower_expr(t, None)?;
                        if !matches!(self.m.ty(tag_ty), TyKind::Tag(_)) {
                            return Err("`evict .. in ..` needs a tag before `in`".to_string());
                        }
                        self.fb().evict_slot(crib_op, tag_op);
                    }
                }
                Ok(())
            }
            StmtKind::Bet(vals) => self.lower_bet(vals),
            StmtKind::Yeet(msg) => {
                let (op, _) = self.lower_expr(msg, None)?;
                self.term_panic(op);
                Ok(())
            }
            StmtKind::Dip => {
                let exit = self.loops.last().map(|l| l.1);
                match exit {
                    Some(bb) => {
                        self.term_goto(bb);
                        Ok(())
                    }
                    None => Err("`dip` (break) outside a loop".into()),
                }
            }
            StmtKind::Skip => {
                let header = self.loops.last().map(|l| l.0);
                match header {
                    Some(bb) => {
                        self.term_goto(bb);
                        Ok(())
                    }
                    None => Err("`skip` (continue) outside a loop".into()),
                }
            }
            StmtKind::Assign {
                targets,
                op,
                values,
            } => self.lower_assign(targets, *op, values),
            StmtKind::Expr(e) => self.lower_expr_stmt(e),
            StmtKind::Squad { var, iter, body } => self.lower_squad(var, iter, body),
            StmtKind::Sheesh { .. } => Err("`sheesh` (panic recovery) is not yet lowered".into()),
            StmtKind::Slide(call) => self.lower_slide(call),
            StmtKind::Bounce(e) => self.lower_bounce(e),
        }
    }

    fn lower_var(&mut self, v: &ast::VarDecl) -> Result<(), String> {
        let decl_ty = match &v.ty {
            Some(t) => Some(self.map_type(t)?),
            None => None,
        };

        // Multi-value destructuring from a single call: `lowkey q, r = divmod(..)`.
        if v.targets.len() > 1 && v.values.len() == 1 {
            let (op, ty) = self.lower_expr(&v.values[0], None)?;
            let elems = match self.m.ty(ty).clone() {
                TyKind::Tuple(es) => es,
                _ => {
                    return Err(format!(
                        "`lowkey {}` binds {} names but the initializer is not multi-valued",
                        v.targets.join(", "),
                        v.targets.len()
                    ));
                }
            };
            if elems.len() != v.targets.len() {
                return Err(format!(
                    "`lowkey` binds {} names but the initializer yields {}",
                    v.targets.len(),
                    elems.len()
                ));
            }
            let tuple_place = self.operand_place(op).ok_or_else(|| {
                "multi-value initializer must be addressable (a call result)".to_string()
            })?;
            for (i, name) in v.targets.iter().enumerate() {
                let ety = elems[i];
                let field = extend(&tuple_place, Proj::Field(i as u32));
                let l = self.new_local(ety);
                self.fb()
                    .assign(Place::local(l), Rvalue::Use(Operand::Copy(field)));
                self.bind(name, l);
            }
            return Ok(());
        }

        if v.targets.len() != v.values.len() {
            return Err(format!(
                "`lowkey` binds {} names but has {} initializers",
                v.targets.len(),
                v.values.len()
            ));
        }
        for (name, val) in v.targets.iter().zip(&v.values) {
            let (op, vty) = self.lower_expr(val, decl_ty)?;
            self.deny_whole_soa_read(&op)?;
            let ty = decl_ty.unwrap_or(vty);
            let l = self.new_local(ty);
            if matches!(self.m.ty(ty), TyKind::Soa(_)) && !matches!(self.m.ty(vty), TyKind::Soa(_))
            {
                // Transposing a plain array into a `soa T[N]`: its storage is transposed, so a
                // flat store won't do — scatter element-by-element into the field arrays. (A
                // `soa`-typed RHS, e.g. `mem.slab[Drip]`, is already transposed: a direct copy.)
                self.scatter_into_soa(&Place::local(l), op)?;
            } else {
                self.fb().assign(Place::local(l), Rvalue::Use(op));
            }
            self.bind(name, l);
        }
        Ok(())
    }

    /// Store an array/soa source into a `soa` destination place by scattering each element's
    /// fields into the parallel per-field arrays. Only a fixed-size `soa T[N]` destination is
    /// supported so far (compile-time N to unroll); the element is always a `drip` struct
    /// (enforced by `map_type`). `dest` must be a `soa` place; `src` an addressable operand.
    fn scatter_into_soa(&mut self, dest: &Place, src: Operand) -> Result<(), String> {
        let soa_ty = self.place_ty(dest)?;
        let (elem, n) = match self.m.ty(soa_ty) {
            TyKind::Soa(inner) => match self.m.ty(*inner) {
                TyKind::Array(e, n) => (*e, *n),
                _ => {
                    return Err(
                        "only a fixed-size `soa T[N]` can be built this way so far — \
                         soa slices and vecs land in a later phase."
                            .into(),
                    );
                }
            },
            _ => return Err("scatter target isn't a soa container".into()),
        };
        let src_place = self
            .operand_place(src)
            .ok_or_else(|| "a soa initializer must be an addressable array".to_string())?;
        let sid = match self.m.ty(elem) {
            TyKind::Struct(s) => *s,
            _ => return Err("soa element isn't a drip".into()),
        };
        let nfields = self.m.struct_def(sid).fields.len() as u32;
        let i64t = self.m.t_i64();
        for i in 0..n {
            let idx = Operand::Const(Const::Int(i as i128, i64t));
            let d_i = extend(dest, Proj::Index(idx.clone()));
            let s_i = extend(&src_place, Proj::Index(idx));
            for j in 0..nfields {
                let d = extend(&d_i, Proj::Field(j));
                let s = extend(&s_i, Proj::Field(j));
                self.fb().assign(d, Rvalue::Use(Operand::Copy(s)));
            }
        }
        Ok(())
    }

    /// True if `place` denotes a *whole element* of a `soa` container — its final projection
    /// is an `Index` whose base has a `soa` type. Such an element is spread across parallel
    /// field arrays, so it has no single address/value; only `soa[i].field` is allowed.
    fn is_whole_soa_elem(&self, place: &Place) -> bool {
        if !matches!(place.proj.last(), Some(Proj::Index(_))) {
            return false;
        }
        let mut base = place.clone();
        base.proj.pop();
        matches!(self.place_ty(&base), Ok(t) if matches!(self.m.ty(t), TyKind::Soa(_)))
    }

    /// Reject reading a whole `soa` element as a value (a copy, a call argument, a return).
    /// The slang wording matches the rest of bet's diagnostics.
    fn deny_whole_soa_read(&self, op: &Operand) -> Result<(), String> {
        if let Operand::Copy(p) | Operand::Move(p) = op
            && self.is_whole_soa_elem(p)
        {
            return Err(
                "nah — can't yoink a whole soa element. soa spreads each field into \
                 its own array, so there's no single element to grab. pull one field at a time \
                 (e.g. arr[i].hp)."
                    .into(),
            );
        }
        Ok(())
    }

    fn lower_crib_decl(&mut self, c: &ast::CribDecl) -> Result<(), String> {
        // A crib handle lives in a local of the `crib T` type; the backend fills in the element
        // size/alignment. `crib name: T[N]` is a typed slab of N slots; `crib name` (no type) is
        // an untyped bump crib, represented with a `void` element.
        let (elem, capacity) = self.crib_elem_and_cap(c.ty.as_ref())?;
        let crib_ty = self.m.intern_ty(TyKind::Crib(elem));
        let l = self.new_local(crib_ty);
        self.fb()
            .assign(Place::local(l), Rvalue::CribNew { elem, capacity });
        self.bind(&c.name, l);
        Ok(())
    }

    /// The element type and slot/byte capacity of a `crib` declaration: a `T[N]` typed slab,
    /// a bare `T` typed crib (capacity 0), or an untyped bump crib (`void`, capacity 0).
    fn crib_elem_and_cap(&mut self, ty: Option<&ast::Type>) -> Result<(TyId, u32), String> {
        Ok(match ty {
            Some(t) => match &t.kind {
                ast::TypeKind::Array(elem, n) => (self.map_type(elem)?, *n as u32),
                _ => (self.map_type(t)?, 0),
            },
            None => (self.m.t_void(), 0),
        })
    }

    fn lower_bet(&mut self, vals: &[Expr]) -> Result<(), String> {
        if vals.len() != self.cur_rets.len() {
            return Err(format!(
                "`bet` returns {} value(s) but the function declares {}",
                vals.len(),
                self.cur_rets.len()
            ));
        }
        let rets = self.cur_rets.clone();
        let mut ops = Vec::with_capacity(vals.len());
        for (e, &rt) in vals.iter().zip(&rets) {
            let (op, _) = self.lower_expr(e, Some(rt))?;
            self.deny_whole_soa_read(&op)?;
            ops.push(op);
        }
        self.term_ret(ops);
        Ok(())
    }

    /// `slide worker()` — spawn a concurrent task via `bet_slide`. The worker (a no-arg
    /// function today) is passed as a code pointer; the task argument is unused, so the entry
    /// pointer doubles as it. The returned `TaskHandle` is discarded.
    fn lower_slide(&mut self, call: &Expr) -> Result<(), String> {
        let ExprKind::Call { callee, args } = &call.kind else {
            return Err("`slide` expects a function call".into());
        };
        if !args.is_empty() {
            return Err("`slide` of a function taking arguments is not yet lowered".into());
        }
        let ExprKind::Name { name, .. } = &callee.kind else {
            return Err("`slide` expects a named function".into());
        };
        let sig = self
            .funcs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("`slide` of unknown function `{name}`"))?;
        if !sig.ok {
            return Err(format!("`slide` of a not-yet-lowerable function `{name}`"));
        }
        // The worker's code pointer, cast to a raw pointer for the ABI.
        let fn_sig = self.m.intern_sig(Sig {
            params: sig.params.clone(),
            rets: sig.rets.clone(),
        });
        let fnptr_ty = self.m.intern_ty(TyKind::FnPtr(fn_sig));
        let fnref = self.new_local(fnptr_ty);
        self.fb().assign(
            Place::local(fnref),
            Rvalue::Use(Operand::Const(Const::FnRef(sig.id))),
        );
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let entry = self.new_local(rawptr);
        self.fb().assign(
            Place::local(entry),
            Rvalue::Cast(
                Operand::Copy(Place::local(fnref)),
                rawptr,
                CastKind::Bitcast,
            ),
        );
        // bet_slide(entry, arg) -> TaskHandle (u64). The worker ignores its arg, so reuse the
        // entry pointer as the (discarded) argument.
        let u64t = self.m.t_int(IntWidth::W64, false);
        let ext = self.get_extern("bet_slide", vec![rawptr, rawptr], vec![u64t]);
        let handle = self.new_local(u64t);
        let entry_op = Operand::Copy(Place::local(entry));
        self.fb().assign(
            Place::local(handle),
            Rvalue::Call(Callee::Extern(ext), vec![entry_op.clone(), entry_op]),
        );
        Ok(())
    }

    /// `bounce y` — early-return-on-error sugar. When the `yikes` `y` is present, return it in
    /// the trailing slot with zero values in the leading slots; otherwise fall through.
    fn lower_bounce(&mut self, e: &Expr) -> Result<(), String> {
        let (op, ty) = self.lower_expr(e, None)?;
        if !self.is_yikes(ty) {
            return Err("`bounce` expects a `yikes` value".into());
        }
        let rets = self.cur_rets.clone();
        if rets.last().map(|&t| self.is_yikes(t)) != Some(true) {
            return Err(
                "`bounce` requires the enclosing function to return a trailing `yikes`".into(),
            );
        }
        let place = self
            .operand_place(op.clone())
            .ok_or_else(|| "`bounce` needs an addressable operand".to_string())?;
        let present = Operand::Copy(extend(&place, Proj::Field(0)));

        // The early-return operands: a zero for each leading slot, then the error itself.
        let mut ret_ops = Vec::with_capacity(rets.len());
        for &rt in &rets[..rets.len() - 1] {
            ret_ops.push(self.zero_operand(rt)?);
        }
        ret_ops.push(op);

        let cond_end = self.cur;
        let merge = self.reserve_block();
        let then_bb = self.new_block();
        self.term_ret(ret_ops);
        self.set_branch(cond_end, present, then_bb, merge);
        self.select(merge);
        Ok(())
    }

    fn lower_assign(
        &mut self,
        targets: &[Expr],
        op: ast::AssignOp,
        values: &[Expr],
    ) -> Result<(), String> {
        if targets.len() != 1 || values.len() != 1 {
            return Err("only single-target assignment is lowered yet".into());
        }
        // `soa_vec[i].field = x`: write a field through its per-field runtime handle (a vec
        // element isn't an addressable place). The per-field write `soa vec` enables.
        if op == ast::AssignOp::Eq
            && let ExprKind::Field { base, name, .. } = &targets[0].kind
            && let ExprKind::Index { base: vbase, index } = &base.kind
            && let ExprKind::Name { name: vname, .. } = &vbase.kind
            && let Some(local) = self.lookup_local(vname)
            && let TyKind::Soa(inner) = self.m.ty(self.local_ty(local))
            && let TyKind::Vec(e) = *self.m.ty(*inner)
        {
            return self.lower_soa_vec_write(local, e, index, name, &values[0]);
        }
        let place = self.lower_place(&targets[0])?;
        let pty = self.place_ty(&place)?;
        // A whole `soa` element can't be written as one value — set fields individually.
        if self.is_whole_soa_elem(&place) {
            return Err(
                "can't slam a whole struct into a soa slot — set the fields one by one \
                 (e.g. arr[i].hp = ...)."
                    .into(),
            );
        }
        if op == ast::AssignOp::Eq {
            let (val, vty) = self.lower_expr(&values[0], Some(pty))?;
            self.deny_whole_soa_read(&val)?;
            // Transposing a plain array into a `soa T[N]` scatters element-by-element (its
            // storage is transposed). A `soa`-typed RHS is already transposed: a direct copy.
            if matches!(self.m.ty(pty), TyKind::Soa(_)) && !matches!(self.m.ty(vty), TyKind::Soa(_))
            {
                return self.scatter_into_soa(&place, val);
            }
            // Coerce an integer RHS to the target's integer type (width/sign), e.g. an `int`
            // value stored into an `i16` slot (`beepbuf[i] = amp`). A no-op when the types match.
            let val = if matches!(self.m.ty(vty), TyKind::Int { .. })
                && matches!(self.m.ty(pty), TyKind::Int { .. })
            {
                self.coerce_int(val, vty, pty)
            } else {
                val
            };
            self.fb().assign(place, Rvalue::Use(val));
            return Ok(());
        }
        // Compound assignment: `place op= rhs` ≡ `place = place <op> rhs`.
        let irop = compound_binop(op);
        let (rhs, _) = self.lower_expr(&values[0], Some(pty))?;
        let mode = self.arith_mode(pty, irop);
        let cur = Operand::Copy(place.clone());
        self.fb().assign(place, Rvalue::BinOp(irop, cur, rhs, mode));
        Ok(())
    }

    fn lower_expr_stmt(&mut self, e: &Expr) -> Result<(), String> {
        // Recognize the `spill.*` print intrinsics (statement-level, void).
        if let ExprKind::Method {
            receiver,
            method,
            generics,
            args,
        } = &e.kind
            && let ExprKind::Name { name, .. } = &receiver.kind
            && name == "spill"
            && self.lookup_local(name).is_none()
        {
            if !generics.is_empty() {
                return Err("`spill` takes no generic arguments".into());
            }
            return self.lower_spill(method, args);
        }
        // Otherwise, evaluate for side effects and discard the result.
        let _ = self.lower_expr(e, None)?;
        Ok(())
    }

    /// Lower a `spill.it` / `spill.f` print to real runtime output.
    ///
    /// `spill.it(x)` prints `x`'s interpreter `display` form followed by a newline;
    /// `spill.f(fmt, args..)` splits the literal `fmt` on `{}` placeholders (honoring `{{`/`}}`)
    /// and interleaves the literal segments with each argument's display, adding no trailing
    /// newline. Both route non-string values through [`Self::lower_print_value`], which emits the
    /// type-directed `bet_print_i64` / `bet_print_u64` / `bet_print_f64` calls; string literals
    /// still go through `bet_print`. Computed (non-literal) strings remain a backend gap.
    fn lower_spill(&mut self, method: &str, args: &[ast::Arg]) -> Result<(), String> {
        match method {
            "it" => {
                if args.len() != 1 {
                    return Err("`spill.it` takes exactly one argument".into());
                }
                // A string *literal* prints newline-terminated in a single `bet_print` — the
                // original tracer-bullet shape, kept byte-for-byte.
                if let ExprKind::Str(s) = &args[0].value.kind {
                    return self.emit_print(format!("{s}\n"));
                }
                self.lower_print_expr(&args[0].value)?;
                // `spill.it` always appends a newline.
                self.emit_print("\n".to_string())
            }
            "f" => {
                let Some((fmt_arg, rest)) = args.split_first() else {
                    return Err("`spill.f` takes a format string".into());
                };
                let ExprKind::Str(fmt) = &fmt_arg.value.kind else {
                    return Err("`spill.f` format must be a string literal".into());
                };
                let segments = split_format(fmt)?;
                let holes = segments
                    .iter()
                    .filter(|s| matches!(s, FmtSeg::Hole))
                    .count();
                if holes != rest.len() {
                    return Err(format!(
                        "`spill.f` format has {holes} `{{}}` placeholder(s) but {} argument(s) \
                         were supplied",
                        rest.len()
                    ));
                }
                // No auto trailing newline: any newline is part of the format string itself.
                let mut next = 0usize;
                for seg in &segments {
                    match seg {
                        FmtSeg::Text(t) => {
                            if !t.is_empty() {
                                self.emit_print(t.clone())?;
                            }
                        }
                        FmtSeg::Hole => {
                            self.lower_print_expr(&rest[next].value)?;
                            next += 1;
                        }
                    }
                }
                Ok(())
            }
            other => Err(format!("`spill.{other}` is not a known print method")),
        }
    }

    /// Lower a single printed argument: evaluate it, then print its value. `ghosted` has no
    /// interned type to lower through `lower_expr`, so its `display` form is printed directly.
    fn lower_print_expr(&mut self, e: &Expr) -> Result<(), String> {
        if matches!(e.kind, ExprKind::Ghosted) {
            return self.emit_print("ghosted".to_string());
        }
        let (op, ty) = self.lower_expr(e, None)?;
        self.lower_print_value(op, ty)
    }

    /// Type-directed value print, matching the interpreter's `display`: emit the runtime print
    /// primitive appropriate to `ty`.
    ///
    /// * signed int (any width) → sign-extend to `i64`, `bet_print_i64`
    /// * unsigned int (incl. `u8` bytes) → zero-extend to `u64`, `bet_print_u64`
    /// * `f32` → `fpext` to `f64`; `f64` → `bet_print_f64`
    /// * `bool` → branch, printing `nocap` / `cap`
    /// * `ghosted` operand → `bet_print("ghosted")`
    /// * string *literal* → `bet_print` of its bytes (computed strings are a backend gap)
    /// * anything else (struct/sum/array/fn/void) → a clean "not yet lowered" error
    fn lower_print_value(&mut self, op: Operand, ty: TyId) -> Result<(), String> {
        if matches!(op, Operand::Const(Const::Ghosted)) {
            return self.emit_print("ghosted".to_string());
        }
        match self.m.ty(ty).clone() {
            TyKind::Int { signed: true, .. } => {
                let i64t = self.m.t_int(IntWidth::W64, true);
                let v = self.coerce_int(op, ty, i64t);
                self.emit_print_num("bet_print_i64", i64t, v);
                Ok(())
            }
            TyKind::Int { signed: false, .. } => {
                let u64t = self.m.t_int(IntWidth::W64, false);
                let v = self.coerce_int(op, ty, u64t);
                self.emit_print_num("bet_print_u64", u64t, v);
                Ok(())
            }
            TyKind::F64 => {
                self.emit_print_num("bet_print_f64", ty, op);
                Ok(())
            }
            TyKind::F32 => {
                // `spill.it(<f32>)` prints at f64 precision (the runtime has one float primitive).
                let f64t = self.m.intern_ty(TyKind::F64);
                let tmp = self.new_local(f64t);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Cast(op, f64t, CastKind::FloatResize),
                );
                self.emit_print_num("bet_print_f64", f64t, Operand::Copy(Place::local(tmp)));
                Ok(())
            }
            TyKind::Bool => self.lower_print_bool(op),
            // `str` is a fat `{ ptr, len }` value; `spill` projects it (a literal takes the
            // interned-global fast-path in the backend, a computed value an extractvalue).
            TyKind::Str => self.emit_print_operand(op),
            // A `yikes` prints as its message (corpus `07-errors`: `spill.it(y)`).
            TyKind::Struct(s) if Some(s) == self.yikes_sid => {
                let place = self
                    .operand_place(op)
                    .ok_or_else(|| "`spill` of a yikes needs an addressable operand".to_string())?;
                let msg = Operand::Copy(extend(&place, Proj::Field(1)));
                self.emit_print_operand(msg)
            }
            other => Err(format!(
                "`spill` of a value of type {other:?} is not yet lowered"
            )),
        }
    }

    /// Print a `bool` by branching: the true edge prints `nocap`, the false edge `cap`, and both
    /// rejoin at a merge block (mirrors `display`'s `nocap`/`cap`).
    fn lower_print_bool(&mut self, cond: Operand) -> Result<(), String> {
        let cond_end = self.cur;
        let merge = self.reserve_block();

        let then_bb = self.new_block();
        self.emit_print("nocap".to_string())?;
        self.term_goto(merge);

        let else_bb = self.new_block();
        self.emit_print("cap".to_string())?;
        self.term_goto(merge);

        self.set_branch(cond_end, cond, then_bb, else_bb);
        self.select(merge);
        Ok(())
    }

    /// Emit `call_extern @name(v)` for one of the numeric print primitives (`v` already the
    /// declared parameter type). The extern is synthesized/deduped on demand.
    fn emit_print_num(&mut self, name: &str, arg_ty: TyId, v: Operand) {
        let voidt = self.m.t_void();
        let ext = self.get_extern(name, vec![arg_ty], vec![]);
        let result = self.new_local(voidt);
        self.fb().assign(
            Place::local(result),
            Rvalue::Call(Callee::Extern(ext), vec![v]),
        );
    }

    /// Widen an integer operand to the print primitive's word type. A no-op when `from` already
    /// is `to`; otherwise a sign/zero-extending cast (never a truncation — `to` is the widest
    /// int, so every source is narrower-or-equal).
    fn coerce_int(&mut self, op: Operand, from: TyId, to: TyId) -> Operand {
        if from == to {
            return op;
        }
        let kind = self.cast_kind(from, to).unwrap_or(CastKind::Bitcast);
        let tmp = self.new_local(to);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Cast(op, to, kind));
        Operand::Copy(Place::local(tmp))
    }

    fn emit_print(&mut self, text: String) -> Result<(), String> {
        self.emit_print_operand(Operand::Const(Const::Str(text)))
    }

    fn emit_print_operand(&mut self, s: Operand) -> Result<(), String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let u64t = self.m.t_int(IntWidth::W64, false);
        let voidt = self.m.t_void();
        let ptr = self.new_local(rawptr);
        let len = self.new_local(u64t);
        let result = self.new_local(voidt);
        self.fb()
            .assign(Place::local(ptr), Rvalue::StrPtr(s.clone()));
        self.fb().assign(Place::local(len), Rvalue::StrLen(s));
        let args = vec![
            Operand::Copy(Place::local(ptr)),
            Operand::Copy(Place::local(len)),
        ];
        let print = self.print_extern;
        self.fb().assign(
            Place::local(result),
            Rvalue::Call(Callee::Extern(print), args),
        );
        Ok(())
    }

    // === lowering: control flow ==============================================

    fn lower_fr(&mut self, fr: &ast::FrStmt) -> Result<(), String> {
        // Flatten to a list of (cond, body) arms plus an optional trailing else.
        let mut arms: Vec<(&Expr, &ast::Block)> = vec![(&fr.cond, &fr.then)];
        for (c, b) in &fr.elifs {
            arms.push((c, b));
        }
        let merge = self.reserve_block();
        self.emit_if_chain(&arms, fr.els.as_ref(), merge)?;
        self.select(merge);
        Ok(())
    }

    fn emit_if_chain(
        &mut self,
        arms: &[(&Expr, &ast::Block)],
        els: Option<&ast::Block>,
        merge: BlockId,
    ) -> Result<(), String> {
        let (cond, body) = arms[0];
        let bool_ty = self.m.t_bool();
        let (cop, _) = self.lower_expr(cond, Some(bool_ty))?;
        let cond_end = self.cur;

        let then_bb = self.new_block();
        self.lower_block(body)?;
        self.term_goto(merge);

        let else_bb = self.new_block();
        if arms.len() > 1 {
            self.emit_if_chain(&arms[1..], els, merge)?;
        } else if let Some(e) = els {
            self.lower_block(e)?;
            self.term_goto(merge);
        } else {
            self.term_goto(merge);
        }

        self.set_branch(cond_end, cop, then_bb, else_bb);
        Ok(())
    }

    fn lower_vibin(&mut self, cond: &Expr, body: &ast::Block) -> Result<(), String> {
        let pre = self.cur;
        let header = self.reserve_block();
        self.set_goto(pre, header);

        self.select(header);
        let bool_ty = self.m.t_bool();
        let (cop, _) = self.lower_expr(cond, Some(bool_ty))?;
        let header_end = self.cur;

        let body_bb = self.new_block();
        let exit = self.reserve_block();
        self.set_branch(header_end, cop, body_bb, exit);

        self.select(body_bb);
        self.loops.push((header, exit));
        self.lower_block(body)?;
        self.loops.pop();
        self.term_goto(header);

        self.select(exit);
        Ok(())
    }

    fn lower_vibe(
        &mut self,
        scrutinee: &Expr,
        arms: &[ast::MatchArm],
        default: Option<&ast::Block>,
    ) -> Result<(), String> {
        let (sop, sty) = self.lower_expr(scrutinee, None)?;
        let base_place = self
            .operand_place(sop)
            .ok_or_else(|| "`vibe` scrutinee must be addressable".to_string())?;
        // Auto-deref a `ref Sum` scrutinee (as produced by `holla`/`trust` over a sum crib).
        let (sid, sum_place) = match self.m.ty(sty) {
            TyKind::Sum(s) => (*s, base_place),
            TyKind::Ref(e) => match self.m.ty(*e) {
                TyKind::Sum(s) => (*s, extend(&base_place, Proj::Deref)),
                other => return Err(format!("`vibe` on a ref to non-sum ({other:?})")),
            },
            other => return Err(format!("`vibe` scrutinee is not a sum type ({other:?})")),
        };

        // discriminant(scrutinee) → a u32 tag we can switch on.
        let disc_ty = self.m.t_int(IntWidth::W32, false);
        let disc = self.new_local(disc_ty);
        self.fb().assign(
            Place::local(disc),
            Rvalue::Discriminant(Operand::Copy(sum_place.clone())),
        );
        let switch_block = self.cur;

        let merge = self.reserve_block();
        let mut cases: Vec<(u64, BlockId)> = Vec::with_capacity(arms.len());

        for arm in arms {
            let variant = self.variant_index(sid, &arm.variant)?;
            let payload = self.m.sum_def(sid).variants[variant as usize]
                .payload
                .clone();
            if arm.bindings.len() != payload.len() {
                return Err(format!(
                    "`vibe` arm `{}` binds {} field(s) but the variant has {}",
                    arm.variant,
                    arm.bindings.len(),
                    payload.len()
                ));
            }
            let arm_bb = self.new_block();
            self.scopes.push(HashMap::new());
            // Bind each payload field: scrutinee.downcast(variant).field(j).
            for (j, (bind, &fty)) in arm.bindings.iter().zip(&payload).enumerate() {
                let mut p = extend(&sum_place, Proj::Downcast(variant));
                p = extend(&p, Proj::Field(j as u32));
                let l = self.new_local(fty);
                self.fb()
                    .assign(Place::local(l), Rvalue::Use(Operand::Copy(p)));
                self.bind(bind, l);
            }
            self.lower_block(&arm.body)?;
            self.scopes.pop();
            self.term_goto(merge);
            cases.push((variant as u64, arm_bb));
        }

        // The default (`naw`) arm, or an unreachable landing pad for an exhaustive match.
        let default_bb = self.new_block();
        match default {
            Some(b) => {
                self.lower_block(b)?;
                self.term_goto(merge);
            }
            None => {
                self.fb().unreachable();
                self.done = true;
            }
        }

        self.set_switch(
            switch_block,
            Operand::Copy(Place::local(disc)),
            cases,
            default_bb,
        );
        self.select(merge);
        Ok(())
    }

    fn lower_holla(
        &mut self,
        binding: &str,
        tag: &Expr,
        crib: &Expr,
        live: &ast::Block,
        ghosted: &ast::Block,
    ) -> Result<(), String> {
        let (tag_op, _) = self.lower_expr(tag, None)?;
        let (crib_op, crib_ty) = self.lower_expr(crib, None)?;
        let elem = match self.m.ty(crib_ty) {
            TyKind::Crib(e) => *e,
            other => return Err(format!("`holla` crib operand is not a crib ({other:?})")),
        };
        let ref_ty = self.m.intern_ty(TyKind::Ref(elem));
        let resolved = self.new_local(ref_ty);
        let check_block = self.cur;

        let merge = self.reserve_block();

        let live_bb = self.new_block();
        self.scopes.push(HashMap::new());
        self.bind(binding, resolved);
        self.lower_block(live)?;
        self.scopes.pop();
        self.term_goto(merge);

        let ghosted_bb = self.new_block();
        self.lower_block(ghosted)?;
        self.term_goto(merge);

        self.set_holla(
            check_block,
            tag_op,
            crib_op,
            Place::local(resolved),
            live_bb,
            ghosted_bb,
        );
        self.select(merge);
        Ok(())
    }

    // === lowering: expressions ===============================================

    /// Lower an expression, emitting any needed statements into the current block and returning
    /// the resulting operand together with its interned type.
    fn lower_expr(&mut self, e: &Expr, hint: Option<TyId>) -> Result<(Operand, TyId), String> {
        match &e.kind {
            ExprKind::Int(v) => {
                let ty = self.int_hint_or_default(hint);
                Ok((Operand::Const(Const::Int(*v, ty)), ty))
            }
            ExprKind::Float(v) => {
                let ty = self.float_hint_or_default(hint);
                Ok((Operand::Const(Const::Float(*v, ty)), ty))
            }
            ExprKind::Bool(b) => Ok((Operand::Const(Const::Bool(*b)), self.m.t_bool())),
            ExprKind::Str(s) => Ok((Operand::Const(Const::Str(s.clone())), self.m.t_str())),
            ExprKind::Byte(b) => {
                let ty = self.m.t_int(IntWidth::W8, false);
                Ok((Operand::Const(Const::Int(*b as i128, ty)), ty))
            }
            ExprKind::Ghosted => match hint {
                // A `ghosted` in a `yikes` context is the no-error value: `{ present: false }`.
                Some(t) if self.is_yikes(t) => {
                    Ok(self.build_yikes(false, Operand::Const(Const::Str(String::new()))))
                }
                Some(t) => Ok((Operand::Const(Const::Ghosted), t)),
                None => Err("`ghosted` needs a known type context to lower".into()),
            },
            ExprKind::Name { name, generics } => {
                if !generics.is_empty() {
                    return Err(format!(
                        "generic instantiation of `{name}` is not yet lowered"
                    ));
                }
                self.lower_name(name, hint)
            }
            ExprKind::Unary(op, inner) => self.lower_unary(*op, inner, hint),
            ExprKind::Binary(op, l, r) => self.lower_binary(*op, l, r, hint),
            ExprKind::Cast(inner, ty) => self.lower_cast(inner, ty),
            ExprKind::Field {
                base,
                name,
                generics,
            } => {
                if !generics.is_empty() {
                    return Err("generic field access is not yet lowered".into());
                }
                self.lower_field(base, name)
            }
            ExprKind::Method {
                receiver,
                method,
                generics,
                args,
            } => self.lower_method(receiver, method, generics, args, hint),
            ExprKind::Call { callee, args } => self.lower_call(callee, args, hint),
            ExprKind::Struct(lit) => self.lower_struct_lit(lit),
            ExprKind::Cop { init, crib } => self.lower_cop(init, crib),
            ExprKind::Trust { tag, crib } => self.lower_trust(tag, crib),
            ExprKind::Index { base, index } => self.lower_index(base, index),
            ExprKind::Array(elems) => self.lower_array_lit(elems, hint),
        }
    }

    fn lower_name(&mut self, name: &str, hint: Option<TyId>) -> Result<(Operand, TyId), String> {
        if let Some(l) = self.lookup_local(name) {
            let ty = self.local_ty(l);
            return Ok((Operand::Copy(Place::local(l)), ty));
        }
        if let Some(cv) = self.lookup_const(name) {
            return Ok((Operand::Const(cv.value), cv.ty));
        }
        // A module-level crib: load its handle from the backing global.
        if let Some(&(id, crib_ty)) = self.crib_globals.get(name) {
            let tmp = self.new_local(crib_ty);
            self.fb().assign(Place::local(tmp), Rvalue::CribGlobal(id));
            return Ok((Operand::Copy(Place::local(tmp)), crib_ty));
        }
        // A bare name that is a nullary `moods` variant is a value constructor.
        if self.variants.contains_key(name) {
            let (sid, variant) = self.resolve_variant(name, hint)?;
            let payload = self.m.sum_def(sid).variants[variant as usize]
                .payload
                .clone();
            if !payload.is_empty() {
                return Err(format!(
                    "variant `{name}` takes {} field(s); use `{name}(..)`",
                    payload.len()
                ));
            }
            let sty = self.m.intern_ty(TyKind::Sum(sid));
            let tmp = self.new_local(sty);
            self.fb().assign(
                Place::local(tmp),
                Rvalue::Aggregate(AggKind::Sum { sum: sid, variant }, vec![]),
            );
            return Ok((Operand::Copy(Place::local(tmp)), sty));
        }
        if let Some(sig) = self.funcs.get(name).cloned() {
            if !sig.ok {
                return Err(format!(
                    "`{name}` is generic and cannot be used as a value yet"
                ));
            }
            let s = self.m.intern_sig(Sig {
                params: sig.params.clone(),
                rets: sig.rets.clone(),
            });
            let ty = self.m.intern_ty(TyKind::FnPtr(s));
            return Ok((Operand::Const(Const::FnRef(sig.id)), ty));
        }
        Err(format!("unresolved name `{name}`"))
    }

    fn lower_unary(
        &mut self,
        op: ast::UnOp,
        inner: &Expr,
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        let (io, ity) = self.lower_expr(inner, hint)?;
        let (irop, res_ty) = match op {
            ast::UnOp::Neg => (UnOp::Neg, ity),
            ast::UnOp::BitNot => (UnOp::BitNot, ity),
            ast::UnOp::Not => (UnOp::Not, self.m.t_bool()),
        };
        let tmp = self.new_local(res_ty);
        self.fb().assign(Place::local(tmp), Rvalue::UnOp(irop, io));
        Ok((Operand::Copy(Place::local(tmp)), res_ty))
    }

    fn lower_binary(
        &mut self,
        op: ast::BinOp,
        l: &Expr,
        r: &Expr,
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        // Short-circuit boolean operators lower to control flow, not a `BinOp`.
        if matches!(op, ast::BinOp::And | ast::BinOp::Or) {
            return self.lower_short_circuit(op, l, r);
        }

        // A `== ghosted` / `!= ghosted` comparison. For a `yikes` operand this reads its
        // `present` flag; for anything else it compares against a type-appropriate `ghosted`.
        if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne)
            && (matches!(l.kind, ExprKind::Ghosted) || matches!(r.kind, ExprKind::Ghosted))
        {
            let (ghost, other) = if matches!(l.kind, ExprKind::Ghosted) {
                (l, r)
            } else {
                (r, l)
            };
            let (oop, oty) = self.lower_expr(other, None)?;
            let boolt = self.m.t_bool();
            if self.is_yikes(oty) {
                let place = self
                    .operand_place(oop)
                    .ok_or_else(|| "`yikes` comparison needs an addressable operand".to_string())?;
                let present = Operand::Copy(extend(&place, Proj::Field(0)));
                // `!= ghosted` is "an error is present"; `== ghosted` is its negation.
                if matches!(op, ast::BinOp::Ne) {
                    return Ok((present, boolt));
                }
                let tmp = self.new_local(boolt);
                self.fb()
                    .assign(Place::local(tmp), Rvalue::UnOp(UnOp::Not, present));
                return Ok((Operand::Copy(Place::local(tmp)), boolt));
            }
            // Non-yikes: compare against a `ghosted` typed like the other operand.
            let (gop, _) = self.lower_expr(ghost, Some(oty))?;
            let tmp = self.new_local(boolt);
            self.fb().assign(
                Place::local(tmp),
                Rvalue::BinOp(map_binop(op), oop, gop, ArithMode::Na),
            );
            return Ok((Operand::Copy(Place::local(tmp)), boolt));
        }

        let irop = map_binop(op);
        // Comparisons produce bool but take operands of the compared type; propagate no hint
        // into a comparison's operands, but do propagate for value-producing ops.
        let (lo, lty) = if irop.is_comparison() {
            self.lower_expr(l, None)?
        } else {
            self.lower_expr(l, hint)?
        };
        // Element-wise SIMD arithmetic/shift. A scalar right operand (`v >> 16`, `v * s`) is
        // broadcast to a vector so the IR `BinOp` sees two same-type vectors; a vector right
        // operand (`a + b`) passes through. Comparisons on vectors aren't part of the surface
        // (min/max are methods), so they fall through to the scalar path.
        if !irop.is_comparison()
            && let TyKind::Simd { elem, .. } = *self.m.ty(lty)
        {
            let (ro, rty) = self.lower_expr(r, Some(elem))?;
            let ro = if rty == lty {
                ro
            } else {
                self.emit_simd(SimdOp::Splat, vec![ro], lty).0
            };
            let tmp = self.new_local(lty);
            self.fb().assign(
                Place::local(tmp),
                Rvalue::BinOp(irop, lo, ro, ArithMode::Na),
            );
            return Ok((Operand::Copy(Place::local(tmp)), lty));
        }
        let (ro, _) = self.lower_expr(r, Some(lty))?;
        let mode = self.arith_mode(lty, irop);
        let res_ty = if irop.is_comparison() {
            self.m.t_bool()
        } else {
            lty
        };
        let tmp = self.new_local(res_ty);
        self.fb()
            .assign(Place::local(tmp), Rvalue::BinOp(irop, lo, ro, mode));
        Ok((Operand::Copy(Place::local(tmp)), res_ty))
    }

    fn lower_short_circuit(
        &mut self,
        op: ast::BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<(Operand, TyId), String> {
        let bool_ty = self.m.t_bool();
        let res = self.new_local(bool_ty);

        let (lo, _) = self.lower_expr(l, Some(bool_ty))?;
        let lhs_end = self.cur;

        // The block that evaluates the right operand and records its value.
        let rhs_bb = self.new_block();
        let (ro, _) = self.lower_expr(r, Some(bool_ty))?;
        let rhs_end = self.cur;
        self.fb().assign(Place::local(res), Rvalue::Use(ro));

        // The short-circuit block, recording the constant answer.
        let short_bb = self.new_block();
        let short_val = matches!(op, ast::BinOp::Or); // `||` short-circuits to true, `&&` to false
        self.fb().assign(
            Place::local(res),
            Rvalue::Use(Operand::Const(Const::Bool(short_val))),
        );

        let merge = self.new_block();
        self.set_goto(rhs_end, merge);
        self.set_goto(short_bb, merge);
        // `&&`: if lhs then eval rhs else short(false). `||`: if lhs then short(true) else rhs.
        if matches!(op, ast::BinOp::And) {
            self.set_branch(lhs_end, lo, rhs_bb, short_bb);
        } else {
            self.set_branch(lhs_end, lo, short_bb, rhs_bb);
        }

        self.select(merge);
        Ok((Operand::Copy(Place::local(res)), bool_ty))
    }

    fn lower_cast(&mut self, inner: &Expr, ty: &ast::Type) -> Result<(Operand, TyId), String> {
        let target = self.map_type(ty)?;
        let (io, ity) = self.lower_expr(inner, None)?;
        let kind = self.cast_kind(ity, target)?;
        let tmp = self.new_local(target);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Cast(io, target, kind));
        Ok((Operand::Copy(Place::local(tmp)), target))
    }

    fn cast_kind(&self, src: TyId, dst: TyId) -> Result<CastKind, String> {
        use TyKind::*;
        let s = self.m.ty(src);
        let d = self.m.ty(dst);
        Ok(match (s, d) {
            (
                Int {
                    width: sw,
                    signed: ss,
                },
                Int { width: dw, .. },
            ) => {
                if dw.bits() > sw.bits() {
                    if *ss {
                        CastKind::IntSext
                    } else {
                        CastKind::IntZext
                    }
                } else if dw.bits() < sw.bits() {
                    CastKind::IntTrunc
                } else {
                    CastKind::Bitcast
                }
            }
            (Int { .. }, F32 | F64) => CastKind::IntToFloat,
            (F32 | F64, Int { .. }) => CastKind::FloatToInt,
            (F32 | F64, F32 | F64) => CastKind::FloatResize,
            _ => CastKind::Bitcast,
        })
    }

    fn lower_field(&mut self, base: &Expr, name: &str) -> Result<(Operand, TyId), String> {
        // `soa_vec[i].field` reads a field through the per-field runtime handle (a vec element
        // isn't an addressable place). Peek the container type off a name root without lowering.
        if let ExprKind::Index { base: vbase, index } = &base.kind
            && let ExprKind::Name { name: vname, .. } = &vbase.kind
            && let Some(local) = self.lookup_local(vname)
            && let TyKind::Soa(inner) = self.m.ty(self.local_ty(local))
            && let TyKind::Vec(e) = *self.m.ty(*inner)
        {
            return self.lower_soa_vec_read(local, e, index, name);
        }
        let (bop, bty) = self.lower_expr(base, None)?;
        // A `<N x elem>` SIMD lane read: `v.x`/`.y`/`.z`/`.w` extract lane 0/1/2/3 as a scalar.
        // Lanes are read-only in v1 (no `v.x = ...`), so this yields a value, not a place.
        if let TyKind::Simd { elem, lanes } = *self.m.ty(bty) {
            let lane = match name {
                "x" => 0u32,
                "y" => 1,
                "z" => 2,
                "w" => 3,
                other => return Err(format!("simd vector has no lane `{other}` (use x/y/z/w)")),
            };
            if lane >= lanes {
                return Err(format!(
                    "lane `.{name}` is out of range for a {lanes}-lane vector"
                ));
            }
            let tmp = self.new_local(elem);
            self.fb().assign(
                Place::local(tmp),
                Rvalue::Simd {
                    op: SimdOp::Lane(lane),
                    args: vec![bop],
                    ty: elem,
                },
            );
            return Ok((Operand::Copy(Place::local(tmp)), elem));
        }
        let base_place = self
            .operand_place(bop)
            .ok_or_else(|| "field access requires an addressable base".to_string())?;
        // Auto-deref a `ref Struct` (as produced by `trust`/`holla`).
        let (sid, place) = match self.m.ty(bty) {
            TyKind::Struct(s) => (*s, base_place),
            TyKind::Ref(e) => match self.m.ty(*e) {
                TyKind::Struct(s) => (*s, extend(&base_place, Proj::Deref)),
                other => return Err(format!("field access through ref to non-struct {other:?}")),
            },
            other => return Err(format!("field access on non-struct value ({other:?})")),
        };
        let def = self.m.struct_def(sid);
        let idx = def
            .fields
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| format!("struct `{}` has no field `{name}`", def.name))?;
        let fty = def.fields[idx].ty;
        let fplace = extend(&place, Proj::Field(idx as u32));
        Ok((Operand::Copy(fplace), fty))
    }

    /// `base[index]` — index into an array or slice, yielding the element place.
    fn lower_index(&mut self, base: &Expr, index: &Expr) -> Result<(Operand, TyId), String> {
        let (bop, bty) = self.lower_expr(base, None)?;
        // A `vec[T]` is a runtime handle, not addressable storage: read via `bet_vec_get`.
        if let TyKind::Vec(e) = *self.m.ty(bty) {
            return self.lower_vec_index(bop, e, index);
        }
        let base_place = self
            .operand_place(bop)
            .ok_or_else(|| "indexing requires an addressable base".to_string())?;
        let elem = match self.m.ty(bty) {
            TyKind::Array(e, _) | TyKind::Slice(e) => *e,
            // Indexing a `soa` container yields its (struct) element place. This is only
            // valid when a field projection follows — a bare `soa[i]` whole-element read is
            // rejected at the value-egress sites (see `ban_soa_elem`); the place produced
            // here is consumed as a *place* by `lower_field`, never loaded as a whole struct.
            TyKind::Soa(inner) => match self.m.ty(*inner) {
                TyKind::Array(e, _) | TyKind::Slice(e) | TyKind::Vec(e) => *e,
                other => return Err(format!("soa index of {other:?}")),
            },
            other => return Err(format!("indexing a non-array/slice value ({other:?})")),
        };
        let i64t = self.m.t_i64();
        let (iop, _ity) = self.lower_expr(index, Some(i64t))?;
        let iplace = extend(&base_place, Proj::Index(iop));
        Ok((Operand::Copy(iplace), elem))
    }

    /// `[a, b, c]` — a fixed array literal. The element type comes from an `[]T`/`[T; N]` hint
    /// or is inferred from the first element; the value is an [`TyKind::Array`] of that arity.
    fn lower_array_lit(
        &mut self,
        elems: &[Expr],
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        let elem_hint = match hint.map(|h| self.m.ty(h).clone()) {
            Some(TyKind::Array(e, _)) | Some(TyKind::Slice(e)) => Some(e),
            _ => None,
        };
        let mut ops = Vec::with_capacity(elems.len());
        let mut elem_ty = elem_hint;
        for e in elems {
            let (op, ty) = self.lower_expr(e, elem_ty)?;
            elem_ty.get_or_insert(ty);
            ops.push(op);
        }
        let elem =
            elem_ty.ok_or_else(|| "empty array literal needs a type annotation".to_string())?;
        let n = elems.len() as u64;
        let aty = self.m.intern_ty(TyKind::Array(elem, n));
        let tmp = self.new_local(aty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Aggregate(AggKind::Array(elem), ops),
        );
        // In a slice context (`[]T`), materialize a slice over the freshly built array's storage.
        if matches!(hint.map(|h| self.m.ty(h)), Some(TyKind::Slice(_))) {
            let rawptr = self.m.intern_ty(TyKind::RawPtr);
            let usize_t = self.m.t_int(IntWidth::W64, false);
            let data = self.new_local(rawptr);
            self.fb()
                .assign(Place::local(data), Rvalue::AddrOf(Place::local(tmp)));
            let slice_ty = self.m.intern_ty(TyKind::Slice(elem));
            let sl = self.new_local(slice_ty);
            self.fb().assign(
                Place::local(sl),
                Rvalue::MakeSlice {
                    data: Operand::Copy(Place::local(data)),
                    len: Operand::Const(Const::Int(n as i128, usize_t)),
                    elem,
                },
            );
            return Ok((Operand::Copy(Place::local(sl)), slice_ty));
        }
        Ok((Operand::Copy(Place::local(tmp)), aty))
    }

    /// `squad x in xs { .. }` — for-each over a fixed array. Lowered to a counter loop
    /// `for i in 0..N { x = xs[i]; body }`; the increment block is the `skip`/continue target.
    fn lower_squad(&mut self, var: &str, iter: &Expr, body: &ast::Block) -> Result<(), String> {
        let (iop, ity) = self.lower_expr(iter, None)?;
        // A `vec[T]` iterates via a runtime length + `bet_vec_get` counter loop.
        if let TyKind::Vec(e) = *self.m.ty(ity) {
            return self.lower_vec_squad(var, iop, e, body);
        }
        let (elem, n) = match self.m.ty(ity) {
            TyKind::Array(e, n) => (*e, *n),
            TyKind::Soa(_) => {
                return Err(
                    "squad over a soa container binds a whole element, and soa don't do \
                     that. loop by index instead (vibin i < N { ... arr[i].field ... })."
                        .into(),
                );
            }
            other => {
                return Err(format!(
                    "`squad` over a non-array value ({other:?}) is not yet lowered"
                ));
            }
        };
        let iter_place = self
            .operand_place(iop)
            .ok_or_else(|| "`squad` iterable must be addressable".to_string())?;
        let i64t = self.m.t_i64();
        let bool_ty = self.m.t_bool();

        // counter = 0
        let ctr = self.new_local(i64t);
        self.fb().assign(
            Place::local(ctr),
            Rvalue::Use(Operand::Const(Const::Int(0, i64t))),
        );

        let pre = self.cur;
        let header = self.reserve_block();
        self.set_goto(pre, header);

        // header: counter < N ?
        self.select(header);
        let cond = self.new_local(bool_ty);
        self.fb().assign(
            Place::local(cond),
            Rvalue::BinOp(
                BinOp::Lt,
                Operand::Copy(Place::local(ctr)),
                Operand::Const(Const::Int(n as i128, i64t)),
                ArithMode::Na,
            ),
        );
        let header_end = self.cur;
        let body_bb = self.new_block();
        let exit = self.reserve_block();
        self.set_branch(header_end, Operand::Copy(Place::local(cond)), body_bb, exit);

        // body: bind `x = xs[counter]`, then the user block.
        self.select(body_bb);
        self.scopes.push(HashMap::new());
        let elem_place = extend(&iter_place, Proj::Index(Operand::Copy(Place::local(ctr))));
        let elem_local = self.new_local(elem);
        self.fb().assign(
            Place::local(elem_local),
            Rvalue::Use(Operand::Copy(elem_place)),
        );
        self.bind(var, elem_local);
        // `skip` continues to the increment block so the counter still advances; `dip` breaks.
        let incr = self.reserve_block();
        self.loops.push((incr, exit));
        self.lower_block(body)?;
        self.loops.pop();
        self.scopes.pop();
        self.term_goto(incr);

        // increment: counter += 1; back to header.
        self.select(incr);
        self.fb().assign(
            Place::local(ctr),
            Rvalue::BinOp(
                BinOp::Add,
                Operand::Copy(Place::local(ctr)),
                Operand::Const(Const::Int(1, i64t)),
                ArithMode::Wrap,
            ),
        );
        self.term_goto(header);

        self.select(exit);
        Ok(())
    }

    fn lower_method(
        &mut self,
        receiver: &Expr,
        method: &str,
        generics: &[ast::Type],
        args: &[ast::Arg],
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        // Stdlib module intrinsics (`math.lap`, ...) when the receiver is a bare module name.
        if let ExprKind::Name { name, .. } = &receiver.kind
            && self.lookup_local(name).is_none()
            && !self.funcs.contains_key(name)
            && self.is_module(name)
        {
            // `stash.new[K, V]()` / `vec.new[T]()` are the intrinsics that need their type args.
            if name == "stash" && method == "new" {
                return self.lower_stash_new(generics, args);
            }
            if name == "vec" && method == "new" {
                return self.lower_vec_new(generics, hint);
            }
            if name == "mem" && method == "slab" {
                return self.lower_mem_slab(generics, args);
            }
            return self.lower_intrinsic(name, method, args);
        }
        if !generics.is_empty() {
            return Err("generic method calls are not yet lowered".into());
        }
        // A user method: the receiver becomes the leading argument.
        if let Some(sig) = self.funcs.get(method).cloned() {
            if !sig.ok || !sig.has_receiver {
                return Err(format!("`{method}` is not a lowerable method"));
            }
            let mut call_args = Vec::with_capacity(args.len() + 1);
            let (recv, _) = self.lower_expr(receiver, Some(sig.params[0]))?;
            call_args.push(recv);
            for (i, a) in args.iter().enumerate() {
                let hint = sig.params.get(i + 1).copied();
                let (op, _) = self.lower_expr(&a.value, hint)?;
                call_args.push(op);
            }
            return self.emit_call(sig.id, &sig.rets, call_args);
        }
        // Otherwise evaluate the receiver: a `stash` (Map) value gets the hash-map methods, a
        // `yikes` gets `.tea`, and a struct with a `finna(..)` field an indirect call.
        let (bop, bty) = self.lower_expr(receiver, None)?;
        if let TyKind::Map(k, v) = *self.m.ty(bty) {
            return self.lower_stash_method(bop, k, v, method, args);
        }
        if let TyKind::Soa(inner) = *self.m.ty(bty)
            && let TyKind::Vec(e) = *self.m.ty(inner)
        {
            return self.lower_soa_vec_method(bop, e, method, args);
        }
        if let TyKind::Vec(e) = *self.m.ty(bty) {
            return self.lower_vec_method(bop, e, method, args);
        }
        if let TyKind::Array(e, n) = *self.m.ty(bty) {
            return self.lower_array_method(bop, e, n, method, args);
        }
        if matches!(*self.m.ty(bty), TyKind::Rng) {
            return self.lower_rng_method(bop, method, args);
        }
        if let TyKind::Simd { elem, lanes } = *self.m.ty(bty) {
            return self.lower_simd_method(bop, bty, elem, lanes, method, args);
        }
        if self.is_yikes(bty) {
            return self.lower_yikes_method(bop, method, args);
        }
        self.lower_fn_field_call(bop, bty, method, args)
    }

    /// `y.tea(context)` — wrap an error, prefixing `"<context>: "` to its message (Go's `%w`).
    fn lower_yikes_method(
        &mut self,
        yop: Operand,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        match method {
            "tea" => {
                if args.len() != 1 {
                    return Err("`yikes.tea` takes a single str context".into());
                }
                let strt = self.m.t_str();
                let (ctx, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let place = self
                    .operand_place(yop)
                    .ok_or_else(|| "`.tea` needs an addressable yikes".to_string())?;
                let msg = Operand::Copy(extend(&place, Proj::Field(1)));
                let sep = Operand::Const(Const::Str(": ".into()));
                let prefixed = self.concat_str(ctx, sep)?;
                let full = self.concat_str(prefixed, msg)?;
                Ok(self.build_yikes(true, full))
            }
            other => Err(format!("unknown `yikes` method `{other}`")),
        }
    }

    /// Concatenate two `str` values into a fresh `str` via `bet_str_concat`. The result length
    /// is the sum of the two byte lengths (`bet_str_concat` copies `a` then `b`).
    fn concat_str(&mut self, a: Operand, b: Operand) -> Result<Operand, String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let strt = self.m.t_str();

        let ap = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(ap), Rvalue::StrPtr(a.clone()));
        let al = self.new_local(usize_t);
        self.fb().assign(Place::local(al), Rvalue::StrLen(a));
        let bp = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(bp), Rvalue::StrPtr(b.clone()));
        let bl = self.new_local(usize_t);
        self.fb().assign(Place::local(bl), Rvalue::StrLen(b));

        let ext = self.get_extern(
            "bet_str_concat",
            vec![rawptr, usize_t, rawptr, usize_t],
            vec![rawptr],
        );
        let out_ptr = self.new_local(rawptr);
        self.fb().assign(
            Place::local(out_ptr),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![
                    Operand::Copy(Place::local(ap)),
                    Operand::Copy(Place::local(al)),
                    Operand::Copy(Place::local(bp)),
                    Operand::Copy(Place::local(bl)),
                ],
            ),
        );
        let out_len = self.new_local(usize_t);
        self.fb().assign(
            Place::local(out_len),
            Rvalue::BinOp(
                BinOp::Add,
                Operand::Copy(Place::local(al)),
                Operand::Copy(Place::local(bl)),
                ArithMode::Wrap,
            ),
        );
        let result = self.new_local(strt);
        self.fb().assign(
            Place::local(result),
            Rvalue::MakeStr {
                data: Operand::Copy(Place::local(out_ptr)),
                len: Operand::Copy(Place::local(out_len)),
            },
        );
        Ok(Operand::Copy(Place::local(result)))
    }

    /// `recv.field(args)` where `field` is a function-pointer field of the receiver's (already
    /// lowered) struct value (auto-dereferencing a `ref Struct`). An indirect call through it.
    fn lower_fn_field_call(
        &mut self,
        bop: Operand,
        bty: TyId,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        let base_place = self
            .operand_place(bop)
            .ok_or_else(|| format!("unknown method `{method}`"))?;
        let (sid, place) = match self.m.ty(bty) {
            TyKind::Struct(s) => (*s, base_place),
            TyKind::Ref(e) => match self.m.ty(*e) {
                TyKind::Struct(s) => (*s, extend(&base_place, Proj::Deref)),
                _ => return Err(format!("unknown method `{method}`")),
            },
            _ => return Err(format!("unknown method `{method}`")),
        };
        let def = self.m.struct_def(sid);
        let idx = def
            .fields
            .iter()
            .position(|f| f.name == method)
            .ok_or_else(|| format!("unknown method `{method}`"))?;
        let fty = def.fields[idx].ty;
        let sig = match self.m.ty(fty) {
            TyKind::FnPtr(s) => self.m.sig(*s).clone(),
            other => {
                return Err(format!(
                    "`{method}` is not a method or function-pointer field ({other:?})"
                ));
            }
        };
        let fptr_op = Operand::Copy(extend(&place, Proj::Field(idx as u32)));
        let call_args = self.lower_args(args, &sig.params)?;
        let ret_ty = self.rets_to_ty(&sig.rets);
        self.emit_call_result(Callee::Indirect(fptr_op), &sig.rets, call_args, ret_ty)
    }

    // === stash (hash maps) ===================================================

    /// `stash.new[K, V]()` — create an empty map. Lowered to `bet_map_new(size_of[V])`.
    ///
    /// With the `in: <crib>` allocator-context override (SP0.1) the creation is scoped by
    /// `bet_ctx_push(crib)` … `bet_ctx_pop()`, redirecting the ambient allocator for the
    /// duration. Routing `bet_map_new`'s own allocations through that context is a deliberate
    /// runtime follow-up (see `runtime` `bet_alloc` note); the language surface + scoped
    /// push/pop are what this task lands, so `stash.new(in: astCrib)` is a valid, wired form.
    fn lower_stash_new(
        &mut self,
        generics: &[ast::Type],
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        if generics.len() != 2 {
            return Err("`stash.new` takes two type arguments `[K, V]`".into());
        }
        let ctx_crib = self.alloc_ctx_arg(args)?;
        let k = self.map_type(&generics[0])?;
        let v = self.map_type(&generics[1])?;
        let map_ty = self.m.intern_ty(TyKind::Map(k, v));
        let usize_t = self.m.t_int(IntWidth::W64, false);
        if let Some((crib_op, crib_ty)) = &ctx_crib {
            let push = self.get_extern("bet_ctx_push", vec![*crib_ty], vec![]);
            self.emit_extern_call(push, &[], vec![crib_op.clone()])?;
        }
        let vsize = self.new_local(usize_t);
        self.fb().assign(Place::local(vsize), Rvalue::SizeOf(v));
        let ext = self.get_extern("bet_map_new", vec![usize_t], vec![map_ty]);
        let tmp = self.new_local(map_ty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![Operand::Copy(Place::local(vsize))],
            ),
        );
        if ctx_crib.is_some() {
            let pop = self.get_extern("bet_ctx_pop", vec![], vec![]);
            self.emit_extern_call(pop, &[], vec![])?;
        }
        Ok((Operand::Copy(Place::local(tmp)), map_ty))
    }

    /// Lower an optional `in: <crib>` allocator-context argument, shared by the collection
    /// constructors. Returns the lowered crib operand + its (crib) type when present. Rejects
    /// positional args and non-crib contexts.
    fn alloc_ctx_arg(&mut self, args: &[ast::Arg]) -> Result<Option<(Operand, TyId)>, String> {
        match args {
            [] => Ok(None),
            [a] if a.label.as_deref() == Some("in") => {
                let (op, ty) = self.lower_expr(&a.value, None)?;
                if !matches!(self.m.ty(ty), TyKind::Crib(_)) {
                    return Err(format!(
                        "`in:` needs a crib allocator context ({:?})",
                        self.m.ty(ty)
                    ));
                }
                Ok(Some((op, ty)))
            }
            _ => Err("collection constructor takes only an optional `in: <crib>`".into()),
        }
    }

    /// Dispatch a method on a `stash[K, V]` value: `put`, `peep`, `yeet`, or `gang`.
    fn lower_stash_method(
        &mut self,
        map_op: Operand,
        k: TyId,
        v: TyId,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        let map_ty = self.m.intern_ty(TyKind::Map(k, v));
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        match method {
            // `m.put(key, value)` → bet_map_put(map, key_ptr, key_len, val_ptr).
            "put" => {
                if args.len() != 2 {
                    return Err("`stash.put` takes a key and a value".into());
                }
                let (key_ptr, key_len) = self.serialize_key(&args[0].value, k)?;
                let (val_op, _) = self.lower_expr(&args[1].value, Some(v))?;
                let vl = self.new_local(v);
                self.fb().assign(Place::local(vl), Rvalue::Use(val_op));
                let val_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(val_ptr), Rvalue::AddrOf(Place::local(vl)));
                let ext =
                    self.get_extern("bet_map_put", vec![map_ty, rawptr, usize_t, rawptr], vec![]);
                let call_args = vec![
                    map_op,
                    key_ptr,
                    key_len,
                    Operand::Copy(Place::local(val_ptr)),
                ];
                self.emit_extern_call(ext, &[], call_args)
            }
            // `m.peep(key)` → (value, found): bet_map_get writes into an out slot; the result
            // is a `(V, bool)` tuple destructured by the caller's `lowkey v, ok = ...`.
            "peep" => {
                if args.len() != 1 {
                    return Err("`stash.peep` takes a single key".into());
                }
                let (key_ptr, key_len) = self.serialize_key(&args[0].value, k)?;
                let out = self.new_local(v);
                let val_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(val_ptr), Rvalue::AddrOf(Place::local(out)));
                let ext = self.get_extern(
                    "bet_map_get",
                    vec![map_ty, rawptr, usize_t, rawptr],
                    vec![boolt],
                );
                let ok = self.new_local(boolt);
                self.fb().assign(
                    Place::local(ok),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            map_op,
                            key_ptr,
                            key_len,
                            Operand::Copy(Place::local(val_ptr)),
                        ],
                    ),
                );
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![v, boolt]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(
                        AggKind::Tuple,
                        vec![
                            Operand::Copy(Place::local(out)),
                            Operand::Copy(Place::local(ok)),
                        ],
                    ),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `m.yeet(key)` → bet_map_del(map, key_ptr, key_len) -> bool (was-present).
            "yeet" => {
                if args.len() != 1 {
                    return Err("`stash.yeet` takes a single key".into());
                }
                let (key_ptr, key_len) = self.serialize_key(&args[0].value, k)?;
                let ext =
                    self.get_extern("bet_map_del", vec![map_ty, rawptr, usize_t], vec![boolt]);
                self.emit_extern_call(ext, &[boolt], vec![map_op, key_ptr, key_len])
            }
            // `m.gang()` → bet_map_len(map) -> usize.
            "gang" => {
                if !args.is_empty() {
                    return Err("`stash.gang` takes no arguments".into());
                }
                let ext = self.get_extern("bet_map_len", vec![map_ty], vec![usize_t]);
                self.emit_extern_call(ext, &[usize_t], vec![map_op])
            }
            other => Err(format!("unknown `stash` method `{other}`")),
        }
    }

    // === vec (growable arrays) ================================================

    /// `mem.slab[T](n) -> []T` — a heap-allocated, zero-initialized buffer of `n` elements,
    /// returned as a `[]T` slice. Fills the gap fixed-array literals (compile-time enumerated),
    /// append-only `vec`, and handle-accessed `crib` slabs leave: a random-access mutable buffer
    /// of runtime length (e.g. a framebuffer). `T` must be a scalar type (size == a valid align).
    /// Lowered to `bet_alloc_zeroed(n * size_of[T], size_of[T])` wrapped in a `MakeSlice`.
    fn lower_mem_slab(
        &mut self,
        generics: &[ast::Type],
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        if generics.len() != 1 {
            return Err("`mem.slab` takes one type argument `[T]`".into());
        }
        if args.len() != 1 {
            return Err("`mem.slab` takes a single element count `n`".into());
        }
        let e = self.map_type(&generics[0])?;
        // A `drip` element makes this a `soa` slab: one zeroed per-field array plus a fat
        // sub-slice per field, bundled as a `soa []Drip`. (An AoS struct slab isn't supported
        // — `mem.slab` needs a scalar-shaped element size/align, which SoA gives per field.)
        if let TyKind::Struct(sid) = *self.m.ty(e) {
            return self.lower_mem_slab_soa(sid, e, &args[0].value);
        }
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let i64t = self.m.t_i64();

        // n (element count), coerced to usize.
        let (n, nty) = self.lower_expr(&args[0].value, Some(i64t))?;
        let n_us = self.coerce_int(n, nty, usize_t);

        // esize = size_of[T] (also used as the allocation alignment — valid for scalar T).
        let esize = self.new_local(usize_t);
        self.fb().assign(Place::local(esize), Rvalue::SizeOf(e));

        // bytes = n * esize.
        let bytes = self.new_local(usize_t);
        self.fb().assign(
            Place::local(bytes),
            Rvalue::BinOp(
                BinOp::Mul,
                n_us.clone(),
                Operand::Copy(Place::local(esize)),
                ArithMode::Wrap,
            ),
        );

        // ptr = bet_alloc_zeroed(bytes, esize).
        let ext = self.get_extern("bet_alloc_zeroed", vec![usize_t, usize_t], vec![rawptr]);
        let ptr = self.new_local(rawptr);
        self.fb().assign(
            Place::local(ptr),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![
                    Operand::Copy(Place::local(bytes)),
                    Operand::Copy(Place::local(esize)),
                ],
            ),
        );

        // slice = { ptr, n } over the fresh storage.
        let slice_ty = self.m.intern_ty(TyKind::Slice(e));
        let sl = self.new_local(slice_ty);
        self.fb().assign(
            Place::local(sl),
            Rvalue::MakeSlice {
                data: Operand::Copy(Place::local(ptr)),
                len: n_us,
                elem: e,
            },
        );
        Ok((Operand::Copy(Place::local(sl)), slice_ty))
    }

    /// `mem.slab[Drip](n) -> soa []Drip` — the struct-of-arrays heap slab. Each field gets its
    /// own `bet_alloc_zeroed(n * size_of[Tj], size_of[Tj])` array wrapped in a fat sub-slice;
    /// the `k` sub-slices are bundled into a `soa []Drip`. Access is `slab[i].field`. Fields
    /// must be scalar-shaped (their size is a valid alignment, as `mem.slab` already requires).
    fn lower_mem_slab_soa(
        &mut self,
        sid: StructId,
        elem: TyId,
        count: &Expr,
    ) -> Result<(Operand, TyId), String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let i64t = self.m.t_i64();

        // Gather field types up front (avoids holding a borrow of `self.m` across lowering).
        let fields: Vec<TyId> = self.m.struct_def(sid).fields.iter().map(|f| f.ty).collect();
        for (j, &fty) in fields.iter().enumerate() {
            if matches!(
                self.m.ty(fty),
                TyKind::Struct(_)
                    | TyKind::Sum(_)
                    | TyKind::Array(..)
                    | TyKind::Tuple(_)
                    | TyKind::Soa(_)
            ) {
                let fname = self.m.struct_def(sid).fields[j].name.clone();
                return Err(format!(
                    "soa mem.slab needs scalar-shaped fields — drip field `{fname}` is an \
                     aggregate. keep soa slab drips flat (ints/floats/bools/handles)."
                ));
            }
        }

        // n, held in a usize local so each field's alloc reuses the same count.
        let (n, nty) = self.lower_expr(count, Some(i64t))?;
        let n_us = self.coerce_int(n, nty, usize_t);
        let n_local = self.new_local(usize_t);
        self.fb().assign(Place::local(n_local), Rvalue::Use(n_us));
        let n_op = || Operand::Copy(Place::local(n_local));

        let soa_ty = {
            let slice_ty = self.m.intern_ty(TyKind::Slice(elem));
            self.m.intern_ty(TyKind::Soa(slice_ty))
        };
        let bundle = self.new_local(soa_ty);

        for (j, &fty) in fields.iter().enumerate() {
            // esize = size_of[Tj]  (also the allocation alignment — valid for scalar fields).
            let esize = self.new_local(usize_t);
            self.fb().assign(Place::local(esize), Rvalue::SizeOf(fty));
            // bytes = n * esize.
            let bytes = self.new_local(usize_t);
            self.fb().assign(
                Place::local(bytes),
                Rvalue::BinOp(
                    BinOp::Mul,
                    n_op(),
                    Operand::Copy(Place::local(esize)),
                    ArithMode::Wrap,
                ),
            );
            // ptr = bet_alloc_zeroed(bytes, esize).
            let ext = self.get_extern("bet_alloc_zeroed", vec![usize_t, usize_t], vec![rawptr]);
            let ptr = self.new_local(rawptr);
            self.fb().assign(
                Place::local(ptr),
                Rvalue::Call(
                    Callee::Extern(ext),
                    vec![
                        Operand::Copy(Place::local(bytes)),
                        Operand::Copy(Place::local(esize)),
                    ],
                ),
            );
            // bundle.field(j) = { ptr, n } over the fresh per-field storage.
            let sub = extend(&Place::local(bundle), Proj::Field(j as u32));
            self.fb().assign(
                sub,
                Rvalue::MakeSlice {
                    data: Operand::Copy(Place::local(ptr)),
                    len: n_op(),
                    elem: fty,
                },
            );
        }
        Ok((Operand::Copy(Place::local(bundle)), soa_ty))
    }

    /// `vec.new[T]()` — create an empty growable array. Lowered to `bet_vec_new(size_of[T])`.
    fn lower_vec_new(
        &mut self,
        generics: &[ast::Type],
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        if generics.len() != 1 {
            return Err("`vec.new` takes one type argument `[T]`".into());
        }
        let e = self.map_type(&generics[0])?;
        // A `soa vec[Drip]` hint makes this a struct-of-arrays vec: one runtime handle per
        // field. (AoS `vec[Drip]` is a normal single-handle vec, so this is hint-driven.)
        if let Some(h) = hint
            && let TyKind::Soa(inner) = self.m.ty(h)
            && matches!(self.m.ty(*inner), TyKind::Vec(ve) if *ve == e)
            && let TyKind::Struct(sid) = *self.m.ty(e)
        {
            return self.lower_soa_vec_new(sid, e);
        }
        let vec_ty = self.m.intern_ty(TyKind::Vec(e));
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let esize = self.new_local(usize_t);
        self.fb().assign(Place::local(esize), Rvalue::SizeOf(e));
        let ext = self.get_extern("bet_vec_new", vec![usize_t], vec![vec_ty]);
        let tmp = self.new_local(vec_ty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![Operand::Copy(Place::local(esize))],
            ),
        );
        Ok((Operand::Copy(Place::local(tmp)), vec_ty))
    }

    /// `vec.new[Drip]()` in a `soa vec[Drip]` context — a bundle of `k` runtime vec handles,
    /// one per struct field (all growing in lockstep). Reuses the `bet_vec_*` ABI unchanged.
    /// Fields must be scalar-shaped (each handle stores a scalar-sized element), as `vec` and
    /// `mem.slab` already require.
    fn lower_soa_vec_new(&mut self, sid: StructId, elem: TyId) -> Result<(Operand, TyId), String> {
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let fields: Vec<TyId> = self.m.struct_def(sid).fields.iter().map(|f| f.ty).collect();
        for (j, &fty) in fields.iter().enumerate() {
            if matches!(
                self.m.ty(fty),
                TyKind::Struct(_)
                    | TyKind::Sum(_)
                    | TyKind::Array(..)
                    | TyKind::Tuple(_)
                    | TyKind::Soa(_)
            ) {
                let fname = self.m.struct_def(sid).fields[j].name.clone();
                return Err(format!(
                    "soa vec needs scalar-shaped fields — drip field `{fname}` is an aggregate. \
                     keep soa vec drips flat (ints/floats/bools/handles)."
                ));
            }
        }
        let soa_ty = {
            let vt = self.m.intern_ty(TyKind::Vec(elem));
            self.m.intern_ty(TyKind::Soa(vt))
        };
        let bundle = self.new_local(soa_ty);
        for (j, &fty) in fields.iter().enumerate() {
            let esize = self.new_local(usize_t);
            self.fb().assign(Place::local(esize), Rvalue::SizeOf(fty));
            let vec_j_ty = self.m.intern_ty(TyKind::Vec(fty));
            let ext = self.get_extern("bet_vec_new", vec![usize_t], vec![vec_j_ty]);
            let handle = self.new_local(vec_j_ty);
            self.fb().assign(
                Place::local(handle),
                Rvalue::Call(
                    Callee::Extern(ext),
                    vec![Operand::Copy(Place::local(esize))],
                ),
            );
            let slot = extend(&Place::local(bundle), Proj::Field(j as u32));
            self.fb()
                .assign(slot, Rvalue::Use(Operand::Copy(Place::local(handle))));
        }
        Ok((Operand::Copy(Place::local(bundle)), soa_ty))
    }

    /// Methods on a `soa vec[Drip]`: `stack` (push — scatter each field into its handle),
    /// `gang` (length — from handle 0, all grow in lockstep). `pop` is rejected (it would
    /// hand back a whole element, which soa can't).
    fn lower_soa_vec_method(
        &mut self,
        soa_op: Operand,
        elem: TyId,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        let bundle = self
            .operand_place(soa_op)
            .ok_or_else(|| "a soa vec method needs an addressable receiver".to_string())?;
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let sid = match self.m.ty(elem) {
            TyKind::Struct(s) => *s,
            _ => return Err("soa vec element isn't a drip".into()),
        };
        let fields: Vec<TyId> = self.m.struct_def(sid).fields.iter().map(|f| f.ty).collect();
        match method {
            "stack" => {
                if args.len() != 1 {
                    return Err("`vec.stack` takes a single element".into());
                }
                // Materialize the element, then push each field into its own handle.
                let (val_op, _) = self.lower_expr(&args[0].value, Some(elem))?;
                let el = self.new_local(elem);
                self.fb().assign(Place::local(el), Rvalue::Use(val_op));
                let mut last = None;
                for (j, &fty) in fields.iter().enumerate() {
                    let vec_j_ty = self.m.intern_ty(TyKind::Vec(fty));
                    let fptr = self.new_local(rawptr);
                    let fplace = extend(&Place::local(el), Proj::Field(j as u32));
                    self.fb().assign(Place::local(fptr), Rvalue::AddrOf(fplace));
                    let handle = Operand::Copy(extend(&bundle, Proj::Field(j as u32)));
                    let ext = self.get_extern("bet_vec_push", vec![vec_j_ty, rawptr], vec![]);
                    last = Some(self.emit_extern_call(
                        ext,
                        &[],
                        vec![handle, Operand::Copy(Place::local(fptr))],
                    )?);
                }
                last.ok_or_else(|| "a soa vec drip needs at least one field".to_string())
            }
            "gang" => {
                if !args.is_empty() {
                    return Err("`vec.gang` takes no arguments".into());
                }
                let f0 = *fields
                    .first()
                    .ok_or_else(|| "a soa vec drip needs at least one field".to_string())?;
                let vec0_ty = self.m.intern_ty(TyKind::Vec(f0));
                let handle0 = Operand::Copy(extend(&bundle, Proj::Field(0)));
                let ext = self.get_extern("bet_vec_len", vec![vec0_ty], vec![usize_t]);
                self.emit_extern_call(ext, &[usize_t], vec![handle0])
            }
            "pop" => Err(
                "soa vec pop would hand back a whole element, and soa can't. read the \
                 fields you need by index, or use a plain vec."
                    .into(),
            ),
            other => Err(format!(
                "`{other}` isn't a soa vec method — try stack / gang, or index a field \
                 (v[i].field)."
            )),
        }
    }

    /// `soa_vec[i].field` — read field `field` of element `i` via `bet_vec_get` on that
    /// field's handle (mirrors [`Self::lower_vec_index`], but selects the per-field vec).
    fn lower_soa_vec_read(
        &mut self,
        bundle: LocalId,
        elem: TyId,
        index: &Expr,
        field: &str,
    ) -> Result<(Operand, TyId), String> {
        let sid = match self.m.ty(elem) {
            TyKind::Struct(s) => *s,
            _ => return Err("soa vec element isn't a drip".into()),
        };
        let (j, fty) = {
            let def = self.m.struct_def(sid);
            let idx = def
                .fields
                .iter()
                .position(|f| f.name == field)
                .ok_or_else(|| format!("drip `{}` has no field `{field}`", def.name))?;
            (idx as u32, def.fields[idx].ty)
        };
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        let vec_j_ty = self.m.intern_ty(TyKind::Vec(fty));
        let (iop, ity) = self.lower_expr(index, Some(usize_t))?;
        let i_us = self.coerce_int(iop, ity, usize_t);
        let out = self.new_local(fty);
        let out_ptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out)));
        let handle = Operand::Copy(extend(&Place::local(bundle), Proj::Field(j)));
        let ext = self.get_extern("bet_vec_get", vec![vec_j_ty, usize_t, rawptr], vec![boolt]);
        let ok = self.new_local(boolt);
        self.fb().assign(
            Place::local(ok),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![handle, i_us, Operand::Copy(Place::local(out_ptr))],
            ),
        );
        Ok((Operand::Copy(Place::local(out)), fty))
    }

    /// `soa_vec[i].field = x` — write field `field` of element `i` via `bet_vec_set` on that
    /// field's handle.
    fn lower_soa_vec_write(
        &mut self,
        bundle: LocalId,
        elem: TyId,
        index: &Expr,
        field: &str,
        value: &Expr,
    ) -> Result<(), String> {
        let sid = match self.m.ty(elem) {
            TyKind::Struct(s) => *s,
            _ => return Err("soa vec element isn't a drip".into()),
        };
        let (j, fty) = {
            let def = self.m.struct_def(sid);
            let idx = def
                .fields
                .iter()
                .position(|f| f.name == field)
                .ok_or_else(|| format!("drip `{}` has no field `{field}`", def.name))?;
            (idx as u32, def.fields[idx].ty)
        };
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        let vec_j_ty = self.m.intern_ty(TyKind::Vec(fty));
        // value -> a slot (coerced to the field's integer width when relevant).
        let (vop, vty) = self.lower_expr(value, Some(fty))?;
        let vop = if matches!(self.m.ty(fty), TyKind::Int { .. })
            && matches!(self.m.ty(vty), TyKind::Int { .. })
        {
            self.coerce_int(vop, vty, fty)
        } else {
            vop
        };
        let slot = self.new_local(fty);
        self.fb().assign(Place::local(slot), Rvalue::Use(vop));
        let sptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(sptr), Rvalue::AddrOf(Place::local(slot)));
        let (iop, ity) = self.lower_expr(index, Some(usize_t))?;
        let i_us = self.coerce_int(iop, ity, usize_t);
        let handle = Operand::Copy(extend(&Place::local(bundle), Proj::Field(j)));
        let ext = self.get_extern("bet_vec_set", vec![vec_j_ty, usize_t, rawptr], vec![boolt]);
        let ok = self.new_local(boolt);
        self.fb().assign(
            Place::local(ok),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![handle, i_us, Operand::Copy(Place::local(sptr))],
            ),
        );
        Ok(())
    }

    /// Dispatch a method on a `vec[T]` value: `stack` (push), `pop`, or `gang` (length).
    fn lower_vec_method(
        &mut self,
        vec_op: Operand,
        e: TyId,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        let vec_ty = self.m.intern_ty(TyKind::Vec(e));
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        match method {
            // `v.stack(x)` → bet_vec_push(vec, elem_ptr): copy `x` to a slot, push its bytes.
            "stack" => {
                if args.len() != 1 {
                    return Err("`vec.stack` takes a single element".into());
                }
                let (val_op, _) = self.lower_expr(&args[0].value, Some(e))?;
                let el = self.new_local(e);
                self.fb().assign(Place::local(el), Rvalue::Use(val_op));
                let elem_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(elem_ptr), Rvalue::AddrOf(Place::local(el)));
                let ext = self.get_extern("bet_vec_push", vec![vec_ty, rawptr], vec![]);
                self.emit_extern_call(
                    ext,
                    &[],
                    vec![vec_op, Operand::Copy(Place::local(elem_ptr))],
                )
            }
            // `v.pop()` → bet_vec_pop writes the removed element into an out slot; yield it. The
            // "was-nonempty" bool is dropped (an empty pop yields the untouched slot).
            "pop" => {
                if !args.is_empty() {
                    return Err("`vec.pop` takes no arguments".into());
                }
                let out = self.new_local(e);
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out)));
                let ext = self.get_extern("bet_vec_pop", vec![vec_ty, rawptr], vec![boolt]);
                let ok = self.new_local(boolt);
                self.fb().assign(
                    Place::local(ok),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![vec_op, Operand::Copy(Place::local(out_ptr))],
                    ),
                );
                Ok((Operand::Copy(Place::local(out)), e))
            }
            // `v.gang()` → bet_vec_len(vec) -> usize.
            "gang" => {
                if !args.is_empty() {
                    return Err("`vec.gang` takes no arguments".into());
                }
                let ext = self.get_extern("bet_vec_len", vec![vec_ty], vec![usize_t]);
                self.emit_extern_call(ext, &[usize_t], vec![vec_op])
            }
            // `v.append(s)` → bet_vec_extend(vec, str_ptr, str_len): bulk-append a str's raw
            // bytes. The string-builder primitive; restricted to `vec[u8]`.
            "append" => {
                if args.len() != 1 {
                    return Err("`vec.append` takes a single str".into());
                }
                let u8t = self.m.t_int(IntWidth::W8, false);
                if e != u8t {
                    return Err("`vec.append` needs a `vec[u8]`".into());
                }
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let sp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(sp), Rvalue::StrPtr(s.clone()));
                let sl = self.new_local(usize_t);
                self.fb().assign(Place::local(sl), Rvalue::StrLen(s));
                let ext = self.get_extern("bet_vec_extend", vec![vec_ty, rawptr, usize_t], vec![]);
                self.emit_extern_call(
                    ext,
                    &[],
                    vec![
                        vec_op,
                        Operand::Copy(Place::local(sp)),
                        Operand::Copy(Place::local(sl)),
                    ],
                )
            }
            // `v.str()` → an owned `str` copied out of a `vec[u8]`'s buffer. Reads the backing
            // pointer (`bet_vec_data`) + length (`bet_vec_len`), then copies via `bet_str_concat`
            // (with an empty second run) so the result outlives any later mutation of the vec.
            "str" => {
                if !args.is_empty() {
                    return Err("`vec.str` takes no arguments".into());
                }
                let u8t = self.m.t_int(IntWidth::W8, false);
                if e != u8t {
                    return Err("`vec.str` needs a `vec[u8]`".into());
                }
                let strt = self.m.t_str();
                let data = self.new_local(rawptr);
                let dext = self.get_extern("bet_vec_data", vec![vec_ty], vec![rawptr]);
                self.fb().assign(
                    Place::local(data),
                    Rvalue::Call(Callee::Extern(dext), vec![vec_op.clone()]),
                );
                let len = self.new_local(usize_t);
                let lext = self.get_extern("bet_vec_len", vec![vec_ty], vec![usize_t]);
                self.fb().assign(
                    Place::local(len),
                    Rvalue::Call(Callee::Extern(lext), vec![vec_op]),
                );
                let cext = self.get_extern(
                    "bet_str_concat",
                    vec![rawptr, usize_t, rawptr, usize_t],
                    vec![rawptr],
                );
                let out_ptr = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(out_ptr),
                    Rvalue::Call(
                        Callee::Extern(cext),
                        vec![
                            Operand::Copy(Place::local(data)),
                            Operand::Copy(Place::local(len)),
                            Operand::Copy(Place::local(data)),
                            Operand::Const(Const::Int(0, usize_t)),
                        ],
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(out_ptr)),
                        len: Operand::Copy(Place::local(len)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `v.vibeCheck(pred)` → filter: keep elements where `pred(x)` holds, into a new vec.
            "vibeCheck" => self.build_filter(CollectionSrc::Vec { handle: vec_op }, e, args),
            // `v.glowUp(f)` → map: `f(x)` for each element, into a new vec of the result type.
            "glowUp" => self.build_map(CollectionSrc::Vec { handle: vec_op }, e, args),
            other => Err(format!(
                "unknown `vec` method `{other}` (have: stack, pop, gang, append, str, vibeCheck, glowUp)"
            )),
        }
    }

    /// `arr.<method>(..)` for a fixed-size array receiver (`[1, 2, 3].glowUp(f)`). Arrays don't
    /// grow, so mutators like `stack`/`pop` stay interp-only; the pure higher-order methods
    /// `vibeCheck`/`glowUp` build a fresh heap `vec`, and `gang` is the compile-time length.
    fn lower_array_method(
        &mut self,
        arr_op: Operand,
        e: TyId,
        n: u64,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        match method {
            "gang" => {
                if !args.is_empty() {
                    return Err("`gang` takes no arguments".into());
                }
                let usize_t = self.m.t_int(IntWidth::W64, false);
                Ok((Operand::Const(Const::Int(n as i128, usize_t)), usize_t))
            }
            "vibeCheck" | "glowUp" => {
                // The counter loop reads elements by `[i]`, so the array needs an address. A
                // temporary like an array literal (`[1,2,3].glowUp(f)`) isn't a place — spill it.
                let base = match self.operand_place(arr_op.clone()) {
                    Some(p) => p,
                    None => {
                        let arr_ty = self.m.intern_ty(TyKind::Array(e, n));
                        let tmp = self.new_local(arr_ty);
                        self.fb().assign(Place::local(tmp), Rvalue::Use(arr_op));
                        Place::local(tmp)
                    }
                };
                let src = CollectionSrc::Array { base, n };
                if method == "glowUp" {
                    self.build_map(src, e, args)
                } else {
                    self.build_filter(src, e, args)
                }
            }
            other => Err(format!(
                "bruh, a squad literal ain't got a `.{other}()` — try gang, vibeCheck, or glowUp"
            )),
        }
    }

    /// `xs.glowUp(f)` — map. Allocate a fresh `vec[U]` (U = `f`'s return type), then for each
    /// element push `f(x)`. Shared by the array and vec receivers via [`CollectionSrc`].
    fn build_map(
        &mut self,
        src: CollectionSrc,
        e: TyId,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        if args.len() != 1 {
            return Err("`glowUp` wants exactly one finna to map with".into());
        }
        let (fop, fty) = self.lower_expr(&args[0].value, None)?;
        let sig = match self.m.ty(fty) {
            TyKind::FnPtr(s) => self.m.sig(*s).clone(),
            other => return Err(format!("`glowUp` wants a finna to map with, not {other:?}")),
        };
        if sig.params.len() != 1 || sig.params[0] != e {
            return Err("`glowUp`'s finna gotta take one arg of the squad's element type".into());
        }
        if sig.rets.is_empty() {
            return Err("`glowUp`'s finna gotta return something to map into".into());
        }
        // Spill the finna to a local so the indirect callee carries a known `FnPtr` place type
        // (a bare `Const::FnRef` operand has none — the backend types the callee from its place).
        let callee = self.spill_fn_value(fop, fty);
        let u = self.rets_to_ty(&sig.rets);
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let out_vec_ty = self.m.intern_ty(TyKind::Vec(u));

        // out = bet_vec_new(sizeof(U))
        let esize = self.new_local(usize_t);
        self.fb().assign(Place::local(esize), Rvalue::SizeOf(u));
        let new_ext = self.get_extern("bet_vec_new", vec![usize_t], vec![out_vec_ty]);
        let out = self.new_local(out_vec_ty);
        self.fb().assign(
            Place::local(out),
            Rvalue::Call(
                Callee::Extern(new_ext),
                vec![Operand::Copy(Place::local(esize))],
            ),
        );
        let push_ext = self.get_extern("bet_vec_push", vec![out_vec_ty, rawptr], vec![]);

        let (ctr, header, exit) = self.open_count_loop(&src, e)?;
        // body: y = f(x); push its bytes onto `out`.
        let x = self.emit_collection_read(&src, ctr, e);
        let (y, _) = self.emit_call_result(
            Callee::Indirect(callee),
            &sig.rets,
            vec![Operand::Copy(Place::local(x))],
            u,
        )?;
        let yl = self.new_local(u);
        self.fb().assign(Place::local(yl), Rvalue::Use(y));
        let yptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(yptr), Rvalue::AddrOf(Place::local(yl)));
        self.emit_extern_call(
            push_ext,
            &[],
            vec![
                Operand::Copy(Place::local(out)),
                Operand::Copy(Place::local(yptr)),
            ],
        )?;
        self.close_count_loop(ctr, header, exit, usize_t);
        Ok((Operand::Copy(Place::local(out)), out_vec_ty))
    }

    /// `xs.vibeCheck(pred)` — filter. Allocate a fresh `vec[T]`, then for each element for which
    /// `pred(x)` is `true`, push `x`. Shared by the array and vec receivers via [`CollectionSrc`].
    fn build_filter(
        &mut self,
        src: CollectionSrc,
        e: TyId,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        if args.len() != 1 {
            return Err("`vibeCheck` wants exactly one finna to filter with".into());
        }
        let (fop, fty) = self.lower_expr(&args[0].value, None)?;
        let sig = match self.m.ty(fty) {
            TyKind::FnPtr(s) => self.m.sig(*s).clone(),
            other => {
                return Err(format!(
                    "`vibeCheck` wants a finna to filter with, not {other:?}"
                ));
            }
        };
        let boolt = self.m.t_bool();
        if sig.params.len() != 1 || sig.params[0] != e {
            return Err(
                "`vibeCheck`'s finna gotta take one arg of the squad's element type".into(),
            );
        }
        if sig.rets.as_slice() != [boolt] {
            return Err(
                "`vibeCheck`'s finna gotta return a `vibe` (bool) — keep it or don't".into(),
            );
        }
        // Spill the finna to a local so the indirect callee carries a known `FnPtr` place type.
        let callee = self.spill_fn_value(fop, fty);
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let out_vec_ty = self.m.intern_ty(TyKind::Vec(e));

        // out = bet_vec_new(sizeof(T))
        let esize = self.new_local(usize_t);
        self.fb().assign(Place::local(esize), Rvalue::SizeOf(e));
        let new_ext = self.get_extern("bet_vec_new", vec![usize_t], vec![out_vec_ty]);
        let out = self.new_local(out_vec_ty);
        self.fb().assign(
            Place::local(out),
            Rvalue::Call(
                Callee::Extern(new_ext),
                vec![Operand::Copy(Place::local(esize))],
            ),
        );
        let push_ext = self.get_extern("bet_vec_push", vec![out_vec_ty, rawptr], vec![]);

        let (ctr, header, exit) = self.open_count_loop(&src, e)?;
        // body: keep = pred(x); if keep, push x. Falls through to the shared increment block.
        let x = self.emit_collection_read(&src, ctr, e);
        let (keep, _) = self.emit_call_result(
            Callee::Indirect(callee),
            &sig.rets,
            vec![Operand::Copy(Place::local(x))],
            boolt,
        )?;
        let body_end = self.cur;
        let push_bb = self.new_block();
        let xptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(xptr), Rvalue::AddrOf(Place::local(x)));
        self.emit_extern_call(
            push_ext,
            &[],
            vec![
                Operand::Copy(Place::local(out)),
                Operand::Copy(Place::local(xptr)),
            ],
        )?;
        let cont = self.new_block();
        self.set_goto(push_bb, cont);
        self.set_branch(body_end, keep, push_bb, cont);
        self.select(cont);
        self.close_count_loop(ctr, header, exit, usize_t);
        Ok((Operand::Copy(Place::local(out)), out_vec_ty))
    }

    /// Open a counter loop over a collection: emit `ctr = 0`, the `ctr < len` header, and enter a
    /// fresh body block. Returns `(ctr, header, exit)`; the caller fills the body then calls
    /// [`Self::close_count_loop`] to wire the increment back to the header.
    fn open_count_loop(
        &mut self,
        src: &CollectionSrc,
        e: TyId,
    ) -> Result<(LocalId, BlockId, BlockId), String> {
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        let len = self.emit_collection_len(src, e);
        let ctr = self.new_local(usize_t);
        self.fb().assign(
            Place::local(ctr),
            Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
        );
        let pre = self.cur;
        let header = self.reserve_block();
        self.set_goto(pre, header);
        self.select(header);
        let cond = self.new_local(boolt);
        self.fb().assign(
            Place::local(cond),
            Rvalue::BinOp(
                BinOp::Lt,
                Operand::Copy(Place::local(ctr)),
                len,
                ArithMode::Na,
            ),
        );
        let header_end = self.cur;
        let body_bb = self.new_block();
        let exit = self.reserve_block();
        self.set_branch(header_end, Operand::Copy(Place::local(cond)), body_bb, exit);
        self.select(body_bb);
        Ok((ctr, header, exit))
    }

    /// Close a counter loop opened by [`Self::open_count_loop`]: `ctr += 1`, jump back to the
    /// header, then select the exit block so lowering continues after the loop.
    fn close_count_loop(&mut self, ctr: LocalId, header: BlockId, exit: BlockId, usize_t: TyId) {
        self.fb().assign(
            Place::local(ctr),
            Rvalue::BinOp(
                BinOp::Add,
                Operand::Copy(Place::local(ctr)),
                Operand::Const(Const::Int(1, usize_t)),
                ArithMode::Wrap,
            ),
        );
        self.term_goto(header);
        self.select(exit);
    }

    /// Store a function value into a fresh `FnPtr`-typed local and return a `Copy` of it. A bare
    /// function name lowers to a `Const::FnRef` operand, which carries no place type; the backend
    /// recovers an indirect callee's signature from its operand's place type, so it must be a
    /// local. (Mirrors the spill in [`Self::lower_slide`].)
    fn spill_fn_value(&mut self, fop: Operand, fty: TyId) -> Operand {
        let fnl = self.new_local(fty);
        self.fb().assign(Place::local(fnl), Rvalue::Use(fop));
        Operand::Copy(Place::local(fnl))
    }

    /// The element count of a collection as a `usize` operand: a compile-time constant for a
    /// fixed array, or a `bet_vec_len` call for a vec.
    fn emit_collection_len(&mut self, src: &CollectionSrc, e: TyId) -> Operand {
        let usize_t = self.m.t_int(IntWidth::W64, false);
        match src {
            CollectionSrc::Array { n, .. } => Operand::Const(Const::Int(*n as i128, usize_t)),
            CollectionSrc::Vec { handle } => {
                let vec_ty = self.m.intern_ty(TyKind::Vec(e));
                let len_ext = self.get_extern("bet_vec_len", vec![vec_ty], vec![usize_t]);
                let len = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(len),
                    Rvalue::Call(Callee::Extern(len_ext), vec![handle.clone()]),
                );
                Operand::Copy(Place::local(len))
            }
        }
    }

    /// Read element `ctr` of a collection into a fresh local of type `e` and return that local: an
    /// `[i]` array read, or a `bet_vec_get` for a vec.
    fn emit_collection_read(&mut self, src: &CollectionSrc, ctr: LocalId, e: TyId) -> LocalId {
        let el = self.new_local(e);
        match src {
            CollectionSrc::Array { base, .. } => {
                let elem = extend(base, Proj::Index(Operand::Copy(Place::local(ctr))));
                self.fb()
                    .assign(Place::local(el), Rvalue::Use(Operand::Copy(elem)));
            }
            CollectionSrc::Vec { handle } => {
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let boolt = self.m.t_bool();
                let vec_ty = self.m.intern_ty(TyKind::Vec(e));
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(el)));
                let get_ext =
                    self.get_extern("bet_vec_get", vec![vec_ty, usize_t, rawptr], vec![boolt]);
                let got = self.new_local(boolt);
                self.fb().assign(
                    Place::local(got),
                    Rvalue::Call(
                        Callee::Extern(get_ext),
                        vec![
                            handle.clone(),
                            Operand::Copy(Place::local(ctr)),
                            Operand::Copy(Place::local(out_ptr)),
                        ],
                    ),
                );
            }
        }
        el
    }

    /// `v[i]` for a `vec[T]` — read element `i` into a fresh slot via `bet_vec_get` and yield it
    /// as a value (a vec index is a runtime read, not an assignable place like an array slot).
    fn lower_vec_index(
        &mut self,
        vec_op: Operand,
        e: TyId,
        index: &Expr,
    ) -> Result<(Operand, TyId), String> {
        let vec_ty = self.m.intern_ty(TyKind::Vec(e));
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();
        let (iop, ity) = self.lower_expr(index, Some(usize_t))?;
        let iop = self.coerce_int(iop, ity, usize_t);
        let out = self.new_local(e);
        let out_ptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out)));
        let ext = self.get_extern("bet_vec_get", vec![vec_ty, usize_t, rawptr], vec![boolt]);
        let ok = self.new_local(boolt);
        self.fb().assign(
            Place::local(ok),
            Rvalue::Call(
                Callee::Extern(ext),
                vec![vec_op, iop, Operand::Copy(Place::local(out_ptr))],
            ),
        );
        Ok((Operand::Copy(Place::local(out)), e))
    }

    /// `squad x in v { .. }` for a `vec[T]` — a counter loop bounded by `bet_vec_len`, binding
    /// `x = bet_vec_get(v, i)` each iteration. Mirrors the fixed-array loop in [`Self::lower_squad`].
    fn lower_vec_squad(
        &mut self,
        var: &str,
        vec_op: Operand,
        e: TyId,
        body: &ast::Block,
    ) -> Result<(), String> {
        let vec_ty = self.m.intern_ty(TyKind::Vec(e));
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let boolt = self.m.t_bool();

        // len = bet_vec_len(v); counter = 0
        let len_ext = self.get_extern("bet_vec_len", vec![vec_ty], vec![usize_t]);
        let len = self.new_local(usize_t);
        self.fb().assign(
            Place::local(len),
            Rvalue::Call(Callee::Extern(len_ext), vec![vec_op.clone()]),
        );
        let ctr = self.new_local(usize_t);
        self.fb().assign(
            Place::local(ctr),
            Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
        );

        let pre = self.cur;
        let header = self.reserve_block();
        self.set_goto(pre, header);

        // header: counter < len ?
        self.select(header);
        let cond = self.new_local(boolt);
        self.fb().assign(
            Place::local(cond),
            Rvalue::BinOp(
                BinOp::Lt,
                Operand::Copy(Place::local(ctr)),
                Operand::Copy(Place::local(len)),
                ArithMode::Na,
            ),
        );
        let header_end = self.cur;
        let body_bb = self.new_block();
        let exit = self.reserve_block();
        self.set_branch(header_end, Operand::Copy(Place::local(cond)), body_bb, exit);

        // body: x = bet_vec_get(v, counter); then the user block.
        self.select(body_bb);
        self.scopes.push(HashMap::new());
        let elem_local = self.new_local(e);
        let out_ptr = self.new_local(rawptr);
        self.fb().assign(
            Place::local(out_ptr),
            Rvalue::AddrOf(Place::local(elem_local)),
        );
        let get_ext = self.get_extern("bet_vec_get", vec![vec_ty, usize_t, rawptr], vec![boolt]);
        let got = self.new_local(boolt);
        self.fb().assign(
            Place::local(got),
            Rvalue::Call(
                Callee::Extern(get_ext),
                vec![
                    vec_op.clone(),
                    Operand::Copy(Place::local(ctr)),
                    Operand::Copy(Place::local(out_ptr)),
                ],
            ),
        );
        self.bind(var, elem_local);
        let incr = self.reserve_block();
        self.loops.push((incr, exit));
        self.lower_block(body)?;
        self.loops.pop();
        self.scopes.pop();
        self.term_goto(incr);

        // increment: counter += 1; back to header.
        self.select(incr);
        self.fb().assign(
            Place::local(ctr),
            Rvalue::BinOp(
                BinOp::Add,
                Operand::Copy(Place::local(ctr)),
                Operand::Const(Const::Int(1, usize_t)),
                ArithMode::Wrap,
            ),
        );
        self.term_goto(header);
        self.select(exit);
        Ok(())
    }

    /// Serialize a key to a `(ptr, len)` pair for the map primitives: a `str` key uses its data
    /// pointer + byte length; any other key is stored into a fresh local and its address + size
    /// taken (so an `int` key hashes over its raw bytes).
    fn serialize_key(&mut self, key_expr: &Expr, kty: TyId) -> Result<(Operand, Operand), String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let (key_op, _) = self.lower_expr(key_expr, Some(kty))?;
        if matches!(self.m.ty(kty), TyKind::Str) {
            let kp = self.new_local(rawptr);
            self.fb()
                .assign(Place::local(kp), Rvalue::StrPtr(key_op.clone()));
            let kl = self.new_local(usize_t);
            self.fb().assign(Place::local(kl), Rvalue::StrLen(key_op));
            Ok((
                Operand::Copy(Place::local(kp)),
                Operand::Copy(Place::local(kl)),
            ))
        } else {
            let slot = self.new_local(kty);
            self.fb().assign(Place::local(slot), Rvalue::Use(key_op));
            let kp = self.new_local(rawptr);
            self.fb()
                .assign(Place::local(kp), Rvalue::AddrOf(Place::local(slot)));
            let kl = self.new_local(usize_t);
            self.fb().assign(Place::local(kl), Rvalue::SizeOf(kty));
            Ok((
                Operand::Copy(Place::local(kp)),
                Operand::Copy(Place::local(kl)),
            ))
        }
    }

    fn is_module(&self, name: &str) -> bool {
        is_builtin_module(name)
    }

    /// Dispatch a method on an `rng` handle (`math.cook`): `roll` (raw 64-bit draw), `frac`
    /// (a float in `[0, 1)`), or `upTo(n)` (an unbiased int in `[0, n)`). The runtime speaks
    /// `u64`; the `int`-facing sides cross the signed/unsigned boundary via `coerce_int`.
    fn lower_rng_method(
        &mut self,
        rng_op: Operand,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        let rng_ty = self.m.intern_ty(TyKind::Rng);
        let u64t = self.m.t_int(IntWidth::W64, false);
        let i64t = self.m.t_i64();
        let f64t = self.m.intern_ty(TyKind::F64);
        match method {
            // `g.roll()` → bet_rng_next(rng) -> u64, presented as an `int`.
            "roll" => {
                if !args.is_empty() {
                    return Err("`rng.roll` takes no arguments".into());
                }
                let ext = self.get_extern("bet_rng_next", vec![rng_ty], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb().assign(
                    Place::local(raw),
                    Rvalue::Call(Callee::Extern(ext), vec![rng_op]),
                );
                let out = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, i64t);
                Ok((out, i64t))
            }
            // `g.frac()` → bet_rng_frac(rng) -> f64 in `[0, 1)`.
            "frac" => {
                if !args.is_empty() {
                    return Err("`rng.frac` takes no arguments".into());
                }
                let ext = self.get_extern("bet_rng_frac", vec![rng_ty], vec![f64t]);
                self.emit_extern_call(ext, &[f64t], vec![rng_op])
            }
            // `g.upTo(n)` → bet_rng_upto(rng, n) -> u64, unbiased in `[0, n)`. Both the bound
            // and the result cross the `int`↔`u64` boundary.
            "upTo" => {
                if args.len() != 1 {
                    return Err("`rng.upTo` takes a single bound".into());
                }
                let (n, nty) = self.lower_expr(&args[0].value, Some(i64t))?;
                let nu = self.coerce_int(n, nty, u64t);
                let ext = self.get_extern("bet_rng_upto", vec![rng_ty, u64t], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb().assign(
                    Place::local(raw),
                    Rvalue::Call(Callee::Extern(ext), vec![rng_op, nu]),
                );
                let out = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, i64t);
                Ok((out, i64t))
            }
            other => Err(format!("unknown `rng` method `{other}`")),
        }
    }

    /// Emit an `Rvalue::Simd` into a fresh local and return it as an operand of type `ty`.
    fn emit_simd(&mut self, op: SimdOp, args: Vec<Operand>, ty: TyId) -> (Operand, TyId) {
        let tmp = self.new_local(ty);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Simd { op, args, ty });
        (Operand::Copy(Place::local(tmp)), ty)
    }

    /// Dispatch a method on a SIMD vector value. `vty` is the vector type, `elem` its scalar
    /// element. Element-wise `+ - * / >> <<` are operators (handled by `lower_binary`); these are
    /// the reductions, min/max/abs, and float length/normalize.
    fn lower_simd_method(
        &mut self,
        recv: Operand,
        vty: TyId,
        elem: TyId,
        _lanes: u32,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        match method {
            "min" | "max" => {
                if args.len() != 1 {
                    return Err(format!("`.{method}` takes one vector argument"));
                }
                let (w, _) = self.lower_expr(&args[0].value, Some(vty))?;
                let op = if method == "min" {
                    SimdOp::Min
                } else {
                    SimdOp::Max
                };
                Ok(self.emit_simd(op, vec![recv, w], vty))
            }
            "abs" => {
                if !args.is_empty() {
                    return Err("`.abs` takes no arguments".into());
                }
                Ok(self.emit_simd(SimdOp::Abs, vec![recv], vty))
            }
            "dot" => {
                if args.len() != 1 {
                    return Err("`.dot` takes one vector argument".into());
                }
                let (w, _) = self.lower_expr(&args[0].value, Some(vty))?;
                Ok(self.emit_simd(SimdOp::Dot, vec![recv, w], elem))
            }
            "sum" => {
                if !args.is_empty() {
                    return Err("`.sum` takes no arguments".into());
                }
                Ok(self.emit_simd(SimdOp::Sum, vec![recv], elem))
            }
            // `v.scale(s)` = `v * splat(s)` — a convenience for the common scalar-broadcast multiply.
            "scale" => {
                if args.len() != 1 {
                    return Err("`.scale` takes one scalar argument".into());
                }
                let (s, _) = self.lower_expr(&args[0].value, Some(elem))?;
                let (splat, _) = self.emit_simd(SimdOp::Splat, vec![s], vty);
                let tmp = self.new_local(vty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::BinOp(BinOp::Mul, recv, splat, ArithMode::Na),
                );
                Ok((Operand::Copy(Place::local(tmp)), vty))
            }
            "length" => {
                if !args.is_empty() {
                    return Err("`.length` takes no arguments".into());
                }
                Ok(self.emit_simd(SimdOp::Length, vec![recv], elem))
            }
            "norm" => {
                if !args.is_empty() {
                    return Err("`.norm` takes no arguments".into());
                }
                Ok(self.emit_simd(SimdOp::Norm, vec![recv], vty))
            }
            other => Err(format!("unknown simd method `{other}`")),
        }
    }

    /// Lower an array/slice argument and take the address of its storage as a `rawptr` — the base
    /// pointer the `gg.*` externs expect for a pixel/sample buffer. An array's address is its
    /// first element's; a slice's data pointer is reached by projecting element 0 through its fat
    /// pointer.
    fn buffer_base_ptr(&mut self, arg: &Expr) -> Result<Operand, String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let (buf, bufty) = self.lower_expr(arg, None)?;
        let place = self
            .operand_place(buf)
            .ok_or_else(|| "`gg` buffer argument must be addressable".to_string())?;
        let base = match self.m.ty(bufty) {
            TyKind::Array(..) => place,
            TyKind::Slice(_) => {
                let i64t = self.m.t_i64();
                extend(&place, Proj::Index(Operand::Const(Const::Int(0, i64t))))
            }
            other => {
                return Err(format!(
                    "`gg` buffer argument must be an array or slice ({other:?})"
                ));
            }
        };
        let ptr = self.new_local(rawptr);
        self.fb().assign(Place::local(ptr), Rvalue::AddrOf(base));
        Ok(Operand::Copy(Place::local(ptr)))
    }

    /// Like [`Self::buffer_base_ptr`], but returns a raw pointer `byteOff` BYTES into the buffer.
    /// The `gg.tex` / `gg.sound` ABI counts the offset in bytes, but midir has no pointer+int
    /// arithmetic — only element-granular indexing — so the byte offset is converted to an element
    /// index by dividing by the element's size (`byteOff / sizeof(T)`). Exact for `byteOff == 0`
    /// (the demo) and any offset that is a whole multiple of the element size; for a `[]u8` buffer
    /// `sizeof == 1`, so it matches `bytes.readU32le`'s plain `Index(byteOff)`.
    fn buffer_byte_ptr(&mut self, buf_arg: &Expr, off_arg: &Expr) -> Result<Operand, String> {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let i64t = self.m.t_i64();
        let (buf, bufty) = self.lower_expr(buf_arg, None)?;
        let place = self
            .operand_place(buf)
            .ok_or_else(|| "`gg` buffer argument must be addressable".to_string())?;
        let elem = match self.m.ty(bufty) {
            TyKind::Array(elem, _) => *elem,
            TyKind::Slice(elem) => *elem,
            other => {
                return Err(format!(
                    "`gg` buffer argument must be an array or slice ({other:?})"
                ));
            }
        };
        let (off, offty) = self.lower_expr(off_arg, Some(i64t))?;
        let off = self.coerce_int(off, offty, i64t);
        // elem_idx = byteOff / sizeof(T): element-granular GEP (midir has no ptr+int arithmetic).
        let esize_us = self.new_local(usize_t);
        self.fb()
            .assign(Place::local(esize_us), Rvalue::SizeOf(elem));
        let esize = self.coerce_int(Operand::Copy(Place::local(esize_us)), usize_t, i64t);
        let idx = self.new_local(i64t);
        self.fb().assign(
            Place::local(idx),
            Rvalue::BinOp(BinOp::Div, off, esize, ArithMode::Trap),
        );
        let elem_place = extend(&place, Proj::Index(Operand::Copy(Place::local(idx))));
        let ptr = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(ptr), Rvalue::AddrOf(elem_place));
        Ok(Operand::Copy(Place::local(ptr)))
    }

    fn lower_intrinsic(
        &mut self,
        module: &str,
        method: &str,
        args: &[ast::Arg],
    ) -> Result<(Operand, TyId), String> {
        match (module, method) {
            // `math.lap(a, b)` — explicit wrapping arithmetic (any build, any signedness).
            ("math", "lap") => {
                if args.len() != 2 {
                    return Err("`math.lap` takes two arguments".into());
                }
                let (a, aty) = self.lower_expr(&args[0].value, None)?;
                let (b, _) = self.lower_expr(&args[1].value, Some(aty))?;
                let tmp = self.new_local(aty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::BinOp(BinOp::Add, a, b, ArithMode::Wrap),
                );
                Ok((Operand::Copy(Place::local(tmp)), aty))
            }
            // `math.cook(seed)` — a seeded PRNG handle (`rng`). The `int` seed is reinterpreted
            // as `u64` for the runtime constructor.
            ("math", "cook") => {
                if args.len() != 1 {
                    return Err("`math.cook` takes a single int seed".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let rng_ty = self.m.intern_ty(TyKind::Rng);
                let (seed, sty) = self.lower_expr(&args[0].value, Some(i64t))?;
                let seedu = self.coerce_int(seed, sty, u64t);
                let ext = self.get_extern("bet_rng_new", vec![u64t], vec![rng_ty]);
                let tmp = self.new_local(rng_ty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Call(Callee::Extern(ext), vec![seedu]),
                );
                Ok((Operand::Copy(Place::local(tmp)), rng_ty))
            }
            // `bytes.readU32le(buf, off)` — a little-endian u32 from `buf[off..off+4]`.
            ("bytes", "readU32le") => {
                if args.len() != 2 {
                    return Err("`bytes.readU32le` takes a byte slice and an offset".into());
                }
                let i64t = self.m.t_i64();
                let u32t = self.m.t_int(IntWidth::W32, false);
                let (buf, bufty) = self.lower_expr(&args[0].value, None)?;
                let elem = match self.m.ty(bufty) {
                    TyKind::Slice(e) | TyKind::Array(e, _) => *e,
                    other => {
                        return Err(format!("`bytes.readU32le` needs a byte slice ({other:?})"));
                    }
                };
                let buf_place = self
                    .operand_place(buf)
                    .ok_or_else(|| "`bytes.readU32le` buffer must be addressable".to_string())?;
                let (off, _) = self.lower_expr(&args[1].value, Some(i64t))?;

                let acc = self.new_local(u32t);
                self.fb().assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(Const::Int(0, u32t))),
                );
                for k in 0..4u32 {
                    // idx = off + k
                    let idx = self.new_local(i64t);
                    self.fb().assign(
                        Place::local(idx),
                        Rvalue::BinOp(
                            BinOp::Add,
                            off.clone(),
                            Operand::Const(Const::Int(k as i128, i64t)),
                            ArithMode::Wrap,
                        ),
                    );
                    // byte = buf[idx] (element type `elem`), zero-extended to u32
                    let byte_place =
                        extend(&buf_place, Proj::Index(Operand::Copy(Place::local(idx))));
                    let widened = self.new_local(u32t);
                    let kind = self.cast_kind(elem, u32t).unwrap_or(CastKind::IntZext);
                    self.fb().assign(
                        Place::local(widened),
                        Rvalue::Cast(Operand::Copy(byte_place), u32t, kind),
                    );
                    // shifted = widened << (8 * k)
                    let shifted = self.new_local(u32t);
                    self.fb().assign(
                        Place::local(shifted),
                        Rvalue::BinOp(
                            BinOp::Shl,
                            Operand::Copy(Place::local(widened)),
                            Operand::Const(Const::Int((8 * k) as i128, u32t)),
                            ArithMode::Na,
                        ),
                    );
                    // acc |= shifted
                    let next = self.new_local(u32t);
                    self.fb().assign(
                        Place::local(next),
                        Rvalue::BinOp(
                            BinOp::BitOr,
                            Operand::Copy(Place::local(acc)),
                            Operand::Copy(Place::local(shifted)),
                            ArithMode::Na,
                        ),
                    );
                    self.fb().assign(
                        Place::local(acc),
                        Rvalue::Use(Operand::Copy(Place::local(next))),
                    );
                }
                Ok((Operand::Copy(Place::local(acc)), u32t))
            }
            // `bytes.readU16le(buf, off)` — a little-endian u16 from `buf[off..off+2]`,
            // zero-extended (keeps the narrow unsigned width, like `readU32le`).
            ("bytes", "readU16le") => {
                if args.len() != 2 {
                    return Err("`bytes.readU16le` takes a byte slice and an offset".into());
                }
                let i64t = self.m.t_i64();
                let u16t = self.m.t_int(IntWidth::W16, false);
                let (buf, bufty) = self.lower_expr(&args[0].value, None)?;
                let elem = match self.m.ty(bufty) {
                    TyKind::Slice(e) | TyKind::Array(e, _) => *e,
                    other => {
                        return Err(format!("`bytes.readU16le` needs a byte slice ({other:?})"));
                    }
                };
                let buf_place = self
                    .operand_place(buf)
                    .ok_or_else(|| "`bytes.readU16le` buffer must be addressable".to_string())?;
                let (off, _) = self.lower_expr(&args[1].value, Some(i64t))?;

                let acc = self.new_local(u16t);
                self.fb().assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(Const::Int(0, u16t))),
                );
                for k in 0..2u32 {
                    let idx = self.new_local(i64t);
                    self.fb().assign(
                        Place::local(idx),
                        Rvalue::BinOp(
                            BinOp::Add,
                            off.clone(),
                            Operand::Const(Const::Int(k as i128, i64t)),
                            ArithMode::Wrap,
                        ),
                    );
                    let byte_place =
                        extend(&buf_place, Proj::Index(Operand::Copy(Place::local(idx))));
                    let widened = self.new_local(u16t);
                    let kind = self.cast_kind(elem, u16t).unwrap_or(CastKind::IntZext);
                    self.fb().assign(
                        Place::local(widened),
                        Rvalue::Cast(Operand::Copy(byte_place), u16t, kind),
                    );
                    let shifted = self.new_local(u16t);
                    self.fb().assign(
                        Place::local(shifted),
                        Rvalue::BinOp(
                            BinOp::Shl,
                            Operand::Copy(Place::local(widened)),
                            Operand::Const(Const::Int((8 * k) as i128, u16t)),
                            ArithMode::Na,
                        ),
                    );
                    let next = self.new_local(u16t);
                    self.fb().assign(
                        Place::local(next),
                        Rvalue::BinOp(
                            BinOp::BitOr,
                            Operand::Copy(Place::local(acc)),
                            Operand::Copy(Place::local(shifted)),
                            ArithMode::Na,
                        ),
                    );
                    self.fb().assign(
                        Place::local(acc),
                        Rvalue::Use(Operand::Copy(Place::local(next))),
                    );
                }
                Ok((Operand::Copy(Place::local(acc)), u16t))
            }
            // `bytes.readI16le(buf, off)` — a little-endian i16 from `buf[off..off+2]`,
            // sign-extended to `int` (i64).
            ("bytes", "readI16le") => {
                if args.len() != 2 {
                    return Err("`bytes.readI16le` takes a byte slice and an offset".into());
                }
                let i64t = self.m.t_i64();
                let u16t = self.m.t_int(IntWidth::W16, false);
                let i16t = self.m.t_int(IntWidth::W16, true);
                let (buf, bufty) = self.lower_expr(&args[0].value, None)?;
                let elem = match self.m.ty(bufty) {
                    TyKind::Slice(e) | TyKind::Array(e, _) => *e,
                    other => {
                        return Err(format!("`bytes.readI16le` needs a byte slice ({other:?})"));
                    }
                };
                let buf_place = self
                    .operand_place(buf)
                    .ok_or_else(|| "`bytes.readI16le` buffer must be addressable".to_string())?;
                let (off, _) = self.lower_expr(&args[1].value, Some(i64t))?;

                let acc = self.new_local(u16t);
                self.fb().assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(Const::Int(0, u16t))),
                );
                for k in 0..2u32 {
                    let idx = self.new_local(i64t);
                    self.fb().assign(
                        Place::local(idx),
                        Rvalue::BinOp(
                            BinOp::Add,
                            off.clone(),
                            Operand::Const(Const::Int(k as i128, i64t)),
                            ArithMode::Wrap,
                        ),
                    );
                    let byte_place =
                        extend(&buf_place, Proj::Index(Operand::Copy(Place::local(idx))));
                    let widened = self.new_local(u16t);
                    let kind = self.cast_kind(elem, u16t).unwrap_or(CastKind::IntZext);
                    self.fb().assign(
                        Place::local(widened),
                        Rvalue::Cast(Operand::Copy(byte_place), u16t, kind),
                    );
                    let shifted = self.new_local(u16t);
                    self.fb().assign(
                        Place::local(shifted),
                        Rvalue::BinOp(
                            BinOp::Shl,
                            Operand::Copy(Place::local(widened)),
                            Operand::Const(Const::Int((8 * k) as i128, u16t)),
                            ArithMode::Na,
                        ),
                    );
                    let next = self.new_local(u16t);
                    self.fb().assign(
                        Place::local(next),
                        Rvalue::BinOp(
                            BinOp::BitOr,
                            Operand::Copy(Place::local(acc)),
                            Operand::Copy(Place::local(shifted)),
                            ArithMode::Na,
                        ),
                    );
                    self.fb().assign(
                        Place::local(acc),
                        Rvalue::Use(Operand::Copy(Place::local(next))),
                    );
                }
                // Reinterpret the u16 pattern as i16 (same width => Bitcast), then sign-extend.
                let signed = self.coerce_int(Operand::Copy(Place::local(acc)), u16t, i16t);
                let wide = self.coerce_int(signed, i16t, i64t);
                Ok((wide, i64t))
            }
            // `fs.peepText(path)` — the whole file at `path` as a `str` (empty on any error).
            // (A future `(str, yikes)` form can layer the error channel on top.)
            ("fs", "peepText") => {
                if args.len() != 1 {
                    return Err("`fs.peepText` takes a single path string".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let (path, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let pp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(pp), Rvalue::StrPtr(path.clone()));
                let pl = self.new_local(usize_t);
                self.fb().assign(Place::local(pl), Rvalue::StrLen(path));
                // An out-parameter local for the read length; pass its address.
                let out_len = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(out_len),
                    Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
                );
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out_len)));
                let ext =
                    self.get_extern("bet_fs_read", vec![rawptr, usize_t, rawptr], vec![rawptr]);
                let data = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(data),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(pp)),
                            Operand::Copy(Place::local(pl)),
                            Operand::Copy(Place::local(out_ptr)),
                        ],
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(data)),
                        len: Operand::Copy(Place::local(out_len)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `fs.peep(path)` — the whole file at `path` as a `[]u8` (empty on any error).
            // A byte-slice sibling of `fs.peepText`; both call `bet_fs_read`, which returns raw
            // bytes, so the slice view is byte-identical to the interpreter's `[]u8`.
            ("fs", "peep") => {
                if args.len() != 1 {
                    return Err("`fs.peep` takes a single path string".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let u8t = self.m.t_int(IntWidth::W8, false);
                let slice_ty = self.m.intern_ty(TyKind::Slice(u8t));
                let strt = self.m.t_str();
                let (path, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let pp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(pp), Rvalue::StrPtr(path.clone()));
                let pl = self.new_local(usize_t);
                self.fb().assign(Place::local(pl), Rvalue::StrLen(path));
                // An out-parameter local for the read length; pass its address.
                let out_len = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(out_len),
                    Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
                );
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out_len)));
                let ext =
                    self.get_extern("bet_fs_read", vec![rawptr, usize_t, rawptr], vec![rawptr]);
                let data = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(data),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(pp)),
                            Operand::Copy(Place::local(pl)),
                            Operand::Copy(Place::local(out_ptr)),
                        ],
                    ),
                );
                let result = self.new_local(slice_ty);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeSlice {
                        data: Operand::Copy(Place::local(data)),
                        len: Operand::Copy(Place::local(out_len)),
                        elem: u8t,
                    },
                );
                Ok((Operand::Copy(Place::local(result)), slice_ty))
            }
            // `fs.drop(path, data)` — write a `[]u8` to a file (create-or-truncate), `nocap`
            // on success. The write sibling of `fs.peep`: both project the `str`/slice fat
            // values into (ptr, len) pairs for the runtime (`bet_fs_write`).
            ("fs", "drop") => {
                if args.len() != 2 {
                    return Err("`fs.drop` takes a path string and a []u8".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let boolt = self.m.t_bool();
                let strt = self.m.t_str();
                let u8t = self.m.t_int(IntWidth::W8, false);
                let slice_ty = self.m.intern_ty(TyKind::Slice(u8t));
                let (path, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let (data, data_ty) = self.lower_expr(&args[1].value, Some(slice_ty))?;
                if !matches!(self.m.ty(data_ty), TyKind::Slice(e) if *e == u8t) {
                    return Err("`fs.drop` data must be a []u8".into());
                }
                let pp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(pp), Rvalue::StrPtr(path.clone()));
                let pl = self.new_local(usize_t);
                self.fb().assign(Place::local(pl), Rvalue::StrLen(path));
                let dp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(dp), Rvalue::SlicePtr(data.clone()));
                let dl = self.new_local(usize_t);
                self.fb().assign(Place::local(dl), Rvalue::SliceLen(data));
                let ext = self.get_extern(
                    "bet_fs_write",
                    vec![rawptr, usize_t, rawptr, usize_t],
                    vec![boolt],
                );
                let ok = self.new_local(boolt);
                self.fb().assign(
                    Place::local(ok),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(pp)),
                            Operand::Copy(Place::local(pl)),
                            Operand::Copy(Place::local(dp)),
                            Operand::Copy(Place::local(dl)),
                        ],
                    ),
                );
                Ok((Operand::Copy(Place::local(ok)), boolt))
            }
            // `sys.argc()` — the process argument count, as an `int`.
            ("sys", "argc") => {
                if !args.is_empty() {
                    return Err("`sys.argc` takes no arguments".into());
                }
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_arg_count", vec![], vec![usize_t]);
                let n = self.new_local(usize_t);
                self.fb()
                    .assign(Place::local(n), Rvalue::Call(Callee::Extern(ext), vec![]));
                let out = self.coerce_int(Operand::Copy(Place::local(n)), usize_t, i64t);
                Ok((out, i64t))
            }
            // `sys.arg(i)` — the `i`-th process argument as a `str`, empty if out of range.
            ("sys", "arg") => {
                if args.len() != 1 {
                    return Err("`sys.arg` takes a single index".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let strt = self.m.t_str();
                let (idx, ity) = self.lower_expr(&args[0].value, Some(i64t))?;
                let idxu = self.coerce_int(idx, ity, usize_t);
                // An out-parameter local for the read length; pass its address.
                let out_len = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(out_len),
                    Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
                );
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out_len)));
                let ext = self.get_extern("bet_arg_get", vec![usize_t, rawptr], vec![rawptr]);
                let data = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(data),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![idxu, Operand::Copy(Place::local(out_ptr))],
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(data)),
                        len: Operand::Copy(Place::local(out_len)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `sys.peep()` — read one stdin line (trailing newline stripped); `""` at EOF. The
            // runtime returns an owned buffer + writes its byte length through the out pointer;
            // a null/zero-length return builds the empty `str`.
            ("sys", "peep") => {
                if !args.is_empty() {
                    return Err("`sys.peep` takes no arguments".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                // An out-parameter local for the line length; pass its address.
                let out_len = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(out_len),
                    Rvalue::Use(Operand::Const(Const::Int(0, usize_t))),
                );
                let out_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(out_ptr), Rvalue::AddrOf(Place::local(out_len)));
                let ext = self.get_extern("bet_read_line", vec![rawptr], vec![rawptr]);
                let data = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(data),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![Operand::Copy(Place::local(out_ptr))],
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(data)),
                        len: Operand::Copy(Place::local(out_len)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `str.len(s)` — the byte length of `s`, as an `int` (the fat-`str` len projection).
            ("str", "len") => {
                if args.len() != 1 {
                    return Err("`str.len` takes a single string".into());
                }
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let lenu = self.new_local(usize_t);
                self.fb().assign(Place::local(lenu), Rvalue::StrLen(s));
                let out = self.coerce_int(Operand::Copy(Place::local(lenu)), usize_t, i64t);
                Ok((out, i64t))
            }
            // `str.at(s, i)` — the byte at index `i`, zero-extended to an `int` (0..=255). Reads
            // through a transient `[]u8` view; `Proj::Index` is an unchecked GEP + load.
            ("str", "at") => {
                if args.len() != 2 {
                    return Err("`str.at` takes a string and a byte index".into());
                }
                let i64t = self.m.t_i64();
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let (idx, ity) = self.lower_expr(&args[1].value, Some(i64t))?;
                let idx = self.coerce_int(idx, ity, i64t);
                let b = self.str_byteslice_local(s);
                let elem = extend(&Place::local(b), Proj::Index(idx));
                let out = self.new_local(i64t);
                self.fb().assign(
                    Place::local(out),
                    Rvalue::Cast(Operand::Copy(elem), i64t, CastKind::IntZext),
                );
                Ok((Operand::Copy(Place::local(out)), i64t))
            }
            // `str.sub(s, start, end)` — the non-copying byte substring `s[start..end]`. Builds a
            // fresh `str` fat value over `StrPtr(s) + start` with length `end - start`.
            ("str", "sub") => {
                if args.len() != 3 {
                    return Err("`str.sub` takes a string and start/end byte offsets".into());
                }
                let i64t = self.m.t_i64();
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let (start, sty) = self.lower_expr(&args[1].value, Some(i64t))?;
                let start = self.coerce_int(start, sty, i64t);
                let (end, ety) = self.lower_expr(&args[2].value, Some(i64t))?;
                let end = self.coerce_int(end, ety, i64t);
                let b = self.str_byteslice_local(s);
                // `&view[start]` = base data pointer advanced by `start` bytes (GEP, no deref).
                let elem = extend(&Place::local(b), Proj::Index(start.clone()));
                let dptr = self.new_local(rawptr);
                self.fb().assign(Place::local(dptr), Rvalue::AddrOf(elem));
                let newlen = self.new_local(i64t);
                self.fb().assign(
                    Place::local(newlen),
                    Rvalue::BinOp(BinOp::Sub, end, start, ArithMode::Wrap),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(dptr)),
                        len: Operand::Copy(Place::local(newlen)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `str.bytes(s)` — a non-copying `[]u8` view sharing `s`'s storage.
            ("str", "bytes") => {
                if args.len() != 1 {
                    return Err("`str.bytes` takes a single string".into());
                }
                let strt = self.m.t_str();
                let u8t = self.m.t_int(IntWidth::W8, false);
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let b = self.str_byteslice_local(s);
                let slice_ty = self.m.intern_ty(TyKind::Slice(u8t));
                Ok((Operand::Copy(Place::local(b)), slice_ty))
            }
            // `str.fromBytesTrust(b)` — reinterpret a `[]u8` as a `str` without validating (the
            // greppable unchecked constructor for the lexer/emitter hot path). Zero-copy.
            ("str", "fromBytesTrust") => {
                if args.len() != 1 {
                    return Err("`str.fromBytesTrust` takes a single byte slice".into());
                }
                let (b, bty) = self.lower_expr(&args[0].value, None)?;
                if !matches!(self.m.ty(bty), TyKind::Slice(_)) {
                    return Err(format!(
                        "`str.fromBytesTrust` needs a byte slice ({:?})",
                        self.m.ty(bty)
                    ));
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(ptr), Rvalue::SlicePtr(b.clone()));
                let len = self.new_local(usize_t);
                self.fb().assign(Place::local(len), Rvalue::SliceLen(b));
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(ptr)),
                        len: Operand::Copy(Place::local(len)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `str.fromBytes(b)` — checked `[]u8` -> `str`: validate UTF-8, yielding an empty
            // string on malformed input. Branchless: `len` is multiplied by the validity bit,
            // so an invalid buffer collapses to a zero-length (empty) `str`.
            ("str", "fromBytes") => {
                if args.len() != 1 {
                    return Err("`str.fromBytes` takes a single byte slice".into());
                }
                let (b, bty) = self.lower_expr(&args[0].value, None)?;
                if !matches!(self.m.ty(bty), TyKind::Slice(_)) {
                    return Err(format!(
                        "`str.fromBytes` needs a byte slice ({:?})",
                        self.m.ty(bty)
                    ));
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(ptr), Rvalue::SlicePtr(b.clone()));
                let len = self.new_local(usize_t);
                self.fb().assign(Place::local(len), Rvalue::SliceLen(b));
                // `bet_str_valid` returns 1 (valid) or 0 (invalid) as a usize.
                let ext = self.get_extern("bet_str_valid", vec![rawptr, usize_t], vec![usize_t]);
                let valid = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(valid),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(ptr)),
                            Operand::Copy(Place::local(len)),
                        ],
                    ),
                );
                // efflen = len * valid → len when valid, 0 when invalid (an empty str).
                let efflen = self.new_local(usize_t);
                self.fb().assign(
                    Place::local(efflen),
                    Rvalue::BinOp(
                        BinOp::Mul,
                        Operand::Copy(Place::local(len)),
                        Operand::Copy(Place::local(valid)),
                        ArithMode::Wrap,
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(ptr)),
                        len: Operand::Copy(Place::local(efflen)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `str.glow(s)` — an ASCII-uppercased copy of `s`.
            ("str", "glow") => {
                if args.len() != 1 {
                    return Err("`str.glow` takes a single string".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let sp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(sp), Rvalue::StrPtr(s.clone()));
                let sl = self.new_local(usize_t);
                self.fb().assign(Place::local(sl), Rvalue::StrLen(s));
                let ext = self.get_extern("bet_str_upper", vec![rawptr, usize_t], vec![rawptr]);
                let out = self.new_local(rawptr);
                self.fb().assign(
                    Place::local(out),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(sp)),
                            Operand::Copy(Place::local(sl)),
                        ],
                    ),
                );
                let result = self.new_local(strt);
                self.fb().assign(
                    Place::local(result),
                    Rvalue::MakeStr {
                        data: Operand::Copy(Place::local(out)),
                        len: Operand::Copy(Place::local(sl)),
                    },
                );
                Ok((Operand::Copy(Place::local(result)), strt))
            }
            // `str.slaps(a, b)` — byte equality of two strings.
            ("str", "slaps") => {
                if args.len() != 2 {
                    return Err("`str.slaps` takes two strings".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let boolt = self.m.t_bool();
                let (a, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let (b, _) = self.lower_expr(&args[1].value, Some(strt))?;
                let ap = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(ap), Rvalue::StrPtr(a.clone()));
                let al = self.new_local(usize_t);
                self.fb().assign(Place::local(al), Rvalue::StrLen(a));
                let bp = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(bp), Rvalue::StrPtr(b.clone()));
                let bl = self.new_local(usize_t);
                self.fb().assign(Place::local(bl), Rvalue::StrLen(b));
                let ext = self.get_extern(
                    "bet_str_eq",
                    vec![rawptr, usize_t, rawptr, usize_t],
                    vec![boolt],
                );
                let out = self.new_local(boolt);
                self.fb().assign(
                    Place::local(out),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![
                            Operand::Copy(Place::local(ap)),
                            Operand::Copy(Place::local(al)),
                            Operand::Copy(Place::local(bp)),
                            Operand::Copy(Place::local(bl)),
                        ],
                    ),
                );
                Ok((Operand::Copy(Place::local(out)), boolt))
            }
            // `yikes.new(msg)` — construct a live error carrying `msg`.
            ("yikes", "new") => {
                if args.len() != 1 {
                    return Err("`yikes.new` takes a single message".into());
                }
                let strt = self.m.t_str();
                let (msg, _) = self.lower_expr(&args[0].value, Some(strt))?;
                Ok(self.build_yikes(true, msg))
            }
            // `mem.scratch()` — the thread's built-in per-frame bump arena (a `crib void`).
            ("mem", "scratch") => {
                if !args.is_empty() {
                    return Err("`mem.scratch` takes no arguments".into());
                }
                let voidt = self.m.t_void();
                let crib_ty = self.m.intern_ty(TyKind::Crib(voidt));
                let ext = self.get_extern("bet_scratch", vec![], vec![crib_ty]);
                let tmp = self.new_local(crib_ty);
                self.fb()
                    .assign(Place::local(tmp), Rvalue::Call(Callee::Extern(ext), vec![]));
                Ok((Operand::Copy(Place::local(tmp)), crib_ty))
            }
            // `gg.blit(pixels, w, h)` — present a framebuffer. Build a `FrameBuffer` in a stack
            // slot (`pixels` = the array's base pointer, `stride` = `width`) and hand its address
            // to `bet_gg_present`. Used as a void statement.
            ("gg", "blit") => {
                if args.len() != 3 {
                    return Err("`gg.blit` takes a pixel buffer, a width, and a height".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let u32t = self.m.t_u32();
                let pixels = self.buffer_base_ptr(&args[0].value)?;
                let (w, wty) = self.lower_expr(&args[1].value, Some(u32t))?;
                let w = self.coerce_int(w, wty, u32t);
                let (h, hty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let h = self.coerce_int(h, hty, u32t);
                let sid = self.frame_struct();
                let sty = self.m.intern_ty(TyKind::Struct(sid));
                let fb_slot = self.new_local(sty);
                // Fields in declaration order: pixels, width, height, stride (= width).
                self.fb().assign(
                    Place::local(fb_slot),
                    Rvalue::Aggregate(AggKind::Struct(sid), vec![pixels, w.clone(), h, w]),
                );
                let fb_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(fb_ptr), Rvalue::AddrOf(Place::local(fb_slot)));
                let ext = self.get_extern("bet_gg_present", vec![rawptr], vec![]);
                self.emit_extern_call(ext, &[], vec![Operand::Copy(Place::local(fb_ptr))])
            }
            // `gg.audio(samples, cnt)` — submit `cnt` interleaved stereo frames. Void statement.
            ("gg", "audio") => {
                if args.len() != 2 {
                    return Err("`gg.audio` takes a sample buffer and a frame count".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let frames = self.buffer_base_ptr(&args[0].value)?;
                let (cnt, cty) = self.lower_expr(&args[1].value, Some(usize_t))?;
                let cnt = self.coerce_int(cnt, cty, usize_t);
                let ext = self.get_extern("bet_gg_audio", vec![rawptr, usize_t], vec![]);
                self.emit_extern_call(ext, &[], vec![frames, cnt])
            }
            // `gg.title(name)` — set the window title from a string (its `{ptr, len}`). Void.
            ("gg", "title") => {
                if args.len() != 1 {
                    return Err("`gg.title` takes a single string".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let strt = self.m.t_str();
                let (s, _) = self.lower_expr(&args[0].value, Some(strt))?;
                let ptr = self.new_local(rawptr);
                let len = self.new_local(usize_t);
                self.fb()
                    .assign(Place::local(ptr), Rvalue::StrPtr(s.clone()));
                self.fb().assign(Place::local(len), Rvalue::StrLen(s));
                let ext = self.get_extern("bet_gg_title", vec![rawptr, usize_t], vec![]);
                self.emit_extern_call(
                    ext,
                    &[],
                    vec![
                        Operand::Copy(Place::local(ptr)),
                        Operand::Copy(Place::local(len)),
                    ],
                )
            }
            // `gg.poll()` -> (int, int) — poll the next input event into a stack slot, then return
            // `(kind, code)` zero-extended to `int`. The `bool` result and `x`/`y` are ignored; a
            // NONE event (`kind == 0`) means the queue was empty.
            ("gg", "poll") => {
                if !args.is_empty() {
                    return Err("`gg.poll` takes no arguments".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let boolt = self.m.t_bool();
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let sid = self.event_struct();
                let ev_ty = self.m.intern_ty(TyKind::Struct(sid));
                // An uninitialized `Event`-shaped slot; the extern fills it in (cf. `stash.peep`).
                let ev = self.new_local(ev_ty);
                let ev_ptr = self.new_local(rawptr);
                self.fb()
                    .assign(Place::local(ev_ptr), Rvalue::AddrOf(Place::local(ev)));
                let ext = self.get_extern("bet_gg_poll", vec![rawptr], vec![boolt]);
                // Bind and discard the `bool` (emptiness is signalled by `kind == NONE == 0`).
                let discard = self.new_local(boolt);
                self.fb().assign(
                    Place::local(discard),
                    Rvalue::Call(
                        Callee::Extern(ext),
                        vec![Operand::Copy(Place::local(ev_ptr))],
                    ),
                );
                // kind = ev.0 (u32@0), code = ev.1 (u32@4); zero-extend each to `int`.
                let kind_u = Operand::Copy(extend(&Place::local(ev), Proj::Field(0)));
                let kind = self.coerce_int(kind_u, u32t, i64t);
                let code_u = Operand::Copy(extend(&Place::local(ev), Proj::Field(1)));
                let code = self.coerce_int(code_u, u32t, i64t);
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![i64t, i64t]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(AggKind::Tuple, vec![kind, code]),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `gg.ticks()` -> int — a monotonic nanosecond timer, presented as `int`.
            ("gg", "ticks") => {
                if !args.is_empty() {
                    return Err("`gg.ticks` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_ticks", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                let v = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, i64t);
                Ok((v, i64t))
            }
            // `gg.size()` -> (int, int) — the live window `(width, height)`, for true dynamic
            // resolution. The ABI packs it as `w << 32 | h`; unpack `w` by a right shift and `h`
            // by truncating to the low 32 bits, presenting both as `int` (like `gg.poll`).
            ("gg", "size") => {
                if !args.is_empty() {
                    return Err("`gg.size` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_size", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                // w = raw >> 32
                let wsh = self.new_local(u64t);
                self.fb().assign(
                    Place::local(wsh),
                    Rvalue::BinOp(
                        BinOp::Shr,
                        Operand::Copy(Place::local(raw)),
                        Operand::Const(Const::Int(32, u64t)),
                        ArithMode::Na,
                    ),
                );
                let w = self.coerce_int(Operand::Copy(Place::local(wsh)), u64t, i64t);
                // h = raw truncated to its low 32 bits
                let h_u32 = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, u32t);
                let h = self.coerce_int(h_u32, u32t, i64t);
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![i64t, i64t]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(AggKind::Tuple, vec![w, h]),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `gg.tex(buf, byteOff, w, h) -> int` — upload an RGBA8 texture (4 bytes/pixel) from a
            // byte-offset view of `buf`; returns its 1-based id (u32 -> int, like `gg.ticks`).
            ("gg", "tex") => {
                if args.len() != 4 {
                    return Err(
                        "`gg.tex` takes a pixel buffer, a byte offset, a width, and a height"
                            .into(),
                    );
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let ptr = self.buffer_byte_ptr(&args[0].value, &args[1].value)?;
                let (w, wty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let w = self.coerce_int(w, wty, u32t);
                let (h, hty) = self.lower_expr(&args[3].value, Some(u32t))?;
                let h = self.coerce_int(h, hty, u32t);
                let ext = self.get_extern("bet_gg_tex", vec![rawptr, u32t, u32t], vec![u32t]);
                let id = self.new_local(u32t);
                self.fb().assign(
                    Place::local(id),
                    Rvalue::Call(Callee::Extern(ext), vec![ptr, w, h]),
                );
                let v = self.coerce_int(Operand::Copy(Place::local(id)), u32t, i64t);
                Ok((v, i64t))
            }
            // `gg.frame(w, h, color)` — begin a frame: (re)size the canvas and clear to `color`
            // (`0x00RRGGBB`). Void statement.
            ("gg", "frame") => {
                if args.len() != 3 {
                    return Err("`gg.frame` takes a width, a height, and a clear color".into());
                }
                let u32t = self.m.t_u32();
                let (w, wty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let w = self.coerce_int(w, wty, u32t);
                let (h, hty) = self.lower_expr(&args[1].value, Some(u32t))?;
                let h = self.coerce_int(h, hty, u32t);
                let (c, cty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let c = self.coerce_int(c, cty, u32t);
                let ext = self.get_extern("bet_gg_frame", vec![u32t, u32t, u32t], vec![]);
                self.emit_extern_call(ext, &[], vec![w, h, c])
            }
            // `gg.sprite(tex, x, y)` — src-over blit of texture `tex` at `(x, y)`. Void statement.
            ("gg", "sprite") => {
                if args.len() != 3 {
                    return Err("`gg.sprite` takes a texture id and an x, y position".into());
                }
                let u32t = self.m.t_u32();
                let i32t = self.m.t_int(IntWidth::W32, true);
                let (t, tty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let t = self.coerce_int(t, tty, u32t);
                let (x, xty) = self.lower_expr(&args[1].value, Some(i32t))?;
                let x = self.coerce_int(x, xty, i32t);
                let (y, yty) = self.lower_expr(&args[2].value, Some(i32t))?;
                let y = self.coerce_int(y, yty, i32t);
                let ext = self.get_extern("bet_gg_sprite", vec![u32t, i32t, i32t], vec![]);
                self.emit_extern_call(ext, &[], vec![t, x, y])
            }
            // `gg.spriteSub(tex, sx, sy, sw, sh, dx, dy)` — src-over blit of a texture's source
            // sub-rectangle to `(dx, dy)`. The glyph-blit primitive behind bitmap text. Void.
            ("gg", "spriteSub") => {
                if args.len() != 7 {
                    return Err(
                        "`gg.spriteSub` takes a texture id, a source x, y, w, h, and a dest x, y"
                            .into(),
                    );
                }
                let u32t = self.m.t_u32();
                let i32t = self.m.t_int(IntWidth::W32, true);
                let (t, tty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let t = self.coerce_int(t, tty, u32t);
                let (sx, sxty) = self.lower_expr(&args[1].value, Some(i32t))?;
                let sx = self.coerce_int(sx, sxty, i32t);
                let (sy, syty) = self.lower_expr(&args[2].value, Some(i32t))?;
                let sy = self.coerce_int(sy, syty, i32t);
                let (sw, swty) = self.lower_expr(&args[3].value, Some(u32t))?;
                let sw = self.coerce_int(sw, swty, u32t);
                let (sh, shty) = self.lower_expr(&args[4].value, Some(u32t))?;
                let sh = self.coerce_int(sh, shty, u32t);
                let (dx, dxty) = self.lower_expr(&args[5].value, Some(i32t))?;
                let dx = self.coerce_int(dx, dxty, i32t);
                let (dy, dyty) = self.lower_expr(&args[6].value, Some(i32t))?;
                let dy = self.coerce_int(dy, dyty, i32t);
                let ext = self.get_extern(
                    "bet_gg_sprite_sub",
                    vec![u32t, i32t, i32t, u32t, u32t, i32t, i32t],
                    vec![],
                );
                self.emit_extern_call(ext, &[], vec![t, sx, sy, sw, sh, dx, dy])
            }
            // `gg.rect(x, y, w, h, color)` — src-over fill with `color` (`0xAARRGGBB`). Void.
            ("gg", "rect") => {
                if args.len() != 5 {
                    return Err("`gg.rect` takes x, y, w, h, and a color".into());
                }
                let u32t = self.m.t_u32();
                let i32t = self.m.t_int(IntWidth::W32, true);
                let (x, xty) = self.lower_expr(&args[0].value, Some(i32t))?;
                let x = self.coerce_int(x, xty, i32t);
                let (y, yty) = self.lower_expr(&args[1].value, Some(i32t))?;
                let y = self.coerce_int(y, yty, i32t);
                let (w, wty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let w = self.coerce_int(w, wty, u32t);
                let (h, hty) = self.lower_expr(&args[3].value, Some(u32t))?;
                let h = self.coerce_int(h, hty, u32t);
                let (c, cty) = self.lower_expr(&args[4].value, Some(u32t))?;
                let c = self.coerce_int(c, cty, u32t);
                let ext =
                    self.get_extern("bet_gg_rect", vec![i32t, i32t, u32t, u32t, u32t], vec![]);
                self.emit_extern_call(ext, &[], vec![x, y, w, h, c])
            }
            // `gg.flush()` — present the composited canvas and pump input. Void statement.
            ("gg", "flush") => {
                if !args.is_empty() {
                    return Err("`gg.flush` takes no arguments".into());
                }
                let ext = self.get_extern("bet_gg_flush", vec![], vec![]);
                self.emit_extern_call(ext, &[], vec![])
            }
            // `gg.sound(buf, byteOff, byteLen, channels, rate) -> int` — register a PCM sound from a
            // byte view of `buf`; returns its 1-based id.
            ("gg", "sound") => {
                if args.len() != 5 {
                    return Err(
                        "`gg.sound` takes a sample buffer, a byte offset, a byte length, a channel \
                         count, and a sample rate"
                            .into(),
                    );
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let usize_t = self.m.t_int(IntWidth::W64, false);
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let ptr = self.buffer_byte_ptr(&args[0].value, &args[1].value)?;
                let (len, lty) = self.lower_expr(&args[2].value, Some(usize_t))?;
                let len = self.coerce_int(len, lty, usize_t);
                let (ch, chty) = self.lower_expr(&args[3].value, Some(u32t))?;
                let ch = self.coerce_int(ch, chty, u32t);
                let (rate, rty) = self.lower_expr(&args[4].value, Some(u32t))?;
                let rate = self.coerce_int(rate, rty, u32t);
                let ext = self.get_extern(
                    "bet_gg_sound",
                    vec![rawptr, usize_t, u32t, u32t],
                    vec![u32t],
                );
                let id = self.new_local(u32t);
                self.fb().assign(
                    Place::local(id),
                    Rvalue::Call(Callee::Extern(ext), vec![ptr, len, ch, rate]),
                );
                let v = self.coerce_int(Operand::Copy(Place::local(id)), u32t, i64t);
                Ok((v, i64t))
            }
            // `gg.play(soundId, loop, volume) -> int` — start a voice; returns its 1-based id.
            ("gg", "play") => {
                if args.len() != 3 {
                    return Err("`gg.play` takes a sound id, a loop flag, and a volume".into());
                }
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let (s, sty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let s = self.coerce_int(s, sty, u32t);
                let (lp, lty) = self.lower_expr(&args[1].value, Some(u32t))?;
                let lp = self.coerce_int(lp, lty, u32t);
                let (vol, vty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let vol = self.coerce_int(vol, vty, u32t);
                let ext = self.get_extern("bet_gg_play", vec![u32t, u32t, u32t], vec![u32t]);
                let id = self.new_local(u32t);
                self.fb().assign(
                    Place::local(id),
                    Rvalue::Call(Callee::Extern(ext), vec![s, lp, vol]),
                );
                let v = self.coerce_int(Operand::Copy(Place::local(id)), u32t, i64t);
                Ok((v, i64t))
            }
            // `gg.stop(voiceId)` — stop a voice. Void statement.
            ("gg", "stop") => {
                if args.len() != 1 {
                    return Err("`gg.stop` takes a single voice id".into());
                }
                let u32t = self.m.t_u32();
                let (v, vty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let v = self.coerce_int(v, vty, u32t);
                let ext = self.get_extern("bet_gg_stop", vec![u32t], vec![]);
                self.emit_extern_call(ext, &[], vec![v])
            }
            // `gg.mouse()` -> (int, int) — the mouse position in logical-canvas coordinates. The ABI
            // packs it `x << 32 | y`; unpack `x` by a right shift and `y` by truncation (like
            // `gg.size`), presenting both as `int`.
            ("gg", "mouse") => {
                if !args.is_empty() {
                    return Err("`gg.mouse` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_mouse", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                let xsh = self.new_local(u64t);
                self.fb().assign(
                    Place::local(xsh),
                    Rvalue::BinOp(
                        BinOp::Shr,
                        Operand::Copy(Place::local(raw)),
                        Operand::Const(Const::Int(32, u64t)),
                        ArithMode::Na,
                    ),
                );
                let x = self.coerce_int(Operand::Copy(Place::local(xsh)), u64t, i64t);
                let y_u32 = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, u32t);
                let y = self.coerce_int(y_u32, u32t, i64t);
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![i64t, i64t]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(AggKind::Tuple, vec![x, y]),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `gg.mouseDelta()` -> (int, int) — the raw mouse movement accumulated since the
            // previous call. The ABI packs two SIGNED i32s `(dx as u32) << 32 | (dy as u32)`, so
            // unlike `gg.mouse`/`gg.size` each half truncates to `i32` and then SIGN-extends to
            // `int` (a negative delta must survive the round trip).
            ("gg", "mouseDelta") => {
                if !args.is_empty() {
                    return Err("`gg.mouseDelta` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let i32t = self.m.t_int(IntWidth::W32, true);
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_mouse_delta", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                // dx = sext(trunc32(raw >> 32))
                let xsh = self.new_local(u64t);
                self.fb().assign(
                    Place::local(xsh),
                    Rvalue::BinOp(
                        BinOp::Shr,
                        Operand::Copy(Place::local(raw)),
                        Operand::Const(Const::Int(32, u64t)),
                        ArithMode::Na,
                    ),
                );
                let dx_i32 = self.coerce_int(Operand::Copy(Place::local(xsh)), u64t, i32t);
                let dx = self.coerce_int(dx_i32, i32t, i64t);
                // dy = sext(trunc32(raw))
                let dy_i32 = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, i32t);
                let dy = self.coerce_int(dy_i32, i32t, i64t);
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![i64t, i64t]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(AggKind::Tuple, vec![dx, dy]),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `gg.tune(voiceId, volume, pan)` — live-update a playing voice's Q8 volume and
            // stereo pan (0 = full left, 128 = center, 255 = full right). Void statement.
            ("gg", "tune") => {
                if args.len() != 3 {
                    return Err("`gg.tune` takes a voice id, a volume, and a pan".into());
                }
                let u32t = self.m.t_u32();
                let (v, vty) = self.lower_expr(&args[0].value, Some(u32t))?;
                let v = self.coerce_int(v, vty, u32t);
                let (vol, volty) = self.lower_expr(&args[1].value, Some(u32t))?;
                let vol = self.coerce_int(vol, volty, u32t);
                let (pan, panty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let pan = self.coerce_int(pan, panty, u32t);
                let ext = self.get_extern("bet_gg_tune", vec![u32t, u32t, u32t], vec![]);
                self.emit_extern_call(ext, &[], vec![v, vol, pan])
            }
            // `gg.show(pixels, w, h)` — present a tightly packed fixed-logical-size `w * h`
            // framebuffer, aspect-fit (integer nearest-neighbor upscale, centered letterbox)
            // into the live window: `gg.blit`'s input model with `gg.flush`'s scaling. Unlike
            // `gg.blit` there is no stride, so the ABI takes the base pointer + dims directly
            // (no `FrameBuffer` struct). Void statement.
            ("gg", "show") => {
                if args.len() != 3 {
                    return Err("`gg.show` takes a pixel buffer, a width, and a height".into());
                }
                let rawptr = self.m.intern_ty(TyKind::RawPtr);
                let u32t = self.m.t_u32();
                let pixels = self.buffer_base_ptr(&args[0].value)?;
                let (w, wty) = self.lower_expr(&args[1].value, Some(u32t))?;
                let w = self.coerce_int(w, wty, u32t);
                let (h, hty) = self.lower_expr(&args[2].value, Some(u32t))?;
                let h = self.coerce_int(h, hty, u32t);
                let ext = self.get_extern("bet_gg_show", vec![rawptr, u32t, u32t], vec![]);
                self.emit_extern_call(ext, &[], vec![pixels, w, h])
            }
            // `gg.audioSpec()` -> (int, int) — the audio device's output `(rate, channels)`.
            // The ABI packs `rate << 32 | channels`; unpack exactly like `gg.size`.
            ("gg", "audioSpec") => {
                if !args.is_empty() {
                    return Err("`gg.audioSpec` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let u32t = self.m.t_u32();
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_audio_spec", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                let rsh = self.new_local(u64t);
                self.fb().assign(
                    Place::local(rsh),
                    Rvalue::BinOp(
                        BinOp::Shr,
                        Operand::Copy(Place::local(raw)),
                        Operand::Const(Const::Int(32, u64t)),
                        ArithMode::Na,
                    ),
                );
                let rate = self.coerce_int(Operand::Copy(Place::local(rsh)), u64t, i64t);
                let ch_u32 = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, u32t);
                let ch = self.coerce_int(ch_u32, u32t, i64t);
                let tuple_ty = self.m.intern_ty(TyKind::Tuple(vec![i64t, i64t]));
                let tup = self.new_local(tuple_ty);
                self.fb().assign(
                    Place::local(tup),
                    Rvalue::Aggregate(AggKind::Tuple, vec![rate, ch]),
                );
                Ok((Operand::Copy(Place::local(tup)), tuple_ty))
            }
            // `gg.pending()` -> int — interleaved i16 samples still queued in the raw `gg.audio`
            // ring (streaming backpressure), presented as `int` like `gg.ticks`.
            ("gg", "pending") => {
                if !args.is_empty() {
                    return Err("`gg.pending` takes no arguments".into());
                }
                let u64t = self.m.t_int(IntWidth::W64, false);
                let i64t = self.m.t_i64();
                let ext = self.get_extern("bet_gg_pending", vec![], vec![u64t]);
                let raw = self.new_local(u64t);
                self.fb()
                    .assign(Place::local(raw), Rvalue::Call(Callee::Extern(ext), vec![]));
                let v = self.coerce_int(Operand::Copy(Place::local(raw)), u64t, i64t);
                Ok((v, i64t))
            }
            ("spill", _) => {
                Err("`spill.*` is a statement-level print, not a value expression".into())
            }
            _ => Err(format!(
                "stdlib intrinsic `{module}.{method}` is not yet lowered"
            )),
        }
    }

    /// Materialize a `[]u8` slice value viewing a `str` operand's storage (`{ StrPtr, StrLen }`),
    /// returning the local that holds it. The backbone of `str.at`/`sub`/`bytes`: once the string
    /// is a slice, the existing `Proj::Index` / `AddrOf` machinery walks its bytes.
    fn str_byteslice_local(&mut self, s: Operand) -> LocalId {
        let rawptr = self.m.intern_ty(TyKind::RawPtr);
        let usize_t = self.m.t_int(IntWidth::W64, false);
        let u8t = self.m.t_int(IntWidth::W8, false);
        let slice_ty = self.m.intern_ty(TyKind::Slice(u8t));
        let sp = self.new_local(rawptr);
        self.fb()
            .assign(Place::local(sp), Rvalue::StrPtr(s.clone()));
        let sl = self.new_local(usize_t);
        self.fb().assign(Place::local(sl), Rvalue::StrLen(s));
        let b = self.new_local(slice_ty);
        self.fb().assign(
            Place::local(b),
            Rvalue::MakeSlice {
                data: Operand::Copy(Place::local(sp)),
                len: Operand::Copy(Place::local(sl)),
                elem: u8t,
            },
        );
        b
    }

    fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[ast::Arg],
        hint: Option<TyId>,
    ) -> Result<(Operand, TyId), String> {
        // Direct call to a named user function or extern.
        if let ExprKind::Name { name, generics } = &callee.kind
            && self.lookup_local(name).is_none()
        {
            if !generics.is_empty() {
                // A generic function call: monomorphize the instance and call it directly.
                if self.generic_funcs.contains_key(name) {
                    let (id, params, rets) = self.mono_fn(name, generics)?;
                    let call_args = self.lower_args(args, &params)?;
                    return self.emit_call(id, &rets, call_args);
                }
                return Err(format!("`{name}[..]` is not a known generic function"));
            }
            // A call whose callee names a SIMD vector type constructs one: `f32x4(a,b,c,d)` builds
            // the vector lane-by-lane; `f32x4(x)` broadcasts one scalar to every lane.
            if simd_type_name(name).is_some() && !self.funcs.contains_key(name) {
                let vty = self.named_type(name)?;
                let (elem, lanes) = match *self.m.ty(vty) {
                    TyKind::Simd { elem, lanes } => (elem, lanes),
                    _ => unreachable!("simd_type_name matched a non-simd type"),
                };
                if args.len() == lanes as usize {
                    let mut ops = Vec::with_capacity(args.len());
                    for a in args {
                        let (op, _) = self.lower_expr(&a.value, Some(elem))?;
                        ops.push(op);
                    }
                    let tmp = self.new_local(vty);
                    self.fb().assign(
                        Place::local(tmp),
                        Rvalue::Aggregate(AggKind::Simd(elem), ops),
                    );
                    return Ok((Operand::Copy(Place::local(tmp)), vty));
                }
                if args.len() == 1 {
                    let (op, _) = self.lower_expr(&args[0].value, Some(elem))?;
                    return Ok(self.emit_simd(SimdOp::Splat, vec![op], vty));
                }
                return Err(format!(
                    "`{name}` takes {lanes} lane values or 1 scalar to splat, got {}",
                    args.len()
                ));
            }
            // A call whose callee names a `moods` variant is a value constructor with payload.
            if self.variants.contains_key(name) && !self.funcs.contains_key(name) {
                let (sid, variant) = self.resolve_variant(name, hint)?;
                let payload = self.m.sum_def(sid).variants[variant as usize]
                    .payload
                    .clone();
                if args.len() != payload.len() {
                    return Err(format!(
                        "variant `{name}` takes {} field(s), got {}",
                        payload.len(),
                        args.len()
                    ));
                }
                let mut ops = Vec::with_capacity(args.len());
                for (a, &pty) in args.iter().zip(&payload) {
                    let (op, _) = self.lower_expr(&a.value, Some(pty))?;
                    ops.push(op);
                }
                let sty = self.m.intern_ty(TyKind::Sum(sid));
                let tmp = self.new_local(sty);
                self.fb().assign(
                    Place::local(tmp),
                    Rvalue::Aggregate(AggKind::Sum { sum: sid, variant }, ops),
                );
                return Ok((Operand::Copy(Place::local(tmp)), sty));
            }
            if let Some(sig) = self.funcs.get(name).cloned() {
                if !sig.ok {
                    return Err(format!("`{name}` is generic and not yet lowerable"));
                }
                let call_args = self.lower_args(args, &sig.params)?;
                return self.emit_call(sig.id, &sig.rets, call_args);
            }
            if let Some((eid, params, rets)) = self.externs.get(name).cloned() {
                let call_args = self.lower_args(args, &params)?;
                return self.emit_extern_call(eid, &rets, call_args);
            }
            return Err(format!("call to unknown function `{name}`"));
        }
        // Indirect call through a function-pointer value.
        let (fop, fty) = self.lower_expr(callee, None)?;
        let sig = match self.m.ty(fty) {
            TyKind::FnPtr(s) => self.m.sig(*s).clone(),
            other => return Err(format!("call of a non-function value ({other:?})")),
        };
        let call_args = self.lower_args(args, &sig.params)?;
        let ret_ty = self.rets_to_ty(&sig.rets);
        let out = self.emit_call_result(Callee::Indirect(fop), &sig.rets, call_args, ret_ty)?;
        Ok(out)
    }

    fn lower_args(&mut self, args: &[ast::Arg], params: &[TyId]) -> Result<Vec<Operand>, String> {
        let mut out = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let hint = params.get(i).copied();
            let (op, _) = self.lower_expr(&a.value, hint)?;
            self.deny_whole_soa_read(&op)?;
            out.push(op);
        }
        Ok(out)
    }

    fn emit_call(
        &mut self,
        id: FuncId,
        rets: &[TyId],
        args: Vec<Operand>,
    ) -> Result<(Operand, TyId), String> {
        let ret_ty = self.rets_to_ty(rets);
        self.emit_call_result(Callee::Direct(id), rets, args, ret_ty)
    }

    fn emit_extern_call(
        &mut self,
        id: ExternId,
        rets: &[TyId],
        args: Vec<Operand>,
    ) -> Result<(Operand, TyId), String> {
        let ret_ty = self.rets_to_ty(rets);
        self.emit_call_result(Callee::Extern(id), rets, args, ret_ty)
    }

    fn emit_call_result(
        &mut self,
        callee: Callee,
        rets: &[TyId],
        args: Vec<Operand>,
        ret_ty: TyId,
    ) -> Result<(Operand, TyId), String> {
        let tmp = self.new_local(ret_ty);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Call(callee, args));
        let _ = rets;
        Ok((Operand::Copy(Place::local(tmp)), ret_ty))
    }

    fn lower_struct_lit(&mut self, lit: &ast::StructLit) -> Result<(Operand, TyId), String> {
        let sid = if lit.generics.is_empty() {
            *self
                .structs
                .get(&lit.name)
                .ok_or_else(|| format!("unknown struct `{}`", lit.name))?
        } else {
            self.mono_struct(&lit.name, &lit.generics)?
        };
        let field_tys: Vec<(String, TyId)> = self
            .m
            .struct_def(sid)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        // Build operands in declaration order, matching each field by name; an omitted
        // field zero-defaults (spec §5 — `zero_value` for the rules).
        let mut ops = Vec::with_capacity(field_tys.len());
        for (fname, fty) in &field_tys {
            let op = match lit.fields.iter().find(|fi| &fi.name == fname) {
                Some(init) => self.lower_expr(&init.value, Some(*fty))?.0,
                None => self.zero_value(*fty).map_err(|why| {
                    format!("`{}` field `{fname}` cannot zero-default: {why}", lit.name)
                })?,
            };
            ops.push(op);
        }
        let sty = self.m.intern_ty(TyKind::Struct(sid));
        let tmp = self.new_local(sty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Aggregate(AggKind::Struct(sid), ops),
        );
        Ok((Operand::Copy(Place::local(tmp)), sty))
    }

    fn lower_cop(&mut self, init: &ast::CopInit, crib: &Expr) -> Result<(Operand, TyId), String> {
        let (crib_op, crib_ty) = self.lower_expr(crib, None)?;
        let elem = match self.m.ty(crib_ty) {
            TyKind::Crib(e) => *e,
            other => return Err(format!("`cop` into a non-crib value ({other:?})")),
        };
        // A typed crib hands back a `tag elem`; a bump (untyped) crib hands back a live `ref` to
        // the freshly bump-allocated value (so `.field` access auto-derefs into the arena).
        let is_bump = matches!(self.m.ty(elem), TyKind::Void);
        let mut bump_struct: Option<StructId> = None;
        let cop_init = match init {
            ast::CopInit::Struct(lit) => {
                let sid = *self
                    .structs
                    .get(&lit.name)
                    .ok_or_else(|| format!("unknown struct `{}`", lit.name))?;
                let field_tys: Vec<(String, TyId)> = self
                    .m
                    .struct_def(sid)
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty))
                    .collect();
                // Every declared field is written into the fresh slot, an omitted one as its
                // zero default (spec §5) — a reused slot must never leak its previous
                // occupant's bytes through `cop T{}`.
                let mut fields = Vec::with_capacity(field_tys.len());
                for (idx, (fname, fty)) in field_tys.iter().enumerate() {
                    let op = match lit.fields.iter().find(|fi| &fi.name == fname) {
                        Some(fi) => self.lower_expr(&fi.value, Some(*fty))?.0,
                        None => self.zero_value(*fty).map_err(|why| {
                            format!(
                                "`cop {}` field `{fname}` cannot zero-default: {why}",
                                lit.name
                            )
                        })?,
                    };
                    fields.push((idx as u32, op));
                }
                bump_struct = Some(sid);
                CopInit::StructLit(sid, fields)
            }
            ast::CopInit::Variant { name, args } => {
                if is_bump {
                    return Err(
                        "`cop` of a variant into an untyped bump crib is not lowered".into(),
                    );
                }
                let sid = match self.m.ty(elem) {
                    TyKind::Sum(s) => *s,
                    _ => return Err("`cop` of a variant into a non-sum crib".into()),
                };
                let variant = self.variant_index(sid, name)?;
                let payload = self.m.sum_def(sid).variants[variant as usize]
                    .payload
                    .clone();
                if args.len() != payload.len() {
                    return Err(format!("`cop` variant `{name}` has the wrong arity"));
                }
                let mut ops = Vec::with_capacity(args.len());
                for (a, &pty) in args.iter().zip(&payload) {
                    let (op, _) = self.lower_expr(&a.value, Some(pty))?;
                    ops.push(op);
                }
                CopInit::SumVariant(sid, variant, ops)
            }
        };
        // Bump crib → `ref Struct`; typed crib → `tag elem`.
        let result_ty = match bump_struct {
            Some(sid) if is_bump => {
                let sty = self.m.intern_ty(TyKind::Struct(sid));
                self.m.intern_ty(TyKind::Ref(sty))
            }
            _ => self.m.intern_ty(TyKind::Tag(elem)),
        };
        let tmp = self.new_local(result_ty);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Cop(crib_op, cop_init));
        Ok((Operand::Copy(Place::local(tmp)), result_ty))
    }

    fn lower_trust(&mut self, tag: &Expr, crib: &Expr) -> Result<(Operand, TyId), String> {
        let (tag_op, _) = self.lower_expr(tag, None)?;
        let (crib_op, crib_ty) = self.lower_expr(crib, None)?;
        let elem = match self.m.ty(crib_ty) {
            TyKind::Crib(e) => *e,
            other => return Err(format!("`trust` against a non-crib value ({other:?})")),
        };
        let ref_ty = self.m.intern_ty(TyKind::Ref(elem));
        let tmp = self.new_local(ref_ty);
        self.fb()
            .assign(Place::local(tmp), Rvalue::Trust(crib_op, tag_op));
        Ok((Operand::Copy(Place::local(tmp)), ref_ty))
    }

    // === lvalues =============================================================

    /// Lower an expression used as an assignment target to a [`Place`].
    fn lower_place(&mut self, e: &Expr) -> Result<Place, String> {
        match &e.kind {
            ExprKind::Name { name, generics } if generics.is_empty() => {
                let l = self
                    .lookup_local(name)
                    .ok_or_else(|| format!("assignment to unknown binding `{name}`"))?;
                Ok(Place::local(l))
            }
            ExprKind::Field {
                base,
                name,
                generics,
            } if generics.is_empty() => {
                let base_place = self.lower_place(base)?;
                let bty = self.place_ty(&base_place)?;
                let (sid, place) = match self.m.ty(bty) {
                    TyKind::Struct(s) => (*s, base_place),
                    TyKind::Ref(e) => match self.m.ty(*e) {
                        TyKind::Struct(s) => (*s, extend(&base_place, Proj::Deref)),
                        other => return Err(format!("field assign through ref to {other:?}")),
                    },
                    other => return Err(format!("field assignment on non-struct ({other:?})")),
                };
                let def = self.m.struct_def(sid);
                let idx = def
                    .fields
                    .iter()
                    .position(|f| f.name == *name)
                    .ok_or_else(|| format!("struct `{}` has no field `{name}`", def.name))?;
                Ok(extend(&place, Proj::Field(idx as u32)))
            }
            // `base[i] = v` — an array or slice element place. (A `vec` element is not an
            // assignable place: it's a runtime handle, written only via its methods.)
            ExprKind::Index { base, index } => {
                let base_place = self.lower_place(base)?;
                let bty = self.place_ty(&base_place)?;
                match self.m.ty(bty) {
                    TyKind::Array(..) | TyKind::Slice(_) => {}
                    // `soa[i].field = v` reaches here for the inner `soa[i]` place; the field
                    // projection is appended by the caller. A bare `soa[i] = v` whole-element
                    // write is rejected at `lower_assign` (see `ban_soa_elem`).
                    TyKind::Soa(inner) => match self.m.ty(*inner) {
                        TyKind::Array(..) | TyKind::Slice(_) => {}
                        TyKind::Vec(_) => {
                            return Err("writing a `soa vec` element by field isn't wired yet \
                                 (vec SoA lands in a later phase)."
                                .into());
                        }
                        other => {
                            return Err(format!("cannot index-assign a soa {other:?}"));
                        }
                    },
                    TyKind::Vec(_) => {
                        return Err(
                            "cannot assign to a `vec` element (vec is append-only; use an array \
                             or `mem.slab`)"
                                .into(),
                        );
                    }
                    other => {
                        return Err(format!("cannot index-assign a non-array/slice ({other:?})"));
                    }
                }
                let i64t = self.m.t_i64();
                let (iop, _ity) = self.lower_expr(index, Some(i64t))?;
                Ok(extend(&base_place, Proj::Index(iop)))
            }
            _ => Err("unsupported assignment target".into()),
        }
    }

    // === builder / block plumbing ============================================

    fn fb(&mut self) -> &mut FuncBuilder {
        self.fb.as_mut().expect("no function under construction")
    }

    /// Create a fresh local of the given type, tracking its type for later reads.
    fn new_local(&mut self, ty: TyId) -> LocalId {
        let l = self.fb().local(ty);
        debug_assert_eq!(l.index(), self.local_tys.len());
        self.local_tys.push(ty);
        l
    }

    fn local_ty(&self, l: LocalId) -> TyId {
        self.local_tys[l.index()]
    }

    /// Create a new block, make it current, and return its id.
    fn new_block(&mut self) -> BlockId {
        let b = self.fb().block();
        self.cur = b;
        self.done = false;
        b
    }

    /// Reserve an empty block (its terminator filled in later) without changing the logical
    /// current block. The builder's cursor is restored to `self.cur` afterwards.
    fn reserve_block(&mut self) -> BlockId {
        let saved = self.cur;
        let b = self.fb().block();
        self.fb().at(saved);
        b
    }

    /// Select an existing block as current, clearing the terminated flag.
    fn select(&mut self, b: BlockId) {
        self.fb().at(b);
        self.cur = b;
        self.done = false;
    }

    // Terminator helpers for the *current* block (no-op if already terminated).
    fn term_goto(&mut self, to: BlockId) {
        if !self.done {
            self.fb().goto(to);
            self.done = true;
        }
    }

    fn term_ret(&mut self, vals: Vec<Operand>) {
        if !self.done {
            self.fb().ret(vals);
            self.done = true;
        }
    }

    fn term_panic(&mut self, msg: Operand) {
        if !self.done {
            self.fb().panic(msg);
            self.done = true;
        }
    }

    // Terminator helpers for an explicitly named (reserved) block. These leave the builder
    // cursor on `from`; callers `select` the continuation afterwards.
    fn set_goto(&mut self, from: BlockId, to: BlockId) {
        self.fb().at(from);
        self.fb().goto(to);
    }

    fn set_branch(&mut self, from: BlockId, cond: Operand, then_bb: BlockId, else_bb: BlockId) {
        self.fb().at(from);
        self.fb().branch(cond, then_bb, else_bb);
    }

    fn set_switch(
        &mut self,
        from: BlockId,
        scrutinee: Operand,
        cases: Vec<(u64, BlockId)>,
        default: BlockId,
    ) {
        self.fb().at(from);
        self.fb().switch(scrutinee, cases, default);
    }

    fn set_holla(
        &mut self,
        from: BlockId,
        tag: Operand,
        crib: Operand,
        resolved: Place,
        live: BlockId,
        ghosted: BlockId,
    ) {
        self.fb().at(from);
        self.fb().holla_check(tag, crib, resolved, live, ghosted);
    }

    // === name resolution & misc ==============================================

    fn bind(&mut self, name: &str, l: LocalId) {
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .insert(name.to_string(), l);
    }

    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lookup_const(&self, name: &str) -> Option<ConstVal> {
        self.local_consts
            .get(name)
            .or_else(|| self.globals.get(name))
            .cloned()
    }

    /// Resolve a variant constructor name to `(sum, index)`, preferring a sum-typed hint and
    /// falling back to a unique global match.
    fn resolve_variant(&self, name: &str, hint: Option<TyId>) -> Result<(SumId, u32), String> {
        let candidates = self
            .variants
            .get(name)
            .ok_or_else(|| format!("unknown variant `{name}`"))?;
        if let Some(t) = hint
            && let TyKind::Sum(sid) = self.m.ty(t)
            && let Some(&(s, v)) = candidates.iter().find(|(s, _)| s == sid)
        {
            return Ok((s, v));
        }
        match candidates.as_slice() {
            [single] => Ok(*single),
            _ => Err(format!(
                "ambiguous variant `{name}` — annotate the expected sum type"
            )),
        }
    }

    fn variant_index(&self, sid: SumId, name: &str) -> Result<u32, String> {
        self.m
            .sum_def(sid)
            .variants
            .iter()
            .position(|v| v.name == name)
            .map(|i| i as u32)
            .ok_or_else(|| format!("sum `{}` has no variant `{name}`", self.m.sum_def(sid).name))
    }

    /// The overflow mode for an integer binop over `ty` (amendment §2.4). Signed integer
    /// arithmetic traps; unsigned wraps; float/bitwise/shift/comparison carry `Na`.
    fn arith_mode(&self, ty: TyId, op: BinOp) -> ArithMode {
        let is_arith = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
        );
        if !is_arith {
            return ArithMode::Na;
        }
        match self.m.ty(ty) {
            TyKind::Int { signed: true, .. } => ArithMode::Trap,
            TyKind::Int { signed: false, .. } => ArithMode::Wrap,
            _ => ArithMode::Na,
        }
    }

    fn rets_to_ty(&mut self, rets: &[TyId]) -> TyId {
        match rets {
            [] => self.m.t_void(),
            [one] => *one,
            many => self.m.intern_ty(TyKind::Tuple(many.to_vec())),
        }
    }

    /// The type of a place, walking its projections (mirrors the validator's rules).
    fn place_ty(&self, place: &Place) -> Result<TyId, String> {
        let mut ty = self.local_ty(place.local);
        let mut pending: Option<(SumId, u32)> = None;
        for proj in &place.proj {
            match proj {
                Proj::Field(i) => {
                    if let Some((sid, v)) = pending.take() {
                        let variant = &self.m.sum_def(sid).variants[v as usize];
                        ty = *variant
                            .payload
                            .get(*i as usize)
                            .ok_or_else(|| "bad variant payload index".to_string())?;
                    } else {
                        ty = match self.m.ty(ty) {
                            TyKind::Struct(s) => self.m.struct_def(*s).fields[*i as usize].ty,
                            TyKind::Tuple(es) => es[*i as usize],
                            other => return Err(format!("field of non-aggregate {other:?}")),
                        };
                    }
                }
                Proj::Deref => {
                    ty = match self.m.ty(ty) {
                        TyKind::Ref(e) => *e,
                        other => return Err(format!("deref of {other:?}")),
                    };
                }
                Proj::Index(_) => {
                    ty = match self.m.ty(ty) {
                        TyKind::Slice(e) | TyKind::Array(e, _) => *e,
                        // A `soa` container indexes to its element type; the layout is a
                        // backend concern, the element type is the inner container's element.
                        TyKind::Soa(inner) => match self.m.ty(*inner) {
                            TyKind::Slice(e) | TyKind::Array(e, _) | TyKind::Vec(e) => *e,
                            other => return Err(format!("soa index of {other:?}")),
                        },
                        other => return Err(format!("index of {other:?}")),
                    };
                }
                Proj::Downcast(v) => match self.m.ty(ty) {
                    TyKind::Sum(s) => pending = Some((*s, *v)),
                    other => return Err(format!("downcast of {other:?}")),
                },
            }
        }
        Ok(ty)
    }

    /// The place underlying a `copy`/`move` operand, if any.
    fn operand_place(&self, op: Operand) -> Option<Place> {
        match op {
            Operand::Copy(p) | Operand::Move(p) => Some(p),
            Operand::Const(_) => None,
        }
    }

    /// Get (or create) a deduped extern with the given name, param, and return types.
    fn get_extern(&mut self, name: &str, params: Vec<TyId>, rets: Vec<TyId>) -> ExternId {
        let key = (name.to_string(), params.clone(), rets.clone());
        if let Some(&id) = self.extern_cache.get(&key) {
            return id;
        }
        let id = self.m.add_extern(Extern {
            name: name.to_string(),
            abi: "C".into(),
            sig: Sig { params, rets },
        });
        self.extern_cache.insert(key, id);
        id
    }
}

/// One piece of a split `spill.f` format string.
enum FmtSeg {
    /// A literal run of text between placeholders (`{{`/`}}` already unescaped).
    Text(String),
    /// A `{}` placeholder consuming the next argument.
    Hole,
}

/// Split a `spill.f` format string into literal-text and placeholder segments, mirroring the
/// interpreter's `format_str`: `{}` is a hole, `{{`/`}}` are literal braces, and a lone `{` or
/// `}` is an error. The result always alternates `Text` (possibly empty) around each `Hole`.
fn split_format(fmt: &str) -> Result<Vec<FmtSeg>, String> {
    let mut segs = Vec::new();
    let mut text = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => match chars.peek() {
                Some('{') => {
                    chars.next();
                    text.push('{');
                }
                Some('}') => {
                    chars.next();
                    segs.push(FmtSeg::Text(std::mem::take(&mut text)));
                    segs.push(FmtSeg::Hole);
                }
                _ => return Err("`spill.f`: lone `{` in format string".into()),
            },
            '}' => match chars.peek() {
                Some('}') => {
                    chars.next();
                    text.push('}');
                }
                _ => return Err("`spill.f`: lone `}` in format string".into()),
            },
            other => text.push(other),
        }
    }
    segs.push(FmtSeg::Text(text));
    Ok(segs)
}

/// Extend a place with one more projection step (a pure value operation).
fn extend(base: &Place, p: Proj) -> Place {
    let mut place = base.clone();
    place.proj.push(p);
    place
}

/// A collection receiver for the shared higher-order methods (`vibeCheck`/`glowUp`): either a
/// fixed-size array (indexed by `[i]` against a place) or a `vec` (a runtime handle indexed via
/// `bet_vec_get`). Lets `build_map`/`build_filter` run one loop over either receiver.
enum CollectionSrc {
    Array { base: Place, n: u64 },
    Vec { handle: Operand },
}

/// Recognize a first-class SIMD vector type name: the `vec2`/`vec3`/`vec4` float aliases, or the
/// generic `<elem>x<N>` form (`f32x4`, `i64x2`, …). Returns `(element type name, lane count)`, or
/// `None` if `name` isn't a vector type. Lane counts are 2–4 in v1; the element must be a scalar
/// numeric type name. Resolved as a plain type name (no lexer keyword), like `rng`.
fn simd_type_name(name: &str) -> Option<(&str, u32)> {
    match name {
        "vec2" => Some(("f32", 2)),
        "vec3" => Some(("f32", 3)),
        "vec4" => Some(("f32", 4)),
        _ => {
            let idx = name.rfind('x')?;
            if idx == 0 {
                return None;
            }
            let (elem, rest) = (&name[..idx], &name[idx + 1..]);
            let lanes: u32 = rest.parse().ok()?;
            if !(2..=4).contains(&lanes) {
                return None;
            }
            match elem {
                "f32" | "f64" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
                | "int" | "uint" => Some((elem, lanes)),
                _ => None,
            }
        }
    }
}

/// Map a non-short-circuit surface binary operator to its IR counterpart.
fn map_binop(op: ast::BinOp) -> BinOp {
    match op {
        ast::BinOp::Eq => BinOp::Eq,
        ast::BinOp::Ne => BinOp::Ne,
        ast::BinOp::Lt => BinOp::Lt,
        ast::BinOp::Le => BinOp::Le,
        ast::BinOp::Gt => BinOp::Gt,
        ast::BinOp::Ge => BinOp::Ge,
        ast::BinOp::BitOr => BinOp::BitOr,
        ast::BinOp::BitXor => BinOp::BitXor,
        ast::BinOp::BitAnd => BinOp::BitAnd,
        ast::BinOp::Shl => BinOp::Shl,
        ast::BinOp::Shr => BinOp::Shr,
        ast::BinOp::Add => BinOp::Add,
        ast::BinOp::Sub => BinOp::Sub,
        ast::BinOp::Mul => BinOp::Mul,
        ast::BinOp::Div => BinOp::Div,
        ast::BinOp::Rem => BinOp::Rem,
        // `And`/`Or` never reach here — they lower to control flow.
        ast::BinOp::And | ast::BinOp::Or => unreachable!("logical ops lower to branches"),
    }
}

/// The IR binary operator underlying a compound-assignment operator.
fn compound_binop(op: ast::AssignOp) -> BinOp {
    match op {
        ast::AssignOp::AddEq => BinOp::Add,
        ast::AssignOp::SubEq => BinOp::Sub,
        ast::AssignOp::MulEq => BinOp::Mul,
        ast::AssignOp::DivEq => BinOp::Div,
        ast::AssignOp::RemEq => BinOp::Rem,
        ast::AssignOp::AndEq => BinOp::BitAnd,
        ast::AssignOp::OrEq => BinOp::BitOr,
        ast::AssignOp::XorEq => BinOp::BitXor,
        ast::AssignOp::ShlEq => BinOp::Shl,
        ast::AssignOp::ShrEq => BinOp::Shr,
        // `Eq` is handled before this is called.
        ast::AssignOp::Eq => unreachable!("plain `=` is not a compound op"),
    }
}
