//! Well-formedness checking for a [`Module`].
//!
//! [`validate`] returns every problem it finds (not just the first). Structural checks
//! (block targets, return arity, place projections, aggregate/call/cop shapes) are always
//! applied; type checks are **best-effort** — where an operand's type can't be determined
//! locally (notably `ghosted`, whose type is contextual), the equality check is skipped
//! rather than reported as an error.

use crate::ir::*;

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
