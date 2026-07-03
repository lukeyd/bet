//! LLVM code generation via `inkwell` (only compiled under `--features llvm`).
//!
//! Beyond the tracer-bullet subset (externs, `str_ptr`/`str_len`, direct/extern calls,
//! scalar constants, `goto`/`branch`/`return`, a synthesized C `main`), this lowers a broad
//! slice of what `midir` actually defines:
//!
//! * **Arithmetic / bitwise / comparison** [`Rvalue::BinOp`]s over integers and floats,
//!   plus [`Rvalue::UnOp`] (`neg`/`not`/`bitnot`). Overflow [`ArithMode`] is currently
//!   lowered as plain wrapping arithmetic (release semantics); trap-on-overflow
//!   instrumentation is deferred.
//! * **Casts** ([`Rvalue::Cast`]): int zext/sext/trunc, int<->float, float resize, bitcast.
//! * **Aggregates via places**: `struct` (`drip`) values, with `Field`/`Deref` place
//!   projections for both reads and writes (GEP + load/store).
//! * **The `tag`/`holla`/crib memory model**: [`Rvalue::Cop`] (typed struct alloc),
//!   [`Rvalue::Trust`], [`Stmt::Evict`], and the [`Terminator::HollaCheck`] generational
//!   access — each lowered to its `rt-abi` entry point (`bet_cop`, `bet_holla_check`,
//!   `bet_slot_ptr`, `bet_evict`). A `tag T` is an 8-byte handle carried as `i64` (matching
//!   the C-ABI coercion of `rt_abi::Tag`); a `crib T`/`ref T`/`fn(..)` is a raw pointer.
//! * **Control flow**: `switch`, `panic` (→ `bet_panic` + `unreachable`), and `unreachable`.
//! * **Function values**: `@f` [`Const::FnRef`] and [`Callee::Indirect`] indirect calls.
//!
//! Anything outside this subset returns [`BackendError::Lower`] with a precise message; the
//! remaining IR (sums, slices/arrays, tuples/multi-value returns, maps, bump `cop`) lands
//! later.

use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module as LlvmModule};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, PointerType, StructType,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::{BackendError, EmitOptions, OptLevel};
use midir::*;

fn lower_err(e: BuilderError) -> BackendError {
    BackendError::Lower(e.to_string())
}

pub fn compile(module: &Module, opts: &EmitOptions) -> Result<Vec<u8>, BackendError> {
    let cx = Context::create();
    let llm = cx.create_module("bet");
    let builder = cx.create_builder();
    let mut cg = Cg {
        cx: &cx,
        m: module,
        llm,
        builder,
        funcs: Vec::new(),
        externs: Vec::new(),
    };
    cg.declare_externs()?;
    cg.declare_funcs()?;
    cg.define_funcs()?;
    if let Some(entry) = &opts.entry {
        cg.synthesize_main(entry)?;
    }
    cg.emit_object(opts)
}

struct Cg<'c> {
    cx: &'c Context,
    m: &'c Module,
    llm: LlvmModule<'c>,
    builder: Builder<'c>,
    /// Indexed by `FuncId`.
    funcs: Vec<FunctionValue<'c>>,
    /// Indexed by `ExternId`.
    externs: Vec<FunctionValue<'c>>,
}

impl<'c> Cg<'c> {
    // --- types ---

