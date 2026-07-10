//! Well-formedness checking for a [`Module`].
//!
//! [`validate`] returns every problem it finds (not just the first). Structural checks
//! (block targets, return arity, place projections, aggregate/call/cop shapes) are always
//! applied; type checks are **best-effort** — where an operand's type can't be determined
//! locally (notably `ghosted`, whose type is contextual), the equality check is skipped
//! rather than reported as an error.

use crate::ir::*;
use std::collections::HashMap;

/// A single well-formedness problem.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("function `{func}` has no blocks")]
    EmptyFunc { func: String },
    #[error("function `{func}`: entry block bb{entry} is out of range")]
    BadEntry { func: String, entry: u32 },
    #[error("function `{func}` bb{block}: control-flow target bb{target} is out of range")]
    BadTarget {
        func: String,
        block: u32,
        target: u32,
    },
    #[error("function `{func}` bb{block}: return has {got} value(s), expected {want}")]
    ReturnArity {
        func: String,
        block: u32,
        got: usize,
        want: usize,
    },
    #[error("function `{func}`: local %{local} is out of range")]
    BadLocal { func: String, local: u32 },
    #[error("function `{func}`: illegal projection — {detail}")]
    BadProjection { func: String, detail: String },
    #[error("function `{func}`: dangling id — {detail}")]
    BadId { func: String, detail: String },
    #[error("function `{func}`: arity/shape error — {detail}")]
    BadShape { func: String, detail: String },
    #[error("function `{func}`: type mismatch — {detail}")]
    TypeMismatch { func: String, detail: String },
    #[error(
        "function `{func}` bb{block}: downcast to variant #{variant} is not dominated by a \
         `Switch` on this value's discriminant selecting that variant (type-confusion risk)"
    )]
    DowncastUnguarded {
        func: String,
        block: u32,
        variant: u32,
    },
}

/// Validate a whole module. `Ok(())` means well-formed; otherwise every problem found.
pub fn validate(module: &Module) -> Result<(), Vec<ValidationError>> {
    let mut c = Checker {
        module,
        errors: Vec::new(),
    };
    for func in module.funcs() {
        c.check_func(func);
    }
    for g in module.globals() {
        c.check_global(g);
    }
    for e in module.externs() {
        c.check_extern(e);
    }
    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

struct Checker<'a> {
    module: &'a Module,
    errors: Vec<ValidationError>,
}

impl<'a> Checker<'a> {
    fn check_func(&mut self, func: &Func) {
        let name = func.name.clone();
        if func.blocks.is_empty() {
            self.errors.push(ValidationError::EmptyFunc { func: name });
            return;
        }
        let nblocks = func.blocks.len() as u32;
        if func.entry.0 >= nblocks {
            self.errors.push(ValidationError::BadEntry {
                func: name.clone(),
                entry: func.entry.0,
            });
        }

        for block in &func.blocks {
            for stmt in &block.stmts {
                self.check_stmt(func, block.id, stmt);
            }
            self.check_term(func, block.id, &block.term, nblocks);
        }

        // Defense-in-depth: every sum `Downcast` must be dominated by a discriminant guard.
        self.check_downcast_guards(func);
    }

    // --- sum-downcast guard analysis (cwage security issue #48) ---
    //
    // A `Proj::Downcast(v)` positions into a sum's payload and overlays the requested
    // variant's fields WITHOUT checking the runtime discriminant (the backend emits a raw
    // pointer-cast). That is sound only where control flow has already proven the
    // discriminant equals `v`. The frontend always guarantees this by dominating each
    // downcast with a `Switch` on `Discriminant(scrutinee)`, but hand-written IR (a `.mir`
    // fed through `compile_mir_source`) could emit a `Downcast` with no such guard and
    // silently type-confuse the payload. This pass rejects any downcast that is not
    // dominated by the matching discriminant test, so the guarantee no longer rests on the
    // frontend alone.
    //
    // Method: build a dominator tree over the function's CFG, resolve every `Switch` whose
    // scrutinee is a single-assignment discriminant temp back to the sum place it reads the
    // discriminant of, and require each `Downcast(v)` on place `P` to be reachable only
    // through the case-`v` edge of a `Switch` on `Discriminant(P)`.
    //
    // Residual gap (best-effort, matching the rest of this file): we do not track a
    // reassignment of the *scrutinee sum place itself* between the guarding switch and the
    // downcast. The frontend never does that — it reads every arm payload as the first
    // statements of the arm block — and closing it would require full place-granular
    // reaching-definitions. Everything else (a missing guard, the wrong variant, a
    // discriminant temp that is overwritten before the switch, an unguarded default edge) is
    // rejected.
    fn check_downcast_guards(&mut self, func: &Func) {
        let sites = collect_downcast_sites(func);
        if sites.is_empty() {
            return;
        }
        let n = func.blocks.len();
        let entry = func.entry.index();
        if entry >= n {
            // Entry is out of range — already reported as `BadEntry`; can't analyze the CFG.
            return;
        }
        let preds = compute_preds(func);
        let doms = compute_dominators(n, entry, &preds);
        let guards = collect_switch_guards(func, &doms);

        for site in &sites {
            if !site_is_guarded(site, &guards, &doms, &preds) {
                self.errors.push(ValidationError::DowncastUnguarded {
                    func: func.name.clone(),
                    block: site.block as u32,
                    variant: site.variant,
                });
            }
        }
    }

    // --- statements ---

    fn check_stmt(&mut self, func: &Func, block: BlockId, stmt: &Stmt) {
        match stmt {
            Stmt::Nop => {}
            Stmt::Evict(op) => {
                self.operand_kind(func, op);
            }
            Stmt::EvictSlot { crib, tag } => {
                self.operand_kind(func, crib);
                self.operand_kind(func, tag);
            }
            Stmt::Assign(place, rvalue) => {
                let place_kind = self.place_kind(func, place);
                let rv_kind = self.rvalue_kind(func, block, rvalue);
                if let (Some(pk), Some(rk)) = (place_kind, rv_kind)
                    && pk != rk
                {
                    self.errors.push(ValidationError::TypeMismatch {
                        func: func.name.clone(),
                        detail: format!("assigning {rk:?} into a place of type {pk:?}"),
                    });
                }
            }
        }
    }

    // --- terminators ---

