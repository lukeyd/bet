//! Semantic tests for the `interp` evaluator.
//!
//! The frontend's real `parse()` lives in another worktree, so these tests build
//! [`frontend::ast`] values **by hand** and assert the captured output. Each test mirrors a
//! specific program in `tests/corpus/**`, so that when our branches merge, the corpus
//! differential harness (`cargo xtask corpus`) lines up program-for-program.

use frontend::ast::*;
use interp::{RunError, run_to_string};

// ============================================================================
// Tiny AST-builder DSL (keeps the tests legible).
// ============================================================================

fn sp() -> Span {
    Span::DUMMY
}
fn ex(kind: ExprKind) -> Expr {
    Expr { kind, span: sp() }
}
fn int(u: u64) -> Expr {
    ex(ExprKind::Int(u as i128))
}
fn float(f: f64) -> Expr {
    ex(ExprKind::Float(f))
}
fn string(s: &str) -> Expr {
    ex(ExprKind::Str(s.into()))
}
fn boolean(b: bool) -> Expr {
    ex(ExprKind::Bool(b))
}
fn ghosted() -> Expr {
    ex(ExprKind::Ghosted)
}
fn name(n: &str) -> Expr {
    ex(ExprKind::Name {
        name: n.into(),
        generics: vec![],
    })
}
/// A generic reference `foo[T]` — the type args are ignored at runtime (monomorphization).
fn name_g(n: &str, generics: Vec<Type>) -> Expr {
    ex(ExprKind::Name {
        name: n.into(),
        generics,
    })
}
fn field(recv: Expr, n: &str) -> Expr {
    ex(ExprKind::Field {
        base: Box::new(recv),
        name: n.into(),
        generics: vec![],
    })
}
fn index(recv: Expr, idx: Expr) -> Expr {
    ex(ExprKind::Index {
        base: Box::new(recv),
        index: Box::new(idx),
    })
}
fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    ex(ExprKind::Call {
        callee: Box::new(callee),
        args: args
            .into_iter()
            .map(|value| Arg { label: None, value })
            .collect(),
    })
}
/// A method call `receiver.method(args)` — the dedicated form for builtins (`spill.*`, `str.*`)
/// and user methods with a receiver.
fn method_call(receiver: Expr, method: &str, args: Vec<Expr>) -> Expr {
    ex(ExprKind::Method {
        receiver: Box::new(receiver),
        method: method.into(),
        generics: vec![],
        args: args
            .into_iter()
            .map(|value| Arg { label: None, value })
            .collect(),
    })
}
fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    ex(ExprKind::Binary(op, Box::new(l), Box::new(r)))
}
fn un(op: UnOp, e: Expr) -> Expr {
    ex(ExprKind::Unary(op, Box::new(e)))
}
fn cast(e: Expr, ty: Type) -> Expr {
    ex(ExprKind::Cast(Box::new(e), ty))
}
fn array(elems: Vec<Expr>) -> Expr {
    ex(ExprKind::Array(elems))
}
fn struct_lit(ty: &str, fields: Vec<(&str, Expr)>) -> Expr {
    ex(ExprKind::Struct(StructLit {
        name: ty.into(),
        generics: vec![],
        fields: fields
            .into_iter()
            .map(|(name, value)| FieldInit {
                name: name.into(),
                value,
                span: sp(),
            })
            .collect(),
        span: sp(),
    }))
}
fn tn(n: &str) -> Type {
    Type {
        kind: TypeKind::Named(n.into(), vec![]),
        span: sp(),
    }
}

// -- statements --------------------------------------------------------------

fn st(kind: StmtKind) -> Stmt {
    Stmt { kind, span: sp() }
}
fn expr_stmt(e: Expr) -> Stmt {
    st(StmtKind::Expr(e))
}
fn bet(vals: Vec<Expr>) -> Stmt {
    st(StmtKind::Bet(vals))
}
fn let_(names: &[&str], ty: Option<Type>, values: Vec<Expr>) -> Stmt {
    st(StmtKind::Var(VarDecl {
        vis: Vis::Hush,
        targets: names.iter().map(|s| s.to_string()).collect(),
        ty,
        values,
        span: sp(),
    }))
}
fn const_(n: &str, ty: Option<Type>, value: Expr) -> Stmt {
    st(StmtKind::Const(ConstDecl {
        vis: Vis::Hush,
        name: n.into(),
        ty,
        value,
        span: sp(),
    }))
}
fn assign(targets: Vec<Expr>, op: AssignOp, values: Vec<Expr>) -> Stmt {
    st(StmtKind::Assign {
        targets,
        op,
        values,
    })
}
fn block(stmts: Vec<Stmt>) -> Block {
    Block { stmts, span: sp() }
}
fn spill_it(e: Expr) -> Stmt {
    expr_stmt(method_call(name("spill"), "it", vec![e]))
}
fn spill_f(fmt: &str, args: Vec<Expr>) -> Stmt {
    let mut all = vec![string(fmt)];
    all.extend(args);
    expr_stmt(method_call(name("spill"), "f", all))
}

// -- items -------------------------------------------------------------------