    /// The one opaque-pointer type (LLVM 18 pointers are typeless).
    fn ptr_ty(&self) -> PointerType<'c> {
        self.cx.ptr_type(AddressSpace::default())
    }

    fn basic_ty(&self, ty: TyId) -> Result<BasicTypeEnum<'c>, BackendError> {
        Ok(match self.m.ty(ty) {
            TyKind::Bool => self.cx.bool_type().into(),
            TyKind::Int { width, .. } => self.cx.custom_width_int_type(width.bits()).into(),
            TyKind::F32 => self.cx.f32_type().into(),
            TyKind::F64 => self.cx.f64_type().into(),
            TyKind::RawPtr => self.ptr_ty().into(),
            // A `tag T` is an 8-byte `(slot, generation)` handle. It is passed by value across
            // the `rt-abi` boundary, where the C ABI coerces the two-`u32` `rt_abi::Tag` to a
            // single 64-bit integer register on our targets — so we carry it as `i64`.
            TyKind::Tag(_) => self.cx.i64_type().into(),
            // Crib handles, live refs, and function values are all raw pointers.
            TyKind::Crib(_) | TyKind::Ref(_) | TyKind::FnPtr(_) | TyKind::Map(_, _) => {
                self.ptr_ty().into()
            }
            TyKind::Struct(sid) => self.struct_llvm_ty(*sid)?.into(),
            other => {
                return Err(BackendError::Lower(format!(
                    "value type {other:?} is not supported yet"
                )));
            }
        })
    }

    /// The LLVM struct type for a `drip`, built structurally from its field types.
    fn struct_llvm_ty(&self, sid: StructId) -> Result<StructType<'c>, BackendError> {
        let def = self.m.struct_def(sid);
        let fields: Vec<BasicTypeEnum> = def
            .fields
            .iter()
            .map(|f| self.basic_ty(f.ty))
            .collect::<Result<_, _>>()?;
        Ok(self.cx.struct_type(&fields, false))
    }

    fn fn_type(&self, params: &[TyId], rets: &[TyId]) -> Result<FunctionType<'c>, BackendError> {
        let ps: Vec<BasicMetadataTypeEnum> = params
            .iter()
            .map(|&t| self.basic_ty(t).map(Into::into))
            .collect::<Result<_, _>>()?;
        Ok(match rets {
            [] => self.cx.void_type().fn_type(&ps, false),
            [one] => self.basic_ty(*one)?.fn_type(&ps, false),
            _ => {
                return Err(BackendError::Lower(
                    "multi-value returns are not supported yet".into(),
                ));
            }
        })
    }

    // --- declarations ---

    fn declare_externs(&mut self) -> Result<(), BackendError> {
        for ext in self.m.externs() {
            let fty = self.fn_type(&ext.sig.params, &ext.sig.rets)?;
            let f = self
                .llm
                .add_function(&ext.name, fty, Some(Linkage::External));
            self.externs.push(f);
        }
        Ok(())
    }

    fn declare_funcs(&mut self) -> Result<(), BackendError> {
        for func in self.m.funcs() {
            let fty = self.fn_type(&func.params, &func.rets)?;
            // Namespace bet functions away from C/extern symbols; the C entry `main` is
            // synthesized separately. Direct calls resolve by index, not by name.
            let name = format!("bet.{}", func.name);
            let f = self.llm.add_function(&name, fty, Some(Linkage::Internal));
            self.funcs.push(f);
        }
        Ok(())
    }

    // --- function bodies ---

    fn define_funcs(&self) -> Result<(), BackendError> {
        for (i, func) in self.m.funcs().iter().enumerate() {
            self.define_func(FuncId(i as u32), func)?;
        }
        Ok(())
    }

    fn define_func(&self, id: FuncId, func: &Func) -> Result<(), BackendError> {
        let fv = self.funcs[id.index()];
        let blocks: Vec<BasicBlock<'c>> = func
            .blocks
            .iter()
            .map(|b| self.cx.append_basic_block(fv, &format!("bb{}", b.id.0)))
            .collect();

        // Prologue: one alloca per non-void local, in the entry block; store incoming params.
        let entry_bb = blocks[func.entry.index()];
        self.builder.position_at_end(entry_bb);
        let mut locals: Vec<Option<PointerValue<'c>>> = Vec::with_capacity(func.locals.len());
        for (li, local) in func.locals.iter().enumerate() {
            if matches!(self.m.ty(local.ty), TyKind::Void) {
                locals.push(None);
                continue;
            }
            let bt = self.basic_ty(local.ty)?;
            let slot = self
                .builder
                .build_alloca(bt, &format!("l{li}"))
                .map_err(lower_err)?;
            locals.push(Some(slot));
        }
        for (pi, slot) in locals.iter().take(func.params.len()).enumerate() {
            if let Some(slot) = slot {
                let arg = fv
                    .get_nth_param(pi as u32)
                    .ok_or_else(|| BackendError::Lower("missing incoming parameter".into()))?;
                self.builder.build_store(*slot, arg).map_err(lower_err)?;
            }
        }

        // Bodies.
        for (bi, b) in func.blocks.iter().enumerate() {
            self.builder.position_at_end(blocks[bi]);
            for s in &b.stmts {
                self.lower_stmt(func, &locals, s)?;
            }
            self.lower_term(func, &locals, &blocks, &b.term)?;
        }
        Ok(())
    }

    fn lower_stmt(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        s: &Stmt,
    ) -> Result<(), BackendError> {
        match s {
            Stmt::Nop => Ok(()),
            Stmt::Assign(place, rv) => {
                let val = self.lower_rvalue(func, locals, rv)?;
                if place.proj.is_empty() {
                    return match (locals[place.local.index()], val) {
                        (Some(slot), Some(v)) => {
                            self.builder.build_store(slot, v).map_err(lower_err)?;
                            Ok(())
                        }
                        // A void rvalue (e.g. a void call) into a void local: nothing to store.
                        (None, None) => Ok(()),
                        (Some(_), None) => Err(BackendError::Lower(
                            "assigning a void value into a non-void local".into(),
                        )),
                        (None, Some(_)) => Ok(()),
                    };
                }
                // Projected lvalue: compute its address and store through it.
                let (ptr, _ty) = self.place_ptr(func, locals, place)?;
                match val {
                    Some(v) => {
                        self.builder.build_store(ptr, v).map_err(lower_err)?;
                        Ok(())
                    }
                    None => Err(BackendError::Lower(
                        "assigning a void value into a projected place".into(),
                    )),
                }
            }
            Stmt::Evict(crib) => {
                let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
                let evict = self.get_or_add(
                    "bet_evict",
                    self.cx.void_type().fn_type(&[self.ptr_ty().into()], false),
                );
                self.builder
                    .build_call(evict, &[crib_v.into()], "")
                    .map_err(lower_err)?;
                Ok(())
            }
        }
    }

    /// Lower an rvalue to its value, or `None` when it produces `void`.
    fn lower_rvalue(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        rv: &Rvalue,
    ) -> Result<Option<BasicValueEnum<'c>>, BackendError> {
        match rv {
            Rvalue::Use(op) => Ok(Some(self.lower_operand(func, locals, op)?)),
            Rvalue::BinOp(op, a, b, _mode) => Ok(Some(self.lower_binop(func, locals, *op, a, b)?)),
            Rvalue::UnOp(op, a) => Ok(Some(self.lower_unop(func, locals, *op, a)?)),
            Rvalue::Cast(op, ty, kind) => Ok(Some(self.lower_cast(func, locals, op, *ty, *kind)?)),
            Rvalue::StrPtr(op) => {
                let s = self.str_literal(op)?;
                let g = self
                    .builder
                    .build_global_string_ptr(&s, "str")
                    .map_err(lower_err)?;
                Ok(Some(g.as_pointer_value().into()))
            }
            Rvalue::StrLen(op) => {
                let s = self.str_literal(op)?;
                let len = self.cx.i64_type().const_int(s.len() as u64, false);
                Ok(Some(len.into()))
            }
            Rvalue::Call(callee, args) => self.lower_call(func, locals, callee, args),
            Rvalue::Cop(crib, init) => self.lower_cop(func, locals, crib, init),
            Rvalue::Trust(crib, tag) => Ok(Some(self.lower_trust(func, locals, crib, tag)?)),
            other => Err(BackendError::Lower(format!(
                "rvalue {other:?} is not supported yet"
            ))),
        }
    }

    // --- arithmetic / logic ---

    fn lower_binop(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: BinOp,
        a: &Operand,
        b: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let lv = self.lower_operand(func, locals, a)?;
        let rv = self.lower_operand(func, locals, b)?;
        match (lv, rv) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let signed = self.int_signed(func, a, b);
                self.lower_int_binop(op, l, r, signed)
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                self.lower_float_binop(op, l, r)
            }
            _ => Err(BackendError::Lower(
                "binary op on non-scalar or mismatched operands".into(),
            )),
        }
    }

    fn lower_int_binop(
        &self,
        op: BinOp,
        l: IntValue<'c>,
        r: IntValue<'c>,
        signed: bool,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        // Overflow mode is currently ignored: both `wrap` and `trap` lower to plain wrapping
        // arithmetic (release semantics). Trap-on-overflow instrumentation lands later.
        let v: BasicValueEnum = match op {
            BinOp::Add => b.build_int_add(l, r, "add").map_err(lower_err)?.into(),
            BinOp::Sub => b.build_int_sub(l, r, "sub").map_err(lower_err)?.into(),
            BinOp::Mul => b.build_int_mul(l, r, "mul").map_err(lower_err)?.into(),
            BinOp::Div => if signed {
                b.build_int_signed_div(l, r, "sdiv")
            } else {
                b.build_int_unsigned_div(l, r, "udiv")
            }
            .map_err(lower_err)?
            .into(),
            BinOp::Rem => if signed {
                b.build_int_signed_rem(l, r, "srem")
            } else {
                b.build_int_unsigned_rem(l, r, "urem")
            }
            .map_err(lower_err)?
            .into(),
            BinOp::BitAnd => b.build_and(l, r, "and").map_err(lower_err)?.into(),
            BinOp::BitOr => b.build_or(l, r, "or").map_err(lower_err)?.into(),
            BinOp::BitXor => b.build_xor(l, r, "xor").map_err(lower_err)?.into(),
            BinOp::Shl => b.build_left_shift(l, r, "shl").map_err(lower_err)?.into(),
            // Arithmetic shift for signed, logical for unsigned.
            BinOp::Shr => b
                .build_right_shift(l, r, signed, "shr")
                .map_err(lower_err)?
                .into(),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let pred = int_predicate(op, signed);
                b.build_int_compare(pred, l, r, "cmp")
                    .map_err(lower_err)?
                    .into()
            }
        };
        Ok(v)
    }

    fn lower_float_binop(
        &self,
        op: BinOp,
        l: inkwell::values::FloatValue<'c>,
        r: inkwell::values::FloatValue<'c>,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        let v: BasicValueEnum = match op {
            BinOp::Add => b.build_float_add(l, r, "fadd").map_err(lower_err)?.into(),
            BinOp::Sub => b.build_float_sub(l, r, "fsub").map_err(lower_err)?.into(),
            BinOp::Mul => b.build_float_mul(l, r, "fmul").map_err(lower_err)?.into(),
            BinOp::Div => b.build_float_div(l, r, "fdiv").map_err(lower_err)?.into(),
            BinOp::Rem => b.build_float_rem(l, r, "frem").map_err(lower_err)?.into(),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let pred = float_predicate(op);
                b.build_float_compare(pred, l, r, "fcmp")
                    .map_err(lower_err)?
                    .into()
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                return Err(BackendError::Lower(
                    "bitwise/shift op on floating-point operands".into(),
                ));
            }
        };
        Ok(v)
    }

    fn lower_unop(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: UnOp,
        a: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let v = self.lower_operand(func, locals, a)?;
        Ok(match (op, v) {
            (UnOp::Neg, BasicValueEnum::IntValue(i)) => self
                .builder
                .build_int_neg(i, "neg")
                .map_err(lower_err)?
                .into(),
            (UnOp::Neg, BasicValueEnum::FloatValue(f)) => self
                .builder
                .build_float_neg(f, "fneg")
                .map_err(lower_err)?
                .into(),
            // `not` on a bool and `bitnot` on an integer are both LLVM `xor -1` / `not`.
            (UnOp::Not | UnOp::BitNot, BasicValueEnum::IntValue(i)) => {
                self.builder.build_not(i, "not").map_err(lower_err)?.into()
            }
            (op, _) => {
                return Err(BackendError::Lower(format!(
                    "unary op {op:?} on an unsupported operand type"
                )));
            }
        })
    }

    fn lower_cast(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &Operand,
        target: TyId,
        kind: CastKind,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let v = self.lower_operand(func, locals, op)?;
        let tgt = self.basic_ty(target)?;
        let b = &self.builder;
        Ok(match kind {
            CastKind::IntZext => b
                .build_int_z_extend(v.into_int_value(), tgt.into_int_type(), "zext")
                .map_err(lower_err)?
                .into(),
            CastKind::IntSext => b
                .build_int_s_extend(v.into_int_value(), tgt.into_int_type(), "sext")
                .map_err(lower_err)?
                .into(),
            CastKind::IntTrunc => b
                .build_int_truncate(v.into_int_value(), tgt.into_int_type(), "trunc")
                .map_err(lower_err)?
                .into(),
            CastKind::IntToFloat => {
                let signed = self
                    .operand_ty(func, op)?
                    .map(|t| matches!(self.m.ty(t), TyKind::Int { signed: true, .. }))
                    .unwrap_or(true);
                if signed {
                    b.build_signed_int_to_float(v.into_int_value(), tgt.into_float_type(), "sitofp")
                } else {
                    b.build_unsigned_int_to_float(
                        v.into_int_value(),
                        tgt.into_float_type(),
                        "uitofp",
                    )
                }
                .map_err(lower_err)?
                .into()
            }
            CastKind::FloatToInt => {
                let signed = matches!(self.m.ty(target), TyKind::Int { signed: true, .. });
                if signed {
                    b.build_float_to_signed_int(v.into_float_value(), tgt.into_int_type(), "fptosi")
                } else {
                    b.build_float_to_unsigned_int(
                        v.into_float_value(),
                        tgt.into_int_type(),
                        "fptoui",
                    )
                }
                .map_err(lower_err)?
                .into()
            }
            CastKind::FloatResize => {
                let src = v.into_float_value();
                let dst_ty = tgt.into_float_type();
                // inkwell's `FloatType` exposes no `get_bit_width()`, so take the widths
                // from the IR types instead (F32 = 32, F64 = 64), same as the int casts
                // above read signedness from `self.m.ty(..)`.
                let float_bits = |k: &TyKind| match k {
                    TyKind::F32 => 32u32,
                    TyKind::F64 => 64u32,
                    _ => 0,
                };
                let dst_bits = float_bits(self.m.ty(target));
                let src_bits = self
                    .operand_ty(func, op)?
                    .map(|t| float_bits(self.m.ty(t)))
                    .unwrap_or(dst_bits);
                if dst_bits > src_bits {
                    b.build_float_ext(src, dst_ty, "fpext")
                        .map_err(lower_err)?
                        .into()
                } else if dst_bits < src_bits {
                    b.build_float_trunc(src, dst_ty, "fptrunc")
                        .map_err(lower_err)?
                        .into()
                } else {
                    v
                }
            }
            CastKind::Bitcast => b.build_bit_cast(v, tgt, "bitcast").map_err(lower_err)?,
        })
    }

    // --- calls ---

    fn lower_call(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        callee: &Callee,
        args: &[Operand],
    ) -> Result<Option<BasicValueEnum<'c>>, BackendError> {
        let arg_vals: Vec<BasicMetadataValueEnum> = args
            .iter()
            .map(|a| self.lower_operand(func, locals, a).map(Into::into))
            .collect::<Result<_, _>>()?;
        let cs = match callee {
            Callee::Direct(fid) => self
                .builder
                .build_call(self.funcs[fid.index()], &arg_vals, "call")
                .map_err(lower_err)?,
            Callee::Extern(eid) => self
                .builder
                .build_call(self.externs[eid.index()], &arg_vals, "call")
                .map_err(lower_err)?,
            Callee::Indirect(op) => {
                // Recover the callee's function type from its `fn(..)` operand type.
                let ty = self.operand_ty(func, op)?.ok_or_else(|| {
                    BackendError::Lower("indirect callee has no known type".into())
                })?;
                let TyKind::FnPtr(sig) = self.m.ty(ty) else {
                    return Err(BackendError::Lower(
                        "indirect call on a non-function-pointer operand".into(),
                    ));
                };
                let s = self.m.sig(*sig);
                let fty = self.fn_type(&s.params, &s.rets)?;
                let fptr = self.lower_operand(func, locals, op)?.into_pointer_value();
                self.builder
                    .build_indirect_call(fty, fptr, &arg_vals, "call_indirect")
                    .map_err(lower_err)?
            }
        };
        Ok(cs.try_as_basic_value().left())
    }

    // --- tag / holla / crib memory model ---

    /// `cop init in crib` for a **typed** crib: reserve a slot (`bet_cop`), resolve its
    /// storage (`bet_holla_check`), initialize the struct fields, and yield the `tag` (`i64`).
    fn lower_cop(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        crib: &Operand,
        init: &CopInit,
    ) -> Result<Option<BasicValueEnum<'c>>, BackendError> {
        let CopInit::StructLit(sid, fields) = init else {
            return Err(BackendError::Lower(
                "`cop` of sum variants / bump cribs is not supported yet".into(),
            ));
        };
        let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();

        let cop = self.get_or_add(
            "bet_cop",
            self.cx.i64_type().fn_type(&[self.ptr_ty().into()], false),
        );
        let tag = self
            .builder
            .build_call(cop, &[crib_v.into()], "cop")
            .map_err(lower_err)?
            .try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower("bet_cop returned void".into()))?
            .into_int_value();

        let holla = self.get_or_add(
            "bet_holla_check",
            self.ptr_ty()
                .fn_type(&[self.ptr_ty().into(), self.cx.i64_type().into()], false),
        );
        let storage = self
            .builder
            .build_call(holla, &[crib_v.into(), tag.into()], "cop.slot")
            .map_err(lower_err)?
            .try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower("bet_holla_check returned void".into()))?
            .into_pointer_value();

        let sty = self.struct_llvm_ty(*sid)?;
        for (fidx, op) in fields {
            let v = self.lower_operand(func, locals, op)?;
            let fptr = self
                .builder
                .build_struct_gep(sty, storage, *fidx, "cop.field")
                .map_err(|_| BackendError::Lower(format!("bad field index {fidx} in cop")))?;
            self.builder.build_store(fptr, v).map_err(lower_err)?;
        }
        Ok(Some(tag.into()))
    }

    /// `tag.trust() in crib` — unchecked resolve to a `ref` (a raw slot pointer). Extracts the
    /// slot index from the low 32 bits of the tag and calls `bet_slot_ptr`.
    fn lower_trust(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        crib: &Operand,
        tag: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
        let tag_v = self.lower_operand(func, locals, tag)?.into_int_value();
        let slot = self
            .builder
            .build_int_truncate(tag_v, self.cx.i32_type(), "slot")
            .map_err(lower_err)?;
        let slot_ptr = self.get_or_add(
            "bet_slot_ptr",
            self.ptr_ty()
                .fn_type(&[self.ptr_ty().into(), self.cx.i32_type().into()], false),
        );
        let cs = self
            .builder
            .build_call(slot_ptr, &[crib_v.into(), slot.into()], "trust")
            .map_err(lower_err)?;
        cs.try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower("bet_slot_ptr returned void".into()))
    }

    // --- operands, places, consts ---

    fn lower_operand(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        match op {
            Operand::Const(c) => self.lower_const(c),
            Operand::Copy(p) | Operand::Move(p) => {
                let (ptr, ty) = self.place_ptr(func, locals, p)?;
                let bt = self.basic_ty(ty)?;
                self.builder.build_load(bt, ptr, "load").map_err(lower_err)
            }
        }
    }

    /// The address of a (possibly projected) place, plus the element `TyId` at that address.
    /// The base local's `alloca` is its address; `Deref` loads a `ref`/`rawptr` to get the
    /// pointee address, and `Field` GEPs into a struct.
    fn place_ptr(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        place: &Place,
    ) -> Result<(PointerValue<'c>, TyId), BackendError> {
        let mut ptr = locals[place.local.index()]
            .ok_or_else(|| BackendError::Lower("addressing a void/zero-sized local".into()))?;
        let mut ty = func.local_ty(place.local);
        for proj in &place.proj {
            match proj {
                Proj::Deref => {
                    let elem = match self.m.ty(ty) {
                        TyKind::Ref(e) => *e,
                        other => {
                            return Err(BackendError::Lower(format!("deref of non-ref {other:?}")));
                        }
                    };
                    ptr = self
                        .builder
                        .build_load(self.ptr_ty(), ptr, "deref")
                        .map_err(lower_err)?
                        .into_pointer_value();
                    ty = elem;
                }
                Proj::Field(i) => {
                    let (sty, field_ty) = match self.m.ty(ty) {
                        TyKind::Struct(sid) => {
                            let def = self.m.struct_def(*sid);
                            let field = def.fields.get(*i as usize).ok_or_else(|| {
                                BackendError::Lower(format!(
                                    "struct `{}` has no field #{i}",
                                    def.name
                                ))
                            })?;
                            (self.struct_llvm_ty(*sid)?, field.ty)
                        }
                        other => {
                            return Err(BackendError::Lower(format!(
                                "field projection on non-struct {other:?}"
                            )));
                        }
                    };
                    ptr = self
                        .builder
                        .build_struct_gep(sty, ptr, *i, "field")
                        .map_err(|_| BackendError::Lower(format!("bad field index {i}")))?;
                    ty = field_ty;
                }
                Proj::Index(_) | Proj::Downcast(_) => {
                    return Err(BackendError::Lower(
                        "index/downcast projections are not supported yet".into(),
                    ));
                }
            }
        }
        Ok((ptr, ty))
    }

    /// The `TyId` of an operand, when statically known. `None` for contextually-typed
    /// constants (`ghosted`, `@f`, bools/strings that carry no interned type id).
    fn operand_ty(&self, func: &Func, op: &Operand) -> Result<Option<TyId>, BackendError> {
        Ok(match op {
            Operand::Const(Const::Int(_, ty)) | Operand::Const(Const::Float(_, ty)) => Some(*ty),
            Operand::Const(_) => None,
            Operand::Copy(p) | Operand::Move(p) => Some(self.place_ty(func, p)?),
        })
    }

    /// Resolve a place's element `TyId` by walking its projections (types only, no codegen).
    fn place_ty(&self, func: &Func, place: &Place) -> Result<TyId, BackendError> {
        let mut ty = func.local_ty(place.local);
        for proj in &place.proj {
            ty = match (proj, self.m.ty(ty)) {
                (Proj::Deref, TyKind::Ref(e)) => *e,
                (Proj::Field(i), TyKind::Struct(sid)) => {
                    let def = self.m.struct_def(*sid);
                    def.fields
                        .get(*i as usize)
                        .ok_or_else(|| {
                            BackendError::Lower(format!("struct `{}` has no field #{i}", def.name))
                        })?
                        .ty
                }
                (Proj::Field(i), TyKind::Tuple(elems)) => *elems
                    .get(*i as usize)
                    .ok_or_else(|| BackendError::Lower(format!("tuple has no element #{i}")))?,
                (p, other) => {
                    return Err(BackendError::Lower(format!(
                        "cannot resolve projection {p:?} on {other:?}"
                    )));
                }
            };
        }
        Ok(ty)
    }

    /// Best-effort signedness for an integer binop: consult whichever operand carries an
    /// integer type; default to unsigned (only reached for sign-agnostic ops like `eq`).
    fn int_signed(&self, func: &Func, a: &Operand, b: &Operand) -> bool {
        for op in [a, b] {
            if let Ok(Some(ty)) = self.operand_ty(func, op)
                && let TyKind::Int { signed, .. } = self.m.ty(ty)
            {
                return *signed;
            }
        }
        false
    }

    fn lower_const(&self, c: &Const) -> Result<BasicValueEnum<'c>, BackendError> {
        match c {
            Const::Int(v, ty) => match self.basic_ty(*ty)? {
                BasicTypeEnum::IntType(it) => Ok(it.const_int(*v as u64, true).into()),
                _ => Err(BackendError::Lower(
                    "integer const with non-int type".into(),
                )),
            },
            Const::Bool(b) => Ok(self.cx.bool_type().const_int(*b as u64, false).into()),
            Const::Float(v, ty) => match self.basic_ty(*ty)? {
                BasicTypeEnum::FloatType(ft) => Ok(ft.const_float(*v).into()),
                _ => Err(BackendError::Lower(
                    "float const with non-float type".into(),
                )),
            },
            // `ghosted` in a tag context is the null tag (`rt_abi::Tag::NULL`: slot=u32::MAX,
            // generation=0), which packs to `0xFFFF_FFFF` as an `i64` and always resolves as
            // ghosted. Other (contextual) uses of `ghosted` are not supported yet.
            Const::Ghosted => Ok(self.cx.i64_type().const_int(0xFFFF_FFFF, false).into()),
            Const::FnRef(fid) => Ok(self.funcs[fid.index()]
                .as_global_value()
                .as_pointer_value()
                .into()),
            Const::Str(_) => Err(BackendError::Lower(
                "a bare string constant has no scalar value; use str_ptr/str_len".into(),
            )),
        }
    }

    fn str_literal(&self, op: &Operand) -> Result<String, BackendError> {
        match op {
            Operand::Const(Const::Str(s)) => Ok(s.clone()),
            _ => Err(BackendError::Lower(
                "str_ptr/str_len require a string-literal operand in this backend".into(),
            )),
        }
    }

    fn lower_term(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        blocks: &[BasicBlock<'c>],
        term: &Terminator,
    ) -> Result<(), BackendError> {
        match term {
            Terminator::Return(vals) => {
                match vals.as_slice() {
                    [] => {
                        self.builder.build_return(None).map_err(lower_err)?;
                    }
                    [op] => {
                        let v = self.lower_operand(func, locals, op)?;
                        self.builder.build_return(Some(&v)).map_err(lower_err)?;
                    }
                    _ => {
                        return Err(BackendError::Lower(
                            "multi-value return is not supported yet".into(),
                        ));
                    }
                }
                Ok(())
            }
            Terminator::Goto(bb) => {
                self.builder
                    .build_unconditional_branch(blocks[bb.index()])
                    .map_err(lower_err)?;
                Ok(())
            }
            Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => {
                let c = self.lower_operand(func, locals, cond)?;
                self.builder
                    .build_conditional_branch(
                        c.into_int_value(),
                        blocks[then_bb.index()],
                        blocks[else_bb.index()],
                    )
                    .map_err(lower_err)?;
                Ok(())
            }
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let s = self
                    .lower_operand(func, locals, scrutinee)?
                    .into_int_value();
                let int_ty = s.get_type();
                let arms: Vec<(IntValue<'c>, BasicBlock<'c>)> = cases
                    .iter()
                    .map(|(v, bb)| (int_ty.const_int(*v, false), blocks[bb.index()]))
                    .collect();
                self.builder
                    .build_switch(s, blocks[default.index()], &arms)
                    .map_err(lower_err)?;
                Ok(())
            }
            Terminator::HollaCheck {
                tag,
                crib,
                resolved,
                live,
                ghosted,
            } => {
                let tag_v = self.lower_operand(func, locals, tag)?.into_int_value();
                let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
                let holla = self.get_or_add(
                    "bet_holla_check",
                    self.ptr_ty()
                        .fn_type(&[self.ptr_ty().into(), self.cx.i64_type().into()], false),
                );
                let storage = self
                    .builder
                    .build_call(holla, &[crib_v.into(), tag_v.into()], "holla")
                    .map_err(lower_err)?
                    .try_as_basic_value()
                    .left()
                    .ok_or_else(|| BackendError::Lower("bet_holla_check returned void".into()))?
                    .into_pointer_value();
                // Bind `resolved` to the storage pointer (valid only on the live edge; the
                // ghosted edge, by contract, never reads it).
                let (dest, _ty) = self.place_ptr(func, locals, resolved)?;
                self.builder.build_store(dest, storage).map_err(lower_err)?;
                // Live iff the checked resolve returned non-null.
                let is_null = self
                    .builder
                    .build_is_null(storage, "ghosted?")
                    .map_err(lower_err)?;
                self.builder
                    .build_conditional_branch(
                        is_null,
                        blocks[ghosted.index()],
                        blocks[live.index()],
                    )
                    .map_err(lower_err)?;
                Ok(())
            }
            Terminator::Panic(msg) => {
                let s = self.str_literal(msg)?;
                let g = self
                    .builder
                    .build_global_string_ptr(&s, "panic")
                    .map_err(lower_err)?;
                let len = self.cx.i64_type().const_int(s.len() as u64, false);
                let panic = self.get_or_add(
                    "bet_panic",
                    self.cx
                        .void_type()
                        .fn_type(&[self.ptr_ty().into(), self.cx.i64_type().into()], false),
                );
                self.builder
                    .build_call(panic, &[g.as_pointer_value().into(), len.into()], "")
                    .map_err(lower_err)?;
                self.builder.build_unreachable().map_err(lower_err)?;
                Ok(())
            }
            Terminator::Unreachable => {
                self.builder.build_unreachable().map_err(lower_err)?;
                Ok(())
            }
        }
    }

    // --- entry synthesis ---

    fn synthesize_main(&self, entry_name: &str) -> Result<(), BackendError> {
        let idx = self
            .m
            .funcs()
            .iter()
            .position(|f| f.name == entry_name)
            .ok_or_else(|| {
                BackendError::Lower(format!("entry function `{entry_name}` not found"))
            })?;
        let bet_main = self.funcs[idx];

        let i32t = self.cx.i32_type();
        let main_fn =
            self.llm
                .add_function("main", i32t.fn_type(&[], false), Some(Linkage::External));
        let bb = self.cx.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(bb);

        let void_fty = self.cx.void_type().fn_type(&[], false);
        let init = self.get_or_add("bet_rt_init", void_fty);
        let shutdown = self.get_or_add("bet_rt_shutdown", void_fty);

        self.builder.build_call(init, &[], "").map_err(lower_err)?;
        self.builder
            .build_call(bet_main, &[], "")
            .map_err(lower_err)?;
        self.builder
            .build_call(shutdown, &[], "")
            .map_err(lower_err)?;
        self.builder
            .build_return(Some(&i32t.const_int(0, false)))
            .map_err(lower_err)?;
        Ok(())
    }

    fn get_or_add(&self, name: &str, fty: FunctionType<'c>) -> FunctionValue<'c> {
        self.llm
            .get_function(name)
            .unwrap_or_else(|| self.llm.add_function(name, fty, Some(Linkage::External)))
    }

    // --- object emission ---

    fn emit_object(&self, opts: &EmitOptions) -> Result<Vec<u8>, BackendError> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(BackendError::Target)?;

        let triple = match &opts.target {
            Some(t) => TargetTriple::create(t),
            None => TargetMachine::get_default_triple(),
        };
        let target =
            Target::from_triple(&triple).map_err(|e| BackendError::Target(e.to_string()))?;
        let opt = match opts.opt {
            OptLevel::O0 => OptimizationLevel::None,
            OptLevel::O2 => OptimizationLevel::Default,
        };
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let tm = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                opt,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| BackendError::Target("could not create target machine".into()))?;

        self.llm.set_triple(&triple);
        self.llm
            .set_data_layout(&tm.get_target_data().get_data_layout());

        if let Err(e) = self.llm.verify() {
            return Err(BackendError::Lower(format!(
                "generated LLVM module failed verification: {e}"
            )));
        }

        let buf = tm
            .write_to_memory_buffer(&self.llm, FileType::Object)
            .map_err(|e| BackendError::Target(e.to_string()))?;
        Ok(buf.as_slice().to_vec())
    }
}

