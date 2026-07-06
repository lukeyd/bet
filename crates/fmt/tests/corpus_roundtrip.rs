//! Whole-corpus round-trip test for the canonical formatter.
//!
//! For every `.bet` program under `tests/corpus/` this asserts the two properties that make a
//! formatter *canonical*:
//!
//! 1. **Idempotency** — `format_source(format_source(x)) == format_source(x)`. The canonical
//!    form is a fixed point: re-running the formatter never changes it.
//! 2. **Parse-stability** — the formatted output re-parses without error, and to a structurally
//!    identical AST. We compare the parsed [`Program`] values with `==` after normalizing spans
//!    to [`Span::DUMMY`] (byte offsets naturally shift when the surface text is rewritten; every
//!    other node — the actual *structure* — must match exactly). This is what catches a
//!    formatter that silently drops, reorders, or mis-nests syntax.

use std::fs;
use std::path::{Path, PathBuf};

use frontend::ast::*;

#[test]
fn corpus_round_trips() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let mut files = Vec::new();
    collect_bet(&corpus, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no .bet files found under {}",
        corpus.display()
    );

    for path in &files {
        let display = path.display();
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {display}: {e}"));

        // (1) Formatting succeeds and is idempotent.
        let once = fmt::format_source(&src).unwrap_or_else(|e| panic!("formatting {display}: {e}"));
        let twice =
            fmt::format_source(&once).unwrap_or_else(|e| panic!("re-formatting {display}: {e}"));
        assert_eq!(once, twice, "formatting is not idempotent for {display}");

        // (2) The formatted output re-parses to a structurally identical AST.
        let mut original =
            frontend::parse(&src).unwrap_or_else(|e| panic!("parsing original {display}: {e}"));
        let mut reparsed = frontend::parse(&once).unwrap_or_else(|e| {
            panic!("formatted output of {display} does not re-parse: {e}\n--- output ---\n{once}")
        });
        normalize(&mut original);
        normalize(&mut reparsed);
        assert_eq!(
            original, reparsed,
            "formatting changed the structure of {display}\n--- output ---\n{once}"
        );
    }

    eprintln!("round-tripped {} corpus programs", files.len());
}

