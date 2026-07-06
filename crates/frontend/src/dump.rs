//! Canonical, bet-reproducible textual dumps of frontend intermediates.
//!
//! These exist for **differential testing the self-hosted frontend against this Rust one**
//! (self-host roadmap, Phase B1 / C1–C4): `bet build --emit=<kind>` prints one of these, and
//! the ported `bet` frontend must print byte-identical output. The formats are deliberately
//! simple — a fixed tag per line, canonical payload escaping — so they are trivial to reproduce
//! in `bet` (no Rust `Debug` formatting, which could not be mirrored).
//!
//! - [`tokens`] — the post-ASI token stream the parser consumes.
//! - [`mir`]    — the `.mir` textual IR, the actual frontend↔backend contract.
//! - [`ast`]    — the parsed AST as a canonical indented tree; the C3 parser port reproduces it.

use crate::CompileError;
use crate::ast;
use crate::lexer::{self, Token};

/// Dump the post-ASI token stream, one token per line.
///
/// Keyword and punctuation tokens print a fixed canonical tag ([`token_tag`]); literal and
/// identifier tokens print `<tag> <payload>` with the payload canonically formatted. `Newline`
/// never appears — [`lexer::tokenize`] has already run ASI, so the stream holds `Semi` only.
pub fn tokens(src: &str) -> Result<String, CompileError> {
    let toks = lexer::tokenize(src).map_err(CompileError::Lex)?;
    let mut out = String::new();
    for s in &toks {
        match &s.tok {
            Token::Ident(name) => {
                out.push_str("ident ");
                out.push_str(name);
            }
            Token::Int(n) => out.push_str(&format!("int {n}")),
            Token::Float(f) => out.push_str(&format!("float {}", fmt_float(*f))),
            Token::Str(v) => {
                out.push_str("str ");
                out.push_str(&quote(v));
            }
            Token::Byte(b) => out.push_str(&format!("byte {b}")),
            other => out.push_str(token_tag(other)),
        }
        out.push('\n');
    }
    Ok(out)
}

/// Dump the `.mir` textual IR for `src` (compile through the frontend, then print).
///
/// This is the highest-value dump: byte-identical `.mir` from the Rust and `bet` frontends is
/// the self-host correctness proof (and makes the M8 fixpoint fall out — see the roadmap).
pub fn mir(src: &str) -> Result<String, CompileError> {
    let module = crate::compile(src)?;
    Ok(midir::print(&module))
}

/// Dump the parsed AST as a canonical, indented tree: one node per line, two spaces per depth
/// level, scalar payloads (names, literals, operators, visibility) inlined on the node's line and
/// child nodes on following lines one level deeper. Spans are omitted (like the token dump), so the
/// output is a pure structural fingerprint the `bet` parser port (C3) reproduces byte-for-byte.
///
/// Conventions: optional single children (a type, a value) sit under a labeled wrapper line
/// (`type` / `value`); fixed-arity children are positional (`binary add` has exactly two exprs);
/// mandatory lists get an always-emitted wrapper (`params`, `args`, `values`, `targets`, `fields`);
/// the optional generic-argument list gets a `typeargs` wrapper only when non-empty.
pub fn ast(src: &str) -> Result<String, CompileError> {
    let program = crate::parse(src)?;
    let mut p = AstDump { out: String::new() };
    p.program(&program);
    Ok(p.out)
}

/// The recursive AST tree-printer (see [`ast`]).
struct AstDump {
    out: String,
}

