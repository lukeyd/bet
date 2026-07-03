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

/// A resolved `facts` constant: the literal value plus its interned type.
#[derive(Clone)]
struct ConstVal {
    value: Const,
    ty: TyId,
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
    /// `bet_print(rawptr, u64) -> void` — the stdout entry point (always present).
    print_extern: ExternId,
    /// Deduped externs synthesized on demand, keyed by `(name, ret-type)`.
    extern_cache: HashMap<(String, TyId), ExternId>,

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
        // 1a. Assign struct/sum ids by appearance order.
        let (mut sn, mut un) = (0u32, 0u32);
        for item in &prog.items {
            match item {
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
                        // A generic `drip` has no monomorphic layout; register a placeholder so
                        // the id count stays aligned, but building a value of it will error.
                        self.m.add_struct(StructDef {
                            name: d.name.clone(),
                            fields: Vec::new(),
                        });
                        continue;
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

        // 3. Function signatures (id by function-item order — the same order lowering adds).
        let mut fid = 0u32;
        for item in &prog.items {
            if let Item::Func(f) = item {
                let sig = self.collect_fn_sig(f, FuncId(fid));
                self.funcs.insert(f.name.clone(), sig);
                fid += 1;
            }
        }

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
                if !generics.is_empty() {
                    return Err(format!(
                        "generic type instantiation `{name}[..]` is not yet lowered (needs monomorphization)"
                    ));
                }
                self.named_type(name)
            }
        }
    }

    fn named_type(&mut self, name: &str) -> Result<TyId, String> {
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
            _ => {
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
        // Pass 2: lower function bodies.
        for item in &prog.items {
            match item {
                Item::Pull(_) | Item::Extern(_) | Item::Drip(_) | Item::Moods(_) => {}
                // Module-level facts already collected; nothing to emit (inlined at use).
                Item::Const(_) => {}
                Item::Crib(_) => {} // registered in pass 1
                Item::Func(f) => self.lower_func(f)?,
                Item::Var(_) => {
                    return Err("module-level `lowkey` is not yet lowered".into());
                }
            }
        }
        Ok(())
    }

    fn lower_func(&mut self, f: &ast::FnDecl) -> Result<(), String> {
        if !f.generics.is_empty() {
            return Err(format!(
                "generic function `{}` is not yet lowered (needs monomorphization)",
                f.name
            ));
        }
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

        // Fresh per-function state.
        let fb = FuncBuilder::new(f.name.clone(), sig.params.clone(), sig.rets.clone());
        self.fb = Some(fb);
        self.local_tys = sig.params.clone();
        self.scopes = vec![HashMap::new()];
        self.local_consts = HashMap::new();
        self.loops = Vec::new();
        self.cur_rets = sig.rets.clone();

        // The entry block, then param bindings (receiver first, if any).
        let entry = self.new_block();
        debug_assert_eq!(entry, BlockId(0));
        let mut pi = 0usize;
        if let Some(r) = &f.receiver {
            self.bind(&r.name, LocalId(pi as u32));
            pi += 1;
        }
        for p in &f.params {
            self.bind(&p.name, LocalId(pi as u32));
            pi += 1;
        }

        self.lower_block(&f.body)?;

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
            StmtKind::Evict(e) => {
                let (op, _) = self.lower_expr(e, None)?;
                self.fb().evict(op);
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
            StmtKind::Slide(_) => Err("`slide` (task spawn) is not yet lowered".into()),
            StmtKind::Bounce(_) => {
                Err("`bounce` (error early-return sugar) is not yet lowered".into())
            }
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
            let ty = decl_ty.unwrap_or(vty);
            let l = self.new_local(ty);
            self.fb().assign(Place::local(l), Rvalue::Use(op));
            self.bind(name, l);
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
            ops.push(op);
        }
        self.term_ret(ops);
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
        let place = self.lower_place(&targets[0])?;
        let pty = self.place_ty(&place)?;
        if op == ast::AssignOp::Eq {
            let (val, _) = self.lower_expr(&values[0], Some(pty))?;
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
        let sid = match self.m.ty(sty) {
            TyKind::Sum(s) => *s,
            other => return Err(format!("`vibe` scrutinee is not a sum type ({other:?})")),
        };
        let sum_place = self
            .operand_place(sop.clone())
            .ok_or_else(|| "`vibe` scrutinee must be addressable".to_string())?;

        // discriminant(scrutinee) → a u32 tag we can switch on.
        let disc_ty = self.m.t_int(IntWidth::W32, false);
        let disc = self.new_local(disc_ty);
        self.fb()
            .assign(Place::local(disc), Rvalue::Discriminant(sop));
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
            } => self.lower_method(receiver, method, generics, args),
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

        let irop = map_binop(op);
        // Comparisons produce bool but take operands of the compared type; propagate no hint
        // into a comparison's operands, but do propagate for value-producing ops.
        let (lo, lty) = if irop.is_comparison() {
            self.lower_expr(l, None)?
        } else {
            self.lower_expr(l, hint)?
        };
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
        let (bop, bty) = self.lower_expr(base, None)?;
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
        let base_place = self
            .operand_place(bop)
            .ok_or_else(|| "indexing requires an addressable base".to_string())?;
        let elem = match self.m.ty(bty) {
            TyKind::Array(e, _) | TyKind::Slice(e) => *e,
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
        let aty = self.m.intern_ty(TyKind::Array(elem, elems.len() as u64));
        let tmp = self.new_local(aty);
        self.fb().assign(
            Place::local(tmp),
            Rvalue::Aggregate(AggKind::Array(elem), ops),
        );
        Ok((Operand::Copy(Place::local(tmp)), aty))
    }

    /// `squad x in xs { .. }` — for-each over a fixed array. Lowered to a counter loop
    /// `for i in 0..N { x = xs[i]; body }`; the increment block is the `skip`/continue target.
    fn lower_squad(&mut self, var: &str, iter: &Expr, body: &ast::Block) -> Result<(), String> {
        let (iop, ity) = self.lower_expr(iter, None)?;
        let (elem, n) = match self.m.ty(ity) {
            TyKind::Array(e, n) => (*e, *n),
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
    ) -> Result<(Operand, TyId), String> {
        // Stdlib module intrinsics (`math.lap`, ...) when the receiver is a bare module name.
        if let ExprKind::Name { name, .. } = &receiver.kind
            && self.lookup_local(name).is_none()
            && !self.funcs.contains_key(name)
            && self.is_module(name)
        {
            return self.lower_intrinsic(name, method, args);
        }
        if !generics.is_empty() {
            return Err("generic method calls are not yet lowered".into());
        }
        // A user method: the receiver becomes the leading argument.
        let sig = self
            .funcs
            .get(method)
            .cloned()
            .ok_or_else(|| format!("unknown method `{method}`"))?;
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
        self.emit_call(sig.id, &sig.rets, call_args)
    }

    fn is_module(&self, name: &str) -> bool {
        matches!(name, "spill" | "str" | "math" | "mem" | "bytes" | "fmt")
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
            ("spill", _) => {
                Err("`spill.*` is a statement-level print, not a value expression".into())
            }
            _ => Err(format!(
                "stdlib intrinsic `{module}.{method}` is not yet lowered"
            )),
        }
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
                return Err(format!("generic call `{name}[..]` is not yet lowered"));
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
        if !lit.generics.is_empty() {
            return Err(format!(
                "generic struct literal `{}[..]` is not yet lowered",
                lit.name
            ));
        }
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
        // Build operands in declaration order, matching each field by name.
        let mut ops = Vec::with_capacity(field_tys.len());
        for (fname, fty) in &field_tys {
            let init = lit
                .fields
                .iter()
                .find(|fi| &fi.name == fname)
                .ok_or_else(|| {
                    format!("struct literal `{}` is missing field `{fname}`", lit.name)
                })?;
            let (op, _) = self.lower_expr(&init.value, Some(*fty))?;
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
                let mut fields = Vec::with_capacity(field_tys.len());
                for (idx, (fname, fty)) in field_tys.iter().enumerate() {
                    let fi = lit
                        .fields
                        .iter()
                        .find(|fi| &fi.name == fname)
                        .ok_or_else(|| format!("`cop {}` is missing field `{fname}`", lit.name))?;
                    let (op, _) = self.lower_expr(&fi.value, Some(*fty))?;
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
        let ret_key = *rets.first().unwrap_or(&TyId(u32::MAX));
        if let Some(&id) = self.extern_cache.get(&(name.to_string(), ret_key)) {
            return id;
        }
        let id = self.m.add_extern(Extern {
            name: name.to_string(),
            abi: "C".into(),
            sig: Sig { params, rets },
        });
        self.extern_cache.insert((name.to_string(), ret_key), id);
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