/// Recursively collect every `*.bet` file under `dir`.
fn collect_bet(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => panic!("reading corpus dir {}: {e}", dir.display()),
    };
    for entry in entries {
        let entry = entry.expect("corpus dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_bet(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("bet") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Span normalization — zero every span so `==` compares structure, not byte offsets.
// ---------------------------------------------------------------------------

fn normalize(p: &mut Program) {
    for item in &mut p.items {
        norm_item(item);
    }
}

fn norm_item(item: &mut Item) {
    match item {
        Item::Pull(x) => x.span = Span::DUMMY,
        Item::Func(x) => norm_fn(x),
        Item::Drip(x) => norm_drip(x),
        Item::Moods(x) => norm_moods(x),
        Item::Crib(x) => norm_crib(x),
        Item::Const(x) => norm_const(x),
        Item::Var(x) => norm_var(x),
        Item::Extern(x) => norm_extern(x),
    }
}

fn norm_fn(x: &mut FnDecl) {
    x.span = Span::DUMMY;
    if let Some(r) = &mut x.receiver {
        r.span = Span::DUMMY;
        norm_type(&mut r.ty);
    }
    for p in &mut x.params {
        p.span = Span::DUMMY;
        norm_type(&mut p.ty);
    }
    norm_ret(&mut x.ret);
    norm_block(&mut x.body);
}

fn norm_drip(x: &mut DripDecl) {
    x.span = Span::DUMMY;
    for f in &mut x.fields {
        f.span = Span::DUMMY;
        norm_type(&mut f.ty);
    }
}

fn norm_moods(x: &mut MoodsDecl) {
    x.span = Span::DUMMY;
    for v in &mut x.variants {
        v.span = Span::DUMMY;
        for t in &mut v.payload {
            norm_type(t);
        }
    }
}

fn norm_crib(x: &mut CribDecl) {
    x.span = Span::DUMMY;
    if let Some(t) = &mut x.ty {
        norm_type(t);
    }
}

fn norm_const(x: &mut ConstDecl) {
    x.span = Span::DUMMY;
    if let Some(t) = &mut x.ty {
        norm_type(t);
    }
    norm_expr(&mut x.value);
}

fn norm_var(x: &mut VarDecl) {
    x.span = Span::DUMMY;
    if let Some(t) = &mut x.ty {
        norm_type(t);
    }
    for v in &mut x.values {
        norm_expr(v);
    }
}

fn norm_extern(x: &mut ExternDecl) {
    x.span = Span::DUMMY;
    for p in &mut x.params {
        p.span = Span::DUMMY;
        norm_type(&mut p.ty);
    }
    norm_ret(&mut x.ret);
}

fn norm_ret(r: &mut RetType) {
    match r {
        RetType::None => {}
        RetType::Single(t) => norm_type(t),
        RetType::Multi(ts) => {
            for t in ts {
                norm_type(t);
            }
        }
    }
}

fn norm_type(t: &mut Type) {
    t.span = Span::DUMMY;
    match &mut t.kind {
        TypeKind::Slice(i) | TypeKind::Tag(i) | TypeKind::Crib(i) | TypeKind::Array(i, _) => {
            norm_type(i)
        }
        TypeKind::Fn(ps, r) => {
            for p in ps {
                norm_type(p);
            }
            norm_type(r);
        }
        TypeKind::RawPtr => {}
        TypeKind::Named(_, gs) => {
            for g in gs {
                norm_type(g);
            }
        }
    }
}

fn norm_block(b: &mut Block) {
    b.span = Span::DUMMY;
    for s in &mut b.stmts {
        norm_stmt(s);
    }
}

fn norm_stmt(s: &mut Stmt) {
    s.span = Span::DUMMY;
    match &mut s.kind {
        StmtKind::Var(v) => norm_var(v),
        StmtKind::Const(c) => norm_const(c),
        StmtKind::Crib(c) => norm_crib(c),
        StmtKind::Fr(fr) => {
            norm_expr(&mut fr.cond);
            norm_block(&mut fr.then);
            for (c, b) in &mut fr.elifs {
                norm_expr(c);
                norm_block(b);
            }
            if let Some(e) = &mut fr.els {
                norm_block(e);
            }
        }
        StmtKind::Vibin { cond, body } => {
            norm_expr(cond);
            norm_block(body);
        }
        StmtKind::Squad { var: _, iter, body } => {
            norm_expr(iter);
            norm_block(body);
        }
        StmtKind::Vibe {
            scrutinee,
            arms,
            default,
        } => {
            norm_expr(scrutinee);
            for a in arms {
                a.span = Span::DUMMY;
                norm_block(&mut a.body);
            }
            if let Some(d) = default {
                norm_block(d);
            }
        }
        StmtKind::Holla {
            binding: _,
            tag,
            crib,
            live,
            ghosted,
        } => {
            norm_expr(tag);
            norm_expr(crib);
            norm_block(live);
            norm_block(ghosted);
        }
        StmtKind::Sheesh { body, recover } => {
            norm_block(body);
            if let Some((_, b)) = recover {
                norm_block(b);
            }
        }
        StmtKind::Evict { crib, tag } => {
            norm_expr(crib);
            if let Some(t) = tag {
                norm_expr(t);
            }
        }
        StmtKind::Slide(e) | StmtKind::Bounce(e) | StmtKind::Yeet(e) => norm_expr(e),
        StmtKind::Bet(vs) => {
            for v in vs {
                norm_expr(v);
            }
        }
        StmtKind::Dip | StmtKind::Skip => {}
        StmtKind::Assign {
            targets,
            op: _,
            values,
        } => {
            for t in targets {
                norm_expr(t);
            }
            for v in values {
                norm_expr(v);
            }
        }
        StmtKind::Expr(e) => norm_expr(e),
    }
}

fn norm_expr(e: &mut Expr) {
    e.span = Span::DUMMY;
    match &mut e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Byte(_)
        | ExprKind::Bool(_)
        | ExprKind::Ghosted => {}
        ExprKind::Name { name: _, generics } => {
            for g in generics {
                norm_type(g);
            }
        }
        ExprKind::Unary(_, x) => norm_expr(x),
        ExprKind::Binary(_, l, r) => {
            norm_expr(l);
            norm_expr(r);
        }
        ExprKind::Cast(x, t) => {
            norm_expr(x);
            norm_type(t);
        }
        ExprKind::Field {
            base,
            name: _,
            generics,
        } => {
            norm_expr(base);
            for g in generics {
                norm_type(g);
            }
        }
        ExprKind::Method {
            receiver,
            method: _,
            generics,
            args,
        } => {
            norm_expr(receiver);
            for g in generics {
                norm_type(g);
            }
            for a in args {
                norm_expr(&mut a.value);
            }
        }
        ExprKind::Call { callee, args } => {
            norm_expr(callee);
            for a in args {
                norm_expr(&mut a.value);
            }
        }
        ExprKind::Index { base, index } => {
            norm_expr(base);
            norm_expr(index);
        }
        ExprKind::Trust { tag, crib } => {
            norm_expr(tag);
            norm_expr(crib);
        }
        ExprKind::Struct(sl) => norm_struct(sl),
        ExprKind::Array(es) => {
            for x in es {
                norm_expr(x);
            }
        }
        ExprKind::Cop { init, crib } => {
            match init.as_mut() {
                CopInit::Struct(sl) => norm_struct(sl),
                CopInit::Variant { name: _, args } => {
                    for a in args {
                        norm_expr(&mut a.value);
                    }
                }
            }
            norm_expr(crib);
        }
    }
}

fn norm_struct(sl: &mut StructLit) {
    sl.span = Span::DUMMY;
    for g in &mut sl.generics {
        norm_type(g);
    }
    for f in &mut sl.fields {
        f.span = Span::DUMMY;
        norm_expr(&mut f.value);
    }
}
