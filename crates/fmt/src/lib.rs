//! `fmt` — bet's canonical formatter (Go's lesson: one formatting, no style wars).
//!
//! Consumes the `frontend` crate so parsing stays single-sourced: [`format_source`] parses
//! arbitrary `bet` source with [`frontend::parse`] and pretty-prints the resulting
//! [`ast::Program`] back to *the* canonical surface form. There is exactly one canonical
//! rendering for any program — running the formatter twice is a no-op (idempotent), and the
//! output always re-parses to the same abstract syntax tree.
//!
//! ## Canonical style
//! - 4-space indentation, one statement per line.
//! - Blocks (`finna`/`drip`/`moods`/`fr`/`vibin`/`squad`/`vibe`/`holla`/`sheesh` bodies) are
//!   always expanded onto their own lines; `{ … }` never stays inline.
//! - `pull` imports form a tight block; consecutive `facts` group too; every other pair of
//!   top-level items is separated by a single blank line.
//! - Spaces around every binary operator; none between a unary operator and its operand.
//! - Redundant parentheses are dropped: the printer re-inserts exactly the parentheses that
//!   precedence (and the header "no struct literal" rule) require, and no more.
//!
//! ## Deliberate non-preservation
//! The surface AST does not carry comments or integer literal bases, so the formatter cannot
//! preserve them: comments are dropped and every integer is printed in decimal. Both are pure
//! surface concerns — the re-parsed AST is structurally identical either way.

use frontend::ast::*;

/// Parse `bet` source and render it in canonical form.
///
/// Returns the formatted program (always terminated by a single newline), or the front-end's
/// error message if the input does not parse.
pub fn format_source(src: &str) -> Result<String, String> {
    let program = frontend::parse(src).map_err(|e| e.to_string())?;
    let mut f = Formatter { out: String::new() };
    f.program(&program);
    Ok(f.out)
}

struct Formatter {
    out: String,
}

impl Formatter {
    // --- program & top-level items -----------------------------------------