fn param(n: &str, ty: &str) -> Param {
    Param {
        name: n.into(),
        ty: tn(ty),
        span: sp(),
    }
}
/// Convert a plain list of return types into the AST's `RetType` shape (unused at runtime).
fn ret_of(tys: Vec<Type>) -> RetType {
    match tys.len() {
        0 => RetType::None,
        1 => RetType::Single(tys.into_iter().next().expect("len 1")),
        _ => RetType::Multi(tys),
    }
}
fn func(name: &str, params: Vec<Param>, ret: Vec<Type>, body: Vec<Stmt>) -> Item {
    Item::Func(FnDecl {
        vis: Vis::Hush,
        receiver: None,
        name: name.into(),
        generics: vec![],
        params,
        ret: ret_of(ret),
        body: block(body),
        span: sp(),
    })
}
fn method(recv: Param, name: &str, params: Vec<Param>, ret: Vec<Type>, body: Vec<Stmt>) -> Item {
    Item::Func(FnDecl {
        vis: Vis::Hush,
        receiver: Some(Receiver {
            name: recv.name,
            ty: recv.ty,
            span: sp(),
        }),
        name: name.into(),
        generics: vec![],
        params,
        ret: ret_of(ret),
        body: block(body),
        span: sp(),
    })
}
fn main_fn(body: Vec<Stmt>) -> Item {
    func("main", vec![], vec![], body)
}
fn drip(name: &str, fields: &[(&str, &str)]) -> Item {
    Item::Drip(DripDecl {
        vis: Vis::Hush,
        name: name.into(),
        generics: vec![],
        fields: fields
            .iter()
            .map(|(n, t)| FieldDecl {
                vis: Some(Vis::Flex),
                name: (*n).into(),
                ty: tn(t),
                span: sp(),
            })
            .collect(),
        span: sp(),
    })
}
fn moods(name: &str, variants: &[(&str, usize)]) -> Item {
    Item::Moods(MoodsDecl {
        vis: Vis::Hush,
        name: name.into(),
        generics: vec![],
        variants: variants
            .iter()
            .map(|(n, arity)| Variant {
                name: (*n).into(),
                payload: vec![tn("int"); *arity],
                span: sp(),
            })
            .collect(),
        span: sp(),
    })
}
fn program(items: Vec<Item>) -> Program {
    Program { items }
}
/// A real `vibe` arm `Variant(binds) { body }` (the `naw` wildcard is a separate default block).
fn arm(variant: &str, binds: &[&str], body: Vec<Stmt>) -> MatchArm {
    MatchArm {
        variant: variant.into(),
        bindings: binds.iter().map(|s| s.to_string()).collect(),
        body: block(body),
        span: sp(),
    }
}

/// Run a single-`main` program and return its captured stdout.
fn run_main(body: Vec<Stmt>) -> String {
    run_to_string(&program(vec![main_fn(body)])).expect("program should run")
}

// -- memory model & error handling builders ----------------------------------

/// A top-level `crib name [: Type]` arena declaration (typed hands `cop` a tag).
fn crib_item(name: &str, typed: bool) -> Item {
    Item::Crib(CribDecl {
        vis: Vis::Hush,
        name: name.into(),
        ty: typed.then(|| tn("Enemy")),
        span: sp(),
    })
}
/// A function-local `crib name` statement (untyped unless `typed`).
fn crib_stmt(name: &str, typed: bool) -> Stmt {
    st(StmtKind::Crib(CribDecl {
        vis: Vis::Hush,
        name: name.into(),
        ty: typed.then(|| tn("Enemy")),
        span: sp(),
    }))
}
/// `cop Ty{ fields } in crib`.
fn cop_struct(ty: &str, fields: Vec<(&str, Expr)>, crib: Expr) -> Expr {
    ex(ExprKind::Cop {
        init: Box::new(CopInit::Struct(StructLit {
            name: ty.into(),
            generics: vec![],
            fields: fields
                .into_iter()
                .map(|(name, value)| FieldInit {
                    name: name.into(),
                    value,
                    span: sp(),
                })
                .collect(),
            span: sp(),
        })),
        crib: Box::new(crib),
    })
}
/// `holla binding = tag in crib { live } ghosted { ghosted }`.
fn holla(binding: &str, tag: Expr, crib: Expr, live: Vec<Stmt>, ghosted: Vec<Stmt>) -> Stmt {
    st(StmtKind::Holla {
        binding: binding.into(),
        tag,
        crib,
        live: block(live),
        ghosted: block(ghosted),
    })
}
/// `tag.trust() in crib`.
fn trust(tag: Expr, crib: Expr) -> Expr {
    ex(ExprKind::Trust {
        tag: Box::new(tag),
        crib: Box::new(crib),
    })
}
fn evict(crib: Expr) -> Stmt {
    st(StmtKind::Evict(crib))
}
/// `yikes.new(msg)` — construct an error value.
fn yikes_new(msg: &str) -> Expr {
    method_call(name("yikes"), "new", vec![string(msg)])
}
fn bounce(e: Expr) -> Stmt {
    st(StmtKind::Bounce(e))
}
fn sheesh(body: Vec<Stmt>, recover: Option<(&str, Vec<Stmt>)>) -> Stmt {
    st(StmtKind::Sheesh {
        body: block(body),
        recover: recover.map(|(n, b)| (n.to_string(), block(b))),
    })
}

// ============================================================================
// 01-basics
// ============================================================================

#[test]
fn hello() {
    // finna main() { spill.it("hi") }
    assert_eq!(run_main(vec![spill_it(string("hi"))]), "hi\n");
}

#[test]
fn spill_format() {
    // spill.f("hp: {} / {}\n", 80, 100)
    assert_eq!(
        run_main(vec![spill_f("hp: {} / {}\n", vec![int(80), int(100)])]),
        "hp: 80 / 100\n"
    );
}