    fn check_term(&mut self, func: &Func, block: BlockId, term: &Terminator, nblocks: u32) {
        let target = |c: &mut Self, bb: BlockId| {
            if bb.0 >= nblocks {
                c.errors.push(ValidationError::BadTarget {
                    func: func.name.clone(),
                    block: block.0,
                    target: bb.0,
                });
            }
        };
        match term {
            Terminator::Goto(bb) => target(self, *bb),
            Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => {
                self.operand_kind(func, cond);
                target(self, *then_bb);
                target(self, *else_bb);
            }
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                self.operand_kind(func, scrutinee);
                for (_, bb) in cases {
                    target(self, *bb);
                }
                target(self, *default);
            }
            Terminator::HollaCheck {
                tag,
                crib,
                resolved,
                live,
                ghosted,
            } => {
                let tag_kind = self.operand_kind(func, tag);
                let crib_kind = self.operand_kind(func, crib);
                let resolved_kind = self.place_kind(func, resolved);
                target(self, *live);
                target(self, *ghosted);
                // crib: crib T; tag: tag T; resolved: ref T — all the same element T.
                if let (Some(TyKind::Crib(ce)), Some(TyKind::Tag(te))) = (&crib_kind, &tag_kind)
                    && ce != te
                {
                    self.errors.push(ValidationError::TypeMismatch {
                        func: func.name.clone(),
                        detail: "holla tag element does not match crib element".into(),
                    });
                }
                if let (Some(TyKind::Crib(ce)), Some(TyKind::Ref(re))) =
                    (&crib_kind, &resolved_kind)
                    && ce != re
                {
                    self.errors.push(ValidationError::TypeMismatch {
                        func: func.name.clone(),
                        detail: "holla resolved ref does not match crib element".into(),
                    });
                }
            }
            Terminator::Return(vals) => {
                if vals.len() != func.rets.len() {
                    self.errors.push(ValidationError::ReturnArity {
                        func: func.name.clone(),
                        block: block.0,
                        got: vals.len(),
                        want: func.rets.len(),
                    });
                }
                for (op, &ret_ty) in vals.iter().zip(&func.rets) {
                    if let Some(k) = self.operand_kind(func, op) {
                        let want = self.module.ty(ret_ty).clone();
                        if k != want {
                            self.errors.push(ValidationError::TypeMismatch {
                                func: func.name.clone(),
                                detail: format!("returning {k:?} where {want:?} was declared"),
                            });
                        }
                    }
                }
            }
            Terminator::Panic(op) => {
                self.operand_kind(func, op);
            }
            Terminator::Unreachable => {}
        }
    }

    // --- rvalue typing (records shape errors; returns the result kind when known) ---

    fn rvalue_kind(&mut self, func: &Func, block: BlockId, rv: &Rvalue) -> Option<TyKind> {
        match rv {
            Rvalue::Use(op) => self.operand_kind(func, op),
            Rvalue::BinOp(op, a, b, mode) => {
                let ka = self.operand_kind(func, a);
                let kb = self.operand_kind(func, b);
                if let (Some(ka), Some(kb)) = (&ka, &kb)
                    && ka != kb
                    && !op.is_comparison()
                {
                    self.errors.push(ValidationError::TypeMismatch {
                        func: func.name.clone(),
                        detail: format!("binary op on mismatched operands {ka:?} and {kb:?}"),
                    });
                }
                self.check_arith_mode(func, *op, *mode, ka.as_ref());
                if op.is_comparison() {
                    Some(TyKind::Bool)
                } else {
                    ka
                }
            }
            Rvalue::UnOp(op, a) => {
                let ka = self.operand_kind(func, a);
                match op {
                    UnOp::Not => Some(TyKind::Bool),
                    UnOp::Neg | UnOp::BitNot => ka,
                }
            }
            Rvalue::Cast(op, ty, kind) => {
                let src = self.operand_kind(func, op);
                let target = self.module.ty(*ty).clone();
                self.check_cast(func, src.as_ref(), &target, *kind);
                Some(target)
            }
            Rvalue::Call(callee, args) => self.call_kind(func, callee, args),
            Rvalue::Aggregate(kind, ops) => {
                for op in ops {
                    self.operand_kind(func, op);
                }
                match kind {
                    AggKind::Struct(sid) => {
                        if let Some(def) = self.struct_def(func, *sid)
                            && def.fields.len() != ops.len()
                        {
                            self.errors.push(ValidationError::BadShape {
                                func: func.name.clone(),
                                detail: format!(
                                    "struct `{}` takes {} field(s), got {}",
                                    def.name,
                                    def.fields.len(),
                                    ops.len()
                                ),
                            });
                        }
                        Some(TyKind::Struct(*sid))
                    }
                    AggKind::Tuple => None,
                    AggKind::Array(elem) => Some(TyKind::Array(*elem, ops.len() as u64)),
                    AggKind::Simd(elem) => Some(TyKind::Simd {
                        elem: *elem,
                        lanes: ops.len() as u32,
                    }),
                    AggKind::Sum { sum, variant } => {
                        if let Some(def) = self.sum_def(func, *sum) {
                            match def.variants.get(*variant as usize) {
                                None => self.errors.push(ValidationError::BadId {
                                    func: func.name.clone(),
                                    detail: format!("sum `{}` has no variant #{variant}", def.name),
                                }),
                                Some(v) if v.payload.len() != ops.len() => {
                                    self.errors.push(ValidationError::BadShape {
                                        func: func.name.clone(),
                                        detail: format!(
                                            "variant `{}::{}` takes {} payload(s), got {}",
                                            def.name,
                                            v.name,
                                            v.payload.len(),
                                            ops.len()
                                        ),
                                    });
                                }
                                Some(_) => {}
                            }
                        }
                        Some(TyKind::Sum(*sum))
                    }
                }
            }
            Rvalue::Simd { op, args, ty } => {
                for a in args {
                    self.operand_kind(func, a);
                }
                let want = match op {
                    SimdOp::Splat
                    | SimdOp::Abs
                    | SimdOp::Lane(_)
                    | SimdOp::Sum
                    | SimdOp::Length
                    | SimdOp::Norm => 1,
                    SimdOp::Min | SimdOp::Max | SimdOp::Dot => 2,
                };
                if args.len() != want {
                    self.errors.push(ValidationError::BadShape {
                        func: func.name.clone(),
                        detail: format!("simd op {op:?} takes {want} arg(s), got {}", args.len()),
                    });
                }
                Some(self.module.ty(*ty).clone())
            }
            Rvalue::Discriminant(op) => {
                self.operand_kind(func, op);
                None
            }
            Rvalue::Cop(crib, init) => {
                let crib_kind = self.operand_kind(func, crib);
                self.check_cop_init(func, init);
                let _ = block;
                match crib_kind {
                    Some(TyKind::Crib(elem)) => {
                        if matches!(self.module.ty(elem), TyKind::Void) {
                            // Bump crib: `cop` yields a live `ref` to the freshly allocated
                            // aggregate (the backend returns the raw storage pointer).
                            let aggregate = match init {
                                CopInit::StructLit(sid, _) => TyKind::Struct(*sid),
                                CopInit::SumVariant(sum, _, _) => TyKind::Sum(*sum),
                            };
                            self.find_ty(&aggregate)
                                .map(TyKind::Ref)
                                .or(Some(TyKind::RawPtr))
                        } else {
                            Some(TyKind::Tag(elem))
                        }
                    }
                    _ => None,
                }
            }
            Rvalue::Trust(crib, tag) => {
                let crib_kind = self.operand_kind(func, crib);
                self.operand_kind(func, tag);
                match crib_kind {
                    Some(TyKind::Crib(elem)) => Some(TyKind::Ref(elem)),
                    _ => None,
                }
            }
            Rvalue::StrPtr(op) => {
                self.expect_str(func, op);
                Some(TyKind::RawPtr)
            }
            Rvalue::StrLen(op) => {
                self.expect_str(func, op);
                Some(TyKind::Int {
                    width: IntWidth::W64,
                    signed: false,
                })
            }
            Rvalue::SlicePtr(op) => {
                self.expect_slice(func, op);
                Some(TyKind::RawPtr)
            }
            Rvalue::SliceLen(op) => {
                self.expect_slice(func, op);
                Some(TyKind::Int {
                    width: IntWidth::W64,
                    signed: false,
                })
            }
            Rvalue::AddrOf(place) => {
                let _ = self.place_kind(func, place);
                Some(TyKind::RawPtr)
            }
            Rvalue::MakeSlice { data, len, elem } => {
                self.operand_kind(func, data);
                self.operand_kind(func, len);
                Some(TyKind::Slice(*elem))
            }
            Rvalue::CribNew { elem, .. } => Some(TyKind::Crib(*elem)),
            Rvalue::CribGlobal(id) => {
                let elem = self.module.crib_global(*id).elem;
                Some(TyKind::Crib(elem))
            }
            Rvalue::SizeOf(_) => Some(TyKind::Int {
                width: IntWidth::W64,
                signed: false,
            }),
            Rvalue::MakeStr { data, len } => {
                self.operand_kind(func, data);
                self.operand_kind(func, len);
                Some(TyKind::Str)
            }
        }
    }

    /// Best-effort: a `str` projection (`str_ptr`/`str_len`) requires a `str` operand. Skips
    /// the check when the operand's kind can't be determined locally (matching file policy).
    fn expect_str(&mut self, func: &Func, op: &Operand) {
        if let Some(k) = self.operand_kind(func, op)
            && k != TyKind::Str
        {
            self.errors.push(ValidationError::TypeMismatch {
                func: func.name.clone(),
                detail: format!("str projection on non-str operand {k:?}"),
            });
        }
    }

    /// Best-effort: a slice projection (`slice_ptr`/`slice_len`) requires a `[]T` operand.
    /// Skips the check when the operand's kind can't be determined locally.
    fn expect_slice(&mut self, func: &Func, op: &Operand) {
        if let Some(k) = self.operand_kind(func, op)
            && !matches!(k, TyKind::Slice(_))
        {
            self.errors.push(ValidationError::TypeMismatch {
                func: func.name.clone(),
                detail: format!("slice projection on non-slice operand {k:?}"),
            });
        }
    }

    fn call_kind(&mut self, func: &Func, callee: &Callee, args: &[Operand]) -> Option<TyKind> {
        // Resolve the callee to its (name, params, rets), cloning out of the shared module so
        // `check_call_args` can borrow `self` mutably. Bad-id callees still visit their args
        // (so a bad local passed to a dangling call is still reported).
        let (name, params, rets): (String, Vec<TyId>, Vec<TyId>) = match callee {
            Callee::Direct(fid) => {
                let Some(target) = self.module.funcs().get(fid.index()) else {
                    self.visit_args(func, args);
                    self.errors.push(ValidationError::BadId {
                        func: func.name.clone(),
                        detail: format!("call to function #{}", fid.0),
                    });
                    return None;
                };
                (
                    target.name.clone(),
                    target.params.clone(),
                    target.rets.clone(),
                )
            }
            Callee::Extern(eid) => {
                let Some(target) = self.module.externs().get(eid.index()) else {
                    self.visit_args(func, args);
                    self.errors.push(ValidationError::BadId {
                        func: func.name.clone(),
                        detail: format!("call to extern #{}", eid.0),
                    });
                    return None;
                };
                (
                    target.name.clone(),
                    target.sig.params.clone(),
                    target.sig.rets.clone(),
                )
            }
            Callee::Indirect(op) => match self.operand_kind(func, op) {
                Some(TyKind::FnPtr(sig)) => {
                    let s = self.module.sig(sig).clone();
                    ("<indirect>".to_string(), s.params, s.rets)
                }
                _ => {
                    self.visit_args(func, args);
                    return None;
                }
            },
        };
        self.check_call_args(func, &name, &params, args);
        Some(self.rets_to_kind(&rets))
    }

    /// Visit each argument operand for its own well-formedness (used when the callee is
    /// unresolvable, so bad locals in the argument list are still reported).
    fn visit_args(&mut self, func: &Func, args: &[Operand]) {
        for op in args {
            self.operand_kind(func, op);
        }
    }

    /// Arity + best-effort per-argument type checking, uniform across direct/extern/indirect
    /// calls. Visits every argument exactly once (so it subsumes [`Self::visit_args`]).
    fn check_call_args(
        &mut self,
        func: &Func,
        callee_name: &str,
        params: &[TyId],
        args: &[Operand],
    ) {
        if params.len() != args.len() {
            self.errors.push(ValidationError::BadShape {
                func: func.name.clone(),
                detail: format!(
                    "call to `{callee_name}` passes {} arg(s), expected {}",
                    args.len(),
                    params.len()
                ),
            });
        }
        for (i, arg) in args.iter().enumerate() {
            let k = self.operand_kind(func, arg);
            if let (Some(k), Some(&pty)) = (k, params.get(i)) {
                let want = self.module.ty(pty).clone();
                if k != want {
                    self.errors.push(ValidationError::TypeMismatch {
                        func: func.name.clone(),
                        detail: format!(
                            "call to `{callee_name}` arg #{i}: passed {k:?}, expected {want:?}"
                        ),
                    });
                }
            }
        }
    }

    fn rets_to_kind(&self, rets: &[TyId]) -> TyKind {
        match rets {
            [] => TyKind::Void,
            [one] => self.module.ty(*one).clone(),
            many => TyKind::Tuple(many.to_vec()),
        }
    }

    /// Find the id of an already-interned type by structural kind (a linear scan; the validator
    /// is not hot). Used to name the `ref Struct`/`ref Sum` a bump `cop` produces.
    fn find_ty(&self, kind: &TyKind) -> Option<TyId> {
        self.module
            .types()
            .iter()
            .position(|k| k == kind)
            .map(|i| TyId(i as u32))
    }

    fn check_cop_init(&mut self, func: &Func, init: &CopInit) {
        match init {
            CopInit::StructLit(sid, fields) => {
                for (_, op) in fields {
                    self.operand_kind(func, op);
                }
                if let Some(def) = self.struct_def(func, *sid)
                    && def.fields.len() != fields.len()
                {
                    self.errors.push(ValidationError::BadShape {
                        func: func.name.clone(),
                        detail: format!(
                            "struct literal `{}` sets {} field(s), expected {}",
                            def.name,
                            fields.len(),
                            def.fields.len()
                        ),
                    });
                }
            }
            CopInit::SumVariant(sid, variant, ops) => {
                for op in ops {
                    self.operand_kind(func, op);
                }
                if let Some(def) = self.sum_def(func, *sid) {
                    match def.variants.get(*variant as usize) {
                        None => self.errors.push(ValidationError::BadId {
                            func: func.name.clone(),
                            detail: format!("sum `{}` has no variant #{variant}", def.name),
                        }),
                        Some(v) if v.payload.len() != ops.len() => {
                            self.errors.push(ValidationError::BadShape {
                                func: func.name.clone(),
                                detail: format!(
                                    "variant `{}::{}` takes {} payload(s), got {}",
                                    def.name,
                                    v.name,
                                    v.payload.len(),
                                    ops.len()
                                ),
                            });
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }

    // --- places & operands ---

    fn operand_kind(&mut self, func: &Func, op: &Operand) -> Option<TyKind> {
        match op {
            Operand::Const(c) => self.const_kind(c),
            Operand::Copy(p) | Operand::Move(p) => self.place_kind(func, p),
        }
    }

    fn const_kind(&mut self, c: &Const) -> Option<TyKind> {
        match c {
            Const::Int(_, ty) | Const::Float(_, ty) => Some(self.module.ty(*ty).clone()),
            Const::Bool(_) => Some(TyKind::Bool),
            Const::Str(_) => Some(TyKind::Str),
            // A function reference's type needs an interned signature we can't build from
            // a read-only module; skip its kind (still valid). `ghosted` and `nullptr` are
            // contextually typed (tag / any handle), so their kind is equally open.
            Const::FnRef(_) | Const::Ghosted | Const::NullPtr => None,
        }
    }

    /// The type of a place, checking every projection. Records an error and returns
    /// `None` if the local or a projection is ill-formed.
    fn place_kind(&mut self, func: &Func, place: &Place) -> Option<TyKind> {
        if place.local.index() >= func.locals.len() {
            self.errors.push(ValidationError::BadLocal {
                func: func.name.clone(),
                local: place.local.0,
            });
            return None;
        }
        let mut ty = self.module.ty(func.local_ty(place.local)).clone();
        let mut pending_variant: Option<(SumId, u32)> = None;

        for proj in &place.proj {
            match proj {
                Proj::Field(i) => {
                    ty = self.field_kind(func, &ty, pending_variant.take(), *i)?;
                }
                Proj::Index(idx) => {
                    self.operand_kind(func, idx);
                    ty = match ty {
                        TyKind::Slice(e) | TyKind::Array(e, _) => self.module.ty(e).clone(),
                        // A `soa` container indexes through to its element type; the transposed
                        // layout is a backend concern (this projection pair must be followed by
                        // a `Field` — the backend rejects a bare whole-element index).
                        TyKind::Soa(inner) => match self.module.ty(inner).clone() {
                            TyKind::Slice(e) | TyKind::Array(e, _) | TyKind::Vec(e) => {
                                self.module.ty(e).clone()
                            }
                            other => {
                                return self.bad_proj(func, format!("index into soa {other:?}"));
                            }
                        },
                        other => return self.bad_proj(func, format!("index into {other:?}")),
                    };
                }
                Proj::Deref => {
                    ty = match ty {
                        TyKind::Ref(e) => self.module.ty(e).clone(),
                        other => return self.bad_proj(func, format!("deref of {other:?}")),
                    };
                }
                Proj::Downcast(v) => match &ty {
                    TyKind::Sum(sid) => pending_variant = Some((*sid, *v)),
                    other => return self.bad_proj(func, format!("downcast of {other:?}")),
                },
            }
        }
        Some(ty)
    }

    fn field_kind(
        &mut self,
        func: &Func,
        ty: &TyKind,
        pending_variant: Option<(SumId, u32)>,
        i: u32,
    ) -> Option<TyKind> {
        if let Some((sid, v)) = pending_variant {
            let def = self.sum_def(func, sid)?;
            let variant = def.variants.get(v as usize).or_else(|| {
                self.errors.push(ValidationError::BadId {
                    func: func.name.clone(),
                    detail: format!("sum `{}` variant #{v}", def.name),
                });
                None
            })?;
            let field_ty = variant.payload.get(i as usize).copied().or_else(|| {
                self.errors.push(ValidationError::BadProjection {
                    func: func.name.clone(),
                    detail: format!(
                        "variant `{}::{}` has no payload #{i}",
                        def.name, variant.name
                    ),
                });
                None
            })?;
            return Some(self.module.ty(field_ty).clone());
        }
        match ty {
            TyKind::Struct(sid) => {
                let def = self.struct_def(func, *sid)?;
                match def.fields.get(i as usize) {
                    Some(f) => Some(self.module.ty(f.ty).clone()),
                    None => self.bad_proj(func, format!("struct `{}` has no field #{i}", def.name)),
                }
            }
            TyKind::Tuple(elems) => match elems.get(i as usize) {
                Some(&e) => Some(self.module.ty(e).clone()),
                None => self.bad_proj(func, format!("tuple has no element #{i}")),
            },
            // Field(j) on a `soa []Drip` / `soa vec[Drip]` bundle is the j-th per-field
            // sub-slice / vec handle (used to construct the bundle). Structural: `Slice(field_j)`
            // or `Vec(field_j)`, no interning required.
            TyKind::Soa(inner) => {
                let (sub_elem, is_vec) = match self.module.ty(*inner) {
                    TyKind::Slice(e) => (*e, false),
                    TyKind::Vec(e) => (*e, true),
                    other => return self.bad_proj(func, format!("field on soa {other:?}")),
                };
                let sid = match self.module.ty(sub_elem) {
                    TyKind::Struct(s) => *s,
                    other => return self.bad_proj(func, format!("soa field on {other:?}")),
                };
                let def = self.struct_def(func, sid)?;
                match def.fields.get(i as usize) {
                    Some(f) => Some(if is_vec {
                        TyKind::Vec(f.ty)
                    } else {
                        TyKind::Slice(f.ty)
                    }),
                    None => self.bad_proj(func, format!("soa struct has no field #{i}")),
                }
            }
            other => self.bad_proj(func, format!("field access on {other:?}")),
        }
    }

    fn bad_proj(&mut self, func: &Func, detail: String) -> Option<TyKind> {
        self.errors.push(ValidationError::BadProjection {
            func: func.name.clone(),
            detail,
        });
        None
    }

    fn struct_def(&mut self, func: &Func, sid: StructId) -> Option<&'a StructDef> {
        let module = self.module; // Copy the &'a Module out, detaching from `self`.
        if sid.index() >= module.structs().len() {
            self.errors.push(ValidationError::BadId {
                func: func.name.clone(),
                detail: format!("struct #{}", sid.0),
            });
            return None;
        }
        Some(module.struct_def(sid))
    }

    fn sum_def(&mut self, func: &Func, sid: SumId) -> Option<&'a SumDef> {
        let module = self.module;
        if sid.index() >= module.sums().len() {
            self.errors.push(ValidationError::BadId {
                func: func.name.clone(),
                detail: format!("sum #{}", sid.0),
            });
            return None;
        }
        Some(module.sum_def(sid))
    }

    // --- best-effort numeric policy (amendment §2.4; ir.rs ArithMode/CastKind docs) ---

    /// The overflow mode must match the operator and operand type: integer arithmetic carries
    /// `Wrap`/`Trap`, while bitwise/shift/comparison ops and float arithmetic carry `Na`.
    /// Skipped when the operand type isn't known locally.
    fn check_arith_mode(
        &mut self,
        func: &Func,
        op: BinOp,
        mode: ArithMode,
        operand: Option<&TyKind>,
    ) {
        let is_arith = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
        );
        if !is_arith {
            // comparisons, bitwise, and shifts always carry Na.
            if mode != ArithMode::Na {
                self.errors.push(ValidationError::TypeMismatch {
                    func: func.name.clone(),
                    detail: format!("{op:?} must use arith mode Na, found {mode:?}"),
                });
            }
            return;
        }
        match operand {
            Some(TyKind::Int { .. }) if mode == ArithMode::Na => {
                self.errors.push(ValidationError::TypeMismatch {
                    func: func.name.clone(),
                    detail: format!("integer {op:?} needs Wrap or Trap, found Na"),
                });
            }
            Some(TyKind::F32 | TyKind::F64) if mode != ArithMode::Na => {
                self.errors.push(ValidationError::TypeMismatch {
                    func: func.name.clone(),
                    detail: format!("float {op:?} must use Na, found {mode:?}"),
                });
            }
            _ => {}
        }
    }

    /// A cast's kind must be consistent with its source and target types (semantics §2).
    /// Skipped when the source type isn't known locally; `Bitcast` is unconstrained here.
    fn check_cast(&mut self, func: &Func, src: Option<&TyKind>, target: &TyKind, kind: CastKind) {
        let Some(src) = src else { return };
        let is_int = |t: &TyKind| matches!(t, TyKind::Int { .. });
        let is_float = |t: &TyKind| matches!(t, TyKind::F32 | TyKind::F64);
        let ok = match kind {
            CastKind::IntZext | CastKind::IntSext | CastKind::IntTrunc => {
                is_int(src) && is_int(target)
            }
            CastKind::IntToFloat => is_int(src) && is_float(target),
            CastKind::FloatToInt => is_float(src) && is_int(target),
            CastKind::FloatResize => is_float(src) && is_float(target),
            CastKind::Bitcast => true,
        };
        if !ok {
            self.errors.push(ValidationError::TypeMismatch {
                func: func.name.clone(),
                detail: format!("cast {kind:?} from {src:?} to {target:?} is ill-typed"),
            });
        }
    }

    // --- module-level declarations (globals & externs) ---

    /// A `facts` global's constant value must have its declared type.
    fn check_global(&mut self, g: &Global) {
        if let Some(k) = self.const_kind(&g.value) {
            let want = self.module.ty(g.ty).clone();
            if k != want {
                self.errors.push(ValidationError::TypeMismatch {
                    func: format!("<global {}>", g.name),
                    detail: format!("value of type {k:?} but declared {want:?}"),
                });
            }
        }
    }

    /// An `extern` signature must reference only in-range interned types.
    fn check_extern(&mut self, e: &Extern) {
        let n = self.module.types().len();
        for &t in e.sig.params.iter().chain(&e.sig.rets) {
            if t.index() >= n {
                self.errors.push(ValidationError::BadId {
                    func: format!("<extern {}>", e.name),
                    detail: format!("signature references out-of-range type #{}", t.0),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sum-downcast guard analysis (helpers for `Checker::check_downcast_guards`).
//
// These are free functions over a single `Func`'s CFG (block index == `BlockId`, per the
// `midir` invariant). They never touch `Module`, so the checker can push errors while they
// borrow `func` immutably. Block indices are used throughout; any control-flow target that
// is out of range is skipped (the structural pass already reports it as `BadTarget`).
// ---------------------------------------------------------------------------

/// A single `Proj::Downcast(v)` occurrence: the block it lives in, the sum place it overlays
/// (the projection prefix up to — but not including — the downcast), and the claimed variant.
struct DowncastSite {
    block: usize,
    prefix: Place,
    variant: u32,
}

/// A `Switch` resolved back to a discriminant guard: the block holding it, the sum place its
/// scrutinee reads the discriminant of, and its case/default targets (as block indices).
struct SwitchGuard {
    block: usize,
    sum_place: Place,
    cases: Vec<(u64, usize)>,
    default: usize,
}

/// The successor block ids of a terminator (empty for `Return`/`Panic`/`Unreachable`).
fn successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Goto(bb) => vec![*bb],
        Terminator::Branch {
            then_bb, else_bb, ..
        } => vec![*then_bb, *else_bb],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<BlockId> = cases.iter().map(|(_, bb)| *bb).collect();
            v.push(*default);
            v
        }
        Terminator::HollaCheck { live, ghosted, .. } => vec![*live, *ghosted],
        Terminator::Return(_) | Terminator::Panic(_) | Terminator::Unreachable => Vec::new(),
    }
}

/// Distinct predecessors of each block, derived from terminator successors.
fn compute_preds(func: &Func) -> Vec<Vec<usize>> {
    let n = func.blocks.len();
    let mut preds = vec![Vec::new(); n];
    for (i, block) in func.blocks.iter().enumerate() {
        for s in successors(&block.term) {
            let si = s.index();
            if si < n && !preds[si].contains(&i) {
                preds[si].push(i);
            }
        }
    }
    preds
}

/// A dominator matrix: `dom[b][a] == true` iff block `a` dominates block `b`. Standard
/// iterative set fixpoint (`dom(b) = {b} ∪ ⋂ dom(pred(b))`); the CFG is small so the O(n³)
/// worst case is irrelevant. Unreachable blocks (no predecessors) resolve to `{b}`, which is
/// harmless — nothing downstream relies on their dominators.
fn compute_dominators(n: usize, entry: usize, preds: &[Vec<usize>]) -> Vec<Vec<bool>> {
    let mut dom = vec![vec![true; n]; n];
    for (a, slot) in dom[entry].iter_mut().enumerate() {
        *slot = a == entry;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for b in 0..n {
            if b == entry {
                continue;
            }
            let mut new = vec![false; n];
            new[b] = true;
            if !preds[b].is_empty() {
                let mut inter = vec![true; n];
                for &p in &preds[b] {
                    for (slot, &pd) in inter.iter_mut().zip(dom[p].iter()) {
                        *slot = *slot && pd;
                    }
                }
                for (slot, &id) in new.iter_mut().zip(inter.iter()) {
                    *slot = *slot || id;
                }
            }
            if new != dom[b] {
                dom[b] = new;
                changed = true;
            }
        }
    }
    dom
}

/// Every `Downcast` in the function, with the block and prefix place it applies to. Walks all
/// places reachable from statements and terminators (including places nested inside `Index`
/// operands), so no downcast can hide from the guard check.
fn collect_downcast_sites(func: &Func) -> Vec<DowncastSite> {
    let mut out = Vec::new();
    for (i, block) in func.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Assign(place, rv) => {
                    sites_in_place(i, place, &mut out);
                    sites_in_rvalue(i, rv, &mut out);
                }
                Stmt::Evict(op) => sites_in_operand(i, op, &mut out),
                Stmt::EvictSlot { crib, tag } => {
                    sites_in_operand(i, crib, &mut out);
                    sites_in_operand(i, tag, &mut out);
                }
                Stmt::Nop => {}
            }
        }
        sites_in_term(i, &block.term, &mut out);
    }
    out
}

fn sites_in_place(block: usize, place: &Place, out: &mut Vec<DowncastSite>) {
    for (k, proj) in place.proj.iter().enumerate() {
        match proj {
            Proj::Downcast(v) => out.push(DowncastSite {
                block,
                prefix: Place {
                    local: place.local,
                    proj: place.proj[..k].to_vec(),
                },
                variant: *v,
            }),
            Proj::Index(op) => sites_in_operand(block, op, out),
            Proj::Field(_) | Proj::Deref => {}
        }
    }
}

fn sites_in_operand(block: usize, op: &Operand, out: &mut Vec<DowncastSite>) {
    match op {
        Operand::Copy(p) | Operand::Move(p) => sites_in_place(block, p, out),
        Operand::Const(_) => {}
    }
}

fn sites_in_rvalue(block: usize, rv: &Rvalue, out: &mut Vec<DowncastSite>) {
    match rv {
        Rvalue::Use(op)
        | Rvalue::UnOp(_, op)
        | Rvalue::Cast(op, _, _)
        | Rvalue::Discriminant(op)
        | Rvalue::StrPtr(op)
        | Rvalue::StrLen(op)
        | Rvalue::SlicePtr(op)
        | Rvalue::SliceLen(op) => sites_in_operand(block, op, out),
        Rvalue::BinOp(_, a, b, _) | Rvalue::Trust(a, b) => {
            sites_in_operand(block, a, out);
            sites_in_operand(block, b, out);
        }
        Rvalue::Call(callee, args) => {
            if let Callee::Indirect(op) = callee {
                sites_in_operand(block, op, out);
            }
            for a in args {
                sites_in_operand(block, a, out);
            }
        }
        Rvalue::Aggregate(_, ops) | Rvalue::Simd { args: ops, .. } => {
            for a in ops {
                sites_in_operand(block, a, out);
            }
        }
        Rvalue::Cop(crib, init) => {
            sites_in_operand(block, crib, out);
            match init {
                CopInit::StructLit(_, fields) => {
                    for (_, op) in fields {
                        sites_in_operand(block, op, out);
                    }
                }
                CopInit::SumVariant(_, _, ops) => {
                    for op in ops {
                        sites_in_operand(block, op, out);
                    }
                }
            }
        }
        Rvalue::AddrOf(place) => sites_in_place(block, place, out),
        Rvalue::MakeSlice { data, len, .. } | Rvalue::MakeStr { data, len } => {
            sites_in_operand(block, data, out);
            sites_in_operand(block, len, out);
        }
        Rvalue::CribNew { .. } | Rvalue::CribGlobal(_) | Rvalue::SizeOf(_) => {}
    }
}

fn sites_in_term(block: usize, term: &Terminator, out: &mut Vec<DowncastSite>) {
    match term {
        Terminator::Goto(_) | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => sites_in_operand(block, cond, out),
        Terminator::Switch { scrutinee, .. } => sites_in_operand(block, scrutinee, out),
        Terminator::HollaCheck {
            tag,
            crib,
            resolved,
            ..
        } => {
            sites_in_operand(block, tag, out);
            sites_in_operand(block, crib, out);
            sites_in_place(block, resolved, out);
        }
        Terminator::Return(vals) => {
            for op in vals {
                sites_in_operand(block, op, out);
            }
        }
        Terminator::Panic(op) => sites_in_operand(block, op, out),
    }
}

/// The bare local a switch scrutinee reads, if the scrutinee is a plain (unprojected) local.
fn scrutinee_local(op: &Operand) -> Option<LocalId> {
    match op {
        Operand::Copy(p) | Operand::Move(p) if p.proj.is_empty() => Some(p.local),
        _ => None,
    }
}

/// Resolve every `Switch` into a discriminant guard we can trust. A switch guards variant
/// membership only if its scrutinee is a discriminant temp that is (a) assigned **exactly
/// once** in the whole function — so its value at the switch is unambiguous and cannot have
/// been overwritten with an unrelated integer — via `temp = discriminant(P)`, and (b) that
/// sole assignment dominates the switch. The recovered `P` is the sum place the switch
/// discriminates over.
fn collect_switch_guards(func: &Func, doms: &[Vec<bool>]) -> Vec<SwitchGuard> {
    let n = func.blocks.len();
    // Count bare-local assignments and remember the sole `= discriminant(place)` for each.
    let mut assign_count: HashMap<u32, u32> = HashMap::new();
    let mut disc_def: HashMap<u32, (usize, Place)> = HashMap::new();
    for (i, block) in func.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            if let Stmt::Assign(place, rv) = stmt
                && place.proj.is_empty()
            {
                *assign_count.entry(place.local.0).or_insert(0) += 1;
                if let Rvalue::Discriminant(Operand::Copy(p) | Operand::Move(p)) = rv {
                    disc_def.insert(place.local.0, (i, p.clone()));
                }
            }
        }
        // `holla`'s `resolved` binding is also a def of its (bare) local.
        if let Terminator::HollaCheck { resolved, .. } = &block.term
            && resolved.proj.is_empty()
        {
            *assign_count.entry(resolved.local.0).or_insert(0) += 1;
        }
    }

    let mut guards = Vec::new();
    for (i, block) in func.blocks.iter().enumerate() {
        let Terminator::Switch {
            scrutinee,
            cases,
            default,
        } = &block.term
        else {
            continue;
        };
        let Some(local) = scrutinee_local(scrutinee) else {
            continue;
        };
        if assign_count.get(&local.0).copied() != Some(1) {
            continue; // Not a single-assignment temp — its value at the switch is not trusted.
        }
        let Some((def_block, place)) = disc_def.get(&local.0) else {
            continue; // The sole assignment is not a `discriminant(..)`.
        };
        if !doms[i][*def_block] {
            continue; // The discriminant read must dominate the switch.
        }
        let cases_idx: Vec<(u64, usize)> = cases
            .iter()
            .filter_map(|(cv, bb)| {
                let t = bb.index();
                (t < n).then_some((*cv, t))
            })
            .collect();
        let d = default.index();
        guards.push(SwitchGuard {
            block: i,
            sum_place: place.clone(),
            cases: cases_idx,
            default: if d < n { d } else { usize::MAX },
        });
    }
    guards
}