    fn program(&mut self, p: &Program) {
        for (i, item) in p.items.iter().enumerate() {
            if i > 0 && !compact_pair(&p.items[i - 1], item) {
                self.out.push('\n');
            }
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Pull(p) => {
                self.out.push_str("pull ");
                self.out.push_str(&escape_str(&p.module));
                if let Some(alias) = &p.alias {
                    self.out.push_str(" as ");
                    self.out.push_str(alias);
                }
                self.out.push('\n');
            }
            Item::Func(f) => self.fn_decl(f),
            Item::Drip(d) => self.drip_decl(d),
            Item::Moods(m) => self.moods_decl(m),
            Item::Crib(c) => {
                self.crib_decl(c);
                self.out.push('\n');
            }
            Item::Const(c) => {
                self.const_decl(c);
                self.out.push('\n');
            }
            Item::Var(v) => {
                self.var_decl(v);
                self.out.push('\n');
            }
            Item::Extern(e) => self.extern_decl(e),
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.vis(f.vis);
        self.out.push_str("finna ");
        if let Some(r) = &f.receiver {
            self.out.push('(');
            self.out.push_str(&r.name);
            self.out.push_str(": ");
            self.out.push_str(&type_str(&r.ty));
            self.out.push_str(") ");
        }
        self.out.push_str(&f.name);
        self.generic_params(&f.generics);
        self.params(&f.params);
        self.ret(&f.ret);
        self.out.push_str(" {\n");
        self.block_body(&f.body, 1);
        self.out.push_str("}\n");
    }

    fn drip_decl(&mut self, d: &DripDecl) {
        self.vis(d.vis);
        self.out.push_str("drip ");
        self.out.push_str(&d.name);
        self.generic_params(&d.generics);
        self.out.push_str(" {\n");
        for field in &d.fields {
            self.indent(1);
            match field.vis {
                Some(Vis::Flex) => self.out.push_str("flex "),
                Some(Vis::Hush) => self.out.push_str("hush "),
                None => {}
            }
            self.out.push_str(&field.name);
            self.out.push_str(": ");
            self.out.push_str(&type_str(&field.ty));
            self.out.push('\n');
        }
        self.out.push_str("}\n");
    }

    fn moods_decl(&mut self, m: &MoodsDecl) {
        self.vis(m.vis);
        self.out.push_str("moods ");
        self.out.push_str(&m.name);
        self.generic_params(&m.generics);
        self.out.push_str(" {\n");
        for (i, v) in m.variants.iter().enumerate() {
            self.indent(1);
            self.out.push_str(&v.name);
            if !v.payload.is_empty() {
                self.out.push('(');
                self.out.push_str(&join_types(&v.payload));
                self.out.push(')');
            }
            if i + 1 < m.variants.len() {
                self.out.push(',');
            }
            self.out.push('\n');
        }
        self.out.push_str("}\n");
    }

    fn crib_decl(&mut self, c: &CribDecl) {
        self.vis(c.vis);
        self.out.push_str("crib ");
        self.out.push_str(&c.name);
        if let Some(t) = &c.ty {
            self.out.push_str(": ");
            self.out.push_str(&type_str(t));
        }
    }

    fn const_decl(&mut self, c: &ConstDecl) {
        self.vis(c.vis);
        self.out.push_str("facts ");
        self.out.push_str(&c.name);
        if let Some(t) = &c.ty {
            self.out.push_str(": ");
            self.out.push_str(&type_str(t));
        }
        self.out.push_str(" = ");
        self.expr(&c.value, false);
    }

    fn var_decl(&mut self, v: &VarDecl) {
        self.vis(v.vis);
        self.out.push_str("lowkey ");
        self.out.push_str(&v.targets.join(", "));
        if let Some(t) = &v.ty {
            self.out.push_str(": ");
            self.out.push_str(&type_str(t));
        }
        self.out.push_str(" = ");
        self.expr_list(&v.values);
    }

    fn extern_decl(&mut self, e: &ExternDecl) {
        self.out.push_str("extern ");
        self.out.push_str(&escape_str(&e.abi));
        self.out.push_str(" finna ");
        self.out.push_str(&e.name);
        self.params(&e.params);
        self.ret(&e.ret);
        self.out.push('\n');
    }

    fn vis(&mut self, vis: Vis) {
        if let Vis::Flex = vis {
            self.out.push_str("flex ");
        }
    }

    fn generic_params(&mut self, g: &[String]) {
        if g.is_empty() {
            return;
        }
        self.out.push('[');
        self.out.push_str(&g.join(", "));
        self.out.push(']');
    }

    fn params(&mut self, params: &[Param]) {
        self.out.push('(');
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.out.push_str(&p.name);
            self.out.push_str(": ");
            self.out.push_str(&type_str(&p.ty));
        }
        self.out.push(')');
    }

    fn ret(&mut self, ret: &RetType) {
        match ret {
            RetType::None => {}
            RetType::Single(t) => {
                self.out.push_str(" -> ");
                self.out.push_str(&type_str(t));
            }
            RetType::Multi(ts) => {
                self.out.push_str(" -> (");
                self.out.push_str(&join_types(ts));
                self.out.push(')');
            }
        }
    }

    // --- statements ---------------------------------------------------------

    fn block_body(&mut self, block: &Block, ind: usize) {
        for s in &block.stmts {
            self.stmt(s, ind);
        }
    }