#[test]
fn spill_format_literal_braces() {
    assert_eq!(run_main(vec![spill_f("{{{}}}\n", vec![int(7)])]), "{7}\n");
}

// ============================================================================
// 02-values
// ============================================================================

#[test]
fn arithmetic() {
    // 7+3, 7-3, 7*3, 7/3, 7%3
    let out = run_main(vec![
        spill_it(bin(BinOp::Add, int(7), int(3))),
        spill_it(bin(BinOp::Sub, int(7), int(3))),
        spill_it(bin(BinOp::Mul, int(7), int(3))),
        spill_it(bin(BinOp::Div, int(7), int(3))),
        spill_it(bin(BinOp::Rem, int(7), int(3))),
    ]);
    assert_eq!(out, "10\n4\n21\n2\n1\n");
}

#[test]
fn bool_logic() {
    // nocap && cap, nocap || cap, !cap, 3 < 5 && 5 <= 5
    let out = run_main(vec![
        spill_it(bin(BinOp::And, boolean(true), boolean(false))),
        spill_it(bin(BinOp::Or, boolean(true), boolean(false))),
        spill_it(un(UnOp::Not, boolean(false))),
        spill_it(bin(
            BinOp::And,
            bin(BinOp::Lt, int(3), int(5)),
            bin(BinOp::Le, int(5), int(5)),
        )),
    ]);
    assert_eq!(out, "cap\nnocap\nnocap\nnocap\n");
}

#[test]
fn short_circuit_and_does_not_eval_rhs() {
    // cap && (undefined name) must NOT evaluate the rhs — short-circuits to cap.
    let out = run_main(vec![spill_it(bin(
        BinOp::And,
        boolean(false),
        name("nope"),
    ))]);
    assert_eq!(out, "cap\n");
}

#[test]
fn casts() {
    // big: int = 300; big as u8 -> 44 ; f: f64 = 3.9; f as int -> 3
    let out = run_main(vec![
        let_(&["big"], Some(tn("int")), vec![int(300)]),
        let_(&["small"], None, vec![cast(name("big"), tn("u8"))]),
        spill_it(name("small")),
        let_(&["f"], Some(tn("f64")), vec![float(3.9)]),
        let_(&["n"], None, vec![cast(name("f"), tn("int"))]),
        spill_it(name("n")),
    ]);
    assert_eq!(out, "44\n3\n");
}

#[test]
fn compound_assign() {
    // n = 10; += 5; -= 3; *= 2; /= 4; %= 4; <<= 3; >>= 1; &= 12; |= 1; ^= 3  => 10
    let out = run_main(vec![
        let_(&["n"], None, vec![int(10)]),
        assign(vec![name("n")], AssignOp::AddEq, vec![int(5)]),
        assign(vec![name("n")], AssignOp::SubEq, vec![int(3)]),
        assign(vec![name("n")], AssignOp::MulEq, vec![int(2)]),
        assign(vec![name("n")], AssignOp::DivEq, vec![int(4)]),
        assign(vec![name("n")], AssignOp::RemEq, vec![int(4)]),
        assign(vec![name("n")], AssignOp::ShlEq, vec![int(3)]),
        assign(vec![name("n")], AssignOp::ShrEq, vec![int(1)]),
        assign(vec![name("n")], AssignOp::AndEq, vec![int(12)]),
        assign(vec![name("n")], AssignOp::OrEq, vec![int(1)]),
        assign(vec![name("n")], AssignOp::XorEq, vec![int(3)]),
        spill_it(name("n")),
    ]);
    assert_eq!(out, "10\n");
}

#[test]
fn lowkey_facts() {
    // lowkey x = 5; x = x + 1; facts MAX: int = 100; print x, MAX
    let out = run_main(vec![
        let_(&["x"], None, vec![int(5)]),
        assign(
            vec![name("x")],
            AssignOp::Eq,
            vec![bin(BinOp::Add, name("x"), int(1))],
        ),
        const_("MAX", Some(tn("int")), int(100)),
        spill_it(name("x")),
        spill_it(name("MAX")),
    ]);
    assert_eq!(out, "6\n100\n");
}

#[test]
fn numeric_tower() {
    let out = run_main(vec![
        let_(&["a"], Some(tn("i32")), vec![int(2_000_000_000)]),
        let_(&["b"], Some(tn("u8")), vec![int(255)]),
        let_(&["c"], Some(tn("i64")), vec![int(9_000_000_000)]),
        let_(&["d"], Some(tn("int")), vec![un(UnOp::Neg, int(42))]),
        spill_it(name("a")),
        spill_it(name("b")),
        spill_it(name("c")),
        spill_it(name("d")),
    ]);
    assert_eq!(out, "2000000000\n255\n9000000000\n-42\n");
}

#[test]
fn strings_stdlib() {
    // s = "hello"; print s, str.glow(s) -> HELLO, str.slaps(s,"hello") -> nocap
    let out = run_main(vec![
        let_(&["s"], None, vec![string("hello")]),
        spill_it(name("s")),
        spill_it(method_call(name("str"), "glow", vec![name("s")])),
        spill_it(method_call(
            name("str"),
            "slaps",
            vec![name("s"), string("hello")],
        )),
    ]);
    assert_eq!(out, "hello\nHELLO\nnocap\n");
}

#[test]
fn bitnot_and_neg() {
    // ~0 == -1 ; -(-5) == 5
    let out = run_main(vec![
        spill_it(un(UnOp::BitNot, int(0))),
        spill_it(un(UnOp::Neg, un(UnOp::Neg, int(5)))),
    ]);
    assert_eq!(out, "-1\n5\n");
}