impl AstDump {
    fn line(&mut self, depth: usize, s: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn program(&mut self, p: &ast::Program) {
        self.line(0, "program");
        for it in &p.items {
            self.item(1, it);
        }
    }

    fn item(&mut self, d: usize, it: &ast::Item) {
        use ast::Item;
        match it {
            Item::Pull(p) => {
                let line = match &p.alias {
                    Some(a) => format!("pull {} as {a}", quote(&p.module)),
                    None => format!("pull {}", quote(&p.module)),
                };
                self.line(d, &line);
            }
            Item::Func(f) => {
                self.line(d, &format!("func {} {}", vis_str(f.vis), f.name));
                if let Some(r) = &f.receiver {
                    self.line(d + 1, &format!("receiver {}", r.name));
                    self.ty(d + 2, &r.ty);
                }
                self.generics(d + 1, &f.generics);
                self.params(d + 1, &f.params);
                self.ret(d + 1, &f.ret);
                self.block(d + 1, &f.body);
            }
            Item::Drip(dd) => {
                self.line(d, &format!("drip {} {}", vis_str(dd.vis), dd.name));
                self.generics(d + 1, &dd.generics);
                for fld in &dd.fields {
                    self.line(d + 1, &format!("field {} {}", field_vis(fld.vis), fld.name));
                    self.ty(d + 2, &fld.ty);
                }
            }
            Item::Moods(m) => {
                self.line(d, &format!("moods {} {}", vis_str(m.vis), m.name));
                self.generics(d + 1, &m.generics);
                for v in &m.variants {
                    self.line(d + 1, &format!("variant {}", v.name));
                    for pty in &v.payload {
                        self.ty(d + 2, pty);
                    }
                }
            }
            Item::Crib(c) => {
                self.line(d, &format!("crib {} {}", vis_str(c.vis), c.name));
                if let Some(t) = &c.ty {
                    self.ty(d + 1, t);
                }
            }
            Item::Const(c) => self.const_decl(d, c),
            Item::Var(v) => self.var_decl(d, v),
            Item::Extern(e) => {
                self.line(d, &format!("extern {} {}", quote(&e.abi), e.name));
                self.params(d + 1, &e.params);
                self.ret(d + 1, &e.ret);
            }
        }
    }

    fn const_decl(&mut self, d: usize, c: &ast::ConstDecl) {
        self.line(d, &format!("const {} {}", vis_str(c.vis), c.name));
        if let Some(t) = &c.ty {
            self.line(d + 1, "type");
            self.ty(d + 2, t);
        }
        self.line(d + 1, "value");
        self.expr(d + 2, &c.value);
    }

    fn var_decl(&mut self, d: usize, v: &ast::VarDecl) {
        self.line(
            d,
            &format!("var {} {}", vis_str(v.vis), v.targets.join(" ")),
        );
        if let Some(t) = &v.ty {
            self.line(d + 1, "type");
            self.ty(d + 2, t);
        }
        self.line(d + 1, "values");
        for e in &v.values {
            self.expr(d + 2, e);
        }
    }

    fn crib_decl(&mut self, d: usize, c: &ast::CribDecl) {
        self.line(d, &format!("crib {} {}", vis_str(c.vis), c.name));
        if let Some(t) = &c.ty {
            self.ty(d + 1, t);
        }
    }

    /// Type-parameter names on a declaration (`generics` alone when empty, else `generics T U`).
    fn generics(&mut self, d: usize, g: &[String]) {
        if g.is_empty() {
            self.line(d, "generics");
        } else {
            self.line(d, &format!("generics {}", g.join(" ")));
        }
    }

    /// A `typeargs` wrapper over a generic-argument list — emitted only when non-empty.
    fn typeargs(&mut self, d: usize, g: &[ast::Type]) {
        if !g.is_empty() {
            self.line(d, "typeargs");
            for t in g {
                self.ty(d + 1, t);
            }
        }
    }

    fn params(&mut self, d: usize, params: &[ast::Param]) {
        self.line(d, "params");
        for p in params {
            self.line(d + 1, &format!("param {}", p.name));
            self.ty(d + 2, &p.ty);
        }
    }

    fn ret(&mut self, d: usize, r: &ast::RetType) {
        use ast::RetType;
        match r {
            RetType::None => self.line(d, "ret none"),
            RetType::Single(t) => {
                self.line(d, "ret single");
                self.ty(d + 1, t);
            }
            RetType::Multi(ts) => {
                self.line(d, "ret multi");
                for t in ts {
                    self.ty(d + 1, t);
                }
            }
        }
    }

    fn ty(&mut self, d: usize, t: &ast::Type) {
        use ast::TypeKind;
        match &t.kind {
            TypeKind::Slice(inner) => {
                self.line(d, "slice");
                self.ty(d + 1, inner);
            }
            TypeKind::Array(inner, n) => {
                self.line(d, &format!("array {n}"));
                self.ty(d + 1, inner);
            }
            TypeKind::Tag(inner) => {
                self.line(d, "tag");
                self.ty(d + 1, inner);
            }
            TypeKind::Crib(inner) => {
                self.line(d, "crib-ty");
                self.ty(d + 1, inner);
            }
            TypeKind::Fn(params, ret) => {
                self.line(d, "fn-ty");
                self.line(d + 1, "params");
                for p in params {
                    self.ty(d + 2, p);
                }
                self.line(d + 1, "ret");
                self.ty(d + 2, ret);
            }
            TypeKind::RawPtr => self.line(d, "rawptr"),
            TypeKind::Named(name, gens) => {
                self.line(d, &format!("named {name}"));
                for g in gens {
                    self.ty(d + 1, g);
                }
            }
        }
    }

    fn block(&mut self, d: usize, b: &ast::Block) {
        self.line(d, "block");
        for s in &b.stmts {
            self.stmt(d + 1, s);
        }
    }

    fn stmt(&mut self, d: usize, s: &ast::Stmt) {
        use ast::StmtKind;
        match &s.kind {
            StmtKind::Var(v) => self.var_decl(d, v),
            StmtKind::Const(c) => self.const_decl(d, c),
            StmtKind::Crib(c) => self.crib_decl(d, c),
            StmtKind::Fr(fr) => {
                self.line(d, "fr");
                self.expr(d + 1, &fr.cond);
                self.block(d + 1, &fr.then);
                for (c, b) in &fr.elifs {
                    self.line(d + 1, "elif");
                    self.expr(d + 2, c);
                    self.block(d + 2, b);
                }
                if let Some(b) = &fr.els {
                    self.line(d + 1, "else");
                    self.block(d + 2, b);
                }
            }
            StmtKind::Vibin { cond, body } => {
                self.line(d, "vibin");
                self.expr(d + 1, cond);
                self.block(d + 1, body);
            }
            StmtKind::Squad { var, iter, body } => {
                self.line(d, &format!("squad {var}"));
                self.expr(d + 1, iter);
                self.block(d + 1, body);
            }
            StmtKind::Vibe {
                scrutinee,
                arms,
                default,
            } => {
                self.line(d, "vibe");
                self.expr(d + 1, scrutinee);
                for a in arms {
                    let binds = if a.bindings.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", a.bindings.join(" "))
                    };
                    self.line(d + 1, &format!("arm {}{}", a.variant, binds));
                    self.block(d + 2, &a.body);
                }
                if let Some(b) = default {
                    self.line(d + 1, "default");
                    self.block(d + 2, b);
                }
            }
            StmtKind::Holla {
                binding,
                tag,
                crib,
                live,
                ghosted,
            } => {
                self.line(d, &format!("holla {binding}"));
                self.line(d + 1, "tag");
                self.expr(d + 2, tag);
                self.line(d + 1, "crib");
                self.expr(d + 2, crib);
                self.line(d + 1, "live");
                self.block(d + 2, live);
                self.line(d + 1, "ghosted");
                self.block(d + 2, ghosted);
            }
            StmtKind::Sheesh { body, recover } => {
                self.line(d, "sheesh");
                self.block(d + 1, body);
                if let Some((name, b)) = recover {
                    self.line(d + 1, &format!("recover {name}"));
                    self.block(d + 2, b);
                }
            }
            StmtKind::Evict { crib, tag } => {
                // The whole-crib form keeps its historical one-child dump shape; the
                // single-slot form gets a distinct head (tag first, then crib).
                match tag {
                    None => {
                        self.line(d, "evict");
                        self.expr(d + 1, crib);
                    }
                    Some(t) => {
                        self.line(d, "evict-slot");
                        self.expr(d + 1, t);
                        self.expr(d + 1, crib);
                    }
                }
            }
            StmtKind::Slide(e) => {
                self.line(d, "slide");
                self.expr(d + 1, e);
            }
            StmtKind::Bet(es) => {
                self.line(d, "bet");
                for e in es {
                    self.expr(d + 1, e);
                }
            }
            StmtKind::Bounce(e) => {
                self.line(d, "bounce");
                self.expr(d + 1, e);
            }
            StmtKind::Yeet(e) => {
                self.line(d, "yeet");
                self.expr(d + 1, e);
            }
            StmtKind::Dip => self.line(d, "dip"),
            StmtKind::Skip => self.line(d, "skip"),
            StmtKind::Assign {
                targets,
                op,
                values,
            } => {
                self.line(d, &format!("assign {}", assign_op_str(*op)));
                self.line(d + 1, "targets");
                for t in targets {
                    self.expr(d + 2, t);
                }
                self.line(d + 1, "values");
                for v in values {
                    self.expr(d + 2, v);
                }
            }
            StmtKind::Expr(e) => {
                self.line(d, "expr-stmt");
                self.expr(d + 1, e);
            }
        }
    }