    fn stmt(&mut self, s: &Stmt, ind: usize) {
        match &s.kind {
            StmtKind::Var(v) => {
                self.indent(ind);
                self.var_decl(v);
                self.out.push('\n');
            }
            StmtKind::Const(c) => {
                self.indent(ind);
                self.const_decl(c);
                self.out.push('\n');
            }
            StmtKind::Crib(c) => {
                self.indent(ind);
                self.crib_decl(c);
                self.out.push('\n');
            }
            StmtKind::Fr(fr) => self.fr_stmt(fr, ind),
            StmtKind::Vibin { cond, body } => {
                self.indent(ind);
                self.out.push_str("vibin ");
                self.expr(cond, true);
                self.out.push_str(" {\n");
                self.block_body(body, ind + 1);
                self.indent(ind);
                self.out.push_str("}\n");
            }
            StmtKind::Squad { var, iter, body } => {
                self.indent(ind);
                self.out.push_str("squad ");
                self.out.push_str(var);
                self.out.push_str(" in ");
                self.expr(iter, true);
                self.out.push_str(" {\n");
                self.block_body(body, ind + 1);
                self.indent(ind);
                self.out.push_str("}\n");
            }
            StmtKind::Vibe {
                scrutinee,
                arms,
                default,
            } => {
                self.indent(ind);
                self.out.push_str("vibe ");
                self.expr(scrutinee, true);
                self.out.push_str(" {\n");
                for arm in arms {
                    self.indent(ind + 1);
                    self.out.push_str(&arm.variant);
                    if !arm.bindings.is_empty() {
                        self.out.push('(');
                        self.out.push_str(&arm.bindings.join(", "));
                        self.out.push(')');
                    }
                    self.out.push_str(" {\n");
                    self.block_body(&arm.body, ind + 2);
                    self.indent(ind + 1);
                    self.out.push_str("}\n");
                }
                if let Some(def) = default {
                    self.indent(ind + 1);
                    self.out.push_str("naw {\n");
                    self.block_body(def, ind + 2);
                    self.indent(ind + 1);
                    self.out.push_str("}\n");
                }
                self.indent(ind);
                self.out.push_str("}\n");
            }
            StmtKind::Holla {
                binding,
                tag,
                crib,
                live,
                ghosted,
            } => {
                self.indent(ind);
                self.out.push_str("holla ");
                self.out.push_str(binding);
                self.out.push_str(" = ");
                self.expr(tag, false);
                self.out.push_str(" in ");
                self.expr(crib, true);
                self.out.push_str(" {\n");
                self.block_body(live, ind + 1);
                self.indent(ind);
                self.out.push_str("} ghosted {\n");
                self.block_body(ghosted, ind + 1);
                self.indent(ind);
                self.out.push_str("}\n");
            }
            StmtKind::Sheesh { body, recover } => {
                self.indent(ind);
                self.out.push_str("sheesh {\n");
                self.block_body(body, ind + 1);
                match recover {
                    Some((name, rblock)) => {
                        self.indent(ind);
                        self.out.push_str("} naw ");
                        self.out.push_str(name);
                        self.out.push_str(" {\n");
                        self.block_body(rblock, ind + 1);
                        self.indent(ind);
                        self.out.push_str("}\n");
                    }
                    None => {
                        self.indent(ind);
                        self.out.push_str("}\n");
                    }
                }
            }
            StmtKind::Evict { crib, tag } => {
                self.indent(ind);
                self.out.push_str("evict ");
                if let Some(t) = tag {
                    self.expr(t, false);
                    self.out.push_str(" in ");
                }
                self.expr(crib, false);
                self.out.push('\n');
            }
            StmtKind::Slide(e) => {
                self.indent(ind);
                self.out.push_str("slide ");
                self.expr(e, false);
                self.out.push('\n');
            }
            StmtKind::Bet(vals) => {
                self.indent(ind);
                self.out.push_str("bet");
                if !vals.is_empty() {
                    self.out.push(' ');
                    self.expr_list(vals);
                }
                self.out.push('\n');
            }
            StmtKind::Bounce(e) => {
                self.indent(ind);
                self.out.push_str("bounce ");
                self.expr(e, false);
                self.out.push('\n');
            }
            StmtKind::Yeet(e) => {
                self.indent(ind);
                self.out.push_str("yeet(");
                self.expr(e, false);
                self.out.push_str(")\n");
            }
            StmtKind::Dip => {
                self.indent(ind);
                self.out.push_str("dip\n");
            }
            StmtKind::Skip => {
                self.indent(ind);
                self.out.push_str("skip\n");
            }
            StmtKind::Assign {
                targets,
                op,
                values,
            } => {
                self.indent(ind);
                self.expr_list(targets);
                self.out.push(' ');
                self.out.push_str(assign_op_str(*op));
                self.out.push(' ');
                self.expr_list(values);
                self.out.push('\n');
            }
            StmtKind::Expr(e) => {
                self.indent(ind);
                self.expr(e, false);
                self.out.push('\n');
            }
        }
    }