// ============================================================================
// 03-control
// ============================================================================

fn fr(cond: Expr, then: Vec<Stmt>, elifs: Vec<(Expr, Vec<Stmt>)>, els: Option<Vec<Stmt>>) -> Stmt {
    st(StmtKind::Fr(FrStmt {
        cond,
        then: block(then),
        elifs: elifs
            .into_iter()
            .map(|(cond, body)| (cond, block(body)))
            .collect(),
        els: els.map(block),
    }))
}

#[test]
fn fr_naw_classify() {
    // finna classify(n) -> str { fr n<0 {bet "neg"} naw fr n==0 {bet "zero"} naw {bet "pos"} }
    let classify = func(
        "classify",
        vec![param("n", "int")],
        vec![tn("str")],
        vec![fr(
            bin(BinOp::Lt, name("n"), int(0)),
            vec![bet(vec![string("neg")])],
            vec![(
                bin(BinOp::Eq, name("n"), int(0)),
                vec![bet(vec![string("zero")])],
            )],
            Some(vec![bet(vec![string("pos")])]),
        )],
    );
    let main = main_fn(vec![
        spill_it(call(name("classify"), vec![un(UnOp::Neg, int(3))])),
        spill_it(call(name("classify"), vec![int(0)])),
        spill_it(call(name("classify"), vec![int(7)])),
    ]);
    let out = run_to_string(&program(vec![classify, main])).unwrap();
    assert_eq!(out, "neg\nzero\npos\n");
}

#[test]
fn vibin_while() {
    // i=1; total=0; while i<=5 { total+=i; i+=1 } print total -> 15
    let out = run_main(vec![
        let_(&["i"], None, vec![int(1)]),
        let_(&["total"], None, vec![int(0)]),
        st(StmtKind::Vibin {
            cond: bin(BinOp::Le, name("i"), int(5)),
            body: block(vec![
                assign(
                    vec![name("total")],
                    AssignOp::Eq,
                    vec![bin(BinOp::Add, name("total"), name("i"))],
                ),
                assign(
                    vec![name("i")],
                    AssignOp::Eq,
                    vec![bin(BinOp::Add, name("i"), int(1))],
                ),
            ]),
        }),
        spill_it(name("total")),
    ]);
    assert_eq!(out, "15\n");
}

#[test]
fn squad_for_in() {
    // xs = [10,20,30]; sum=0; squad x in xs { sum = sum + x } -> 60
    let out = run_main(vec![
        let_(&["xs"], None, vec![array(vec![int(10), int(20), int(30)])]),
        let_(&["sum"], None, vec![int(0)]),
        st(StmtKind::Squad {
            var: "x".into(),
            iter: name("xs"),
            body: block(vec![assign(
                vec![name("sum")],
                AssignOp::Eq,
                vec![bin(BinOp::Add, name("sum"), name("x"))],
            )]),
        }),
        spill_it(name("sum")),
    ]);
    assert_eq!(out, "60\n");
}

#[test]
fn nested_loops_dip_skip() {
    // vibin i<100 { i+=1; fr sum>20 {dip}; fr i%2==0 {skip}; sum=sum+i } -> 25
    let out = run_main(vec![
        let_(&["sum"], None, vec![int(0)]),
        let_(&["i"], None, vec![int(0)]),
        st(StmtKind::Vibin {
            cond: bin(BinOp::Lt, name("i"), int(100)),
            body: block(vec![
                assign(
                    vec![name("i")],
                    AssignOp::Eq,
                    vec![bin(BinOp::Add, name("i"), int(1))],
                ),
                fr(
                    bin(BinOp::Gt, name("sum"), int(20)),
                    vec![st(StmtKind::Dip)],
                    vec![],
                    None,
                ),
                fr(
                    bin(BinOp::Eq, bin(BinOp::Rem, name("i"), int(2)), int(0)),
                    vec![st(StmtKind::Skip)],
                    vec![],
                    None,
                ),
                assign(
                    vec![name("sum")],
                    AssignOp::Eq,
                    vec![bin(BinOp::Add, name("sum"), name("i"))],
                ),
            ]),
        }),
        spill_it(name("sum")),
    ]);
    assert_eq!(out, "25\n");
}

// ============================================================================
// 04-functions
// ============================================================================

#[test]
fn finna_basics() {
    // finna add(a,b)->int { bet a+b }  main { spill.it(add(2,40)) } -> 42
    let add = func(
        "add",
        vec![param("a", "int"), param("b", "int")],
        vec![tn("int")],
        vec![bet(vec![bin(BinOp::Add, name("a"), name("b"))])],
    );
    let main = main_fn(vec![spill_it(call(name("add"), vec![int(2), int(40)]))]);
    assert_eq!(run_to_string(&program(vec![add, main])).unwrap(), "42\n");
}

#[test]
fn multi_return_divmod() {
    // finna divmod(a,b) -> (int,int) { bet a/b, a%b }
    // lowkey q, r = divmod(17,5); spill.f("{} r {}\n", q, r) -> "3 r 2"
    let divmod = func(
        "divmod",
        vec![param("a", "int"), param("b", "int")],
        vec![tn("int"), tn("int")],
        vec![bet(vec![
            bin(BinOp::Div, name("a"), name("b")),
            bin(BinOp::Rem, name("a"), name("b")),
        ])],
    );
    let main = main_fn(vec![
        let_(
            &["q", "r"],
            None,
            vec![call(name("divmod"), vec![int(17), int(5)])],
        ),
        spill_f("{} r {}\n", vec![name("q"), name("r")]),
    ]);
    assert_eq!(
        run_to_string(&program(vec![divmod, main])).unwrap(),
        "3 r 2\n"
    );
}