/// The LLVM integer comparison predicate for a comparison [`BinOp`] and signedness.
fn int_predicate(op: BinOp, signed: bool) -> IntPredicate {
    match op {
        BinOp::Eq => IntPredicate::EQ,
        BinOp::Ne => IntPredicate::NE,
        BinOp::Lt if signed => IntPredicate::SLT,
        BinOp::Lt => IntPredicate::ULT,
        BinOp::Le if signed => IntPredicate::SLE,
        BinOp::Le => IntPredicate::ULE,
        BinOp::Gt if signed => IntPredicate::SGT,
        BinOp::Gt => IntPredicate::UGT,
        BinOp::Ge if signed => IntPredicate::SGE,
        BinOp::Ge => IntPredicate::UGE,
        _ => unreachable!("int_predicate on a non-comparison op"),
    }
}

/// The LLVM ordered floating-point comparison predicate for a comparison [`BinOp`].
fn float_predicate(op: BinOp) -> FloatPredicate {
    match op {
        BinOp::Eq => FloatPredicate::OEQ,
        BinOp::Ne => FloatPredicate::ONE,
        BinOp::Lt => FloatPredicate::OLT,
        BinOp::Le => FloatPredicate::OLE,
        BinOp::Gt => FloatPredicate::OGT,
        BinOp::Ge => FloatPredicate::OGE,
        _ => unreachable!("float_predicate on a non-comparison op"),
    }
}