    fn fr_stmt(&mut self, fr: &FrStmt, ind: usize) {
        self.indent(ind);
        self.out.push_str("fr ");
        self.expr(&fr.cond, true);
        self.out.push_str(" {\n");
        self.block_body(&fr.then, ind + 1);
        for (c, b) in &fr.elifs {
            self.indent(ind);
            self.out.push_str("} naw fr ");
            self.expr(c, true);
            self.out.push_str(" {\n");
            self.block_body(b, ind + 1);
        }
        if let Some(els) = &fr.els {
            self.indent(ind);
            self.out.push_str("} naw {\n");
            self.block_body(els, ind + 1);
        }
        self.indent(ind);
        self.out.push_str("}\n");
    }

    // --- expressions --------------------------------------------------------

    /// Render an expression. `no_struct` mirrors the parser's rule (`spec/grammar.ebnf` §L6):
    /// at the top level of an `fr`/`vibin`/`squad`/`vibe`/`holla`-crib header a bare `Name{ … }`
    /// reads as a block, not a struct literal, so any struct literal reachable there without an
    /// intervening bracket must be parenthesized. The flag propagates through operators and
    /// postfix bases but resets to `false` inside any `( )`, `[ ]`, or `{ }`.
    fn expr(&mut self, e: &Expr, no_struct: bool) {
        match &e.kind {
            ExprKind::Int(v) => self.out.push_str(&v.to_string()),
            ExprKind::Float(v) => self.out.push_str(&fmt_float(*v)),
            ExprKind::Str(s) => self.out.push_str(&escape_str(s)),
            ExprKind::Byte(b) => self.out.push_str(&escape_byte(*b)),
            ExprKind::Bool(true) => self.out.push_str("nocap"),
            ExprKind::Bool(false) => self.out.push_str("cap"),
            ExprKind::Ghosted => self.out.push_str("ghosted"),
            ExprKind::Name { name, generics } => {
                self.out.push_str(name);
                self.type_args(generics);
            }
            ExprKind::Unary(op, x) => {
                self.out.push_str(unop_str(*op));
                self.operand(x, expr_prec(x) < PREC_UNARY, no_struct);
            }
            ExprKind::Binary(op, l, r) => {
                let p = binop_prec(*op);
                self.operand(l, expr_prec(l) < p, no_struct);
                self.out.push(' ');
                self.out.push_str(binop_str(*op));
                self.out.push(' ');
                self.operand(r, expr_prec(r) <= p, no_struct);
            }
            ExprKind::Cast(x, ty) => {
                self.operand(x, expr_prec(x) < PREC_CAST, no_struct);
                self.out.push_str(" as ");
                self.out.push_str(&type_str(ty));
            }
            ExprKind::Field {
                base,
                name,
                generics,
            } => {
                self.operand(base, expr_prec(base) < PREC_POSTFIX, no_struct);
                self.out.push('.');
                self.out.push_str(name);
                self.type_args(generics);
            }
            ExprKind::Method {
                receiver,
                method,
                generics,
                args,
            } => {
                self.operand(receiver, expr_prec(receiver) < PREC_POSTFIX, no_struct);
                self.out.push('.');
                self.out.push_str(method);
                self.type_args(generics);
                self.args(args);
            }
            ExprKind::Call { callee, args } => {
                self.operand(callee, expr_prec(callee) < PREC_POSTFIX, no_struct);
                self.args(args);
            }
            ExprKind::Index { base, index } => {
                self.operand(base, expr_prec(base) < PREC_POSTFIX, no_struct);
                self.out.push('[');
                self.expr(index, false);
                self.out.push(']');
            }
            ExprKind::Trust { tag, crib } => {
                self.operand(tag, expr_prec(tag) < PREC_POSTFIX, no_struct);
                self.out.push_str(".trust() in ");
                self.operand(crib, expr_prec(crib) < PREC_POSTFIX, no_struct);
            }
            ExprKind::Struct(sl) => {
                if no_struct {
                    self.out.push('(');
                    self.struct_lit(sl);
                    self.out.push(')');
                } else {
                    self.struct_lit(sl);
                }
            }
            ExprKind::Array(elems) => {
                self.out.push('[');
                for (i, el) in elems.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.expr(el, false);
                }
                self.out.push(']');
            }
            ExprKind::Cop { init, crib } => {
                self.out.push_str("cop ");
                match init.as_ref() {
                    CopInit::Struct(sl) => self.struct_lit(sl),
                    CopInit::Variant { name, args } => {
                        self.out.push_str(name);
                        if !args.is_empty() {
                            self.args(args);
                        }
                    }
                }
                self.out.push_str(" in ");
                self.operand(crib, expr_prec(crib) < PREC_POSTFIX, no_struct);
            }
        }
    }

    /// Emit a sub-expression, wrapping it in parentheses when `need_paren`. Inside inserted
    /// parentheses struct literals are legal again, so `no_struct` resets to `false`; otherwise
    /// the child inherits the caller's `no_struct` context.
    fn operand(&mut self, e: &Expr, need_paren: bool, no_struct: bool) {
        if need_paren {
            self.out.push('(');
            self.expr(e, false);
            self.out.push(')');
        } else {
            self.expr(e, no_struct);
        }
    }

    fn expr_list(&mut self, es: &[Expr]) {
        for (i, e) in es.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.expr(e, false);
        }
    }

    fn args(&mut self, args: &[Arg]) {
        self.out.push('(');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            if let Some(label) = &a.label {
                self.out.push_str(label);
                self.out.push_str(": ");
            }
            self.expr(&a.value, false);
        }
        self.out.push(')');
    }

    fn struct_lit(&mut self, sl: &StructLit) {
        self.out.push_str(&sl.name);
        self.type_args(&sl.generics);
        if sl.fields.is_empty() {
            self.out.push_str("{}");
        } else {
            self.out.push_str("{ ");
            for (i, fi) in sl.fields.iter().enumerate() {
                if i > 0 {
                    self.out.push_str(", ");
                }
                self.out.push_str(&fi.name);
                self.out.push_str(": ");
                self.expr(&fi.value, false);
            }
            self.out.push_str(" }");
        }
    }

    fn type_args(&mut self, generics: &[Type]) {
        if generics.is_empty() {
            return;
        }
        self.out.push('[');
        self.out.push_str(&join_types(generics));
        self.out.push(']');
    }

    fn indent(&mut self, n: usize) {
        for _ in 0..n {
            self.out.push_str("    ");
        }
    }
}