    fn expr(&mut self, d: usize, e: &ast::Expr) {
        use ast::ExprKind;
        match &e.kind {
            ExprKind::Int(n) => self.line(d, &format!("int {n}")),
            ExprKind::Float(f) => self.line(d, &format!("float {}", fmt_float(*f))),
            ExprKind::Str(s) => self.line(d, &format!("str {}", quote(s))),
            ExprKind::Byte(b) => self.line(d, &format!("byte {b}")),
            ExprKind::Bool(b) => self.line(d, &format!("bool {b}")),
            ExprKind::Ghosted => self.line(d, "ghosted"),
            ExprKind::Name { name, generics } => {
                self.line(d, &format!("name {name}"));
                self.typeargs(d + 1, generics);
            }
            ExprKind::Unary(op, x) => {
                self.line(d, &format!("unary {}", unop_str(*op)));
                self.expr(d + 1, x);
            }
            ExprKind::Binary(op, l, r) => {
                self.line(d, &format!("binary {}", binop_str(*op)));
                self.expr(d + 1, l);
                self.expr(d + 1, r);
            }
            ExprKind::Cast(x, t) => {
                self.line(d, "cast");
                self.expr(d + 1, x);
                self.ty(d + 1, t);
            }
            ExprKind::Field {
                base,
                name,
                generics,
            } => {
                self.line(d, &format!("field {name}"));
                self.expr(d + 1, base);
                self.typeargs(d + 1, generics);
            }
            ExprKind::Method {
                receiver,
                method,
                generics,
                args,
            } => {
                self.line(d, &format!("method {method}"));
                self.line(d + 1, "receiver");
                self.expr(d + 2, receiver);
                self.typeargs(d + 1, generics);
                self.args(d + 1, args);
            }
            ExprKind::Call { callee, args } => {
                self.line(d, "call");
                self.line(d + 1, "callee");
                self.expr(d + 2, callee);
                self.args(d + 1, args);
            }
            ExprKind::Index { base, index } => {
                self.line(d, "index");
                self.expr(d + 1, base);
                self.expr(d + 1, index);
            }
            ExprKind::Trust { tag, crib } => {
                self.line(d, "trust");
                self.expr(d + 1, tag);
                self.expr(d + 1, crib);
            }
            ExprKind::Struct(sl) => self.struct_lit(d, sl),
            ExprKind::Array(es) => {
                self.line(d, "array-lit");
                for e in es {
                    self.expr(d + 1, e);
                }
            }
            ExprKind::Cop { init, crib } => {
                self.line(d, "cop");
                match init.as_ref() {
                    ast::CopInit::Struct(sl) => self.struct_lit(d + 1, sl),
                    ast::CopInit::Variant { name, args } => {
                        self.line(d + 1, &format!("variant {name}"));
                        self.args(d + 2, args);
                    }
                }
                self.line(d + 1, "crib");
                self.expr(d + 2, crib);
            }
        }
    }

