//! LLVM code generation via `inkwell` (only compiled under `--features llvm`).
//!
//! Minimal, tracer-bullet scope: enough of `midir` to lower `spill.it("hi")` — externs,
//! `str_ptr`/`str_len` on string literals, direct/extern calls, integer/bool/float consts,
//! bare-local places, `goto`/`branch`/`return` — plus a synthesized C `main` entry that
//! brackets the program with `bet_rt_init` / `bet_rt_shutdown`. Anything outside this subset
//! returns [`BackendError::Lower`] with a precise message; the full lowering lands later.

use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module as LlvmModule};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, OptimizationLevel};

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

    fn basic_ty(&self, ty: TyId) -> Result<BasicTypeEnum<'c>, BackendError> {
        Ok(match self.m.ty(ty) {
            TyKind::Bool => self.cx.bool_type().into(),
            TyKind::Int { width, .. } => self.cx.custom_width_int_type(width.bits()).into(),
            TyKind::F32 => self.cx.f32_type().into(),
            TyKind::F64 => self.cx.f64_type().into(),
            TyKind::RawPtr => self.cx.ptr_type(AddressSpace::default()).into(),
            other => {
                return Err(BackendError::Lower(format!(
                    "value type {other:?} is not supported yet"
                )));
            }
        })
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
                if !place.proj.is_empty() {
                    return Err(BackendError::Lower(
                        "place projections are not supported yet".into(),
                    ));
                }
                let val = self.lower_rvalue(func, locals, rv)?;
                match (locals[place.local.index()], val) {
                    (Some(slot), Some(v)) => {
                        self.builder.build_store(slot, v).map_err(lower_err)?;
                        Ok(())
                    }
                    // A void rvalue (e.g. a void call) assigned into a void local: nothing to store.
                    (None, None) => Ok(()),
                    (Some(_), None) => Err(BackendError::Lower(
                        "assigning a void value into a non-void local".into(),
                    )),
                    (None, Some(_)) => Ok(()),
                }
            }
            Stmt::Evict(_) => Err(BackendError::Lower("`evict` is not supported yet".into())),
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
            other => Err(BackendError::Lower(format!(
                "rvalue {other:?} is not supported yet"
            ))),
        }
    }

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
        let fv = match callee {
            Callee::Direct(fid) => self.funcs[fid.index()],
            Callee::Extern(eid) => self.externs[eid.index()],
            Callee::Indirect(_) => {
                return Err(BackendError::Lower(
                    "indirect calls are not supported yet".into(),
                ));
            }
        };
        let cs = self
            .builder
            .build_call(fv, &arg_vals, "call")
            .map_err(lower_err)?;
        Ok(cs.try_as_basic_value().left())
    }

    fn lower_operand(
        &self,
        func: &Func,
        locals: &[Option<PointerValue<'c>>],
        op: &Operand,
    ) -> Result<BasicValueEnum<'c>, BackendError> {
        match op {
            Operand::Const(c) => self.lower_const(c),
            Operand::Copy(p) | Operand::Move(p) => {
                if !p.proj.is_empty() {
                    return Err(BackendError::Lower(
                        "place projections are not supported yet".into(),
                    ));
                }
                let slot = locals[p.local.index()]
                    .ok_or_else(|| BackendError::Lower("reading a void local".into()))?;
                let ty = self.basic_ty(func.local_ty(p.local))?;
                self.builder.build_load(ty, slot, "load").map_err(lower_err)
            }
        }
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
            Const::Str(_) => Err(BackendError::Lower(
                "a bare string constant has no scalar value; use str_ptr/str_len".into(),
            )),
            Const::Ghosted => Err(BackendError::Lower(
                "`ghosted` const is not supported yet".into(),
            )),
            Const::FnRef(_) => Err(BackendError::Lower(
                "function-reference const is not supported yet".into(),
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
            other => Err(BackendError::Lower(format!(
                "terminator {other:?} is not supported yet"
            ))),
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
