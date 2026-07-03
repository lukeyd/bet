//! End-to-end tests for the `midir` contract crate: the builder, the validator, and the
//! textual `.mir` round-trip — exercised on a realistic function (a shrunk `mini-compiler`
//! `eval`) plus the hand-written fixtures under `tests/mir/`.

use midir::*;
use std::fs;
use std::path::Path;

/// `node.deref` — the resolved sum value behind a `ref` local.
fn node_deref(fb: &FuncBuilder, node: LocalId) -> Place {
    fb.deref(&fb.place(node))
}

/// `node.deref.downcast(variant).field(i)` — a sum-variant payload slot.
fn pay(fb: &FuncBuilder, node: LocalId, variant: u32, i: u32) -> Place {
    fb.field(&fb.downcast(&node_deref(fb, node), variant), i)
}

/// Build the shrunk `mini-compiler` evaluator:
///
/// ```text
/// sum Expr { Lit(i64), Add(tag Expr, tag Expr), Mul(tag Expr, tag Expr) }
/// fn eval(e: tag Expr, ast: crib Expr) -> (i64, tag Yikes)
/// ```
///
/// It resolves the node via `holla_check`, dispatches on the discriminant, extracts payloads
/// through `downcast`+`field`, recurses, propagates errors (`bounce` = branch on a non-ghosted
/// error), and returns two values. This single function spans most of the IR node set.
fn build_eval() -> Module {
    let mut m = Module::new();

    let i64t = m.t_i64();
    let strt = m.t_str();
    let boolt = m.t_bool();

    // An error carrier reached as `tag Yikes`; `ghosted` is the ok case.
    let yikes_sid = m.add_struct(StructDef {
        name: "Yikes".into(),
        fields: vec![Field {
            name: "msg".into(),
            ty: strt,
            vis: Vis::Flex,
        }],
    });
    let yikes_ty = m.intern_ty(TyKind::Struct(yikes_sid));
    let tag_yikes = m.t_tag(yikes_ty);

    // A self-referential sum (variants hold `tag Expr`): reserve the id, then build.
    let expr_sid = SumId(m.sums().len() as u32);
    let expr_ty = m.intern_ty(TyKind::Sum(expr_sid));
    let tag_expr = m.t_tag(expr_ty);
    let variants = vec![
        Variant {
            name: "Lit".into(),
            payload: vec![i64t],
        },
        Variant {
            name: "Add".into(),
            payload: vec![tag_expr, tag_expr],
        },
        Variant {
            name: "Mul".into(),
            payload: vec![tag_expr, tag_expr],
        },
    ];
    let sid = m.add_sum(SumDef {
        name: "Expr".into(),
        variants,
    });
    assert_eq!(sid, expr_sid);

    let ref_expr = m.t_ref(expr_ty);
    let crib_expr = m.t_crib(expr_ty);
    let tuple_ty = m.intern_ty(TyKind::Tuple(vec![i64t, tag_yikes]));

    let self_id = FuncId(0); // eval is the only (first) function
    let mut fb = FuncBuilder::new("eval", vec![tag_expr, crib_expr], vec![i64t, tag_yikes]);
    let p_e = fb.param(0);
    let p_ast = fb.param(1);
    let l_node = fb.local(ref_expr); // %2
    let l_disc = fb.local(i64t); // %3
    let l_a = fb.local(tuple_ty); // %4  eval(left)
    let l_b = fb.local(tuple_ty); // %5  eval(right)
    let l_chk = fb.local(boolt); // %6  error check

    let bb: Vec<BlockId> = (0..14).map(|_| fb.block()).collect();

    // bb0 — resolve the node handle.
    fb.at(bb[0]);
    fb.holla_check(
        fb.copy(fb.place(p_e)),
        fb.copy(fb.place(p_ast)),
        fb.place(l_node),
        bb[1],
        bb[13],
    );

    // bb1 — dispatch on the variant.
    fb.at(bb[1]);
    let disc = Rvalue::Discriminant(fb.copy(node_deref(&fb, l_node)));
    fb.assign(fb.place(l_disc), disc);
    fb.switch(
        fb.copy(fb.place(l_disc)),
        vec![(0, bb[2]), (1, bb[3]), (2, bb[8])],
        bb[13],
    );

    // bb2 — Lit(n): return n, ghosted.
    fb.at(bb[2]);
    let n = fb.copy(pay(&fb, l_node, 0, 0));
    fb.ret(vec![n, Operand::Const(Const::Ghosted)]);

    // Add (variant 1): bb3..bb7.
    build_binary_arm(
        &mut fb,
        &bb,
        l_node,
        l_a,
        l_b,
        l_chk,
        i64t,
        self_id,
        p_ast,
        1,
        BinOp::Add,
        [bb[3], bb[4], bb[5], bb[6], bb[7]],
    );

    // Mul (variant 2): bb8..bb12.
    build_binary_arm(
        &mut fb,
        &bb,
        l_node,
        l_a,
        l_b,
        l_chk,
        i64t,
        self_id,
        p_ast,
        2,
        BinOp::Mul,
        [bb[8], bb[9], bb[10], bb[11], bb[12]],
    );

    // bb13 — dangling node / default: return 0, ghosted.
    fb.at(bb[13]);
    fb.ret(vec![
        Operand::Const(Const::Int(0, i64t)),
        Operand::Const(Const::Ghosted),
    ]);

    m.add_func(fb.finish());
    m
}

