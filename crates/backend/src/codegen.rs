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
//!   `bet_slot_ptr`, `bet_evict`). A `tag T` is a 16-byte generational handle carried by value
//!   as the struct `{ i32 slot, i64 generation }` (matching `rt_abi::Tag` after the issue-#34
//!   `u64`-generation widening); a `crib T`/`ref T`/`fn(..)` is a raw pointer.
//! * **Control flow**: `switch`, `panic` (→ `bet_panic` + `unreachable`), and `unreachable`.
//! * **Function values**: `@f` [`Const::FnRef`] and [`Callee::Indirect`] indirect calls.
//! * **Aggregates & sums**: [`Rvalue::Aggregate`] (`drip` structs, tuples, and by-value
//!   `moods` sums), [`Rvalue::Discriminant`], a tagged-union [`TyKind::Sum`] layout
//!   (`{ i32 tag, [W x i64] payload }`, where the payload union is sized to hold the widest
//!   variant and each variant's fields are placed by their natural struct layout — so a
//!   payload field wider than a word, e.g. a `drip`, is carried faithfully), fixed
//!   [`TyKind::Array`] values, and the [`Proj::Index`]/[`Proj::Downcast`] place projections.
//! * **Multi-value returns**: a [`TyKind::Tuple`] is an anonymous return struct;
//!   [`Terminator::Return`] with several operands packs into it and callers destructure with
//!   tuple `Field` projections.
//!
//! * **Fat-pointer values**: `str` and `[]T` slices are `{ ptr, i64 len }` values. String
//!   literals ([`Const::Str`]) build the fat value from an interned global; [`Rvalue::StrPtr`]/
//!   [`Rvalue::StrLen`] project it (with a literal fast-path); [`Rvalue::MakeSlice`] packs one
//!   from a data pointer + length; [`Rvalue::AddrOf`] takes a place's address; and
//!   [`Proj::Index`] on a slice loads the data pointer before indexing. Array literals build
//!   inline [`TyKind::Array`] values via [`AggKind::Array`].
//!
//! Anything outside this subset returns [`BackendError::Lower`] with a precise message; the
//! remaining IR (maps and bump `cop`) lands later.

use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module as LlvmModule};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetData, TargetMachine,
    TargetTriple,
};
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, PointerType, StructType,
    VectorType,
};
use inkwell::values::{
    ArrayValue, BasicMetadataValueEnum, BasicValueEnum, FunctionValue, GlobalValue, IntValue,
    PointerValue, StructValue, VectorValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::{BackendError, EmitKind, EmitOptions, OptLevel};
use midir::*;

fn lower_err(e: BuilderError) -> BackendError {
    BackendError::Lower(e.to_string())
}

/// Whether an LLVM value is a compile-time constant. Used to pick the O(n) constant-aggregate
/// path when building `[]T` / struct values whose elements are all constants.
fn is_const_value(v: &BasicValueEnum) -> bool {
    match v {
        BasicValueEnum::IntValue(x) => x.is_const(),
        BasicValueEnum::FloatValue(x) => x.is_const(),
        BasicValueEnum::PointerValue(x) => x.is_const(),
        BasicValueEnum::ArrayValue(x) => x.is_const(),
        BasicValueEnum::StructValue(x) => x.is_const(),
        BasicValueEnum::VectorValue(x) => x.is_const(),
    }
}

/// Build a single constant array of `elem_ty` from constant element `vals` (all assumed constant,
/// per [`is_const_value`]). One `ConstantArray`, allocated once — the O(n) counterpart to chaining
/// `insertvalue` on a growing constant aggregate.
fn const_array<'c>(elem_ty: BasicTypeEnum<'c>, vals: &[BasicValueEnum<'c>]) -> ArrayValue<'c> {
    match elem_ty {
        BasicTypeEnum::IntType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_int_value()).collect();
            t.const_array(&e)
        }
        BasicTypeEnum::FloatType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_float_value()).collect();
            t.const_array(&e)
        }
        BasicTypeEnum::PointerType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_pointer_value()).collect();
            t.const_array(&e)
        }
        BasicTypeEnum::ArrayType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_array_value()).collect();
            t.const_array(&e)
        }
        BasicTypeEnum::StructType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_struct_value()).collect();
            t.const_array(&e)
        }
        BasicTypeEnum::VectorType(t) => {
            let e: Vec<_> = vals.iter().map(|v| v.into_vector_value()).collect();
            t.const_array(&e)
        }
    }
}

/// Whether a place's computed address will be dereferenced or merely taken — it decides how the
/// array/slice bounds check (issue #32) treats the one-past-the-end index.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    /// The address is loaded from / stored through, so an index must be strictly in bounds
    /// (`idx < len`) — the interpreter's "index out of bounds" rule.
    Access,
    /// Only the address is taken (`AddrOf`); the one-past-the-end slot (`idx == len`) is a legal
    /// address — the base of an empty tail sub-slice like `str.sub(s, len, len)` — so only a
    /// truly past-the-end index (`idx > len`) panics.
    Addr,
}

pub fn compile(module: &Module, opts: &EmitOptions) -> Result<Vec<u8>, BackendError> {
    let cx = Context::create();
    let llm = cx.create_module("bet");
    let builder = cx.create_builder();

    // Build the target machine up front so codegen can query type sizes/alignments from the
    // data layout (crib element layout: `bet_crib_new` / `bet_bump_alloc`).
    Target::initialize_native(&InitializationConfig::default()).map_err(BackendError::Target)?;
    let triple = match &opts.target {
        Some(t) => TargetTriple::create(t),
        None => TargetMachine::get_default_triple(),
    };
    let target = Target::from_triple(&triple).map_err(|e| BackendError::Target(e.to_string()))?;
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
    let td = tm.get_target_data();
    llm.set_triple(&triple);
    llm.set_data_layout(&td.get_data_layout());

    let mut cg = Cg {
        cx: &cx,
        m: module,
        llm,
        builder,
        td,
        tm,
        funcs: Vec::new(),
        externs: Vec::new(),
        crib_globals: Vec::new(),
    };
    cg.declare_externs()?;
    cg.declare_crib_globals();
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
    /// The target data layout — the source of truth for type sizes/alignments.
    td: TargetData,
    /// The target machine, used to emit the final object.
    tm: TargetMachine,
    /// Indexed by `FuncId`.
    funcs: Vec<FunctionValue<'c>>,
    /// Indexed by `ExternId`.
    externs: Vec<FunctionValue<'c>>,
    /// Indexed by `CribGlobalId` — the LLVM global holding each module-level crib's handle.
    crib_globals: Vec<GlobalValue<'c>>,
}

impl<'c> Cg<'c> {
    // --- types ---

