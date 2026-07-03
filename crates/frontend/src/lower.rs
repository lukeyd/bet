//! Lower the AST to `midir`. Each `finna` becomes a `() -> void` function; each
//! `spill.it("text")` lowers to the `spill.it(x) → bet_print` row of the surface→IR map
//! (`spec/semantics.md` appendix): the string literal (with the println newline appended) is
//! decomposed into its data pointer and byte length and passed to the `bet_print` extern.

use crate::parser::{Program, Stmt};
use midir::*;

pub fn lower(prog: &Program) -> Result<Module, String> {
    let mut m = Module::new();
    let rawptr = m.intern_ty(TyKind::RawPtr);
    let u64t = m.t_int(IntWidth::W64, false);
    let voidt = m.t_void();

    // `extern "C" fn bet_print(rawptr, u64) -> void` — the stdout entry point.
    let bet_print = m.add_extern(Extern {
        name: "bet_print".into(),
        abi: "C".into(),
        sig: Sig {
            params: vec![rawptr, u64t],
            rets: vec![],
        },
    });

    for func in &prog.funcs {
        let mut fb = FuncBuilder::new(func.name.clone(), vec![], vec![]);
        fb.block(); // bb0, current
        for stmt in &func.body {
            match stmt {
                Stmt::Print(text) => {
                    // `spill.it` is println-like: carry the trailing newline in the literal.
                    let line = format!("{text}\n");
                    let ptr = fb.local(rawptr);
                    let len = fb.local(u64t);
                    let result = fb.local(voidt);
                    fb.assign(
                        fb.place(ptr),
                        Rvalue::StrPtr(Operand::Const(Const::Str(line.clone()))),
                    );
                    fb.assign(
                        fb.place(len),
                        Rvalue::StrLen(Operand::Const(Const::Str(line))),
                    );
                    let args = vec![fb.copy(fb.place(ptr)), fb.copy(fb.place(len))];
                    fb.assign(
                        fb.place(result),
                        Rvalue::Call(Callee::Extern(bet_print), args),
                    );
                }
            }
        }
        fb.ret(vec![]);
        m.add_func(fb.finish());
    }

    Ok(m)
}