#[test]
fn receivers_method() {
    // drip Counter { n: int }; finna (c: Counter) bump(by) -> int { bet c.n + by }
    // c = Counter{n:10}; spill.it(c.bump(5)) -> 15
    let counter = drip("Counter", &[("n", "int")]);
    let bump = method(
        param("c", "Counter"),
        "bump",
        vec![param("by", "int")],
        vec![tn("int")],
        vec![bet(vec![bin(
            BinOp::Add,
            field(name("c"), "n"),
            name("by"),
        )])],
    );
    let main = main_fn(vec![
        let_(
            &["c"],
            None,
            vec![struct_lit("Counter", vec![("n", int(10))])],
        ),
        spill_it(method_call(name("c"), "bump", vec![int(5)])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![counter, bump, main])).unwrap(),
        "15\n"
    );
}

#[test]
fn first_class_fn() {
    // finna dub(x)->int{bet x*2}  finna inc(x)->int{bet x+1}
    // finna apply(f: finna(int)->int, x) -> int { bet f(x) }
    // spill.it(apply(dub, 21)) -> 42 ; g = inc; spill.it(g(41)) -> 42
    let dub = func(
        "dub",
        vec![param("x", "int")],
        vec![tn("int")],
        vec![bet(vec![bin(BinOp::Mul, name("x"), int(2))])],
    );
    let inc = func(
        "inc",
        vec![param("x", "int")],
        vec![tn("int")],
        vec![bet(vec![bin(BinOp::Add, name("x"), int(1))])],
    );
    let fn_ty = Type {
        kind: TypeKind::Fn(vec![tn("int")], Box::new(tn("int"))),
        span: sp(),
    };
    let apply = func(
        "apply",
        vec![
            Param {
                name: "f".into(),
                ty: fn_ty.clone(),
                span: sp(),
            },
            param("x", "int"),
        ],
        vec![tn("int")],
        vec![bet(vec![call(name("f"), vec![name("x")])])],
    );
    let main = main_fn(vec![
        spill_it(call(name("apply"), vec![name("dub"), int(21)])),
        let_(&["g"], Some(fn_ty), vec![name("inc")]),
        spill_it(call(name("g"), vec![int(41)])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![dub, inc, apply, main])).unwrap(),
        "42\n42\n"
    );
}

#[test]
fn generics_fn_monomorphized() {
    // finna pickFirst[T](a: T, b: T) -> T { bet a }
    // spill.it(pickFirst[int](7,9)) -> 7 ; spill.it(pickFirst[str]("a","b")) -> a
    let mut pf = func(
        "pickFirst",
        vec![param("a", "T"), param("b", "T")],
        vec![tn("T")],
        vec![bet(vec![name("a")])],
    );
    if let Item::Func(f) = &mut pf {
        f.generics = vec!["T".into()];
    }
    let main = main_fn(vec![
        spill_it(call(
            name_g("pickFirst", vec![tn("int")]),
            vec![int(7), int(9)],
        )),
        spill_it(call(
            name_g("pickFirst", vec![tn("str")]),
            vec![string("a"), string("b")],
        )),
    ]);
    assert_eq!(run_to_string(&program(vec![pf, main])).unwrap(), "7\na\n");
}

// ============================================================================
// 05-structs
// ============================================================================

#[test]
fn drip_value_copy() {
    // p = Player{hp:100, name:"Ada"}; q = p; q.hp = 50;
    // spill.f("{} has {} hp\n", p.name, p.hp) -> "Ada has 100 hp" ; spill.it(q.hp) -> 50
    let player = drip("Player", &[("hp", "int"), ("name", "str")]);
    let main = main_fn(vec![
        let_(
            &["p"],
            None,
            vec![struct_lit(
                "Player",
                vec![("hp", int(100)), ("name", string("Ada"))],
            )],
        ),
        let_(&["q"], None, vec![name("p")]),
        assign(vec![field(name("q"), "hp")], AssignOp::Eq, vec![int(50)]),
        spill_f(
            "{} has {} hp\n",
            vec![field(name("p"), "name"), field(name("p"), "hp")],
        ),
        spill_it(field(name("q"), "hp")),
    ]);
    assert_eq!(
        run_to_string(&program(vec![player, main])).unwrap(),
        "Ada has 100 hp\n50\n"
    );
}

#[test]
fn drip_generic_uses_base_name() {
    // Pair[int]{a:3,b:4}; pi.a + pi.b -> 7 ; Pair[str]{a:"x",b:"y"}; ps.b -> y
    let pair = {
        let mut d = drip("Pair", &[("a", "T"), ("b", "T")]);
        if let Item::Drip(dd) = &mut d {
            dd.generics = vec!["T".into()];
        }
        d
    };
    // Struct literal with a generic type head Pair[int].
    let pair_int = ex(ExprKind::Struct(StructLit {
        name: "Pair".into(),
        generics: vec![tn("int")],
        fields: vec![
            FieldInit {
                name: "a".into(),
                value: int(3),
                span: sp(),
            },
            FieldInit {
                name: "b".into(),
                value: int(4),
                span: sp(),
            },
        ],
        span: sp(),
    }));
    let pair_str = ex(ExprKind::Struct(StructLit {
        name: "Pair".into(),
        generics: vec![tn("str")],
        fields: vec![
            FieldInit {
                name: "a".into(),
                value: string("x"),
                span: sp(),
            },
            FieldInit {
                name: "b".into(),
                value: string("y"),
                span: sp(),
            },
        ],
        span: sp(),
    }));
    let main = main_fn(vec![
        let_(&["pi"], None, vec![pair_int]),
        let_(&["ps"], None, vec![pair_str]),
        spill_it(bin(
            BinOp::Add,
            field(name("pi"), "a"),
            field(name("pi"), "b"),
        )),
        spill_it(field(name("ps"), "b")),
    ]);
    assert_eq!(run_to_string(&program(vec![pair, main])).unwrap(), "7\ny\n");
}