    /// The one opaque-pointer type (LLVM 18 pointers are typeless).
    fn ptr_ty(&self) -> PointerType<'c> {
        self.cx.ptr_type(AddressSpace::default())
    }

    /// The fat-pointer layout `{ ptr, i64 len }` shared by `str` and `[]T` slice values.
    /// Field 0 is the data pointer; field 1 is the element/byte length.
    fn fat_ptr_ty(&self) -> StructType<'c> {
        self.cx
            .struct_type(&[self.ptr_ty().into(), self.cx.i64_type().into()], false)
    }

    /// The LLVM struct `{ i32 slot, i64 generation }` a `tag T` is carried and passed by value
    /// as — the exact 16-byte layout of `rt_abi::Tag` after the issue-#34 `u64`-generation
    /// widening (slot at offset 0, generation at offset 8, size 16, align 8). Every entry point
    /// that takes or returns a `Tag` (`bet_cop`, `bet_holla_check`, `bet_evict_slot`) uses this
    /// type, and it MUST be identical at all those call sites (`get_or_add` keeps the first
    /// declaration, so a divergent signature would silently mis-type the ABI).
    fn tag_ty(&self) -> StructType<'c> {
        self.cx.struct_type(
            &[self.cx.i32_type().into(), self.cx.i64_type().into()],
            false,
        )
    }

    fn basic_ty(&self, ty: TyId) -> Result<BasicTypeEnum<'c>, BackendError> {
        Ok(match self.m.ty(ty) {
            TyKind::Bool => self.cx.bool_type().into(),
            TyKind::Int { width, .. } => self.cx.custom_width_int_type(width.bits()).into(),
            TyKind::F32 => self.cx.f32_type().into(),
            TyKind::F64 => self.cx.f64_type().into(),
            TyKind::RawPtr => self.ptr_ty().into(),
            // A `tag T` is the 16-byte `{ i32 slot, i64 generation }` handle (issue #34),
            // passed by value across the `rt-abi` boundary as that struct.
            TyKind::Tag(_) => self.tag_ty().into(),
            // Crib handles, live refs, and function values are all raw pointers.
            TyKind::Crib(_)
            | TyKind::Ref(_)
            | TyKind::FnPtr(_)
            | TyKind::Map(_, _)
            | TyKind::Vec(_)
            | TyKind::Rng => self.ptr_ty().into(),
            TyKind::Struct(sid) => self.struct_llvm_ty(*sid)?.into(),
            TyKind::Sum(sid) => self.sum_llvm_ty(*sid).into(),
            // `str` and `[]T` slices are fat `{ ptr, len }` values.
            TyKind::Str | TyKind::Slice(_) => self.fat_ptr_ty().into(),
            // A fixed-size array is an inline value.
            TyKind::Array(elem, n) => self.basic_ty(*elem)?.array_type(*n as u32).into(),
            // A `soa` container is stored transposed — one parallel array per struct field.
            TyKind::Soa(inner) => self.soa_llvm_ty(*inner)?,
            // A `<N x elem>` SIMD vector — an LLVM vector type over the scalar element.
            TyKind::Simd { elem, lanes } => match self.basic_ty(*elem)? {
                BasicTypeEnum::IntType(it) => it.vec_type(*lanes).into(),
                BasicTypeEnum::FloatType(ft) => ft.vec_type(*lanes).into(),
                other => {
                    return Err(BackendError::Lower(format!(
                        "simd element must be a scalar int/float, got {other:?}"
                    )));
                }
            },
            TyKind::Tuple(elems) => {
                let fields: Vec<BasicTypeEnum> = elems
                    .iter()
                    .map(|&t| self.basic_ty(t))
                    .collect::<Result<_, _>>()?;
                self.cx.struct_type(&fields, false).into()
            }
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

    /// The transposed LLVM type for a `soa` container: one parallel array per struct field.
    /// `soa T[N]` lowers to `{ [N x f0], [N x f1], ... }` — the struct-of-arrays layout. So
    /// `soa[i].field(j)` addresses `field-array j`, then index `i` (see `place_ptr`). Only
    /// fixed-size arrays are wired so far; soa slices/vecs land in later phases.
    fn soa_llvm_ty(&self, inner: TyId) -> Result<BasicTypeEnum<'c>, BackendError> {
        enum SoaFieldKind {
            Array(u64),
            Slice,
            Vec,
        }
        // Per struct field: `soa T[N]` → `[N x Tj]` (inline); `soa []T` → a fat sub-slice
        // `{ptr,len}`; `soa vec[T]` → a runtime vec handle (a pointer).
        let (elem, kind) = match self.m.ty(inner) {
            TyKind::Array(e, n) => (*e, SoaFieldKind::Array(*n)),
            TyKind::Slice(e) => (*e, SoaFieldKind::Slice),
            TyKind::Vec(e) => (*e, SoaFieldKind::Vec),
            other => {
                return Err(BackendError::Lower(format!(
                    "soa layout for {other:?} isn't implemented yet"
                )));
            }
        };
        let sid = match self.m.ty(elem) {
            TyKind::Struct(sid) => *sid,
            other => {
                return Err(BackendError::Lower(format!(
                    "soa element must be a drip, got {other:?}"
                )));
            }
        };
        let def = self.m.struct_def(sid);
        let fields: Vec<BasicTypeEnum> = def
            .fields
            .iter()
            .map(|f| {
                Ok(match kind {
                    SoaFieldKind::Array(n) => self.basic_ty(f.ty)?.array_type(n as u32).into(),
                    SoaFieldKind::Slice => self.fat_ptr_ty().into(),
                    SoaFieldKind::Vec => self.ptr_ty().into(),
                })
            })
            .collect::<Result<_, BackendError>>()?;
        Ok(self.cx.struct_type(&fields, false).into())
    }

    /// An over-estimating count of 8-byte words needed to store a value of `ty`, used only to
    /// size a `moods` payload union. Over-estimation is always safe: the union is a byte blob
    /// that must merely be *big enough* for any variant. Max scalar/pointer alignment on our
    /// targets is 8, so an `[N x i64]` blob is correctly aligned for any payload it holds.
    fn ty_words(&self, ty: TyId) -> u32 {
        match self.m.ty(ty) {
            TyKind::Void => 0,
            TyKind::Bool
            | TyKind::Int { .. }
            | TyKind::F32
            | TyKind::F64
            | TyKind::RawPtr
            | TyKind::Crib(_)
            | TyKind::Ref(_)
            | TyKind::FnPtr(_)
            | TyKind::Map(_, _)
            | TyKind::Vec(_)
            | TyKind::Rng => 1,
            // A `tag T` is the 16-byte `{ i32, i64 }` handle (issue #34), and `str`/slices are
            // fat `(ptr, len)` values — two words each.
            TyKind::Tag(_) | TyKind::Str | TyKind::Slice(_) => 2,
            TyKind::Array(elem, n) => self.ty_words(*elem).saturating_mul(*n as u32),
            // A `soa` container occupies the same total storage as its AoS inner (only the
            // field order within is transposed), so its word count is the inner's.
            TyKind::Soa(inner) => self.ty_words(*inner),
            // A SIMD vector: one word per lane is a safe over-estimate (an `f32x4` is really 2).
            TyKind::Simd { elem, lanes } => self.ty_words(*elem).saturating_mul(*lanes),
            TyKind::Tuple(elems) => elems.iter().map(|&t| self.ty_words(t)).sum(),
            TyKind::Struct(sid) => self
                .m
                .struct_def(*sid)
                .fields
                .iter()
                .map(|f| self.ty_words(f.ty))
                .sum(),
            // A nested by-value sum is a tag word plus its own payload union.
            TyKind::Sum(sid) => 1 + self.sum_payload_words(*sid),
        }
    }

    /// The number of `i64` words the payload union of a `moods` needs: the max over variants
    /// of the (over-estimated) word size of that variant's whole payload. Zero for a sum whose
    /// every variant is nullary.
    fn sum_payload_words(&self, sid: SumId) -> u32 {
        self.m
            .sum_def(sid)
            .variants
            .iter()
            .map(|v| v.payload.iter().map(|&t| self.ty_words(t)).sum::<u32>())
            .max()
            .unwrap_or(0)
    }

    /// The LLVM struct laid over a single variant's payload fields, in their natural layout.
    /// Constructing and reading a variant place each field through this struct, so a payload
    /// field wider than a word lands at the right offset within the payload union.
    fn variant_payload_struct(
        &self,
        sid: SumId,
        variant: u32,
    ) -> Result<StructType<'c>, BackendError> {
        let v = &self.m.sum_def(sid).variants[variant as usize];
        let fields: Vec<BasicTypeEnum> = v
            .payload
            .iter()
            .map(|&t| self.basic_ty(t))
            .collect::<Result<_, _>>()?;
        Ok(self.cx.struct_type(&fields, false))
    }

    /// The LLVM layout for a `moods`: `{ i32 tag, [W x i64] payload }`, where `W` sizes the
    /// payload union to the widest variant. A sum whose every variant is nullary is just
    /// `{ i32 tag }`.
    fn sum_llvm_ty(&self, sid: SumId) -> StructType<'c> {
        let tag = self.cx.i32_type();
        let words = self.sum_payload_words(sid);
        if words == 0 {
            self.cx.struct_type(&[tag.into()], false)
        } else {
            let payload = self.cx.i64_type().array_type(words);
            self.cx.struct_type(&[tag.into(), payload.into()], false)
        }
    }

    fn fn_type(&self, params: &[TyId], rets: &[TyId]) -> Result<FunctionType<'c>, BackendError> {
        let ps: Vec<BasicMetadataTypeEnum> = params
            .iter()
            .map(|&t| self.basic_ty(t).map(Into::into))
            .collect::<Result<_, _>>()?;
        Ok(match rets {
            [] => self.cx.void_type().fn_type(&ps, false),
            [one] => self.basic_ty(*one)?.fn_type(&ps, false),
            // Multi-value returns are carried as one anonymous struct (a `Tuple`).
            many => self.tuple_llvm_ty(many)?.fn_type(&ps, false),
        })
    }

    /// The anonymous LLVM struct that carries a multi-value return / `Tuple`.
    fn tuple_llvm_ty(&self, elems: &[TyId]) -> Result<StructType<'c>, BackendError> {
        let fields: Vec<BasicTypeEnum> = elems
            .iter()
            .map(|&t| self.basic_ty(t))
            .collect::<Result<_, _>>()?;
        Ok(self.cx.struct_type(&fields, false))
    }

    // --- declarations ---

    fn declare_externs(&mut self) -> Result<(), BackendError> {
        for ext in self.m.externs() {
            // Reuse an existing declaration when the same C symbol appears under more than one
            // midir signature (e.g. `bet_map_new` per `stash[K, V]` monomorphization — all
            // coerce to the same `ptr fn(i64)` at the ABI). One LLVM function per name.
            let f = match self.llm.get_function(&ext.name) {
                Some(existing) => existing,
                None => {
                    let fty = self.fn_type(&ext.sig.params, &ext.sig.rets)?;
                    self.llm
                        .add_function(&ext.name, fty, Some(Linkage::External))
                }
            };
            self.externs.push(f);
        }
        Ok(())
    }

    /// Reserve one LLVM global per module-level `crib`, holding its runtime `CribHandle`
    /// (a pointer), zero-initialized. Filled in at startup by `synthesize_main`.
    fn declare_crib_globals(&mut self) {
        for c in self.m.crib_globals() {
            let g = self
                .llm
                .add_global(self.ptr_ty(), None, &format!("crib.{}", c.name));
            g.set_linkage(Linkage::Internal);
            g.set_initializer(&self.ptr_ty().const_null());
            self.crib_globals.push(g);
        }
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
                // Projected lvalue: compute its address and store through it (a write ⇒ the
                // index must be strictly in bounds).
                let (ptr, _ty) = self.place_ptr(func, locals, place, IndexMode::Access)?;
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
            // `evict tag in crib` — free one slot. The tag rides by value as the
            // `{ i32, i64 }` struct (issue #34), exactly as in `bet_holla_check`.
            Stmt::EvictSlot { crib, tag } => {
                let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
                let tag_v = self.lower_operand(func, locals, tag)?;
                let evict_slot = self.get_or_add(
                    "bet_evict_slot",
                    self.cx
                        .void_type()
                        .fn_type(&[self.ptr_ty().into(), self.tag_ty().into()], false),
                );
                self.builder
                    .build_call(evict_slot, &[crib_v.into(), tag_v.into()], "")
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
            Rvalue::BinOp(op, a, b, mode) => {
                Ok(Some(self.lower_binop(func, locals, *op, a, b, *mode)?))
            }
            Rvalue::UnOp(op, a) => Ok(Some(self.lower_unop(func, locals, *op, a)?)),
            Rvalue::Cast(op, ty, kind) => Ok(Some(self.lower_cast(func, locals, op, *ty, *kind)?)),
            Rvalue::StrPtr(op) => Ok(Some(self.str_projection(func, locals, op, 0)?)),
            Rvalue::StrLen(op) => Ok(Some(self.str_projection(func, locals, op, 1)?)),
            // A slice shares the fat `{ ptr, len }` layout, so the same projection reads it.
            Rvalue::SlicePtr(op) => Ok(Some(self.str_projection(func, locals, op, 0)?)),
            Rvalue::SliceLen(op) => Ok(Some(self.str_projection(func, locals, op, 1)?)),
            Rvalue::AddrOf(place) => {
                // Taking an address only — the one-past-the-end slot is a legal base (e.g. the
                // empty tail sub-slice `str.sub(s, len, len)`), so bounds-check inclusively.
                let (ptr, _ty) = self.place_ptr(func, locals, place, IndexMode::Addr)?;
                Ok(Some(ptr.into()))
            }
            Rvalue::MakeSlice { data, len, .. } => {
                let d = self.lower_operand(func, locals, data)?;
                let l = self.lower_operand(func, locals, len)?;
                Ok(Some(self.build_fat_ptr(d, l)?))
            }
            Rvalue::CribNew { elem, capacity } => Ok(Some(self.lower_crib_new(*elem, *capacity)?)),
            Rvalue::CribGlobal(id) => {
                let g = self.crib_globals[id.index()];
                let v = self
                    .builder
                    .build_load(self.ptr_ty(), g.as_pointer_value(), "crib.g")
                    .map_err(lower_err)?;
                Ok(Some(v))
            }
            Rvalue::SizeOf(ty) => {
                let bt = self.basic_ty(*ty)?;
                let size = self.td.get_store_size(&bt);
                Ok(Some(self.cx.i64_type().const_int(size, false).into()))
            }
            Rvalue::MakeStr { data, len } => {
                let d = self.lower_operand(func, locals, data)?;
                let l = self.lower_operand(func, locals, len)?;
                Ok(Some(self.build_fat_ptr(d, l)?))
            }
            Rvalue::Call(callee, args) => self.lower_call(func, locals, callee, args),
            Rvalue::Cop(crib, init) => self.lower_cop(func, locals, crib, init),
            Rvalue::Trust(crib, tag) => Ok(Some(self.lower_trust(func, locals, crib, tag)?)),
            Rvalue::Aggregate(kind, ops) => {
                Ok(Some(self.lower_aggregate(func, locals, kind, ops)?))
            }
            Rvalue::Simd { op, args, ty } => {
                Ok(Some(self.lower_simd_op(func, locals, op, args, *ty)?))
            }
            Rvalue::Discriminant(op) => Ok(Some(self.lower_discriminant(func, locals, op)?)),
        }
    }

    // --- aggregates & sums ---

    /// Build a by-value aggregate: a `drip` struct, a tuple, or a `moods` sum value.
    fn lower_aggregate(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        kind: &AggKind,
        ops: &[Operand],
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        match kind {
            AggKind::Struct(sid) => {
                let sty = self.struct_llvm_ty(*sid)?;
                self.build_struct_value(func, locals, sty, ops)
            }
            AggKind::Tuple => {
                // Recover the element types from each operand to form the tuple's struct type.
                let mut elems = Vec::with_capacity(ops.len());
                for op in ops {
                    let ty = self.operand_ty(func, op)?.ok_or_else(|| {
                        BackendError::Lower("tuple element has no statically-known type".into())
                    })?;
                    elems.push(ty);
                }
                let sty = self.tuple_llvm_ty(&elems)?;
                self.build_struct_value(func, locals, sty, ops)
            }
            AggKind::Array(elem) => {
                let elem_ty = self.basic_ty(*elem)?;
                let n = ops.len();
                let arr_ty = elem_ty.array_type(n as u32);
                // Lower the elements. A zero-defaulted `cop T{}` field emits `vec![z; n]` — n
                // identical operands — so lower a splat once and reuse it (values are Copy), which
                // keeps even the load count O(1) for a big `[N]` field.
                let vals: Vec<BasicValueEnum<'c>> = if n > 1 && ops.iter().all(|o| *o == ops[0]) {
                    let v = self.lower_operand(func, locals, &ops[0])?;
                    vec![v; n]
                } else {
                    let mut vs = Vec::with_capacity(n);
                    for op in ops {
                        vs.push(self.lower_operand(func, locals, op)?);
                    }
                    vs
                };
                // All-constant elements (zero-defaulted scalar arrays, literal arrays): assemble
                // ONE constant array, O(n).
                if vals.iter().all(is_const_value) {
                    return Ok(const_array(elem_ty, &vals).into());
                }
                // Otherwise (e.g. an array of zeroed structs, whose element is a runtime `load`):
                // materialize through a stack slot with one store per element — O(n). Chaining
                // `insertvalue` on the whole array value instead is O(n^2) in the aggregate size,
                // which makes a large `[N]` field of structs explode (GameState's `TagBox[32768]`
                // drove a single `cop GameState{}` past 24 GB).
                let slot = self
                    .builder
                    .build_alloca(arr_ty, "arr.tmp")
                    .map_err(lower_err)?;
                let i32t = self.cx.i32_type();
                for (i, v) in vals.into_iter().enumerate() {
                    // `slot` points at the array, i.e. at element 0, so a single-index GEP by
                    // element type lands on `slot[i]` (opaque pointers make the array/elem-0
                    // pointers identical).
                    let idx = i32t.const_int(i as u64, false);
                    let gep = self.gep_index_elem(elem_ty, slot, idx)?;
                    self.builder.build_store(gep, v).map_err(lower_err)?;
                }
                self.builder
                    .build_load(arr_ty, slot, "arr.val")
                    .map_err(lower_err)
            }
            AggKind::Sum { sum, variant } => {
                self.build_sum_value(func, locals, *sum, *variant, ops)
            }
            // `<N x elem>` SIMD construction from N lane operands.
            AggKind::Simd(elem) => {
                let elem_ty = self.basic_ty(*elem)?;
                let n = ops.len() as u32;
                let vec_ty = self.vec_ty_of(elem_ty, n)?;
                let mut vals = Vec::with_capacity(ops.len());
                for op in ops {
                    vals.push(self.lower_operand(func, locals, op)?);
                }
                if vals.iter().all(is_const_value) {
                    return Ok(VectorType::const_vector(&vals).into());
                }
                let mut acc = vec_ty.get_undef();
                let i32t = self.cx.i32_type();
                for (i, v) in vals.into_iter().enumerate() {
                    let idx = i32t.const_int(i as u64, false);
                    acc = self
                        .builder
                        .build_insert_element(acc, v, idx, "vins")
                        .map_err(lower_err)?;
                }
                Ok(acc.into())
            }
        }
    }

    /// The LLVM `<n x elem>` vector type for a scalar element type.
    fn vec_ty_of(&self, elem: BasicTypeEnum<'c>, n: u32) -> Result<VectorType<'c>, BackendError> {
        match elem {
            BasicTypeEnum::IntType(it) => Ok(it.vec_type(n)),
            BasicTypeEnum::FloatType(ft) => Ok(ft.vec_type(n)),
            other => Err(BackendError::Lower(format!(
                "simd element must be a scalar int/float, got {other:?}"
            ))),
        }
    }

    /// Broadcast a scalar to every lane of `vec_ty` (`n` lanes): insert at lane 0, then a
    /// `shufflevector` with an all-zero mask.
    fn build_splat(
        &self,
        scalar: BasicValueEnum<'c>,
        vec_ty: VectorType<'c>,
        n: u32,
    ) -> Result<VectorValue<'c>, BackendError> {
        let i32t = self.cx.i32_type();
        let undef = vec_ty.get_undef();
        let with0 = self
            .builder
            .build_insert_element(undef, scalar, i32t.const_zero(), "splat0")
            .map_err(lower_err)?;
        let mask_elems: Vec<IntValue> = (0..n).map(|_| i32t.const_zero()).collect();
        let mask = VectorType::const_vector(&mask_elems);
        self.builder
            .build_shuffle_vector(with0, undef, mask, "splat")
            .map_err(lower_err)
    }

    /// `sqrt(x)` via the `llvm.sqrt` intrinsic (the one intrinsic the backend uses). Correctly
    /// rounded, so it matches the interpreter's `f32::sqrt`/`f64::sqrt`.
    fn build_sqrt(
        &self,
        x: inkwell::values::FloatValue<'c>,
    ) -> Result<inkwell::values::FloatValue<'c>, BackendError> {
        let intr = inkwell::intrinsics::Intrinsic::find("llvm.sqrt")
            .ok_or_else(|| BackendError::Lower("llvm.sqrt intrinsic not found".into()))?;
        let fty: BasicTypeEnum = x.get_type().into();
        let decl = intr
            .get_declaration(&self.llm, &[fty])
            .ok_or_else(|| BackendError::Lower("could not declare llvm.sqrt".into()))?;
        let cs = self
            .builder
            .build_call(decl, &[x.into()], "sqrt")
            .map_err(lower_err)?;
        Ok(cs
            .try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower("llvm.sqrt returned no value".into()))?
            .into_float_value())
    }

    /// The (element `TyId`, lane count) of a `TyKind::Simd`.
    fn simd_parts(&self, ty: TyId) -> Result<(TyId, u32), BackendError> {
        match self.m.ty(ty) {
            TyKind::Simd { elem, lanes } => Ok((*elem, *lanes)),
            other => Err(BackendError::Lower(format!(
                "expected a simd type, got {other:?}"
            ))),
        }
    }

    /// Lower a non-arithmetic SIMD op (`Rvalue::Simd`): construction/broadcast, lane extraction,
    /// min/max/abs, and the horizontal reductions. Element-wise `+ - * / >> <<` are lowered via
    /// `lower_binop` (two vector operands) instead.
    fn lower_simd_op(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &SimdOp,
        args: &[Operand],
        ty: TyId,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        match op {
            SimdOp::Splat => {
                let scalar = self.lower_operand(func, locals, &args[0])?;
                let (elem, n) = self.simd_parts(ty)?;
                let vec_ty = self.vec_ty_of(self.basic_ty(elem)?, n)?;
                Ok(self.build_splat(scalar, vec_ty, n)?.into())
            }
            SimdOp::Lane(i) => {
                let v = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let idx = self.cx.i32_type().const_int(*i as u64, false);
                Ok(b.build_extract_element(v, idx, "lane").map_err(lower_err)?)
            }
            SimdOp::Min | SimdOp::Max => {
                let a = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let bb = self
                    .lower_operand(func, locals, &args[1])?
                    .into_vector_value();
                let is_float =
                    matches!(a.get_type().get_element_type(), BasicTypeEnum::FloatType(_));
                let want_min = matches!(op, SimdOp::Min);
                let mask = if is_float {
                    let pred = if want_min {
                        FloatPredicate::OLT
                    } else {
                        FloatPredicate::OGT
                    };
                    b.build_float_compare(pred, a, bb, "vcmp")
                        .map_err(lower_err)?
                } else {
                    let signed = self.simd_signed(func, &args[0], &args[1]);
                    let pred = match (want_min, signed) {
                        (true, true) => IntPredicate::SLT,
                        (true, false) => IntPredicate::ULT,
                        (false, true) => IntPredicate::SGT,
                        (false, false) => IntPredicate::UGT,
                    };
                    b.build_int_compare(pred, a, bb, "vcmp")
                        .map_err(lower_err)?
                };
                Ok(b.build_select(mask, a, bb, "vsel").map_err(lower_err)?)
            }
            SimdOp::Abs => {
                let v = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                if matches!(v.get_type().get_element_type(), BasicTypeEnum::FloatType(_)) {
                    let zero = v.get_type().const_zero();
                    let neg = b.build_float_neg(v, "vfneg").map_err(lower_err)?;
                    let mask = b
                        .build_float_compare(FloatPredicate::OLT, v, zero, "vlt0")
                        .map_err(lower_err)?;
                    Ok(b.build_select(mask, neg, v, "vabs").map_err(lower_err)?)
                } else {
                    let zero = v.get_type().const_zero();
                    let neg = b.build_int_sub(zero, v, "vneg").map_err(lower_err)?;
                    let mask = b
                        .build_int_compare(IntPredicate::SLT, v, zero, "vlt0")
                        .map_err(lower_err)?;
                    Ok(b.build_select(mask, neg, v, "vabs").map_err(lower_err)?)
                }
            }
            SimdOp::Dot => {
                let a = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let bb = self
                    .lower_operand(func, locals, &args[1])?
                    .into_vector_value();
                let is_float =
                    matches!(a.get_type().get_element_type(), BasicTypeEnum::FloatType(_));
                let prod: VectorValue = if is_float {
                    b.build_float_mul(a, bb, "vmul").map_err(lower_err)?
                } else {
                    b.build_int_mul(a, bb, "vmul").map_err(lower_err)?
                };
                self.reduce_add(prod, is_float)
            }
            SimdOp::Sum => {
                let v = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let is_float =
                    matches!(v.get_type().get_element_type(), BasicTypeEnum::FloatType(_));
                self.reduce_add(v, is_float)
            }
            SimdOp::Length => {
                let v = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let prod = b.build_float_mul(v, v, "vmul").map_err(lower_err)?;
                let dot = self.reduce_add(prod, true)?.into_float_value();
                Ok(self.build_sqrt(dot)?.into())
            }
            SimdOp::Norm => {
                let v = self
                    .lower_operand(func, locals, &args[0])?
                    .into_vector_value();
                let prod = b.build_float_mul(v, v, "vmul").map_err(lower_err)?;
                let dot = self.reduce_add(prod, true)?.into_float_value();
                let len = self.build_sqrt(dot)?;
                let one = len.get_type().const_float(1.0);
                let inv = b.build_float_div(one, len, "vinv").map_err(lower_err)?;
                let (elem, n) = self.simd_parts(ty)?;
                let vec_ty = self.vec_ty_of(self.basic_ty(elem)?, n)?;
                let inv_vec = self.build_splat(inv.into(), vec_ty, n)?;
                Ok(b.build_float_mul(v, inv_vec, "vnorm")
                    .map_err(lower_err)?
                    .into())
            }
        }
    }

    /// Horizontal sum of a vector's lanes, folded left in lane order 0→N-1 (the same order the
    /// interpreter uses, so float reductions are bit-identical).
    fn reduce_add(
        &self,
        v: VectorValue<'c>,
        is_float: bool,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        let n = v.get_type().get_size();
        let i32t = self.cx.i32_type();
        let mut acc = b
            .build_extract_element(v, i32t.const_zero(), "l0")
            .map_err(lower_err)?;
        for i in 1..n {
            let lane = b
                .build_extract_element(v, i32t.const_int(i as u64, false), "li")
                .map_err(lower_err)?;
            acc = if is_float {
                b.build_float_add(acc.into_float_value(), lane.into_float_value(), "hadd")
                    .map_err(lower_err)?
                    .into()
            } else {
                b.build_int_add(acc.into_int_value(), lane.into_int_value(), "hadd")
                    .map_err(lower_err)?
                    .into()
            };
        }
        Ok(acc)
    }

    /// Materialize a struct/tuple value by inserting each field into an undef aggregate.
    fn build_struct_value(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        sty: StructType<'c>,
        ops: &[Operand],
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let mut vals = Vec::with_capacity(ops.len());
        for op in ops {
            vals.push(self.lower_operand(func, locals, op)?);
        }
        // Fast path: an all-constant struct/tuple (e.g. a zero-defaulted `cop T{}`, or one whose
        // fields are themselves constant aggregates) becomes a single constant — O(fields) — so a
        // large struct never pays the O(n^2) of chaining `insertvalue` on a constant aggregate.
        if vals.iter().all(is_const_value) {
            return Ok(sty.const_named_struct(&vals).into());
        }
        let mut agg = sty.get_undef();
        for (i, v) in vals.into_iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(agg, v, i as u32, "agg")
                .map_err(lower_err)?
                .into_struct_value();
        }
        Ok(agg.into())
    }

    /// Store a `moods` value into `storage` (a pointer to sum-typed memory): set the
    /// discriminant, then store each payload field through the active variant's natural struct
    /// layout (so a field wider than a word lands at the right offset within the payload union).
    fn store_sum_into(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        storage: PointerValue<'c>,
        sum: SumId,
        variant: u32,
        ops: &[Operand],
    ) -> Result<(), BackendError> {
        let sty = self.sum_llvm_ty(sum);
        let tag_ptr = self
            .builder
            .build_struct_gep(sty, storage, 0, "sum.tag")
            .map_err(|_| BackendError::Lower("sum tag gep".into()))?;
        let tag = self.cx.i32_type().const_int(variant as u64, false);
        self.builder.build_store(tag_ptr, tag).map_err(lower_err)?;
        if !ops.is_empty() {
            let vps = self.variant_payload_struct(sum, variant)?;
            let payload_ptr = self
                .builder
                .build_struct_gep(sty, storage, 1, "sum.payload")
                .map_err(|_| BackendError::Lower("sum payload gep".into()))?;
            for (j, op) in ops.iter().enumerate() {
                let v = self.lower_operand(func, locals, op)?;
                let elem = self
                    .builder
                    .build_struct_gep(vps, payload_ptr, j as u32, "sum.field")
                    .map_err(|_| BackendError::Lower(format!("sum payload field {j} gep")))?;
                self.builder.build_store(elem, v).map_err(lower_err)?;
            }
        }
        Ok(())
    }

    /// Materialize a by-value `moods` value via a scratch alloca (a `store_sum_into` + reload).
    fn build_sum_value(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        sum: SumId,
        variant: u32,
        ops: &[Operand],
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let sty = self.sum_llvm_ty(sum);
        let slot = self.builder.build_alloca(sty, "sum").map_err(lower_err)?;
        self.store_sum_into(func, locals, slot, sum, variant, ops)?;
        let val = self
            .builder
            .build_load(sty, slot, "sum.val")
            .map_err(lower_err)?;
        Ok(val)
    }

    /// Read a sum value's discriminant (its `i32` tag) via `extractvalue`.
    fn lower_discriminant(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let v = self.lower_operand(func, locals, op)?;
        let sv = v.into_struct_value();
        self.builder
            .build_extract_value(sv, 0, "disc")
            .map_err(lower_err)
    }

    /// GEP to a dynamically-indexed element of a slice/array (`base[i]`).
    #[allow(unsafe_code)] // inkwell's GEP builders are `unsafe`; bounds are the frontend's job.
    fn gep_index_elem(
        &self,
        elem_ty: BasicTypeEnum<'c>,
        base: PointerValue<'c>,
        idx: IntValue<'c>,
    ) -> Result<PointerValue<'c>, BackendError> {
        unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, base, &[idx], "index")
                .map_err(lower_err)
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
        mode: ArithMode,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let lv = self.lower_operand(func, locals, a)?;
        let rv = self.lower_operand(func, locals, b)?;
        match (lv, rv) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let signed = self.int_signed(func, a, b);
                self.lower_int_binop(op, l, r, signed, mode)
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                self.lower_float_binop(op, l, r)
            }
            // Element-wise SIMD arithmetic/shifts: LLVM's `build_int_*`/`build_float_*` are
            // element-wise over vector operands (the inkwell math-value traits accept `VectorValue`),
            // so `f32x4 + f32x4` emits a single `fadd <4 x float>`.
            (BasicValueEnum::VectorValue(l), BasicValueEnum::VectorValue(r)) => {
                let signed = self.simd_signed(func, a, b);
                self.lower_vec_binop(op, l, r, signed)
            }
            // Aggregate operands reach here for `tag`/`ref` equality: `Tag` lowers to the struct
            // `{ i32 slot, i64 generation }`, so `tag == ghosted` / `t1 != t2` is a field-wise compare
            // (before the generation counter widened to u64, a Tag was a scalar `i64` and this went
            // through the Int arm). Only `==`/`!=` are defined on aggregates.
            (BasicValueEnum::StructValue(l), BasicValueEnum::StructValue(r)) => {
                self.lower_agg_eq(op, l, r)
            }
            _ => Err(BackendError::Lower(
                "binary op on non-scalar or mismatched operands".into(),
            )),
        }
    }

    /// Lower `==` / `!=` on two aggregate (struct) operands — the `tag`/`ref` equality case, where
    /// `Tag` is `{ i32 slot, i64 generation }`. Compares each field and AND-s the results (negated
    /// for `!=`). Non-equality ops, mismatched struct types, or non-scalar fields stay a lowering
    /// error, matching the interpreter, which only defines equality on tags.
    fn lower_agg_eq(
        &self,
        op: BinOp,
        l: StructValue<'c>,
        r: StructValue<'c>,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let want_eq = match op {
            BinOp::Eq => true,
            BinOp::Ne => false,
            _ => {
                return Err(BackendError::Lower(
                    "only == / != are defined on aggregate (tag) operands".into(),
                ));
            }
        };
        if l.get_type() != r.get_type() {
            return Err(BackendError::Lower(
                "equality on mismatched aggregate operands".into(),
            ));
        }
        let b = &self.builder;
        let mut all_eq: Option<IntValue<'c>> = None;
        for i in 0..l.get_type().count_fields() {
            let lf = b.build_extract_value(l, i, "lf").map_err(lower_err)?;
            let rf = b.build_extract_value(r, i, "rf").map_err(lower_err)?;
            let feq = match (lf, rf) {
                (BasicValueEnum::IntValue(x), BasicValueEnum::IntValue(y)) => b
                    .build_int_compare(IntPredicate::EQ, x, y, "feq")
                    .map_err(lower_err)?,
                (BasicValueEnum::FloatValue(x), BasicValueEnum::FloatValue(y)) => b
                    .build_float_compare(FloatPredicate::OEQ, x, y, "feq")
                    .map_err(lower_err)?,
                _ => {
                    return Err(BackendError::Lower(
                        "equality on an aggregate with a non-scalar field".into(),
                    ));
                }
            };
            all_eq = Some(match all_eq {
                None => feq,
                Some(prev) => b.build_and(prev, feq, "aeq").map_err(lower_err)?,
            });
        }
        // A field-less struct compares equal; every real Tag has fields.
        let all_eq = all_eq.unwrap_or_else(|| self.cx.bool_type().const_int(1, false));
        let result = if want_eq {
            all_eq
        } else {
            b.build_not(all_eq, "ane").map_err(lower_err)?
        };
        Ok(result.into())
    }

    /// Element-wise arithmetic/shift on two same-type SIMD vectors. Comparisons and float
    /// bitwise/shift are not part of the vector-operator surface (min/max are [`SimdOp`]s).
    fn lower_vec_binop(
        &self,
        op: BinOp,
        l: VectorValue<'c>,
        r: VectorValue<'c>,
        signed: bool,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        let is_float = matches!(l.get_type().get_element_type(), BasicTypeEnum::FloatType(_));
        let v: BasicValueEnum = if is_float {
            match op {
                BinOp::Add => b.build_float_add(l, r, "fadd").map_err(lower_err)?.into(),
                BinOp::Sub => b.build_float_sub(l, r, "fsub").map_err(lower_err)?.into(),
                BinOp::Mul => b.build_float_mul(l, r, "fmul").map_err(lower_err)?.into(),
                BinOp::Div => b.build_float_div(l, r, "fdiv").map_err(lower_err)?.into(),
                _ => {
                    return Err(BackendError::Lower(format!(
                        "unsupported SIMD float operator {op:?}"
                    )));
                }
            }
        } else {
            match op {
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
                BinOp::Shr => b
                    .build_right_shift(l, r, signed, "shr")
                    .map_err(lower_err)?
                    .into(),
                _ => {
                    return Err(BackendError::Lower(format!(
                        "unsupported SIMD int operator {op:?}"
                    )));
                }
            }
        };
        Ok(v)
    }

    /// Whether a SIMD binop's integer lanes are signed (arithmetic vs logical shift, signed div).
    /// Reads the element type off the operands' `Simd { elem }` midir type, like [`Self::int_signed`].
    fn simd_signed(&self, func: &Func, a: &Operand, b: &Operand) -> bool {
        for op in [a, b] {
            if let Ok(Some(ty)) = self.operand_ty(func, op)
                && let TyKind::Simd { elem, .. } = self.m.ty(ty)
                && let TyKind::Int { signed, .. } = self.m.ty(*elem)
            {
                return *signed;
            }
        }
        false
    }

    // --- runtime safety guards (issues #32, #36) ---

    /// Emit a runtime guard: if `cond` (an `i1`) is set, branch to a fresh block that calls
    /// `bet_panic(msg)` and is `unreachable`; otherwise fall through into a fresh continuation
    /// block. Splits the current basic block and leaves the builder positioned at the
    /// continuation, so lowering resumes on the safe path. Modeled on the `Terminator::Panic`
    /// arm and the `HollaCheck` branch pattern — the shared primitive behind the div-by-zero,
    /// overflow-trap, and bounds-check guards.
    fn guard_panic(&self, cond: IntValue<'c>, msg: &str) -> Result<(), BackendError> {
        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| BackendError::Lower("panic guard outside a function".into()))?;
        let panic_bb = self.cx.append_basic_block(cur_fn, "panic");
        let cont_bb = self.cx.append_basic_block(cur_fn, "cont");
        self.builder
            .build_conditional_branch(cond, panic_bb, cont_bb)
            .map_err(lower_err)?;
        // Panic edge: `bet_panic(msg, len)` then `unreachable` (same shape as Terminator::Panic).
        self.builder.position_at_end(panic_bb);
        let g = self
            .builder
            .build_global_string_ptr(msg, "panicmsg")
            .map_err(lower_err)?;
        let len = self.cx.i64_type().const_int(msg.len() as u64, false);
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
        // Safe edge: resume lowering here.
        self.builder.position_at_end(cont_bb);
        Ok(())
    }

    /// Zero-extend an integer to `i64` (a no-op if it already is), so a bounds compare can be
    /// done in one width regardless of the index/length types.
    fn zext_to_i64(&self, v: IntValue<'c>) -> Result<IntValue<'c>, BackendError> {
        if v.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_z_extend(v, self.cx.i64_type(), "zext64")
                .map_err(lower_err)
        } else {
            Ok(v)
        }
    }

    /// Bounds-check an array/slice index before the element GEP, matching the interpreter's
    /// "index out of bounds" panic (issue #32). An [`IndexMode::Access`] must be strictly in
    /// bounds (`idx >= len` panics); an [`IndexMode::Addr`] (a bare `AddrOf`) may name the
    /// one-past-the-end slot, so only `idx > len` panics. Both operands widen to `i64` so a
    /// narrow index and an `i64` slice length compare in one type.
    fn bounds_check(
        &self,
        idx: IntValue<'c>,
        len: IntValue<'c>,
        mode: IndexMode,
    ) -> Result<(), BackendError> {
        let idx = self.zext_to_i64(idx)?;
        let len = self.zext_to_i64(len)?;
        let pred = match mode {
            IndexMode::Access => IntPredicate::UGE,
            IndexMode::Addr => IntPredicate::UGT,
        };
        let oob = self
            .builder
            .build_int_compare(pred, idx, len, "oob")
            .map_err(lower_err)?;
        self.guard_panic(oob, "index out of bounds")
    }

    /// Mask a shift amount to `[0, bit_width)` (`amt & (bit_width - 1)`) before a shift, matching
    /// the interpreter's `wrapping_shl`/`wrapping_shr` and dodging LLVM's shift-past-width UB. Our
    /// integer widths are powers of two, so `bit_width - 1` is the exact mask.
    fn mask_shift_amount(
        &self,
        shifted: IntValue<'c>,
        amt: IntValue<'c>,
    ) -> Result<IntValue<'c>, BackendError> {
        let bits = shifted.get_type().get_bit_width();
        let mask = amt.get_type().const_int((bits - 1) as u64, false);
        self.builder
            .build_and(amt, mask, "shmask")
            .map_err(lower_err)
    }

    /// Emit an overflow-checked `add`/`sub`/`mul` via `llvm.{s,u}{add,sub,mul}.with.overflow`
    /// (`name`), panicking on the overflow bit (`Trap` arith mode). The intrinsic returns
    /// `{ iN result, i1 overflow }`; panic on the bit, yield the result.
    fn checked_arith(
        &self,
        name: &str,
        l: IntValue<'c>,
        r: IntValue<'c>,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let intr = inkwell::intrinsics::Intrinsic::find(name)
            .ok_or_else(|| BackendError::Lower(format!("{name} intrinsic not found")))?;
        let ity: BasicTypeEnum = l.get_type().into();
        let decl = intr
            .get_declaration(&self.llm, &[ity])
            .ok_or_else(|| BackendError::Lower(format!("could not declare {name}")))?;
        let agg = self
            .builder
            .build_call(decl, &[l.into(), r.into()], "ovf")
            .map_err(lower_err)?
            .try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower(format!("{name} returned no value")))?
            .into_struct_value();
        let res = self
            .builder
            .build_extract_value(agg, 0, "ovf.res")
            .map_err(lower_err)?;
        let bit = self
            .builder
            .build_extract_value(agg, 1, "ovf.bit")
            .map_err(lower_err)?
            .into_int_value();
        self.guard_panic(bit, "arithmetic overflow")?;
        Ok(res)
    }

    /// Guard an integer `div`/`rem`: panic on a zero divisor, and (for signed division) on the
    /// `INT_MIN / -1` overflow — both a hardware trap / LLVM UB — mapping each to the
    /// interpreter's panic.
    fn guard_div(
        &self,
        l: IntValue<'c>,
        r: IntValue<'c>,
        signed: bool,
    ) -> Result<(), BackendError> {
        let zero = r.get_type().const_zero();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, r, zero, "divzero")
            .map_err(lower_err)?;
        self.guard_panic(is_zero, "divide by zero")?;
        if signed {
            let bits = l.get_type().get_bit_width();
            let int_min = l.get_type().const_int(1u64 << (bits - 1), false);
            let neg_one = r.get_type().const_all_ones();
            let l_min = self
                .builder
                .build_int_compare(IntPredicate::EQ, l, int_min, "lmin")
                .map_err(lower_err)?;
            let r_neg1 = self
                .builder
                .build_int_compare(IntPredicate::EQ, r, neg_one, "rneg1")
                .map_err(lower_err)?;
            let both = self
                .builder
                .build_and(l_min, r_neg1, "divovf")
                .map_err(lower_err)?;
            self.guard_panic(both, "divide overflow")?;
        }
        Ok(())
    }

    fn lower_int_binop(
        &self,
        op: BinOp,
        l: IntValue<'c>,
        r: IntValue<'c>,
        signed: bool,
        mode: ArithMode,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let b = &self.builder;
        // `Trap` arith mode (signed arithmetic) traps on overflow via the checked intrinsics;
        // `Wrap`/`Na` stay plain wrapping arithmetic. Div/rem always guard the zero-divisor
        // (and signed `INT_MIN / -1`) UB, and shifts always mask the amount — the safety guards
        // are unconditional, matching the interpreter (issues #32, #36).
        let trap = mode == ArithMode::Trap;
        let v: BasicValueEnum = match op {
            BinOp::Add if trap => {
                let name = if signed {
                    "llvm.sadd.with.overflow"
                } else {
                    "llvm.uadd.with.overflow"
                };
                self.checked_arith(name, l, r)?
            }
            BinOp::Sub if trap => {
                let name = if signed {
                    "llvm.ssub.with.overflow"
                } else {
                    "llvm.usub.with.overflow"
                };
                self.checked_arith(name, l, r)?
            }
            BinOp::Mul if trap => {
                let name = if signed {
                    "llvm.smul.with.overflow"
                } else {
                    "llvm.umul.with.overflow"
                };
                self.checked_arith(name, l, r)?
            }
            BinOp::Add => b.build_int_add(l, r, "add").map_err(lower_err)?.into(),
            BinOp::Sub => b.build_int_sub(l, r, "sub").map_err(lower_err)?.into(),
            BinOp::Mul => b.build_int_mul(l, r, "mul").map_err(lower_err)?.into(),
            BinOp::Div => {
                self.guard_div(l, r, signed)?;
                if signed {
                    b.build_int_signed_div(l, r, "sdiv")
                } else {
                    b.build_int_unsigned_div(l, r, "udiv")
                }
                .map_err(lower_err)?
                .into()
            }
            BinOp::Rem => {
                self.guard_div(l, r, signed)?;
                if signed {
                    b.build_int_signed_rem(l, r, "srem")
                } else {
                    b.build_int_unsigned_rem(l, r, "urem")
                }
                .map_err(lower_err)?
                .into()
            }
            BinOp::BitAnd => b.build_and(l, r, "and").map_err(lower_err)?.into(),
            BinOp::BitOr => b.build_or(l, r, "or").map_err(lower_err)?.into(),
            BinOp::BitXor => b.build_xor(l, r, "xor").map_err(lower_err)?.into(),
            BinOp::Shl => {
                let amt = self.mask_shift_amount(l, r)?;
                b.build_left_shift(l, amt, "shl").map_err(lower_err)?.into()
            }
            // Arithmetic shift for signed, logical for unsigned.
            BinOp::Shr => {
                let amt = self.mask_shift_amount(l, r)?;
                b.build_right_shift(l, amt, signed, "shr")
                    .map_err(lower_err)?
                    .into()
            }
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
                // Saturating float→int (issue #36): a plain `fptosi`/`fptoui` is UB when the
                // value is out of the target's range or NaN. The `llvm.fpto{s,u}i.sat`
                // intrinsics clamp to the min/max and yield 0 for NaN — matching the
                // interpreter, which saturates and maps NaN→0. We still make the NaN→0 explicit
                // with a `select` (pure value, no branch) to be robust across LLVM versions.
                let signed = matches!(self.m.ty(target), TyKind::Int { signed: true, .. });
                let src = v.into_float_value();
                let int_ty = tgt.into_int_type();
                let name = if signed {
                    "llvm.fptosi.sat"
                } else {
                    "llvm.fptoui.sat"
                };
                let intr = inkwell::intrinsics::Intrinsic::find(name)
                    .ok_or_else(|| BackendError::Lower(format!("{name} intrinsic not found")))?;
                let int_basic: BasicTypeEnum = int_ty.into();
                let float_basic: BasicTypeEnum = src.get_type().into();
                let decl = intr
                    .get_declaration(&self.llm, &[int_basic, float_basic])
                    .ok_or_else(|| BackendError::Lower(format!("could not declare {name}")))?;
                let sat = b
                    .build_call(decl, &[src.into()], "fptosat")
                    .map_err(lower_err)?
                    .try_as_basic_value()
                    .left()
                    .ok_or_else(|| BackendError::Lower(format!("{name} returned no value")))?
                    .into_int_value();
                let is_nan = b
                    .build_float_compare(FloatPredicate::UNO, src, src, "isnan")
                    .map_err(lower_err)?;
                b.build_select(is_nan, int_ty.const_zero(), sat, "ftoi.sat")
                    .map_err(lower_err)?
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

    /// The slab stride (alloc size) and alignment of a crib element, from the data layout. The
    /// stride matches the `slot * elem_size` arithmetic in the runtime's typed cribs.
    fn crib_elem_layout(&self, elem: TyId) -> Result<(u64, u32), BackendError> {
        let bt = self.basic_ty(elem)?;
        Ok((self.td.get_abi_size(&bt), self.td.get_abi_alignment(&bt)))
    }

    /// `crib name: T[N]` / `crib name` — allocate a fresh crib via its `rt-abi` entry point. A
    /// typed crib passes the element's `(size, align)` from the data layout; a bump crib
    /// (`elem` = void) passes a byte reserve.
    fn lower_crib_new(
        &self,
        elem: TyId,
        capacity: u32,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let i64t = self.cx.i64_type();
        let i32t = self.cx.i32_type();
        let call = if matches!(self.m.ty(elem), TyKind::Void) {
            let reserve = if capacity == 0 {
                64 * 1024
            } else {
                capacity as u64
            };
            let f = self.get_or_add(
                "bet_crib_new_bump",
                self.ptr_ty().fn_type(&[i64t.into()], false),
            );
            self.builder
                .build_call(f, &[i64t.const_int(reserve, false).into()], "crib.bump")
                .map_err(lower_err)?
        } else {
            let (size, align) = self.crib_elem_layout(elem)?;
            let f = self.get_or_add(
                "bet_crib_new",
                self.ptr_ty()
                    .fn_type(&[i64t.into(), i64t.into(), i32t.into()], false),
            );
            let args = [
                i64t.const_int(size, false).into(),
                i64t.const_int(align as u64, false).into(),
                i32t.const_int(capacity as u64, false).into(),
            ];
            self.builder
                .build_call(f, &args, "crib.typed")
                .map_err(lower_err)?
        };
        call.try_as_basic_value()
            .left()
            .ok_or_else(|| BackendError::Lower("crib_new returned void".into()))
    }

    /// `cop init in crib`. A **typed** crib reserves a slot (`bet_cop`), resolves its storage
    /// (`bet_holla_check`), initializes the fields, and yields the `tag`. A **bump** crib
    /// (`Crib(void)`) bump-allocates the struct (`bet_bump_alloc`) and yields a raw `ref`.
    fn lower_cop(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        crib: &Operand,
        init: &CopInit,
    ) -> Result<Option<BasicValueEnum<'c>>, BackendError> {
        let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();

        // A sum-variant `cop` into a typed crib: reserve a slot and store the variant value.
        if let CopInit::SumVariant(sum, variant, ops) = init {
            let cop = self.get_or_add(
                "bet_cop",
                self.tag_ty().fn_type(&[self.ptr_ty().into()], false),
            );
            let tag = self
                .builder
                .build_call(cop, &[crib_v.into()], "cop")
                .map_err(lower_err)?
                .try_as_basic_value()
                .left()
                .ok_or_else(|| BackendError::Lower("bet_cop returned void".into()))?;
            let holla = self.get_or_add(
                "bet_holla_check",
                self.ptr_ty()
                    .fn_type(&[self.ptr_ty().into(), self.tag_ty().into()], false),
            );
            let storage = self
                .builder
                .build_call(holla, &[crib_v.into(), tag.into()], "cop.slot")
                .map_err(lower_err)?
                .try_as_basic_value()
                .left()
                .ok_or_else(|| BackendError::Lower("bet_holla_check returned void".into()))?
                .into_pointer_value();
            self.store_sum_into(func, locals, storage, *sum, *variant, ops)?;
            return Ok(Some(tag));
        }

        let CopInit::StructLit(sid, fields) = init else {
            unreachable!("cop init is struct or sum")
        };
        let sty = self.struct_llvm_ty(*sid)?;

        // Is this a bump (untyped) crib? Its handle has type `Crib(void)`.
        let is_bump = match self.operand_ty(func, crib)? {
            Some(t) => {
                matches!(self.m.ty(t), TyKind::Crib(e) if matches!(self.m.ty(*e), TyKind::Void))
            }
            None => false,
        };

        // Resolve the storage pointer (and, for a typed crib, the tag to yield).
        let (storage, result) = if is_bump {
            let bt: BasicTypeEnum = sty.into();
            let i64t = self.cx.i64_type();
            let size = i64t.const_int(self.td.get_abi_size(&bt), false);
            let align = i64t.const_int(self.td.get_abi_alignment(&bt) as u64, false);
            let f = self.get_or_add(
                "bet_bump_alloc",
                self.ptr_ty()
                    .fn_type(&[self.ptr_ty().into(), i64t.into(), i64t.into()], false),
            );
            let storage = self
                .builder
                .build_call(f, &[crib_v.into(), size.into(), align.into()], "bump")
                .map_err(lower_err)?
                .try_as_basic_value()
                .left()
                .ok_or_else(|| BackendError::Lower("bet_bump_alloc returned void".into()))?
                .into_pointer_value();
            // A bump `cop` yields a live `ref` (the raw storage pointer) directly.
            (storage, storage.into())
        } else {
            let cop = self.get_or_add(
                "bet_cop",
                self.tag_ty().fn_type(&[self.ptr_ty().into()], false),
            );
            let tag = self
                .builder
                .build_call(cop, &[crib_v.into()], "cop")
                .map_err(lower_err)?
                .try_as_basic_value()
                .left()
                .ok_or_else(|| BackendError::Lower("bet_cop returned void".into()))?;
            let holla = self.get_or_add(
                "bet_holla_check",
                self.ptr_ty()
                    .fn_type(&[self.ptr_ty().into(), self.tag_ty().into()], false),
            );
            let storage = self
                .builder
                .build_call(holla, &[crib_v.into(), tag.into()], "cop.slot")
                .map_err(lower_err)?
                .try_as_basic_value()
                .left()
                .ok_or_else(|| BackendError::Lower("bet_holla_check returned void".into()))?
                .into_pointer_value();
            (storage, tag)
        };

        for (fidx, op) in fields {
            let v = self.lower_operand(func, locals, op)?;
            let fptr = self
                .builder
                .build_struct_gep(sty, storage, *fidx, "cop.field")
                .map_err(|_| BackendError::Lower(format!("bad field index {fidx} in cop")))?;
            self.builder.build_store(fptr, v).map_err(lower_err)?;
        }
        Ok(Some(result))
    }

    /// `tag.trust() in crib` — unchecked resolve to a `ref` (a raw slot pointer). Extracts the
    /// slot index (struct field 0 of the `{ i32 slot, i64 generation }` tag) and calls
    /// `bet_slot_ptr`.
    fn lower_trust(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        crib: &Operand,
        tag: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
        let tag_v = self.lower_operand(func, locals, tag)?.into_struct_value();
        let slot = self
            .builder
            .build_extract_value(tag_v, 0, "slot")
            .map_err(lower_err)?
            .into_int_value();
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
                // A read ⇒ the index must be strictly in bounds.
                let (ptr, ty) = self.place_ptr(func, locals, p, IndexMode::Access)?;
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
        mode: IndexMode,
    ) -> Result<(PointerValue<'c>, TyId), BackendError> {
        let mut ptr = locals[place.local.index()]
            .ok_or_else(|| BackendError::Lower("addressing a void/zero-sized local".into()))?;
        let mut ty = func.local_ty(place.local);
        // A `Downcast(v)` positions us at the sum's payload array; the following `Field(j)`
        // then indexes that array. This carries the pending `(sum, variant)` between them.
        let mut pending: Option<(SumId, u32)> = None;
        // `soa[i].field(j)` fuses an `Index` with the following `Field`; when the pair is
        // consumed we set `skip_field` so the next iteration skips that trailing `Field`.
        let mut skip_field = false;
        for (k, proj) in place.proj.iter().enumerate() {
            if skip_field {
                skip_field = false;
                continue;
            }
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
                    if let Some((sid, v)) = pending.take() {
                        // Payload field of a downcast sum: `ptr` points at the payload union;
                        // place the field by the active variant's natural struct layout.
                        let vps = self.variant_payload_struct(sid, v)?;
                        ptr = self
                            .builder
                            .build_struct_gep(vps, ptr, *i, "sum.field")
                            .map_err(|_| {
                                BackendError::Lower(format!("sum payload field {i} gep"))
                            })?;
                        let variant = &self.m.sum_def(sid).variants[v as usize];
                        ty = *variant.payload.get(*i as usize).ok_or_else(|| {
                            BackendError::Lower(format!(
                                "variant `{}::{}` has no payload #{i}",
                                self.m.sum_def(sid).name,
                                variant.name
                            ))
                        })?;
                        continue;
                    }
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
                        TyKind::Tuple(elems) => {
                            let field_ty = *elems.get(*i as usize).ok_or_else(|| {
                                BackendError::Lower(format!("tuple has no element #{i}"))
                            })?;
                            (self.tuple_llvm_ty(elems)?, field_ty)
                        }
                        // Field(j) on a `soa` slice/vec bundle addresses the j-th per-field slot
                        // (a fat sub-slice for `soa []T`, a vec handle for `soa vec[T]`). The
                        // slot's storage matches `basic_ty(inner)` — a fat ptr or a plain ptr —
                        // so the inner container type is exactly the right type at that address.
                        // (A `soa T[N]` field is only ever reached fused with an Index.)
                        TyKind::Soa(inner) => (self.basic_ty(ty)?.into_struct_type(), *inner),
                        other => {
                            return Err(BackendError::Lower(format!(
                                "field projection on non-aggregate {other:?}"
                            )));
                        }
                    };
                    ptr = self
                        .builder
                        .build_struct_gep(sty, ptr, *i, "field")
                        .map_err(|_| BackendError::Lower(format!("bad field index {i}")))?;
                    ty = field_ty;
                }
                Proj::Downcast(v) => {
                    let sid = match self.m.ty(ty) {
                        TyKind::Sum(s) => *s,
                        other => {
                            return Err(BackendError::Lower(format!(
                                "downcast of non-sum {other:?}"
                            )));
                        }
                    };
                    // Position at the payload array (struct field 1); the next Field indexes it.
                    let sty = self.sum_llvm_ty(sid);
                    ptr = self
                        .builder
                        .build_struct_gep(sty, ptr, 1, "downcast")
                        .map_err(|_| {
                            BackendError::Lower("sum has no payload to downcast".into())
                        })?;
                    pending = Some((sid, *v));
                }
                Proj::Index(idx) => {
                    let idx_v = self.lower_operand(func, locals, idx)?.into_int_value();
                    match self.m.ty(ty) {
                        // An array is stored inline: GEP straight off its address. The length is
                        // the static extent `n` — bounds-check against it (issue #32).
                        TyKind::Array(e, n) => {
                            let elem_llvm = self.basic_ty(*e)?;
                            let len = self.cx.i64_type().const_int(*n, false);
                            self.bounds_check(idx_v, len, mode)?;
                            ptr = self.gep_index_elem(elem_llvm, ptr, idx_v)?;
                            ty = *e;
                        }
                        // A slice is a fat `{ ptr, len }`: `ptr` is the address of that value, so
                        // load its length (field 1) to bounds-check (issue #32), then its data
                        // pointer (field 0) before indexing the backing storage.
                        TyKind::Slice(e) => {
                            let elem_llvm = self.basic_ty(*e)?;
                            let fat = self.fat_ptr_ty();
                            let len_addr = self
                                .builder
                                .build_struct_gep(fat, ptr, 1, "slice.len")
                                .map_err(|_| BackendError::Lower("slice len gep".into()))?;
                            let len = self
                                .builder
                                .build_load(self.cx.i64_type(), len_addr, "slice.len.val")
                                .map_err(lower_err)?
                                .into_int_value();
                            self.bounds_check(idx_v, len, mode)?;
                            let data_ptr_addr = self
                                .builder
                                .build_struct_gep(fat, ptr, 0, "slice.ptr")
                                .map_err(|_| BackendError::Lower("slice ptr gep".into()))?;
                            let data = self
                                .builder
                                .build_load(self.ptr_ty(), data_ptr_addr, "slice.data")
                                .map_err(lower_err)?
                                .into_pointer_value();
                            ptr = self.gep_index_elem(elem_llvm, data, idx_v)?;
                            ty = *e;
                        }
                        // A `soa` container is transposed: `soa[i].field(j)` addresses
                        // field-array `j` first, then index `i`. The `Index` MUST be followed
                        // by a `Field` — a bare whole-element index has no single address, so
                        // the frontend rejects it; this is the soundness backstop.
                        TyKind::Soa(inner) => {
                            let field_j = match place.proj.get(k + 1) {
                                Some(Proj::Field(j)) => *j,
                                _ => {
                                    return Err(BackendError::Lower(
                                        "whole-element access of a soa container reached codegen \
                                         (a frontend ban was missed)"
                                            .into(),
                                    ));
                                }
                            };
                            // `soa T[N]` field `j` is the inline `[N x Tj]` array; `soa []T`
                            // field `j` is a fat sub-slice whose data pointer must be loaded.
                            // `static_len` carries `N` for the inline case so the index can be
                            // bounds-checked against it (issue #32); the slice case loads its
                            // runtime length below.
                            let (elem, is_slice, static_len) = match self.m.ty(*inner) {
                                TyKind::Array(e, n) => (*e, false, Some(*n)),
                                TyKind::Slice(e) => (*e, true, None),
                                other => {
                                    return Err(BackendError::Lower(format!(
                                        "soa index for {other:?} isn't implemented yet"
                                    )));
                                }
                            };
                            let sid = match self.m.ty(elem) {
                                TyKind::Struct(s) => *s,
                                other => {
                                    return Err(BackendError::Lower(format!(
                                        "soa element must be a drip, got {other:?}"
                                    )));
                                }
                            };
                            let field_ty = self.m.struct_def(sid).fields[field_j as usize].ty;
                            let soa_llvm = self.basic_ty(ty)?.into_struct_type();
                            // GEP to the bundle's per-field slot `j`.
                            let field_slot = self
                                .builder
                                .build_struct_gep(soa_llvm, ptr, field_j, "soa.field")
                                .map_err(|_| {
                                    BackendError::Lower(format!("soa field gep {field_j}"))
                                })?;
                            let elem_llvm = self.basic_ty(field_ty)?;
                            ptr = if is_slice {
                                // Sub-slice: bounds-check against its runtime length (fat field
                                // 1), mirroring the `Slice` arm, then load its data pointer (fat
                                // field 0) and index (issue #32).
                                let fat = self.fat_ptr_ty();
                                let len_addr = self
                                    .builder
                                    .build_struct_gep(fat, field_slot, 1, "soa.sub.len")
                                    .map_err(|_| {
                                        BackendError::Lower("soa sub-slice len gep".into())
                                    })?;
                                let len = self
                                    .builder
                                    .build_load(self.cx.i64_type(), len_addr, "soa.sub.len.val")
                                    .map_err(lower_err)?
                                    .into_int_value();
                                self.bounds_check(idx_v, len, mode)?;
                                let data_addr = self
                                    .builder
                                    .build_struct_gep(fat, field_slot, 0, "soa.sub.ptr")
                                    .map_err(|_| {
                                        BackendError::Lower("soa sub-slice ptr gep".into())
                                    })?;
                                let data = self
                                    .builder
                                    .build_load(self.ptr_ty(), data_addr, "soa.sub.data")
                                    .map_err(lower_err)?
                                    .into_pointer_value();
                                self.gep_index_elem(elem_llvm, data, idx_v)?
                            } else {
                                // Inline field-array: bounds-check against the static extent `N`
                                // (issue #32), then index straight off it.
                                let len = self.cx.i64_type().const_int(
                                    static_len.expect("inline soa array has a static length"),
                                    false,
                                );
                                self.bounds_check(idx_v, len, mode)?;
                                self.gep_index_elem(elem_llvm, field_slot, idx_v)?
                            };
                            ty = field_ty;
                            skip_field = true;
                        }
                        other => {
                            return Err(BackendError::Lower(format!(
                                "index into non-array {other:?}"
                            )));
                        }
                    }
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
        let mut pending: Option<(SumId, u32)> = None;
        for proj in &place.proj {
            if let Proj::Field(i) = proj
                && let Some((sid, v)) = pending.take()
            {
                ty = *self.m.sum_def(sid).variants[v as usize]
                    .payload
                    .get(*i as usize)
                    .ok_or_else(|| BackendError::Lower(format!("bad payload #{i}")))?;
                continue;
            }
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
                (Proj::Index(_), TyKind::Array(e, _) | TyKind::Slice(e)) => *e,
                (Proj::Downcast(v), TyKind::Sum(sid)) => {
                    pending = Some((*sid, *v));
                    ty
                }
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
            // generation=0), built as the `{ i32 slot, i64 generation }` struct value (issue
            // #34). Its slot is out of range for any crib, so it always resolves as ghosted.
            // Other (contextual) uses of `ghosted` are not supported yet.
            Const::Ghosted => {
                let slot = self.cx.i32_type().const_int(0xFFFF_FFFF, false);
                let generation = self.cx.i64_type().const_zero();
                Ok(self
                    .tag_ty()
                    .const_named_struct(&[slot.into(), generation.into()])
                    .into())
            }
            // A null pointer: the zero-default for handle-shaped struct fields (fn values,
            // `vec`/`stash`/`rng` handles, raw pointers). Safe to hold/overwrite; a crash
            // only on use, like any zeroed handle in C.
            Const::NullPtr => Ok(self.ptr_ty().const_null().into()),
            Const::FnRef(fid) => Ok(self.funcs[fid.index()]
                .as_global_value()
                .as_pointer_value()
                .into()),
            // A `str` literal is a fat `{ ptr, len }` value: an interned global byte array plus
            // its byte length. This makes `str` a first-class value (locals, params, returns).
            Const::Str(s) => {
                let g = self
                    .builder
                    .build_global_string_ptr(s, "str")
                    .map_err(lower_err)?;
                let len = self.cx.i64_type().const_int(s.len() as u64, false);
                Ok(self.build_fat_ptr(g.as_pointer_value().into(), len.into())?)
            }
        }
    }

    /// Pack a `{ ptr, len }` fat value (a `str` or `[]T` slice) from a data pointer and a length.
    fn build_fat_ptr(
        &self,
        data: BasicValueEnum<'c>,
        len: BasicValueEnum<'c>,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        let fat = self.fat_ptr_ty();
        let agg = self
            .builder
            .build_insert_value(fat.get_undef(), data, 0, "fat.ptr")
            .map_err(lower_err)?;
        let agg = self
            .builder
            .build_insert_value(agg, len, 1, "fat.len")
            .map_err(lower_err)?;
        Ok(agg.into_struct_value().into())
    }

    /// Extract field `idx` (0 = data ptr, 1 = len) from a fat `str`/slice operand, or take the
    /// literal fast-path for a `str` constant (intern its global / use its static length).
    fn str_projection(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &Operand,
        idx: u32,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        if let Operand::Const(Const::Str(s)) = op {
            return Ok(if idx == 0 {
                self.builder
                    .build_global_string_ptr(s, "str")
                    .map_err(lower_err)?
                    .as_pointer_value()
                    .into()
            } else {
                self.cx.i64_type().const_int(s.len() as u64, false).into()
            });
        }
        let fat = self.lower_operand(func, locals, op)?.into_struct_value();
        self.builder
            .build_extract_value(fat, idx, "str.proj")
            .map_err(lower_err)
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
                    many => {
                        // Pack the values into the function's anonymous return struct.
                        let sty = self.tuple_llvm_ty(&func.rets)?;
                        let mut agg = sty.get_undef();
                        for (i, op) in many.iter().enumerate() {
                            let v = self.lower_operand(func, locals, op)?;
                            agg = self
                                .builder
                                .build_insert_value(agg, v, i as u32, "ret")
                                .map_err(lower_err)?
                                .into_struct_value();
                        }
                        self.builder.build_return(Some(&agg)).map_err(lower_err)?;
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
                let tag_v = self.lower_operand(func, locals, tag)?;
                let crib_v = self.lower_operand(func, locals, crib)?.into_pointer_value();
                let holla = self.get_or_add(
                    "bet_holla_check",
                    self.ptr_ty()
                        .fn_type(&[self.ptr_ty().into(), self.tag_ty().into()], false),
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
                // ghosted edge, by contract, never reads it). A plain local binding — no index.
                let (dest, _ty) = self.place_ptr(func, locals, resolved, IndexMode::Access)?;
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
        let ptr_t = self.ptr_ty();
        // `int main(int argc, char** argv)` — the C entry point; we forward argc/argv to the
        // runtime so `sys.arg` / `sys.argc` can read them.
        let main_fn = self.llm.add_function(
            "main",
            i32t.fn_type(&[i32t.into(), ptr_t.into()], false),
            Some(Linkage::External),
        );
        let bb = self.cx.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(bb);

        let void_fty = self.cx.void_type().fn_type(&[], false);
        let init = self.get_or_add("bet_rt_init", void_fty);
        let shutdown = self.get_or_add("bet_rt_shutdown", void_fty);

        self.builder.build_call(init, &[], "").map_err(lower_err)?;
        // Capture the process arguments before user code runs.
        let args_init_fty = self
            .cx
            .void_type()
            .fn_type(&[i32t.into(), ptr_t.into()], false);
        let args_init = self.get_or_add("bet_args_init", args_init_fty);
        let argc = main_fn
            .get_nth_param(0)
            .expect("main argc param")
            .into_int_value();
        let argv = main_fn
            .get_nth_param(1)
            .expect("main argv param")
            .into_pointer_value();
        self.builder
            .build_call(args_init, &[argc.into(), argv.into()], "")
            .map_err(lower_err)?;
        // Initialize every module-level crib once, after the runtime is up.
        for (i, c) in self.m.crib_globals().iter().enumerate() {
            let handle = self.lower_crib_new(c.elem, c.capacity)?;
            self.builder
                .build_store(self.crib_globals[i].as_pointer_value(), handle)
                .map_err(lower_err)?;
        }
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
        // The triple and data layout were set on the module in `compile`.
        if let Err(e) = self.llm.verify() {
            return Err(BackendError::Lower(format!(
                "generated LLVM module failed verification: {e}"
            )));
        }

        // Run the mid-level optimization pipeline. At `-O0` we emit straight from the unoptimized
        // module (fastest, allocas left for the register allocator). At `-O2` we run LLVM's new
        // pass manager `default<O2>` pipeline — inliner, mem2reg, SROA, and the loop + SLP
        // vectorizers — which is what collapses zero-cost abstractions and auto-vectorizes `soa`
        // loops. We deliberately set no fast-math option (there is none on `PassBuilderOptions`,
        // and the frontend never sets fast-math flags), so float results stay bit-identical to the
        // interpreter oracle.
        if opts.opt != OptLevel::O0 {
            let pbo = PassBuilderOptions::create();
            pbo.set_loop_vectorization(true);
            pbo.set_loop_slp_vectorization(true);
            pbo.set_loop_unrolling(true);
            self.llm
                .run_passes("default<O2>", &self.tm, pbo)
                .map_err(|e| BackendError::Target(e.to_string()))?;
        }

        let file_type = match opts.emit {
            EmitKind::Object => FileType::Object,
            EmitKind::Assembly => FileType::Assembly,
        };
        let buf = self
            .tm
            .write_to_memory_buffer(&self.llm, file_type)
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