// ---------------------------------------------------------------------------
// Precedence — mirrors the parser's §E1 ladder. Higher binds tighter.
// ---------------------------------------------------------------------------

const PREC_CAST: u8 = 10;
const PREC_UNARY: u8 = 11;
const PREC_POSTFIX: u8 = 12;

fn binop_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::BitOr => 4,
        BinOp::BitXor => 5,
        BinOp::BitAnd => 6,
        BinOp::Shl | BinOp::Shr => 7,
        BinOp::Add | BinOp::Sub => 8,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 9,
    }
}

/// The binding tightness of an expression when it appears as an operand of a tighter form.
/// Atoms and postfix expressions never need parentheses (they are already the tightest).
fn expr_prec(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Binary(op, _, _) => binop_prec(*op),
        ExprKind::Cast(_, _) => PREC_CAST,
        ExprKind::Unary(_, _) => PREC_UNARY,
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Byte(_)
        | ExprKind::Bool(_)
        | ExprKind::Ghosted
        | ExprKind::Name { .. }
        | ExprKind::Field { .. }
        | ExprKind::Method { .. }
        | ExprKind::Call { .. }
        | ExprKind::Index { .. }
        | ExprKind::Trust { .. }
        | ExprKind::Struct(_)
        | ExprKind::Array(_)
        | ExprKind::Cop { .. } => PREC_POSTFIX,
    }
}

// ---------------------------------------------------------------------------
// Operator spellings.
// ---------------------------------------------------------------------------

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "||",
        BinOp::And => "&&",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::BitAnd => "&",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
    }
}