// ============================================================================
// 06-sumtypes
// ============================================================================

#[test]
fn moods_basics_vibe_payloads() {
    // moods Shape { Circle(int), Rect(int,int), Dot }
    // finna area(s) -> int { vibe s { Circle(r){bet 3*r*r} Rect(w,h){bet w*h} Dot{bet 0} } }
    let shape = moods("Shape", &[("Circle", 1), ("Rect", 2), ("Dot", 0)]);
    let area = func(
        "area",
        vec![param("s", "Shape")],
        vec![tn("int")],
        vec![st(StmtKind::Vibe {
            scrutinee: name("s"),
            arms: vec![
                arm(
                    "Circle",
                    &["r"],
                    vec![bet(vec![bin(
                        BinOp::Mul,
                        bin(BinOp::Mul, int(3), name("r")),
                        name("r"),
                    )])],
                ),
                arm(
                    "Rect",
                    &["w", "h"],
                    vec![bet(vec![bin(BinOp::Mul, name("w"), name("h"))])],
                ),
                arm("Dot", &[], vec![bet(vec![int(0)])]),
            ],
            default: None,
        })],
    );
    let main = main_fn(vec![
        spill_it(call(name("area"), vec![call(name("Circle"), vec![int(2)])])),
        spill_it(call(
            name("area"),
            vec![call(name("Rect"), vec![int(3), int(4)])],
        )),
        spill_it(call(name("area"), vec![name("Dot")])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![shape, area, main])).unwrap(),
        "12\n12\n0\n"
    );
}

#[test]
fn moods_exhaustive_wildcard() {
    // moods Token { Num(int), Plus, Minus, Times }
    // vibe t { Num(n){bet "number"} Plus{bet "op"} naw {bet "other op"} }
    let token = moods(
        "Token",
        &[("Num", 1), ("Plus", 0), ("Minus", 0), ("Times", 0)],
    );
    let describe = func(
        "describe",
        vec![param("t", "Token")],
        vec![tn("str")],
        vec![st(StmtKind::Vibe {
            scrutinee: name("t"),
            arms: vec![
                arm("Num", &["n"], vec![bet(vec![string("number")])]),
                arm("Plus", &[], vec![bet(vec![string("op")])]),
            ],
            default: Some(block(vec![bet(vec![string("other op")])])),
        })],
    );
    let main = main_fn(vec![
        spill_it(call(
            name("describe"),
            vec![call(name("Num"), vec![int(5)])],
        )),
        spill_it(call(name("describe"), vec![name("Plus")])),
        spill_it(call(name("describe"), vec![name("Minus")])),
        spill_it(call(name("describe"), vec![name("Times")])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![token, describe, main])).unwrap(),
        "number\nop\nother op\nother op\n"
    );
}

#[test]
fn expr_eval_moods_in_drip_field() {
    // moods Op { Add, Sub, Mul }; drip Calc { op: Op, lhs: int, rhs: int }
    // finna eval(c) -> int { vibe c.op { Add{bet c.lhs+c.rhs} Sub{...} Mul{...} } }
    let op = moods("Op", &[("Add", 0), ("Sub", 0), ("Mul", 0)]);
    let calc = drip("Calc", &[("op", "Op"), ("lhs", "int"), ("rhs", "int")]);
    let eval = func(
        "eval",
        vec![param("c", "Calc")],
        vec![tn("int")],
        vec![st(StmtKind::Vibe {
            scrutinee: field(name("c"), "op"),
            arms: vec![
                arm(
                    "Add",
                    &[],
                    vec![bet(vec![bin(
                        BinOp::Add,
                        field(name("c"), "lhs"),
                        field(name("c"), "rhs"),
                    )])],
                ),
                arm(
                    "Sub",
                    &[],
                    vec![bet(vec![bin(
                        BinOp::Sub,
                        field(name("c"), "lhs"),
                        field(name("c"), "rhs"),
                    )])],
                ),
                arm(
                    "Mul",
                    &[],
                    vec![bet(vec![bin(
                        BinOp::Mul,
                        field(name("c"), "lhs"),
                        field(name("c"), "rhs"),
                    )])],
                ),
            ],
            default: None,
        })],
    );
    let mk = |opname: &str, l: u64, r: u64| {
        call(
            name("eval"),
            vec![struct_lit(
                "Calc",
                vec![("op", name(opname)), ("lhs", int(l)), ("rhs", int(r))],
            )],
        )
    };
    let main = main_fn(vec![
        spill_it(mk("Add", 6, 7)),
        spill_it(mk("Sub", 10, 4)),
        spill_it(mk("Mul", 3, 9)),
    ]);
    assert_eq!(
        run_to_string(&program(vec![op, calc, eval, main])).unwrap(),
        "13\n6\n27\n"
    );
}