/// Emit one binary-operator arm (`Add`/`Mul`): recurse on both children with `bounce`-style
/// early error returns, then combine. `blocks` = [entry, err_l, ok_l, err_r, ret].
#[allow(clippy::too_many_arguments)]
fn build_binary_arm(
    fb: &mut FuncBuilder,
    _bb: &[BlockId],
    l_node: LocalId,
    l_a: LocalId,
    l_b: LocalId,
    l_chk: LocalId,
    i64t: TyId,
    self_id: FuncId,
    p_ast: LocalId,
    variant: u32,
    op: BinOp,
    blocks: [BlockId; 5],
) {
    let [entry, err_l, ok_l, err_r, ret] = blocks;

    // entry: a = eval(left); if a.err != ghosted -> err_l else ok_l
    fb.at(entry);
    let left = fb.copy(pay(fb, l_node, variant, 0));
    fb.assign(
        fb.place(l_a),
        Rvalue::Call(
            Callee::Direct(self_id),
            vec![left, fb.copy(fb.place(p_ast))],
        ),
    );
    let a_err = fb.copy(fb.field(&fb.place(l_a), 1));
    fb.assign(
        fb.place(l_chk),
        Rvalue::BinOp(
            BinOp::Ne,
            a_err,
            Operand::Const(Const::Ghosted),
            ArithMode::Na,
        ),
    );
    fb.branch(fb.copy(fb.place(l_chk)), err_l, ok_l);

    // err_l: return 0, a.err
    fb.at(err_l);
    let a_err = fb.copy(fb.field(&fb.place(l_a), 1));
    fb.ret(vec![Operand::Const(Const::Int(0, i64t)), a_err]);

    // ok_l: b = eval(right); if b.err != ghosted -> err_r else ret
    fb.at(ok_l);
    let right = fb.copy(pay(fb, l_node, variant, 1));
    fb.assign(
        fb.place(l_b),
        Rvalue::Call(
            Callee::Direct(self_id),
            vec![right, fb.copy(fb.place(p_ast))],
        ),
    );
    let b_err = fb.copy(fb.field(&fb.place(l_b), 1));
    fb.assign(
        fb.place(l_chk),
        Rvalue::BinOp(
            BinOp::Ne,
            b_err,
            Operand::Const(Const::Ghosted),
            ArithMode::Na,
        ),
    );
    fb.branch(fb.copy(fb.place(l_chk)), err_r, ret);

    // err_r: return 0, b.err
    fb.at(err_r);
    let b_err = fb.copy(fb.field(&fb.place(l_b), 1));
    fb.ret(vec![Operand::Const(Const::Int(0, i64t)), b_err]);

    // ret: return a.val <op> b.val, ghosted
    fb.at(ret);
    let av = fb.copy(fb.field(&fb.place(l_a), 0));
    let bv = fb.copy(fb.field(&fb.place(l_b), 0));
    let combined = fb.local(i64t);
    fb.assign(
        fb.place(combined),
        Rvalue::BinOp(op, av, bv, ArithMode::Trap),
    );
    fb.ret(vec![
        fb.copy(fb.place(combined)),
        Operand::Const(Const::Ghosted),
    ]);
}

#[test]
fn eval_validates() {
    let m = build_eval();
    validate(&m).expect("hand-built eval should be well-formed");
}