fn assign_op_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Eq => "=",
        AssignOp::AddEq => "+=",
        AssignOp::SubEq => "-=",
        AssignOp::MulEq => "*=",
        AssignOp::DivEq => "/=",
        AssignOp::RemEq => "%=",
        AssignOp::AndEq => "&=",
        AssignOp::OrEq => "|=",
        AssignOp::XorEq => "^=",
        AssignOp::ShlEq => "<<=",
        AssignOp::ShrEq => ">>=",
    }
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Depth ceiling for type pretty-printing (issue #38), a stack-overflow backstop mirroring the
/// parser's recursion guard. A *parsed* type can never reach this — the parser caps type nesting
/// far lower — so this only fires on a pathologically deep hand-built AST, where we emit a
/// truncation marker instead of recursing off the stack.
const MAX_TYPE_DEPTH: usize = 1024;

fn type_str(t: &Type) -> String {
    type_str_depth(t, 0)
}

fn type_str_depth(t: &Type, depth: usize) -> String {
    if depth >= MAX_TYPE_DEPTH {
        return "…".to_string();
    }
    let d = depth + 1;
    match &t.kind {
        TypeKind::Slice(inner) => format!("[]{}", type_str_depth(inner, d)),
        TypeKind::Array(inner, n) => format!("{}[{}]", type_str_depth(inner, d), n),
        TypeKind::Tag(inner) => format!("tag {}", type_str_depth(inner, d)),
        TypeKind::Crib(inner) => format!("crib {}", type_str_depth(inner, d)),
        TypeKind::Soa(inner) => format!("soa {}", type_str_depth(inner, d)),
        TypeKind::Fn(params, ret) => {
            format!(
                "finna({}) -> {}",
                join_types_depth(params, d),
                type_str_depth(ret, d)
            )
        }
        TypeKind::RawPtr => "rawptr".to_string(),
        TypeKind::Named(name, generics) => {
            if generics.is_empty() {
                name.clone()
            } else {
                format!("{}[{}]", name, join_types_depth(generics, d))
            }
        }
    }
}

fn join_types(ts: &[Type]) -> String {
    join_types_depth(ts, 0)
}

fn join_types_depth(ts: &[Type], depth: usize) -> String {
    ts.iter()
        .map(|t| type_str_depth(t, depth))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Literals.
// ---------------------------------------------------------------------------

/// Render an `f64` so it always re-lexes as a `Float` token (never bare digits that would lex
/// as an `Int`): Rust's `{:?}` yields the shortest round-tripping form and always includes a
/// `.` or exponent for finite values; we defensively append `.0` if somehow neither is present.
fn fmt_float(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Re-encode a decoded string literal as canonical `bet` source (surrounding quotes included).
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Re-encode a `u8` byte literal as canonical `bet` source (surrounding quotes included).
///
/// Unlike string literals, the byte-literal grammar (`spec/grammar.ebnf` §L3) admits only a
/// single character or a one-character escape between the quotes — there is *no* `\xHH` form for
/// bytes. So any value the parser can produce arrived either as one of the named escapes or as a
/// raw character (a high byte such as `0xFF` comes in as the literal char `U+00FF`); we round-trip
/// it the same way, emitting the raw character for everything outside the named-escape set.
fn escape_byte(b: u8) -> String {
    let mut out = String::with_capacity(4);
    out.push('\'');
    match b {
        b'\\' => out.push_str("\\\\"),
        b'\'' => out.push_str("\\'"),
        b'\n' => out.push_str("\\n"),
        b'\t' => out.push_str("\\t"),
        b'\r' => out.push_str("\\r"),
        0 => out.push_str("\\0"),
        other => out.push(char::from(other)),
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Blank-line policy between top-level items.
// ---------------------------------------------------------------------------

/// Whether two adjacent top-level items render *without* a blank line between them: `pull`
/// imports form a tight block, and consecutive `facts` constants group together. Everything
/// else gets a single separating blank line.
fn compact_pair(a: &Item, b: &Item) -> bool {
    matches!(
        (a, b),
        (Item::Pull(_), Item::Pull(_)) | (Item::Const(_), Item::Const(_))
    )
}

#[cfg(test)]
mod tests {
    use super::format_source;

    /// Format `src` and assert it renders exactly to `want`, and that the result is a fixed
    /// point (formatting it again is a no-op).
    fn check(src: &str, want: &str) {
        let got = format_source(src).expect("format");
        assert_eq!(got, want, "\n--- got ---\n{got}\n--- want ---\n{want}");
        let again = format_source(&got).expect("re-format");
        assert_eq!(again, got, "not idempotent");
    }

    #[test]
    fn imports_group_but_declarations_are_separated() {
        check(
            "pull \"a\"\npull \"b\"\nfinna main() { spill.it(1) }\n",
            "pull \"a\"\npull \"b\"\n\nfinna main() {\n    spill.it(1)\n}\n",
        );
    }

    #[test]
    fn consecutive_facts_group_together() {
        check(
            "facts A: int = 1\nfacts B: int = 2\n",
            "facts A: int = 1\nfacts B: int = 2\n",
        );
    }

    #[test]
    fn redundant_parens_are_dropped() {
        // `(a * b) >> c` — the parens are precedence-redundant and canonicalize away.
        check(
            "finna f(a: int, b: int, c: int) -> int { bet (a * b) >> c }\n",
            "finna f(a: int, b: int, c: int) -> int {\n    bet a * b >> c\n}\n",
        );
    }

    #[test]
    fn required_parens_are_kept() {
        // Right-associativity and precedence-inversion parens must survive.
        check(
            "finna f(a: int, b: int, c: int) -> int { bet a - (b - c) }\n",
            "finna f(a: int, b: int, c: int) -> int {\n    bet a - (b - c)\n}\n",
        );
        check(
            "finna f(a: int, b: int) -> int { bet (a + b) as int }\n",
            "finna f(a: int, b: int) -> int {\n    bet (a + b) as int\n}\n",
        );
    }

    #[test]
    fn struct_literal_in_header_is_parenthesized() {
        // At a header's top level a bare `Name{ … }` would read as the block, so it needs parens.
        check(
            "finna f() { fr (Foo{ a: 1 }).ok { skip } }\n",
            "finna f() {\n    fr (Foo{ a: 1 }).ok {\n        skip\n    }\n}\n",
        );
    }

    #[test]
    fn float_always_re_lexes_as_float() {
        // An integer-valued float must keep its `.0` so it does not lex back as an `Int`.
        check(
            "finna f() -> f64 { bet 3.0 }\n",
            "finna f() -> f64 {\n    bet 3.0\n}\n",
        );
    }

    #[test]
    fn integers_canonicalize_to_decimal() {
        // The AST does not carry the literal base, so hex/binary become decimal.
        check("facts M: u32 = 0x40\n", "facts M: u32 = 64\n");
    }

    #[test]
    fn labeled_call_arguments_are_preserved() {
        check(
            "finna f() { g(count: 3, 4) }\n",
            "finna f() {\n    g(count: 3, 4)\n}\n",
        );
    }

    #[test]
    fn byte_high_value_round_trips_as_raw_char() {
        // 0xFF has no `\\xHH` byte escape in the grammar; it round-trips as the raw char U+00FF.
        check(
            "finna f() { lowkey b = 'ÿ' }\n",
            "finna f() {\n    lowkey b = 'ÿ'\n}\n",
        );
    }

    #[test]
    fn parse_error_is_reported() {
        assert!(format_source("finna (").is_err());
    }

    /// Issue #38: `type_str` must not overflow the stack on a pathologically deep type. The
    /// parser caps type nesting far below this, so such a type is only reachable via a hand-built
    /// AST — but pretty-printing it must still terminate with a truncation marker.
    #[test]
    fn deeply_nested_type_truncates_without_overflow() {
        use super::{MAX_TYPE_DEPTH, type_str};
        use frontend::ast::{Span, Type, TypeKind};

        let mut t = Type {
            kind: TypeKind::Named("int".into(), vec![]),
            span: Span::DUMMY,
        };
        for _ in 0..(MAX_TYPE_DEPTH + 2000) {
            t = Type {
                kind: TypeKind::Tag(Box::new(t)),
                span: Span::DUMMY,
            };
        }
        let s = type_str(&t);
        assert!(s.contains('…'), "expected a truncation marker");
    }
}