    fn struct_lit(&mut self, d: usize, sl: &ast::StructLit) {
        self.line(d, &format!("struct {}", sl.name));
        self.typeargs(d + 1, &sl.generics);
        self.line(d + 1, "fields");
        for fi in &sl.fields {
            self.line(d + 2, &format!("field {}", fi.name));
            self.expr(d + 3, &fi.value);
        }
    }

    fn args(&mut self, d: usize, args: &[ast::Arg]) {
        self.line(d, "args");
        for a in args {
            match &a.label {
                Some(l) => self.line(d + 1, &format!("arg {l}")),
                None => self.line(d + 1, "arg"),
            }
            self.expr(d + 2, &a.value);
        }
    }
}

fn vis_str(v: ast::Vis) -> &'static str {
    match v {
        ast::Vis::Flex => "flex",
        ast::Vis::Hush => "hush",
    }
}

fn field_vis(v: Option<ast::Vis>) -> &'static str {
    match v {
        Some(ast::Vis::Flex) => "flex",
        Some(ast::Vis::Hush) => "hush",
        None => "default",
    }
}

fn unop_str(op: ast::UnOp) -> &'static str {
    match op {
        ast::UnOp::Neg => "neg",
        ast::UnOp::Not => "not",
        ast::UnOp::BitNot => "bitnot",
    }
}