#[test]
fn eval_text_round_trips() {
    let m = build_eval();
    let t1 = print(&m);
    let m2 = parse(&t1).expect("printed eval should re-parse");
    validate(&m2).expect("re-parsed eval should still validate");
    let t2 = print(&m2);
    assert_eq!(t1, t2, "print/parse must reach a fixpoint");
}

#[test]
fn golden_inc_prints_exactly() {
    let mut m = Module::new();
    let i64t = m.t_i64();
    let mut fb = FuncBuilder::new("inc", vec![i64t], vec![i64t]);
    let p = fb.param(0);
    let t = fb.local(i64t);
    fb.block();
    fb.assign(
        fb.place(t),
        Rvalue::BinOp(
            BinOp::Add,
            fb.copy(fb.place(p)),
            Operand::Const(Const::Int(1, i64t)),
            ArithMode::Trap,
        ),
    );
    fb.ret(vec![fb.copy(fb.place(t))]);
    m.add_func(fb.finish());

    let expected = "\nfn inc(%0: i64) -> i64 {\n  let %1: i64\n  bb0:\n    %1 = add.trap(%0, 1i64)\n    return %1\n}\n";
    assert_eq!(print(&m), expected);
}

// --- validation error cases (one per representative variant) ---

fn errs(src: &str) -> Vec<ValidationError> {
    let m = parse(src).expect("negative fixture should still parse");
    validate(&m).expect_err("expected a validation failure")
}