// ============================================================================
// misc values & error paths
// ============================================================================

#[test]
fn ghosted_displays() {
    assert_eq!(run_main(vec![spill_it(ghosted())]), "ghosted\n");
}

#[test]
fn array_indexing() {
    let out = run_main(vec![
        let_(&["xs"], None, vec![array(vec![int(11), int(22), int(33)])]),
        spill_it(index(name("xs"), int(1))),
    ]);
    assert_eq!(out, "22\n");
}

#[test]
fn no_main_is_an_error() {
    let prog = program(vec![func("helper", vec![], vec![], vec![])]);
    assert_eq!(run_to_string(&prog), Err(RunError::NoMain));
}

#[test]
fn division_by_zero_is_an_error() {
    let err = run_to_string(&program(vec![main_fn(vec![spill_it(bin(
        BinOp::Div,
        int(1),
        int(0),
    ))])]));
    assert_eq!(err, Err(RunError::DivByZero));
}

#[test]
fn yeet_panics_with_value() {
    let err = run_to_string(&program(vec![main_fn(vec![st(StmtKind::Yeet(string(
        "boom",
    )))])]));
    assert_eq!(err, Err(RunError::Yeet(interp::Value::Str("boom".into()))));
}

#[test]
fn undefined_name_is_an_error() {
    let err = run_to_string(&program(vec![main_fn(vec![spill_it(name("nope"))])]));
    assert_eq!(err, Err(RunError::Undefined("nope".into())));
}

#[test]
fn cop_into_undefined_crib_errors_cleanly() {
    // `cop Foo{} in arena` where `arena` names no crib is a clean error, not a panic.
    let cop = cop_struct("Foo", vec![], name("arena"));
    let err = run_to_string(&program(vec![main_fn(vec![expr_stmt(cop)])]));
    assert_eq!(err, Err(RunError::Undefined("arena".into())));
}

// ============================================================================
// 07-errors — yikes / tea / bounce / yeet-sheesh
// ============================================================================

#[test]
fn yikes_value_and_ghosted_idiom() {
    // y = yikes.new("boom"); fr y != ghosted { spill.it(y) } naw { spill.it("clean") } -> boom
    let out = run_main(vec![
        let_(&["y"], None, vec![yikes_new("boom")]),
        fr(
            bin(BinOp::Ne, name("y"), ghosted()),
            vec![spill_it(name("y"))],
            vec![],
            Some(vec![spill_it(string("clean"))]),
        ),
    ]);
    assert_eq!(out, "boom\n");
}

#[test]
fn tea_prefixes_context() {
    // yikes.new("no such file").tea("loading config") -> "loading config: no such file"
    let wrapped = method_call(
        yikes_new("no such file"),
        "tea",
        vec![string("loading config")],
    );
    assert_eq!(
        run_main(vec![spill_it(wrapped)]),
        "loading config: no such file\n"
    );
}

#[test]
fn bounce_early_returns_the_error() {
    // step(n) -> (int, yikes): negative n is an error; pipe bounces it, else returns the value.
    let step = func(
        "step",
        vec![param("n", "int")],
        vec![tn("int"), tn("yikes")],
        vec![fr(
            bin(BinOp::Lt, name("n"), int(0)),
            vec![bet(vec![int(0), yikes_new("negative")])],
            vec![],
            Some(vec![bet(vec![
                bin(BinOp::Mul, name("n"), int(2)),
                ghosted(),
            ])]),
        )],
    );
    let pipe = func(
        "pipe",
        vec![param("n", "int")],
        vec![tn("int"), tn("yikes")],
        vec![
            let_(&["a", "y"], None, vec![call(name("step"), vec![name("n")])]),
            bounce(name("y")),
            bet(vec![name("a"), ghosted()]),
        ],
    );
    let main = main_fn(vec![
        let_(
            &["r", "y"],
            None,
            vec![call(name("pipe"), vec![un(UnOp::Neg, int(1))])],
        ),
        fr(
            bin(BinOp::Ne, name("y"), ghosted()),
            vec![spill_it(name("y"))],
            vec![],
            Some(vec![spill_it(name("r"))]),
        ),
        let_(&["r2", "y2"], None, vec![call(name("pipe"), vec![int(3)])]),
        fr(
            bin(BinOp::Ne, name("y2"), ghosted()),
            vec![spill_it(name("y2"))],
            vec![],
            Some(vec![spill_it(name("r2"))]),
        ),
    ]);
    assert_eq!(
        run_to_string(&program(vec![step, pipe, main])).unwrap(),
        "negative\n6\n"
    );
}

#[test]
fn sheesh_recovers_a_yeet() {
    // risky(0) yeets; sheesh catches it and binds the yeeted value to `p`.
    let risky = func(
        "risky",
        vec![param("n", "int")],
        vec![],
        vec![fr(
            bin(BinOp::Eq, name("n"), int(0)),
            vec![st(StmtKind::Yeet(string("div by zero")))],
            vec![],
            Some(vec![spill_it(bin(BinOp::Div, int(100), name("n")))]),
        )],
    );
    let main = main_fn(vec![sheesh(
        vec![
            expr_stmt(call(name("risky"), vec![int(5)])),
            expr_stmt(call(name("risky"), vec![int(0)])),
            spill_it(string("after")),
        ],
        Some(("p", vec![spill_f("recovered: {}\n", vec![name("p")])])),
    )]);
    assert_eq!(
        run_to_string(&program(vec![risky, main])).unwrap(),
        "20\nrecovered: div by zero\n"
    );
}