fn binop_str(op: ast::BinOp) -> &'static str {
    use ast::BinOp;
    match op {
        BinOp::Or => "or",
        BinOp::And => "and",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
        BinOp::BitOr => "bitor",
        BinOp::BitXor => "bitxor",
        BinOp::BitAnd => "bitand",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::Rem => "rem",
    }
}

fn assign_op_str(op: ast::AssignOp) -> &'static str {
    use ast::AssignOp;
    match op {
        AssignOp::Eq => "eq",
        AssignOp::AddEq => "addeq",
        AssignOp::SubEq => "subeq",
        AssignOp::MulEq => "muleq",
        AssignOp::DivEq => "diveq",
        AssignOp::RemEq => "remeq",
        AssignOp::AndEq => "andeq",
        AssignOp::OrEq => "oreq",
        AssignOp::XorEq => "xoreq",
        AssignOp::ShlEq => "shleq",
        AssignOp::ShrEq => "shreq",
    }
}

/// The canonical one-word tag for a keyword or punctuation token. Literal/identifier tokens
/// carry a payload and are handled by [`tokens`] directly, so they map to their bare kind here.
fn token_tag(t: &Token) -> &'static str {
    match t {
        // statement termination
        Token::Newline => "newline", // unreachable post-ASI, mapped for totality
        Token::Semi => "semi",
        // keywords (canonical = the surface spelling)
        Token::Finna => "finna",
        Token::Bet => "bet",
        Token::Lowkey => "lowkey",
        Token::Facts => "facts",
        Token::Drip => "drip",
        Token::Moods => "moods",
        Token::Pull => "pull",
        Token::Extern => "extern",
        Token::Fr => "fr",
        Token::Naw => "naw",
        Token::Vibin => "vibin",
        Token::Squad => "squad",
        Token::Dip => "dip",
        Token::Skip => "skip",
        Token::Vibe => "vibe",
        Token::Slide => "slide",
        Token::Yeet => "yeet",
        Token::Sheesh => "sheesh",
        Token::Bounce => "bounce",
        Token::Nocap => "nocap",
        Token::Cap => "cap",
        Token::Ghosted => "ghosted",
        Token::Flex => "flex",
        Token::Hush => "hush",
        Token::Crib => "crib",
        Token::Cop => "cop",
        Token::Evict => "evict",
        Token::Tag => "tag",
        Token::Holla => "holla",
        Token::Trust => "trust",
        Token::In => "in",
        Token::As => "as",
        // operators & punctuation
        Token::Arrow => "arrow",
        Token::EqEq => "eqeq",
        Token::Ne => "ne",
        Token::Le => "le",
        Token::Ge => "ge",
        Token::ShlEq => "shleq",
        Token::ShrEq => "shreq",
        Token::Shl => "shl",
        Token::Shr => "shr",
        Token::AndAnd => "andand",
        Token::OrOr => "oror",
        Token::PlusEq => "pluseq",
        Token::MinusEq => "minuseq",
        Token::StarEq => "stareq",
        Token::SlashEq => "slasheq",
        Token::PercentEq => "percenteq",
        Token::AmpEq => "ampeq",
        Token::PipeEq => "pipeeq",
        Token::CaretEq => "careteq",
        Token::Lt => "lt",
        Token::Gt => "gt",
        Token::Eq => "eq",
        Token::Plus => "plus",
        Token::Minus => "minus",
        Token::Star => "star",
        Token::Slash => "slash",
        Token::Percent => "percent",
        Token::Amp => "amp",
        Token::Pipe => "pipe",
        Token::Caret => "caret",
        Token::Tilde => "tilde",
        Token::Bang => "bang",
        Token::Dot => "dot",
        Token::Comma => "comma",
        Token::Colon => "colon",
        Token::LBracket => "lbracket",
        Token::RBracket => "rbracket",
        Token::LParen => "lparen",
        Token::RParen => "rparen",
        Token::LBrace => "lbrace",
        Token::RBrace => "rbrace",
        // literals/identifiers are formatted with payloads by `tokens`
        Token::Ident(_) => "ident",
        Token::Int(_) => "int",
        Token::Float(_) => "float",
        Token::Str(_) => "str",
        Token::Byte(_) => "byte",
    }
}