#[test]
fn detects_return_arity() {
    let e = errs("fn f() -> i64 { bb0: return }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::ReturnArity { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_target() {
    let e = errs("fn f() -> void { bb0: goto bb9 }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadTarget { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_local() {
    let e = errs("fn f() -> i64 { bb0: return %9 }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadLocal { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_type_mismatch() {
    let e = errs("fn f(%0: i64) -> void { let %1: i64\n bb0: %1 = eq.na(%0, %0)\n return }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::TypeMismatch { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_projection() {
    let e = errs("fn f(%0: i64) -> i64 { bb0: return %0.field(0) }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadProjection { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_shape() {
    let src = "struct P { flex a: i64, flex b: i64 } \
               fn f() -> void { let %0: P\n bb0: %0 = make P(1i64)\n return }";
    let e = errs(src);
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadShape { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_empty_and_bad_entry_and_bad_id() {
    // EmptyFunc — a function with no blocks.
    let mut m = Module::new();
    m.add_func(Func {
        name: "empty".into(),
        params: vec![],
        rets: vec![],
        locals: vec![],
        blocks: vec![],
        entry: BlockId(0),
    });
    assert!(
        validate(&m)
            .unwrap_err()
            .iter()
            .any(|x| matches!(x, ValidationError::EmptyFunc { .. }))
    );

    // BadEntry — entry index past the end.
    let mut m = Module::new();
    m.add_func(Func {
        name: "e".into(),
        params: vec![],
        rets: vec![],
        locals: vec![],
        blocks: vec![Block {
            id: BlockId(0),
            stmts: vec![],
            term: Terminator::Return(vec![]),
        }],
        entry: BlockId(5),
    });
    assert!(
        validate(&m)
            .unwrap_err()
            .iter()
            .any(|x| matches!(x, ValidationError::BadEntry { .. }))
    );

    // BadId — a call to a nonexistent function.
    let mut m = Module::new();
    let vt = m.t_void();
    let mut fb = FuncBuilder::new("f", vec![], vec![]);
    let t = fb.local(vt);
    fb.block();
    fb.assign(
        fb.place(t),
        Rvalue::Call(Callee::Direct(FuncId(99)), vec![]),
    );
    fb.ret(vec![]);
    m.add_func(fb.finish());
    assert!(
        validate(&m)
            .unwrap_err()
            .iter()
            .any(|x| matches!(x, ValidationError::BadId { .. }))
    );
}

// --- hand-written .mir fixtures (also the backend's Step-2 inputs) ---

#[test]
fn mir_fixtures_validate_and_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mir");
    let mut count = 0;
    for entry in fs::read_dir(&dir).expect("tests/mir should exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mir") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        let m = parse(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        validate(&m).unwrap_or_else(|e| panic!("validate {}: {e:?}", path.display()));
        let t1 = print(&m);
        let m2 = parse(&t1).unwrap_or_else(|e| panic!("reparse {}: {e}", path.display()));
        let t2 = print(&m2);
        assert_eq!(t1, t2, "round-trip fixpoint for {}", path.display());
        count += 1;
    }
    assert!(count >= 4, "expected >= 4 .mir fixtures, found {count}");
}

// --- extern calls + string lowering (Step-1 gap fix; the tracer-bullet path) ---

#[test]
fn extern_call_and_str_projections_round_trip() {
    // The `spill.it("hi")` → `bet_print(ptr, len)` shape: a `str` literal decomposed into
    // its data pointer and byte length, passed to an FFI import.
    let src = r#"
extern "C" fn bet_print(rawptr, u64) -> void
fn main() -> void {
  let %0: rawptr
  let %1: u64
  let %2: void
  bb0:
    %0 = str_ptr("hi\n")
    %1 = str_len("hi\n")
    %2 = call_extern @bet_print(%0, %1)
    return
}
"#;
    let m = parse(src).expect("extern-call program should parse");
    validate(&m).expect("extern-call program should validate");
    let t1 = print(&m);
    let m2 = parse(&t1).expect("printed extern-call program should re-parse");
    validate(&m2).expect("re-parsed program should still validate");
    let t2 = print(&m2);
    assert_eq!(t1, t2, "extern-call print/parse must reach a fixpoint");
}

#[test]
fn detects_str_projection_on_non_str() {
    // `str_len` on an `i64` operand is a type error.
    let e = errs("fn f(%0: i64) -> u64 { let %1: u64\n bb0: %1 = str_len(%0)\n return %1 }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::TypeMismatch { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_extern_arity_mismatch() {
    // `bet_print` takes two args; calling it with none is a shape error.
    let src = "extern \"C\" fn bet_print(rawptr, u64) -> void \
               fn f() -> void { let %0: void\n bb0: %0 = call_extern @bet_print()\n return }";
    let e = errs(src);
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadShape { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_extern_id() {
    // A call to a nonexistent extern is a dangling id.
    let mut m = Module::new();
    let vt = m.t_void();
    let mut fb = FuncBuilder::new("f", vec![], vec![]);
    let t = fb.local(vt);
    fb.block();
    fb.assign(
        fb.place(t),
        Rvalue::Call(Callee::Extern(ExternId(99)), vec![]),
    );
    fb.ret(vec![]);
    m.add_func(fb.finish());
    assert!(
        validate(&m)
            .unwrap_err()
            .iter()
            .any(|x| matches!(x, ValidationError::BadId { .. }))
    );
}

// --- by-value sum construction + validator hardening (Step-1 gap fix) ---

#[test]
fn by_value_sum_round_trips() {
    let src = "sum Opt { None, Some(i64) } \
               fn f() -> Opt { let %0: Opt\n bb0: %0 = make Opt::Some(7i64)\n return %0 }";
    let m = parse(src).expect("by-value sum program should parse");
    validate(&m).expect("by-value sum program should validate");
    let t1 = print(&m);
    let m2 = parse(&t1).expect("printed by-value sum should re-parse");
    validate(&m2).expect("re-parsed by-value sum should still validate");
    assert_eq!(t1, print(&m2), "by-value sum print/parse fixpoint");
}

#[test]
fn detects_by_value_sum_arity() {
    // `Some` carries one payload; building it with none is a shape error.
    let src = "sum Opt { None, Some(i64) } \
               fn f() -> Opt { let %0: Opt\n bb0: %0 = make Opt::Some()\n return %0 }";
    let e = errs(src);
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::BadShape { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_arith_mode() {
    // Integer `add` must carry Wrap or Trap, never Na.
    let e = errs("fn f(%0: i64) -> i64 { let %1: i64\n bb0: %1 = add.na(%0, %0)\n return %1 }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::TypeMismatch { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_bad_cast() {
    // `itof` (int→float) from a float source is ill-typed.
    let e =
        errs("fn f(%0: f64) -> f64 { let %1: f64\n bb0: %1 = cast.itof(%0 as f64)\n return %1 }");
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::TypeMismatch { .. })),
        "{e:?}"
    );
}

#[test]
fn detects_global_type_mismatch() {
    // A `str`-valued global declared `i64` is a type mismatch.
    let e = errs(
        "const BAD: i64 = \"nope\" \
                  fn f() -> void { bb0: return }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, ValidationError::TypeMismatch { .. })),
        "{e:?}"
    );
}