// ============================================================================
// 08-memory — crib / cop / tag / holla / trust / evict
// ============================================================================

#[test]
fn untyped_crib_cop_returns_direct_reference() {
    // crib frame; a = cop Particle{x:1,y:2} in frame; b = cop {x:3,y:4}; a.x + b.y -> 5
    let out = run_main(vec![
        crib_stmt("frame", false),
        let_(
            &["a"],
            None,
            vec![cop_struct(
                "Particle",
                vec![("x", int(1)), ("y", int(2))],
                name("frame"),
            )],
        ),
        let_(
            &["b"],
            None,
            vec![cop_struct(
                "Particle",
                vec![("x", int(3)), ("y", int(4))],
                name("frame"),
            )],
        ),
        spill_it(bin(
            BinOp::Add,
            field(name("a"), "x"),
            field(name("b"), "y"),
        )),
        evict(name("frame")),
        spill_it(string("evicted")),
    ]);
    assert_eq!(out, "5\nevicted\n");
}

#[test]
fn typed_crib_holla_and_generation_reuse() {
    // A top-level typed crib + idOf: a fresh tag resolves; after evict + reuse, the OLD tag
    // is safely ghosted rather than reading the new occupant (spec §7.3-§7.4).
    let slots = crib_item("slots", true);
    let id_of = func(
        "idOf",
        vec![param("e", "Enemy")],
        vec![tn("int")],
        vec![holla(
            "r",
            name("e"),
            name("slots"),
            vec![bet(vec![field(name("r"), "id")])],
            vec![bet(vec![un(UnOp::Neg, int(1))])],
        )],
    );
    let main = main_fn(vec![
        let_(
            &["first"],
            None,
            vec![cop_struct("Enemy", vec![("id", int(7))], name("slots"))],
        ),
        spill_it(call(name("idOf"), vec![name("first")])),
        evict(name("slots")),
        let_(
            &["second"],
            None,
            vec![cop_struct("Enemy", vec![("id", int(42))], name("slots"))],
        ),
        spill_it(call(name("idOf"), vec![name("second")])),
        spill_it(call(name("idOf"), vec![name("first")])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![slots, id_of, main])).unwrap(),
        "7\n42\n-1\n"
    );
}

#[test]
fn holla_ghosted_after_evict() {
    // A tag dangles once its crib is evicted: the live arm runs first, the ghosted arm after.
    let enemies = crib_item("enemies", true);
    let hp_of = func(
        "hpOf",
        vec![param("e", "Enemy")],
        vec![tn("int")],
        vec![holla(
            "r",
            name("e"),
            name("enemies"),
            vec![bet(vec![field(name("r"), "hp")])],
            vec![bet(vec![un(UnOp::Neg, int(1))])],
        )],
    );
    let main = main_fn(vec![
        let_(
            &["a"],
            None,
            vec![cop_struct("Enemy", vec![("hp", int(30))], name("enemies"))],
        ),
        spill_it(call(name("hpOf"), vec![name("a")])),
        evict(name("enemies")),
        spill_it(call(name("hpOf"), vec![name("a")])),
    ]);
    assert_eq!(
        run_to_string(&program(vec![enemies, hp_of, main])).unwrap(),
        "30\n-1\n"
    );
}

#[test]
fn trust_reads_the_slot_unchecked() {
    // `e.trust() in enemies` skips the generation check and reads the slot directly.
    let enemies = crib_item("enemies", true);
    let main = main_fn(vec![
        let_(
            &["e"],
            None,
            vec![cop_struct("Enemy", vec![("hp", int(77))], name("enemies"))],
        ),
        let_(&["r"], None, vec![trust(name("e"), name("enemies"))]),
        spill_it(field(name("r"), "hp")),
    ]);
    assert_eq!(
        run_to_string(&program(vec![enemies, main])).unwrap(),
        "77\n"
    );
}

// ============================================================================
// 10-stdlib (bytes) & index assignment
// ============================================================================

#[test]
fn bytes_read_u32le() {
    // Little-endian u32 decode out of a []u8 (corpus 10-stdlib/bytes-parse).
    let out = run_main(vec![
        let_(
            &["buf"],
            None,
            vec![array(vec![
                int(0x2A),
                int(0),
                int(0),
                int(0),
                int(0x07),
                int(0),
                int(0),
                int(0),
            ])],
        ),
        spill_it(method_call(
            name("bytes"),
            "readU32le",
            vec![name("buf"), int(0)],
        )),
        spill_it(method_call(
            name("bytes"),
            "readU32le",
            vec![name("buf"), int(4)],
        )),
    ]);
    assert_eq!(out, "42\n7\n");
}

#[test]
fn index_assignment_updates_element() {
    // xs[1] = 99
    let out = run_main(vec![
        let_(&["xs"], None, vec![array(vec![int(1), int(2), int(3)])]),
        assign(vec![index(name("xs"), int(1))], AssignOp::Eq, vec![int(99)]),
        spill_it(index(name("xs"), int(1))),
    ]);
    assert_eq!(out, "99\n");
}

#[test]
fn signed_overflow_traps() {
    // i64::MAX + 1 traps (amendment §2.4 debug-build behavior).
    let err = run_to_string(&program(vec![main_fn(vec![spill_it(bin(
        BinOp::Add,
        int(i64::MAX as u64),
        int(1),
    ))])]));
    assert!(matches!(err, Err(RunError::Overflow(_))), "got {err:?}");
}