/// Canonical shortest-round-trip float rendering (matches Rust's `{}` for `f64`); a whole number
/// keeps a `.0` so it never collides with an integer token in the dump.
fn fmt_float(f: f64) -> String {
    let s = format!("{f}");
    if s.contains(['.', 'e', 'E', 'n', 'i']) {
        s // has a point / exponent / nan / inf already
    } else {
        format!("{s}.0")
    }
}

/// Quote a string payload with a minimal, canonical escape set (`\` `"` newline tab CR).
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_hello() {
        let src = "pull \"spill\"\nfinna main() {\n    spill.it(\"hi\")\n}\n";
        let dump = tokens(src).expect("hello should tokenize");
        assert_eq!(
            dump,
            "pull\nstr \"spill\"\nsemi\nfinna\nident main\nlparen\nrparen\nlbrace\n\
             ident spill\ndot\nident it\nlparen\nstr \"hi\"\nrparen\nsemi\nrbrace\nsemi\n"
        );
    }

    #[test]
    fn tokens_numbers_and_ops() {
        let dump = tokens("lowkey x = 3.0 + 0xff << 2\n").unwrap();
        assert_eq!(
            dump,
            "lowkey\nident x\neq\nfloat 3.0\nplus\nint 255\nshl\nint 2\nsemi\n"
        );
    }

    #[test]
    fn mir_hello_roundtrips_through_printer() {
        let src = "pull \"spill\"\nfinna main() {\n    spill.it(\"hi\")\n}\n";
        let text = mir(src).expect("hello should lower");
        // Sanity: the printed IR parses back (the format is the real contract).
        assert!(midir::parse(&text).is_ok(), "emitted .mir must re-parse");
        assert!(text.contains("main"));
    }

    #[test]
    fn ast_hello() {
        let src = "pull \"spill\"\nfinna main() {\n    spill.it(\"hi\")\n}\n";
        let dump = ast(src).expect("hello should parse");
        let expected = [
            "program",
            "  pull \"spill\"",
            "  func hush main",
            "    generics",
            "    params",
            "    ret none",
            "    block",
            "      expr-stmt",
            "        method it",
            "          receiver",
            "            name spill",
            "          args",
            "            arg",
            "              str \"hi\"",
            "",
        ]
        .join("\n");
        assert_eq!(dump, expected);
    }

    #[test]
    fn ast_covers_decls_types_and_exprs() {
        // A compact program touching a drip, generics, a typed param, a return type, a `fr`
        // chain, a compound-assign, and a binary expr — a broad structural smoke test.
        let src = "\
drip Pt { x: int, flex y: int }
finna f[T](a: []T, n: int) -> int {
    lowkey s = 0
    fr n >= 0 {
        s += a[n]
    } naw {
        s = 0 - 1
    }
    bet s
}
";
        let dump = ast(src).expect("should parse");
        // Spot-check representative lines rather than pin the whole tree.
        assert!(dump.contains("drip hush Pt"));
        assert!(dump.contains("  field default x"));
        assert!(dump.contains("  field flex y"));
        assert!(dump.contains("func hush f"));
        assert!(dump.contains("generics T"));
        assert!(dump.contains("slice"));
        assert!(dump.contains("ret single"));
        assert!(dump.contains("assign addeq"));
        assert!(dump.contains("binary ge"));
        assert!(dump.contains("binary sub"));
    }
}
