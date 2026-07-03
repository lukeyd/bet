//! Lower the surface [`ast`] to `midir`.
//!
//! Tracer-bullet scope: this lowers exactly the constructs `tests/corpus/01-basics/hello.bet`
//! needs — `finna` functions whose bodies are `spill.it("literal")` prints — reproducing the
//! same IR the Step-2 tracer bullet produced (each `finna` becomes a `() -> void` function;
//! each `spill.it("text")` lowers to the `bet_print` extern via the surface→IR map in
//! `spec/semantics.md`). Everything else is a clean "not yet" [`Err`]; the full AST→IR lowering
//! (arithmetic, control flow, drips/moods, the memory model) lands with the interpreter- and
//! backend-side of the fan-out. The richer surface is fully parsed by [`crate::parse`] already.

use crate::ast::{self, Expr, ExprKind, Item, Stmt, StmtKind};
use midir::*;

pub fn lower(prog: &ast::Program) -> Result<Module, String> {
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

    for item in &prog.items {
        match item {
            // Imports are recognized and resolved to a no-op for now.
            Item::Pull(_) => {}
            Item::Func(func) => lower_func(&mut m, func, bet_print, rawptr, u64t, voidt)?,
            other => {
                return Err(format!(
                    "this construct is parsed but not yet lowered to midir: {}",
                    item_kind(other)
                ));
            }
        }
    }

    Ok(m)
}

fn lower_func(
    m: &mut Module,
    func: &ast::FnDecl,
    bet_print: ExternId,
    rawptr: TyId,
    u64t: TyId,
    voidt: TyId,
) -> Result<(), String> {
    if !func.params.is_empty() || !matches!(func.ret, ast::RetType::None) || func.receiver.is_some()
    {
        return Err(format!(
            "function `{}` uses parameters/returns/receivers not yet lowered to midir",
            func.name
        ));
    }
    let mut fb = FuncBuilder::new(func.name.clone(), vec![], vec![]);
    fb.block(); // bb0, current
    for stmt in &func.body.stmts {
        lower_print_stmt(&mut fb, stmt, bet_print, rawptr, u64t, voidt)?;
    }
    fb.ret(vec![]);
    m.add_func(fb.finish());
    Ok(())
}

/// Lower a single statement, which in tracer-bullet scope must be `spill.it("literal")`.
fn lower_print_stmt(
    fb: &mut FuncBuilder,
    stmt: &Stmt,
    bet_print: ExternId,
    rawptr: TyId,
    u64t: TyId,
    voidt: TyId,
) -> Result<(), String> {
    let text = match &stmt.kind {
        StmtKind::Expr(e) => print_text(e)?,
        _ => return Err("only `spill.it(\"…\")` statements are lowered yet".into()),
    };
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
    Ok(())
}

/// Recognize `spill.it("text")` and return its string literal, else an [`Err`].
fn print_text(e: &Expr) -> Result<String, String> {
    let ExprKind::Method {
        receiver,
        method,
        generics,
        args,
    } = &e.kind
    else {
        return Err("only `spill.it(\"…\")` expression statements are lowered yet".into());
    };
    let ExprKind::Name { name, .. } = &receiver.kind else {
        return Err("only `spill.it(\"…\")` is lowered yet".into());
    };
    if name != "spill" || method != "it" || !generics.is_empty() || args.len() != 1 {
        return Err(format!(
            "only `spill.it(\"…\")` is lowered yet, found `{name}.{method}(...)`"
        ));
    }
    match &args[0].value.kind {
        ExprKind::Str(s) => Ok(s.clone()),
        _ => Err("`spill.it` takes a string literal in this lowering".into()),
    }
}

fn item_kind(item: &Item) -> &'static str {
    match item {
        Item::Pull(_) => "pull",
        Item::Func(_) => "finna",
        Item::Drip(_) => "drip",
        Item::Moods(_) => "moods",
        Item::Crib(_) => "crib",
        Item::Const(_) => "facts",
        Item::Var(_) => "lowkey",
        Item::Extern(_) => "extern",
    }
}