/// Is this downcast dominated by a discriminant guard that proves its variant?
///
/// True iff some guard switches on `Discriminant(prefix)` and has a case-`v` edge to a block
/// `t` such that entering `t` proves `discriminant == v` and `t` dominates the downcast:
///
/// * `t` dominates the downcast's block (so every path to it passes through `t`),
/// * `t`'s only predecessor is the switch block (so `t` is entered only across this edge),
/// * and `t` is the target of case value `v` alone — never another case or the `default`
///   edge (so crossing into `t` genuinely pins the discriminant to `v`).
///
/// The `default` edge is never accepted: a default arm does not pin the discriminant to any
/// single variant, so a downcast reached only through it is rejected (conservative).
fn site_is_guarded(
    site: &DowncastSite,
    guards: &[SwitchGuard],
    doms: &[Vec<bool>],
    preds: &[Vec<usize>],
) -> bool {
    let d = site.block;
    let v = site.variant as u64;
    for g in guards {
        if g.sum_place != site.prefix {
            continue;
        }
        for &(cv, t) in &g.cases {
            if cv != v {
                continue;
            }
            let single_pred = preds[t].len() == 1 && preds[t][0] == g.block;
            if doms[d][t] && single_pred && case_target_is_exclusive(g, t, v) {
                return true;
            }
        }
    }
    false
}

/// True iff block `t` is reached from switch `g` only by case value `v`: `t` is not the
/// `default` target, and no other case value also targets `t`.
fn case_target_is_exclusive(g: &SwitchGuard, t: usize, v: u64) -> bool {
    if g.default == t {
        return false;
    }
    g.cases.iter().all(|&(cv, tt)| tt != t || cv == v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::FuncBuilder;

    /// A module with `sum Opt { None, Some(i64) }`; returns `(module, opt_ty, i64, u32)`.
    fn opt_module() -> (Module, TyId, TyId, TyId) {
        let mut m = Module::new();
        let i64t = m.t_i64();
        let u32t = m.t_u32();
        let opt = m.add_sum(SumDef {
            name: "Opt".into(),
            variants: vec![
                Variant {
                    name: "None".into(),
                    payload: vec![],
                },
                Variant {
                    name: "Some".into(),
                    payload: vec![i64t],
                },
            ],
        });
        let opt_ty = m.intern_ty(TyKind::Sum(opt));
        (m, opt_ty, i64t, u32t)
    }

    /// The canonical frontend `vibe` shape: `disc = discriminant(x)`, a `Switch` on it, and
    /// each arm reading its payload through `downcast(variant).field(j)`. Must validate.
    #[test]
    fn guarded_downcast_validates() {
        let (mut m, opt_ty, i64t, u32t) = opt_module();
        let mut fb = FuncBuilder::new("unwrap_or_zero", vec![opt_ty], vec![i64t]);
        let arg = fb.param(0);
        let disc = fb.local(u32t);
        let payload = fb.local(i64t);
        let bb0 = fb.block();
        let bb_none = fb.block();
        let bb_some = fb.block();
        let bb_def = fb.block();

        fb.at(bb0);
        fb.assign(fb.place(disc), Rvalue::Discriminant(fb.copy(fb.place(arg))));
        fb.switch(
            fb.copy(fb.place(disc)),
            vec![(0, bb_none), (1, bb_some)],
            bb_def,
        );

        fb.at(bb_none);
        fb.ret(vec![Operand::Const(Const::Int(0, i64t))]);

        fb.at(bb_some); // reached only when discriminant == 1 (Some)
        let src = fb.field(&fb.downcast(&fb.place(arg), 1), 0);
        fb.assign(fb.place(payload), Rvalue::Use(fb.copy(src)));
        fb.ret(vec![fb.copy(fb.place(payload))]);

        fb.at(bb_def);
        fb.unreachable();

        m.add_func(fb.finish());
        validate(&m).expect("a discriminant-guarded downcast must validate");
    }

    /// The same guard, but the downcast lives in a block only *dominated by* the arm (not the
    /// arm itself) — exercises the dominator check beyond the trivial `arm == downcast` case.
    #[test]
    fn guarded_downcast_in_dominated_block_validates() {
        let (mut m, opt_ty, i64t, u32t) = opt_module();
        let mut fb = FuncBuilder::new("nested", vec![opt_ty], vec![i64t]);
        let arg = fb.param(0);
        let disc = fb.local(u32t);
        let payload = fb.local(i64t);
        let bb0 = fb.block();
        let bb_none = fb.block();
        let bb_some = fb.block();
        let bb_inner = fb.block();
        let bb_def = fb.block();

        fb.at(bb0);
        fb.assign(fb.place(disc), Rvalue::Discriminant(fb.copy(fb.place(arg))));
        fb.switch(
            fb.copy(fb.place(disc)),
            vec![(0, bb_none), (1, bb_some)],
            bb_def,
        );

        fb.at(bb_none);
        fb.ret(vec![Operand::Const(Const::Int(0, i64t))]);

        fb.at(bb_some);
        fb.goto(bb_inner);

        fb.at(bb_inner); // dominated by bb_some, whose sole predecessor is the case-1 edge
        let src = fb.field(&fb.downcast(&fb.place(arg), 1), 0);
        fb.assign(fb.place(payload), Rvalue::Use(fb.copy(src)));
        fb.ret(vec![fb.copy(fb.place(payload))]);

        fb.at(bb_def);
        fb.unreachable();

        m.add_func(fb.finish());
        validate(&m).expect("a downcast dominated by the guarded arm must validate");
    }

    /// A downcast with no discriminant test anywhere is a type-confusion hole — reject it.
    #[test]
    fn unguarded_downcast_rejected() {
        let (mut m, opt_ty, i64t, _u32t) = opt_module();
        let mut fb = FuncBuilder::new("wild_downcast", vec![opt_ty], vec![i64t]);
        let arg = fb.param(0);
        let payload = fb.local(i64t);
        let bb0 = fb.block();
        let bb1 = fb.block();

        fb.at(bb0);
        fb.goto(bb1); // no discriminant guard at all

        fb.at(bb1);
        let src = fb.field(&fb.downcast(&fb.place(arg), 1), 0);
        fb.assign(fb.place(payload), Rvalue::Use(fb.copy(src)));
        fb.ret(vec![fb.copy(fb.place(payload))]);

        m.add_func(fb.finish());
        let errs = validate(&m).expect_err("an unguarded downcast must be rejected");
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DowncastUnguarded { variant: 1, .. })),
            "{errs:?}"
        );
    }

    /// The switch proves variant 0 (None) on this edge, but the arm downcasts variant 1
    /// (Some) — the classic guarded-but-wrong-variant confusion. Reject it.
    #[test]
    fn mismatched_variant_downcast_rejected() {
        let (mut m, opt_ty, i64t, u32t) = opt_module();
        let mut fb = FuncBuilder::new("confused", vec![opt_ty], vec![i64t]);
        let arg = fb.param(0);
        let disc = fb.local(u32t);
        let payload = fb.local(i64t);
        let bb0 = fb.block();
        let bb_none = fb.block();
        let bb_some = fb.block();
        let bb_def = fb.block();

        fb.at(bb0);
        fb.assign(fb.place(disc), Rvalue::Discriminant(fb.copy(fb.place(arg))));
        fb.switch(
            fb.copy(fb.place(disc)),
            vec![(0, bb_none), (1, bb_some)],
            bb_def,
        );

        fb.at(bb_none); // reached only when discriminant == 0 (None)...
        let src = fb.field(&fb.downcast(&fb.place(arg), 1), 0); // ...but claims Some (variant 1)!
        fb.assign(fb.place(payload), Rvalue::Use(fb.copy(src)));
        fb.ret(vec![fb.copy(fb.place(payload))]);

        fb.at(bb_some);
        fb.ret(vec![Operand::Const(Const::Int(0, i64t))]);

        fb.at(bb_def);
        fb.unreachable();

        m.add_func(fb.finish());
        let errs = validate(&m).expect_err("a variant-mismatched downcast must be rejected");
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DowncastUnguarded { variant: 1, .. })),
            "{errs:?}"
        );
    }

    /// A downcast reached only through the `default` edge is not pinned to any variant, so it
    /// must be rejected (the default arm proves nothing about the discriminant).
    #[test]
    fn default_edge_downcast_rejected() {
        let (mut m, opt_ty, i64t, u32t) = opt_module();
        let mut fb = FuncBuilder::new("default_downcast", vec![opt_ty], vec![i64t]);
        let arg = fb.param(0);
        let disc = fb.local(u32t);
        let payload = fb.local(i64t);
        let bb0 = fb.block();
        let bb_none = fb.block();
        let bb_def = fb.block();

        fb.at(bb0);
        fb.assign(fb.place(disc), Rvalue::Discriminant(fb.copy(fb.place(arg))));
        fb.switch(fb.copy(fb.place(disc)), vec![(0, bb_none)], bb_def);

        fb.at(bb_none);
        fb.ret(vec![Operand::Const(Const::Int(0, i64t))]);

        fb.at(bb_def); // the default arm — discriminant is only known to be != 0
        let src = fb.field(&fb.downcast(&fb.place(arg), 1), 0);
        fb.assign(fb.place(payload), Rvalue::Use(fb.copy(src)));
        fb.ret(vec![fb.copy(fb.place(payload))]);

        m.add_func(fb.finish());
        let errs = validate(&m).expect_err("a default-edge downcast must be rejected");
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DowncastUnguarded { variant: 1, .. })),
            "{errs:?}"
        );
    }
}
