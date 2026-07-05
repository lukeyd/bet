//! Textual `.mir` format — printer and parser.
//!
//! The format is the interchange for differential testing and the backend's hand-written
//! test inputs. It is **keyword-driven and newline-insensitive**: every construct is
//! self-delimiting (binary ops print as `add.trap(%1, %2)`, calls as `call @f(..)`), so the
//! parser is a plain recursive descent over a token stream with no significant whitespace.
//!
//! The load-bearing invariant is **round-trip stability**: `parse(print(m))` reprints to the
//! same text. Local *names* are cosmetic and intentionally not serialized.

use crate::ir::*;
use std::collections::HashMap;
use std::fmt::Write as _;

/// A `.mir` parse failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("lex error: {0}")]
    Lex(String),
    #[error("parse error: {0}")]
    Parse(String),
}

// ===========================================================================
// Printer
// ===========================================================================

/// Render a module to canonical `.mir` text.
pub fn print(module: &Module) -> String {
    let mut p = Printer {
        m: module,
        out: String::new(),
    };
    p.run();
    p.out
}

struct Printer<'a> {
    m: &'a Module,
    out: String,
}

impl Printer<'_> {
    fn run(&mut self) {
        for def in self.m.structs() {
            self.struct_def(def);
        }
        for def in self.m.sums() {
            self.sum_def(def);
        }
        for ext in self.m.externs() {
            self.extern_decl(ext);
        }
        for g in self.m.globals() {
            self.global(g);
        }
        for c in self.m.crib_globals() {
            let _ = writeln!(
                self.out,
                "crib @{}: {}[{}]",
                c.name,
                self.ty(c.elem),
                c.capacity
            );
        }
        for f in self.m.funcs() {
            self.out.push('\n');
            self.func(f);
        }
    }

    fn struct_def(&mut self, def: &StructDef) {
        let _ = write!(self.out, "struct {} {{", def.name);
        for (i, f) in def.fields.iter().enumerate() {
            let vis = match f.vis {
                Vis::Flex => "flex",
                Vis::Hush => "hush",
            };
            let sep = if i == 0 { " " } else { ", " };
            let _ = write!(self.out, "{sep}{vis} {}: {}", f.name, self.ty(f.ty));
        }
        self.out.push_str(" }\n");
    }

    fn sum_def(&mut self, def: &SumDef) {
        let _ = write!(self.out, "sum {} {{", def.name);
        for (i, v) in def.variants.iter().enumerate() {
            let sep = if i == 0 { " " } else { ", " };
            let _ = write!(self.out, "{sep}{}", v.name);
            if !v.payload.is_empty() {
                let tys: Vec<String> = v.payload.iter().map(|&t| self.ty(t)).collect();
                let _ = write!(self.out, "({})", tys.join(", "));
            }
        }
        self.out.push_str(" }\n");
    }

    fn extern_decl(&mut self, ext: &Extern) {
        let params: Vec<String> = ext.sig.params.iter().map(|&t| self.ty(t)).collect();
        let _ = writeln!(
            self.out,
            "extern \"{}\" fn {}({}) -> {}",
            ext.abi,
            ext.name,
            params.join(", "),
            self.rets(&ext.sig.rets)
        );
    }

    fn global(&mut self, g: &Global) {
        let _ = writeln!(
            self.out,
            "const {}: {} = {}",
            g.name,
            self.ty(g.ty),
            self.const_val(&g.value)
        );
    }

    fn func(&mut self, f: &Func) {
        let params: Vec<String> = f
            .params
            .iter()
            .enumerate()
            .map(|(i, &t)| format!("%{i}: {}", self.ty(t)))
            .collect();
        let _ = writeln!(
            self.out,
            "fn {}({}) -> {} {{",
            f.name,
            params.join(", "),
            self.rets(&f.rets)
        );
        for (i, l) in f.locals.iter().enumerate() {
            if l.kind == LocalKind::Temp {
                let _ = writeln!(self.out, "  let %{i}: {}", self.ty(l.ty));
            }
        }
        for b in &f.blocks {
            let _ = writeln!(self.out, "  bb{}:", b.id.0);
            for s in &b.stmts {
                let _ = writeln!(self.out, "    {}", self.stmt(s));
            }
            let _ = writeln!(self.out, "    {}", self.term(&b.term));
        }
        self.out.push_str("}\n");
    }

    // --- types ---

    fn ty(&self, id: TyId) -> String {
        match self.m.ty(id) {
            TyKind::Bool => "bool".into(),
            TyKind::Int { width, signed } => int_name(*width, *signed),
            TyKind::F32 => "f32".into(),
            TyKind::F64 => "f64".into(),
            TyKind::Str => "str".into(),
            TyKind::Void => "void".into(),
            TyKind::RawPtr => "rawptr".into(),
            TyKind::Struct(s) => self.m.struct_def(*s).name.clone(),
            TyKind::Sum(s) => self.m.sum_def(*s).name.clone(),
            TyKind::Slice(e) => format!("[]{}", self.ty(*e)),
            TyKind::Array(e, n) => format!("[{}; {n}]", self.ty(*e)),
            TyKind::Tag(e) => format!("tag {}", self.ty(*e)),
            TyKind::Crib(e) => format!("crib {}", self.ty(*e)),
            TyKind::Ref(e) => format!("ref {}", self.ty(*e)),
            TyKind::Map(k, v) => format!("map[{}, {}]", self.ty(*k), self.ty(*v)),
            TyKind::Vec(e) => format!("vec[{}]", self.ty(*e)),
            TyKind::FnPtr(sig) => {
                let s = self.m.sig(*sig);
                let ps: Vec<String> = s.params.iter().map(|&t| self.ty(t)).collect();
                format!("fn({}) -> {}", ps.join(", "), self.rets(&s.rets))
            }
            TyKind::Tuple(elems) => {
                let ts: Vec<String> = elems.iter().map(|&t| self.ty(t)).collect();
                format!("({})", ts.join(", "))
            }
        }
    }

    fn rets(&self, rets: &[TyId]) -> String {
        match rets {
            [] => "void".into(),
            [one] => self.ty(*one),
            many => {
                let ts: Vec<String> = many.iter().map(|&t| self.ty(t)).collect();
                format!("({})", ts.join(", "))
            }
        }
    }

    // --- statements & terminators ---

    fn stmt(&self, s: &Stmt) -> String {
        match s {
            Stmt::Nop => "nop".into(),
            Stmt::Evict(op) => format!("evict {}", self.operand(op)),
            Stmt::Assign(place, rv) => format!("{} = {}", self.place(place), self.rvalue(rv)),
        }
    }

    fn term(&self, t: &Terminator) -> String {
        match t {
            Terminator::Goto(bb) => format!("goto bb{}", bb.0),
            Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => format!(
                "branch {} -> bb{} else bb{}",
                self.operand(cond),
                then_bb.0,
                else_bb.0
            ),
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let cs: Vec<String> = cases
                    .iter()
                    .map(|(v, bb)| format!("{v} -> bb{}", bb.0))
                    .collect();
                format!(
                    "switch {} [{}] else bb{}",
                    self.operand(scrutinee),
                    cs.join(", "),
                    default.0
                )
            }
            Terminator::HollaCheck {
                tag,
                crib,
                resolved,
                live,
                ghosted,
            } => format!(
                "holla_check({}, {}, {}) -> bb{} else bb{}",
                self.operand(tag),
                self.operand(crib),
                self.place(resolved),
                live.0,
                ghosted.0
            ),
            Terminator::Return(vals) => {
                if vals.is_empty() {
                    "return".into()
                } else {
                    let vs: Vec<String> = vals.iter().map(|o| self.operand(o)).collect();
                    format!("return {}", vs.join(", "))
                }
            }
            Terminator::Panic(op) => format!("panic {}", self.operand(op)),
            Terminator::Unreachable => "unreachable".into(),
        }
    }

    // --- rvalues, operands, places, consts ---

    fn rvalue(&self, rv: &Rvalue) -> String {
        match rv {
            Rvalue::Use(op) => self.operand(op),
            Rvalue::BinOp(op, a, b, mode) => format!(
                "{}.{}({}, {})",
                binop_name(*op),
                mode_name(*mode),
                self.operand(a),
                self.operand(b)
            ),
            Rvalue::UnOp(op, a) => format!("{}({})", unop_name(*op), self.operand(a)),
            Rvalue::Cast(op, ty, kind) => format!(
                "cast.{}({} as {})",
                cast_name(*kind),
                self.operand(op),
                self.ty(*ty)
            ),
            Rvalue::Call(callee, args) => {
                let a: Vec<String> = args.iter().map(|o| self.operand(o)).collect();
                match callee {
                    Callee::Direct(f) => {
                        format!("call @{}({})", self.m.func(*f).name, a.join(", "))
                    }
                    Callee::Indirect(op) => {
                        format!("call_indirect {}({})", self.operand(op), a.join(", "))
                    }
                    Callee::Extern(e) => {
                        format!(
                            "call_extern @{}({})",
                            self.m.extern_def(*e).name,
                            a.join(", ")
                        )
                    }
                }
            }
            Rvalue::Aggregate(kind, ops) => {
                let a: Vec<String> = ops.iter().map(|o| self.operand(o)).collect();
                match kind {
                    AggKind::Struct(s) => {
                        format!("make {}({})", self.m.struct_def(*s).name, a.join(", "))
                    }
                    AggKind::Tuple => format!("tuple({})", a.join(", ")),
                    AggKind::Array(elem) => {
                        format!("array[{}]({})", self.ty(*elem), a.join(", "))
                    }
                    AggKind::Sum { sum, variant } => {
                        let def = self.m.sum_def(*sum);
                        let vname = &def.variants[*variant as usize].name;
                        format!("make {}::{}({})", def.name, vname, a.join(", "))
                    }
                }
            }
            Rvalue::Discriminant(op) => format!("discriminant({})", self.operand(op)),
            Rvalue::Cop(crib, init) => {
                format!("cop({}, {})", self.operand(crib), self.cop_init(init))
            }
            Rvalue::Trust(crib, tag) => {
                format!("trust({}, {})", self.operand(crib), self.operand(tag))
            }
            Rvalue::StrPtr(op) => format!("str_ptr({})", self.operand(op)),
            Rvalue::StrLen(op) => format!("str_len({})", self.operand(op)),
            Rvalue::SlicePtr(op) => format!("slice_ptr({})", self.operand(op)),
            Rvalue::SliceLen(op) => format!("slice_len({})", self.operand(op)),
            Rvalue::AddrOf(place) => format!("addr_of({})", self.place(place)),
            Rvalue::MakeSlice { data, len, elem } => format!(
                "make_slice[{}]({}, {})",
                self.ty(*elem),
                self.operand(data),
                self.operand(len)
            ),
            Rvalue::CribNew { elem, capacity } => {
                format!("crib_new[{}; {}]", self.ty(*elem), capacity)
            }
            Rvalue::CribGlobal(id) => {
                format!("crib_global(@{})", self.m.crib_global(*id).name)
            }
            Rvalue::SizeOf(ty) => format!("size_of[{}]", self.ty(*ty)),
            Rvalue::MakeStr { data, len } => {
                format!("make_str({}, {})", self.operand(data), self.operand(len))
            }
        }
    }

    fn cop_init(&self, init: &CopInit) -> String {
        match init {
            CopInit::StructLit(s, fields) => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|(i, op)| format!("{i}: {}", self.operand(op)))
                    .collect();
                format!("{}{{{}}}", self.m.struct_def(*s).name, fs.join(", "))
            }
            CopInit::SumVariant(s, v, ops) => {
                let def = self.m.sum_def(*s);
                let vname = &def.variants[*v as usize].name;
                if ops.is_empty() {
                    format!("{}::{}", def.name, vname)
                } else {
                    let a: Vec<String> = ops.iter().map(|o| self.operand(o)).collect();
                    format!("{}::{}({})", def.name, vname, a.join(", "))
                }
            }
        }
    }

    fn operand(&self, op: &Operand) -> String {
        match op {
            Operand::Const(c) => self.const_val(c),
            Operand::Copy(p) => self.place(p),
            Operand::Move(p) => format!("move {}", self.place(p)),
        }
    }

    fn place(&self, p: &Place) -> String {
        let mut s = format!("%{}", p.local.0);
        for proj in &p.proj {
            match proj {
                Proj::Field(i) => {
                    let _ = write!(s, ".field({i})");
                }
                Proj::Index(op) => {
                    let _ = write!(s, ".index({})", self.operand(op));
                }
                Proj::Deref => s.push_str(".deref"),
                Proj::Downcast(v) => {
                    let _ = write!(s, ".downcast({v})");
                }
            }
        }
        s
    }

    fn const_val(&self, c: &Const) -> String {
        match c {
            Const::Int(v, ty) => {
                let suffix = match self.m.ty(*ty) {
                    TyKind::Int { width, signed } => int_name(*width, *signed),
                    _ => "i64".into(),
                };
                format!("{v}{suffix}")
            }
            Const::Float(v, ty) => {
                let suffix = if matches!(self.m.ty(*ty), TyKind::F32) {
                    "f32"
                } else {
                    "f64"
                };
                format!("{v}{suffix}")
            }
            Const::Bool(b) => if *b { "true" } else { "false" }.into(),
            Const::Str(s) => format!("\"{}\"", escape(s)),
            Const::Ghosted => "ghosted".into(),
            Const::FnRef(f) => format!("@{}", self.m.func(*f).name),
        }
    }
}

// ===========================================================================
// Naming tables (shared by printer & parser)
// ===========================================================================

fn int_name(w: IntWidth, signed: bool) -> String {
    format!("{}{}", if signed { "i" } else { "u" }, w.bits())
}

fn parse_int_suffix(s: &str) -> Option<(IntWidth, bool)> {
    let (signed, rest) = match s.split_at_checked(1)? {
        ("i", r) => (true, r),
        ("u", r) => (false, r),
        _ => return None,
    };
    let width = match rest {
        "8" => IntWidth::W8,
        "16" => IntWidth::W16,
        "32" => IntWidth::W32,
        "64" => IntWidth::W64,
        _ => return None,
    };
    Some((width, signed))
}

fn binop_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::Rem => "rem",
        BinOp::BitAnd => "bitand",
        BinOp::BitOr => "bitor",
        BinOp::BitXor => "bitxor",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
    }
}

fn parse_binop(s: &str) -> Option<BinOp> {
    Some(match s {
        "add" => BinOp::Add,
        "sub" => BinOp::Sub,
        "mul" => BinOp::Mul,
        "div" => BinOp::Div,
        "rem" => BinOp::Rem,
        "bitand" => BinOp::BitAnd,
        "bitor" => BinOp::BitOr,
        "bitxor" => BinOp::BitXor,
        "shl" => BinOp::Shl,
        "shr" => BinOp::Shr,
        "eq" => BinOp::Eq,
        "ne" => BinOp::Ne,
        "lt" => BinOp::Lt,
        "le" => BinOp::Le,
        "gt" => BinOp::Gt,
        "ge" => BinOp::Ge,
        _ => return None,
    })
}

fn unop_name(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
        UnOp::BitNot => "bitnot",
    }
}

fn mode_name(m: ArithMode) -> &'static str {
    match m {
        ArithMode::Wrap => "wrap",
        ArithMode::Trap => "trap",
        ArithMode::Na => "na",
    }
}

fn parse_mode(s: &str) -> Option<ArithMode> {
    Some(match s {
        "wrap" => ArithMode::Wrap,
        "trap" => ArithMode::Trap,
        "na" => ArithMode::Na,
        _ => return None,
    })
}

fn cast_name(k: CastKind) -> &'static str {
    match k {
        CastKind::IntZext => "zext",
        CastKind::IntSext => "sext",
        CastKind::IntTrunc => "trunc",
        CastKind::IntToFloat => "itof",
        CastKind::FloatToInt => "ftoi",
        CastKind::FloatResize => "fresize",
        CastKind::Bitcast => "bitcast",
    }
}

fn parse_cast(s: &str) -> Option<CastKind> {
    Some(match s {
        "zext" => CastKind::IntZext,
        "sext" => CastKind::IntSext,
        "trunc" => CastKind::IntTrunc,
        "itof" => CastKind::IntToFloat,
        "ftoi" => CastKind::FloatToInt,
        "fresize" => CastKind::FloatResize,
        "bitcast" => CastKind::Bitcast,
        _ => return None,
    })
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

// ===========================================================================
// Lexer
// ===========================================================================

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Ident(String),
    /// A raw numeric literal (may include a leading `-`, a fraction, and a type suffix).
    Num(String),
    Str(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    ColonColon,
    Semi,
    Percent,
    At,
    Arrow,
    Eq,
    Dot,
}

fn lex(src: &str) -> Result<Vec<Tok>, ParseError> {
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut toks = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // line comments
        if c == '/' && bytes.get(i + 1) == Some(&'/') {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        match c {
            '(' => push(&mut toks, Tok::LParen, &mut i),
            ')' => push(&mut toks, Tok::RParen, &mut i),
            '{' => push(&mut toks, Tok::LBrace, &mut i),
            '}' => push(&mut toks, Tok::RBrace, &mut i),
            '[' => push(&mut toks, Tok::LBracket, &mut i),
            ']' => push(&mut toks, Tok::RBracket, &mut i),
            ',' => push(&mut toks, Tok::Comma, &mut i),
            ';' => push(&mut toks, Tok::Semi, &mut i),
            '%' => push(&mut toks, Tok::Percent, &mut i),
            '@' => push(&mut toks, Tok::At, &mut i),
            '=' => push(&mut toks, Tok::Eq, &mut i),
            '.' => push(&mut toks, Tok::Dot, &mut i),
            ':' => {
                if bytes.get(i + 1) == Some(&':') {
                    toks.push(Tok::ColonColon);
                    i += 2;
                } else {
                    push(&mut toks, Tok::Colon, &mut i);
                }
            }
            '-' => {
                if bytes.get(i + 1) == Some(&'>') {
                    toks.push(Tok::Arrow);
                    i += 2;
                } else if bytes.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    toks.push(lex_num(&bytes, &mut i));
                } else {
                    return Err(ParseError::Lex(format!("stray '-' at char {i}")));
                }
            }
            '"' => {
                toks.push(lex_str(&bytes, &mut i)?);
            }
            _ if c.is_ascii_digit() => toks.push(lex_num(&bytes, &mut i)),
            _ if c.is_alphabetic() || c == '_' => {
                // `$` joins monomorphization-mangled names (e.g. `unbox$str`) into one identifier
                // so the .mir text round-trips (frontend emits `$`, the backend must read it back).
                let mut s = String::new();
                while i < bytes.len()
                    && (bytes[i].is_alphanumeric() || bytes[i] == '_' || bytes[i] == '$')
                {
                    s.push(bytes[i]);
                    i += 1;
                }
                toks.push(Tok::Ident(s));
            }
            _ => return Err(ParseError::Lex(format!("unexpected char {c:?} at {i}"))),
        }
    }
    Ok(toks)
}

fn push(toks: &mut Vec<Tok>, t: Tok, i: &mut usize) {
    toks.push(t);
    *i += 1;
}

fn lex_num(bytes: &[char], i: &mut usize) -> Tok {
    let mut s = String::new();
    if bytes[*i] == '-' {
        s.push('-');
        *i += 1;
    }
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        s.push(bytes[*i]);
        *i += 1;
    }
    // fraction
    if *i + 1 < bytes.len() && bytes[*i] == '.' && bytes[*i + 1].is_ascii_digit() {
        s.push('.');
        *i += 1;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            s.push(bytes[*i]);
            *i += 1;
        }
    }
    // type suffix (e.g. i64, u32, f64)
    while *i < bytes.len() && (bytes[*i].is_alphanumeric() || bytes[*i] == '_') {
        s.push(bytes[*i]);
        *i += 1;
    }
    Tok::Num(s)
}

fn lex_str(bytes: &[char], i: &mut usize) -> Result<Tok, ParseError> {
    *i += 1; // opening quote
    let mut s = String::new();
    while *i < bytes.len() && bytes[*i] != '"' {
        if bytes[*i] == '\\' {
            *i += 1;
            let e = bytes
                .get(*i)
                .ok_or_else(|| ParseError::Lex("unterminated escape".into()))?;
            match e {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                '\\' => s.push('\\'),
                '"' => s.push('"'),
                other => return Err(ParseError::Lex(format!("bad escape \\{other}"))),
            }
            *i += 1;
        } else {
            s.push(bytes[*i]);
            *i += 1;
        }
    }
    if *i >= bytes.len() {
        return Err(ParseError::Lex("unterminated string".into()));
    }
    *i += 1; // closing quote
    Ok(Tok::Str(s))
}

// ===========================================================================
// Parser
// ===========================================================================

/// Parse `.mir` text into a module.
pub fn parse(src: &str) -> Result<Module, ParseError> {
    let toks = lex(src)?;
    // `extern_ids` (name→first id) is intentionally ignored: overloaded externs share a name, so
    // calls are resolved structurally by `Parser::resolve_extern` over the full extern table.
    let (struct_ids, sum_ids, func_ids, _extern_ids, crib_global_ids) = prescan(&toks);
    let mut p = Parser {
        toks,
        pos: 0,
        m: Module::new(),
        struct_ids,
        sum_ids,
        func_ids,
        crib_global_ids,
        cur_locals: Vec::new(),
        dest_ty: None,
    };
    p.module()?;
    Ok(p.m)
}

/// The name→id maps produced by [`prescan`], keyed by declaration name.
type PrescanIds = (
    HashMap<String, StructId>,
    HashMap<String, SumId>,
    HashMap<String, FuncId>,
    HashMap<String, ExternId>,
    HashMap<String, CribGlobalId>,
);

/// Assign ids to every named struct/sum/fn/extern up front, so bodies may refer to names that
/// are self-referential or defined later. Ids follow appearance order per kind (matching the
/// printer, which emits each kind contiguously in id order).
fn prescan(toks: &[Tok]) -> PrescanIds {
    let (mut structs, mut sums, mut funcs, mut externs, mut cribs) = (
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let (mut sn, mut un, mut fnc, mut en, mut cn) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut i = 0;
    while i < toks.len() {
        // `extern "C" fn NAME` — register the extern and skip its `fn NAME` so it is not
        // also miscounted as a function (which would give it a dangling FuncId).
        if matches!(&toks[i], Tok::Ident(kw) if kw == "extern")
            && matches!(toks.get(i + 1), Some(Tok::Str(_)))
            && matches!(toks.get(i + 2), Some(Tok::Ident(f)) if f == "fn")
            && let Some(Tok::Ident(name)) = toks.get(i + 3)
        {
            externs.entry(name.clone()).or_insert(ExternId(en));
            en += 1;
            i += 4;
            continue;
        }
        // `crib @NAME: ...` — a module-level crib (global arena).
        if matches!(&toks[i], Tok::Ident(kw) if kw == "crib")
            && matches!(toks.get(i + 1), Some(Tok::At))
            && let Some(Tok::Ident(name)) = toks.get(i + 2)
        {
            cribs.entry(name.clone()).or_insert(CribGlobalId(cn));
            cn += 1;
            i += 3;
            continue;
        }
        if let (Tok::Ident(kw), Some(Tok::Ident(name))) = (&toks[i], toks.get(i + 1)) {
            match kw.as_str() {
                "struct" => {
                    structs.entry(name.clone()).or_insert(StructId(sn));
                    sn += 1;
                }
                "sum" => {
                    sums.entry(name.clone()).or_insert(SumId(un));
                    un += 1;
                }
                "fn" => {
                    funcs.entry(name.clone()).or_insert(FuncId(fnc));
                    fnc += 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    (structs, sums, funcs, externs, cribs)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    m: Module,
    struct_ids: HashMap<String, StructId>,
    sum_ids: HashMap<String, SumId>,
    func_ids: HashMap<String, FuncId>,
    crib_global_ids: HashMap<String, CribGlobalId>,
    /// Types of the current function's locals (params + `let`s), used to resolve overloaded
    /// extern calls by argument/destination type. Populated by [`Parser::fn_decl`].
    cur_locals: Vec<TyId>,
    /// The type of the place on the left of the assignment currently being parsed (if any) —
    /// disambiguates overloaded externs that differ only in return type.
    dest_ty: Option<TyId>,
}

impl Parser {
    // --- token helpers ---

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Result<(), ParseError> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(self.err(&format!("expected {t:?}")))
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            other => Err(self.err(&format!("expected identifier, found {other:?}"))),
        }
    }

    fn peek_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn err(&self, msg: &str) -> ParseError {
        ParseError::Parse(format!("{msg} (at token {})", self.pos))
    }

    // --- top level ---

    fn module(&mut self) -> Result<(), ParseError> {
        while let Some(tok) = self.peek() {
            let Tok::Ident(kw) = tok else {
                return Err(self.err("expected a top-level item"));
            };
            match kw.as_str() {
                "struct" => self.struct_decl()?,
                "sum" => self.sum_decl()?,
                "extern" => self.extern_decl()?,
                "const" => self.const_decl()?,
                "crib" => self.crib_global_decl()?,
                "fn" => self.fn_decl()?,
                other => return Err(self.err(&format!("unknown top-level keyword `{other}`"))),
            }
        }
        Ok(())
    }

    fn crib_global_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("crib");
        self.expect(&Tok::At)?;
        let name = self.expect_ident()?;
        self.expect(&Tok::Colon)?;
        let elem = self.ty()?;
        self.expect(&Tok::LBracket)?;
        let capacity = self.bare_u64()? as u32;
        self.expect(&Tok::RBracket)?;
        self.m.add_crib_global(CribGlobal {
            name,
            elem,
            capacity,
        });
        Ok(())
    }

    fn struct_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("struct");
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        while !self.eat(&Tok::RBrace) {
            if !fields.is_empty() {
                self.expect(&Tok::Comma)?;
            }
            let vis = if self.eat_kw("flex") {
                Vis::Flex
            } else if self.eat_kw("hush") {
                Vis::Hush
            } else {
                return Err(self.err("expected `flex` or `hush`"));
            };
            let fname = self.expect_ident()?;
            self.expect(&Tok::Colon)?;
            let ty = self.ty()?;
            fields.push(Field {
                name: fname,
                ty,
                vis,
            });
        }
        self.m.add_struct(StructDef { name, fields });
        Ok(())
    }

    fn sum_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("sum");
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace)?;
        let mut variants = Vec::new();
        while !self.eat(&Tok::RBrace) {
            if !variants.is_empty() {
                self.expect(&Tok::Comma)?;
            }
            let vname = self.expect_ident()?;
            let mut payload = Vec::new();
            if self.eat(&Tok::LParen) {
                while !self.eat(&Tok::RParen) {
                    if !payload.is_empty() {
                        self.expect(&Tok::Comma)?;
                    }
                    payload.push(self.ty()?);
                }
            }
            variants.push(Variant {
                name: vname,
                payload,
            });
        }
        self.m.add_sum(SumDef { name, variants });
        Ok(())
    }

    fn extern_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("extern");
        let abi = match self.bump() {
            Some(Tok::Str(s)) => s,
            other => return Err(self.err(&format!("expected ABI string, found {other:?}"))),
        };
        if !self.eat_kw("fn") {
            return Err(self.err("expected `fn` in extern decl"));
        }
        let name = self.expect_ident()?;
        let params = self.param_types()?;
        self.expect(&Tok::Arrow)?;
        let rets = self.ret_types()?;
        self.m.add_extern(Extern {
            name,
            abi,
            sig: Sig { params, rets },
        });
        Ok(())
    }

    fn const_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("const");
        let name = self.expect_ident()?;
        self.expect(&Tok::Colon)?;
        let ty = self.ty()?;
        self.expect(&Tok::Eq)?;
        let value = self.const_val()?;
        self.m.add_global(Global { name, ty, value });
        Ok(())
    }

    // --- functions ---

    fn fn_decl(&mut self) -> Result<(), ParseError> {
        self.eat_kw("fn");
        let name = self.expect_ident()?;
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        let mut locals = Vec::new();
        // Track local types on `self` so overloaded `call_extern` resolution can see them. The
        // printer emits every `let` before any block body, so this is complete by the time a
        // statement is parsed.
        self.cur_locals.clear();
        while !self.eat(&Tok::RParen) {
            if !params.is_empty() {
                self.expect(&Tok::Comma)?;
            }
            let idx = self.local_index()?;
            debug_assert_eq!(idx as usize, params.len());
            self.expect(&Tok::Colon)?;
            let ty = self.ty()?;
            params.push(ty);
            locals.push(Local {
                ty,
                name: None,
                kind: LocalKind::Param,
            });
            self.cur_locals.push(ty);
        }
        self.expect(&Tok::Arrow)?;
        let rets = self.ret_types()?;
        self.expect(&Tok::LBrace)?;

        let mut blocks: Vec<Block> = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                break;
            }
            if self.eat_kw("let") {
                let idx = self.local_index()?;
                if idx as usize != locals.len() {
                    return Err(self.err("out-of-order `let` local"));
                }
                self.expect(&Tok::Colon)?;
                let ty = self.ty()?;
                locals.push(Local {
                    ty,
                    name: None,
                    kind: LocalKind::Temp,
                });
                self.cur_locals.push(ty);
                continue;
            }
            // otherwise a block label `bbN:` opening a block body
            let label = self.block_id()?;
            if label.0 as usize != blocks.len() {
                return Err(self.err("out-of-order block label"));
            }
            self.expect(&Tok::Colon)?;
            let (stmts, term) = self.block_body()?;
            blocks.push(Block {
                id: label,
                stmts,
                term,
            });
        }

        if blocks.is_empty() {
            return Err(self.err("function has no blocks"));
        }
        self.m.add_func(Func {
            name,
            params,
            rets,
            locals,
            blocks,
            entry: BlockId(0),
        });
        Ok(())
    }

    fn block_body(&mut self) -> Result<(Vec<Stmt>, Terminator), ParseError> {
        let mut stmts = Vec::new();
        loop {
            let kw = match self.peek() {
                Some(Tok::Ident(s)) => Some(s.as_str()),
                _ => None,
            };
            match kw {
                Some(
                    "goto" | "branch" | "switch" | "holla_check" | "return" | "panic"
                    | "unreachable",
                ) => {
                    return Ok((stmts, self.terminator()?));
                }
                _ => stmts.push(self.statement()?),
            }
        }
    }

    fn statement(&mut self) -> Result<Stmt, ParseError> {
        if self.eat_kw("nop") {
            return Ok(Stmt::Nop);
        }
        if self.eat_kw("evict") {
            return Ok(Stmt::Evict(self.operand()?));
        }
        // place = rvalue
        let place = self.place()?;
        self.expect(&Tok::Eq)?;
        // Record the destination type so `call_extern` can disambiguate overloaded externs whose
        // signatures differ only in return type (e.g. `bet_vec_new` per element type).
        self.dest_ty = self.place_ty(&place);
        let rv = self.rvalue()?;
        self.dest_ty = None;
        Ok(Stmt::Assign(place, rv))
    }

    /// The interned type of a place, walking projections read-only over the module. Returns
    /// `None` if a local or projection can't be resolved — treated as a wildcard by
    /// [`Parser::resolve_extern`]. Records no errors; the validator does the real checking.
    fn place_ty(&self, place: &Place) -> Option<TyId> {
        let mut ty = *self.cur_locals.get(place.local.index())?;
        let mut pending: Option<(SumId, u32)> = None;
        for proj in &place.proj {
            match proj {
                Proj::Field(i) => match pending.take() {
                    Some((sid, v)) => {
                        ty = *self
                            .m
                            .sum_def(sid)
                            .variants
                            .get(v as usize)?
                            .payload
                            .get(*i as usize)?;
                    }
                    None => match self.m.ty(ty).clone() {
                        TyKind::Struct(sid) => {
                            ty = self.m.struct_def(sid).fields.get(*i as usize)?.ty;
                        }
                        _ => return None,
                    },
                },
                Proj::Index(_) => match self.m.ty(ty).clone() {
                    TyKind::Slice(e) | TyKind::Array(e, _) => ty = e,
                    _ => return None,
                },
                Proj::Deref => match self.m.ty(ty).clone() {
                    TyKind::Ref(e) => ty = e,
                    _ => return None,
                },
                Proj::Downcast(v) => match self.m.ty(ty).clone() {
                    TyKind::Sum(sid) => pending = Some((sid, *v)),
                    _ => return None,
                },
            }
        }
        Some(ty)
    }

    /// The interned type of an operand, or `None` when it doesn't pin one down (a wildcard for
    /// overload resolution). Only the cases that appear as overloaded-extern arguments matter.
    fn operand_ty(&self, op: &Operand) -> Option<TyId> {
        match op {
            Operand::Const(Const::Int(_, ty) | Const::Float(_, ty)) => Some(*ty),
            Operand::Const(_) => None,
            Operand::Copy(p) | Operand::Move(p) => self.place_ty(p),
        }
    }

    /// Resolve a `call_extern @name` to a specific [`ExternId`]. The frontend emits one extern per
    /// vec/map element type — all sharing a C symbol name — so a name alone is ambiguous on
    /// re-parse. Pick the same-named extern whose parameter types match the argument types and
    /// (for overloads that differ only in return type, e.g. `bet_vec_new`) whose single return
    /// matches the destination. Falls back to the first same-named extern — the only one, and
    /// correct, when the name is not overloaded.
    fn resolve_extern(
        &self,
        name: &str,
        args: &[Operand],
        dest_ty: Option<TyId>,
    ) -> Option<ExternId> {
        let mut fallback: Option<ExternId> = None;
        for (i, ext) in self.m.externs().iter().enumerate() {
            if ext.name != name {
                continue;
            }
            let eid = ExternId(i as u32);
            if fallback.is_none() {
                fallback = Some(eid);
            }
            if ext.sig.params.len() != args.len() {
                continue;
            }
            let params_ok = ext
                .sig
                .params
                .iter()
                .zip(args)
                .all(|(&pty, arg)| self.operand_ty(arg).is_none_or(|aty| aty == pty));
            if !params_ok {
                continue;
            }
            let rets_ok = match (dest_ty, ext.sig.rets.as_slice()) {
                (Some(d), [r]) => *r == d,
                _ => true,
            };
            if rets_ok {
                return Some(eid);
            }
        }
        fallback
    }

    fn terminator(&mut self) -> Result<Terminator, ParseError> {
        if self.eat_kw("goto") {
            return Ok(Terminator::Goto(self.block_id()?));
        }
        if self.eat_kw("branch") {
            let cond = self.operand()?;
            self.expect(&Tok::Arrow)?;
            let then_bb = self.block_id()?;
            if !self.eat_kw("else") {
                return Err(self.err("expected `else` in branch"));
            }
            let else_bb = self.block_id()?;
            return Ok(Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            });
        }
        if self.eat_kw("switch") {
            let scrutinee = self.operand()?;
            self.expect(&Tok::LBracket)?;
            let mut cases = Vec::new();
            while !self.eat(&Tok::RBracket) {
                if !cases.is_empty() {
                    self.expect(&Tok::Comma)?;
                }
                let v = self.bare_u64()?;
                self.expect(&Tok::Arrow)?;
                let bb = self.block_id()?;
                cases.push((v, bb));
            }
            if !self.eat_kw("else") {
                return Err(self.err("expected `else` in switch"));
            }
            let default = self.block_id()?;
            return Ok(Terminator::Switch {
                scrutinee,
                cases,
                default,
            });
        }
        if self.eat_kw("holla_check") {
            self.expect(&Tok::LParen)?;
            let tag = self.operand()?;
            self.expect(&Tok::Comma)?;
            let crib = self.operand()?;
            self.expect(&Tok::Comma)?;
            let resolved = self.place()?;
            self.expect(&Tok::RParen)?;
            self.expect(&Tok::Arrow)?;
            let live = self.block_id()?;
            if !self.eat_kw("else") {
                return Err(self.err("expected `else` in holla_check"));
            }
            let ghosted = self.block_id()?;
            return Ok(Terminator::HollaCheck {
                tag,
                crib,
                resolved,
                live,
                ghosted,
            });
        }
        if self.eat_kw("return") {
            let mut vals = Vec::new();
            // zero or more comma-separated operands
            if self.starts_operand() {
                vals.push(self.operand()?);
                while self.eat(&Tok::Comma) {
                    vals.push(self.operand()?);
                }
            }
            return Ok(Terminator::Return(vals));
        }
        if self.eat_kw("panic") {
            return Ok(Terminator::Panic(self.operand()?));
        }
        if self.eat_kw("unreachable") {
            return Ok(Terminator::Unreachable);
        }
        Err(self.err("expected a terminator"))
    }

    // --- rvalues ---

    fn rvalue(&mut self) -> Result<Rvalue, ParseError> {
        let kw = match self.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            _ => return Ok(Rvalue::Use(self.operand()?)),
        };
        if let Some(op) = parse_binop(&kw) {
            self.pos += 1;
            self.expect(&Tok::Dot)?;
            let mode_s = self.expect_ident()?;
            let mode = parse_mode(&mode_s).ok_or_else(|| self.err("bad arith mode"))?;
            self.expect(&Tok::LParen)?;
            let a = self.operand()?;
            self.expect(&Tok::Comma)?;
            let b = self.operand()?;
            self.expect(&Tok::RParen)?;
            return Ok(Rvalue::BinOp(op, a, b, mode));
        }
        match kw.as_str() {
            "neg" | "not" | "bitnot" => {
                self.pos += 1;
                let op = match kw.as_str() {
                    "neg" => UnOp::Neg,
                    "not" => UnOp::Not,
                    _ => UnOp::BitNot,
                };
                self.expect(&Tok::LParen)?;
                let a = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::UnOp(op, a))
            }
            "cast" => {
                self.pos += 1;
                self.expect(&Tok::Dot)?;
                let kind_s = self.expect_ident()?;
                let kind = parse_cast(&kind_s).ok_or_else(|| self.err("bad cast kind"))?;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                if !self.eat_kw("as") {
                    return Err(self.err("expected `as` in cast"));
                }
                let ty = self.ty()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::Cast(op, ty, kind))
            }
            "call" => {
                self.pos += 1;
                self.expect(&Tok::At)?;
                let name = self.expect_ident()?;
                let f = *self
                    .func_ids
                    .get(&name)
                    .ok_or_else(|| self.err(&format!("unknown function `{name}`")))?;
                let args = self.arg_list()?;
                Ok(Rvalue::Call(Callee::Direct(f), args))
            }
            "call_indirect" => {
                self.pos += 1;
                let callee = self.operand()?;
                let args = self.arg_list()?;
                Ok(Rvalue::Call(Callee::Indirect(callee), args))
            }
            "call_extern" => {
                self.pos += 1;
                self.expect(&Tok::At)?;
                let name = self.expect_ident()?;
                let args = self.arg_list()?;
                // The frontend emits one extern per vec/map element type, all sharing a C symbol
                // name, so a name alone is ambiguous on re-parse. Resolve to the same-named extern
                // whose parameter types match the arguments (and, for overloads that differ only
                // in return type, whose return matches the assignment destination).
                let e = self
                    .resolve_extern(&name, &args, self.dest_ty)
                    .ok_or_else(|| self.err(&format!("unknown extern `{name}`")))?;
                Ok(Rvalue::Call(Callee::Extern(e), args))
            }
            "str_ptr" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::StrPtr(op))
            }
            "str_len" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::StrLen(op))
            }
            "slice_ptr" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::SlicePtr(op))
            }
            "slice_len" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::SliceLen(op))
            }
            "make" => {
                self.pos += 1;
                let name = self.expect_ident()?;
                if self.eat(&Tok::ColonColon) {
                    // `make Name::Variant(args)` — a by-value sum value.
                    let sid = *self
                        .sum_ids
                        .get(&name)
                        .ok_or_else(|| self.err(&format!("unknown sum `{name}`")))?;
                    let vname = self.expect_ident()?;
                    let variant = self.variant_index(sid, &vname)?;
                    let args = self.arg_list()?;
                    Ok(Rvalue::Aggregate(AggKind::Sum { sum: sid, variant }, args))
                } else {
                    let sid = *self
                        .struct_ids
                        .get(&name)
                        .ok_or_else(|| self.err(&format!("unknown struct `{name}`")))?;
                    let args = self.arg_list()?;
                    Ok(Rvalue::Aggregate(AggKind::Struct(sid), args))
                }
            }
            "tuple" => {
                self.pos += 1;
                let args = self.arg_list()?;
                Ok(Rvalue::Aggregate(AggKind::Tuple, args))
            }
            "array" => {
                self.pos += 1;
                self.expect(&Tok::LBracket)?;
                let elem = self.ty()?;
                self.expect(&Tok::RBracket)?;
                let args = self.arg_list()?;
                Ok(Rvalue::Aggregate(AggKind::Array(elem), args))
            }
            "addr_of" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let place = self.place()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::AddrOf(place))
            }
            "make_slice" => {
                self.pos += 1;
                self.expect(&Tok::LBracket)?;
                let elem = self.ty()?;
                self.expect(&Tok::RBracket)?;
                self.expect(&Tok::LParen)?;
                let data = self.operand()?;
                self.expect(&Tok::Comma)?;
                let len = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::MakeSlice { data, len, elem })
            }
            "crib_new" => {
                self.pos += 1;
                self.expect(&Tok::LBracket)?;
                let elem = self.ty()?;
                self.expect(&Tok::Semi)?;
                let capacity = self.bare_u64()? as u32;
                self.expect(&Tok::RBracket)?;
                Ok(Rvalue::CribNew { elem, capacity })
            }
            "crib_global" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                self.expect(&Tok::At)?;
                let name = self.expect_ident()?;
                self.expect(&Tok::RParen)?;
                let id = *self
                    .crib_global_ids
                    .get(&name)
                    .ok_or_else(|| self.err(&format!("unknown module-level crib `{name}`")))?;
                Ok(Rvalue::CribGlobal(id))
            }
            "size_of" => {
                self.pos += 1;
                self.expect(&Tok::LBracket)?;
                let ty = self.ty()?;
                self.expect(&Tok::RBracket)?;
                Ok(Rvalue::SizeOf(ty))
            }
            "make_str" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let data = self.operand()?;
                self.expect(&Tok::Comma)?;
                let len = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::MakeStr { data, len })
            }
            "discriminant" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let op = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::Discriminant(op))
            }
            "cop" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let crib = self.operand()?;
                self.expect(&Tok::Comma)?;
                let init = self.cop_init()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::Cop(crib, init))
            }
            "trust" => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let crib = self.operand()?;
                self.expect(&Tok::Comma)?;
                let tag = self.operand()?;
                self.expect(&Tok::RParen)?;
                Ok(Rvalue::Trust(crib, tag))
            }
            // not an rvalue keyword — must be an operand (`use`)
            _ => Ok(Rvalue::Use(self.operand()?)),
        }
    }

    fn cop_init(&mut self) -> Result<CopInit, ParseError> {
        let name = self.expect_ident()?;
        if self.eat(&Tok::ColonColon) {
            let sid = *self
                .sum_ids
                .get(&name)
                .ok_or_else(|| self.err(&format!("unknown sum `{name}`")))?;
            let vname = self.expect_ident()?;
            let vidx = self.variant_index(sid, &vname)?;
            let mut ops = Vec::new();
            if self.eat(&Tok::LParen) {
                while !self.eat(&Tok::RParen) {
                    if !ops.is_empty() {
                        self.expect(&Tok::Comma)?;
                    }
                    ops.push(self.operand()?);
                }
            }
            Ok(CopInit::SumVariant(sid, vidx, ops))
        } else {
            let sid = *self
                .struct_ids
                .get(&name)
                .ok_or_else(|| self.err(&format!("unknown struct `{name}`")))?;
            self.expect(&Tok::LBrace)?;
            let mut fields = Vec::new();
            while !self.eat(&Tok::RBrace) {
                if !fields.is_empty() {
                    self.expect(&Tok::Comma)?;
                }
                let idx = self.bare_u64()? as u32;
                self.expect(&Tok::Colon)?;
                let op = self.operand()?;
                fields.push((idx, op));
            }
            Ok(CopInit::StructLit(sid, fields))
        }
    }

    fn variant_index(&self, sid: SumId, name: &str) -> Result<u32, ParseError> {
        self.m
            .sum_def(sid)
            .variants
            .iter()
            .position(|v| v.name == name)
            .map(|i| i as u32)
            .ok_or_else(|| self.err(&format!("no variant `{name}`")))
    }

    fn arg_list(&mut self) -> Result<Vec<Operand>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut args = Vec::new();
        while !self.eat(&Tok::RParen) {
            if !args.is_empty() {
                self.expect(&Tok::Comma)?;
            }
            args.push(self.operand()?);
        }
        Ok(args)
    }

    // --- operands & places ---

    fn starts_operand(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Percent | Tok::Num(_) | Tok::Str(_) | Tok::At)
        ) || self.peek_kw("move")
            || self.peek_kw("true")
            || self.peek_kw("false")
            || self.peek_kw("ghosted")
    }

    fn operand(&mut self) -> Result<Operand, ParseError> {
        if self.eat_kw("move") {
            return Ok(Operand::Move(self.place()?));
        }
        match self.peek() {
            Some(Tok::Percent) => Ok(Operand::Copy(self.place()?)),
            _ => Ok(Operand::Const(self.const_val()?)),
        }
    }

    fn place(&mut self) -> Result<Place, ParseError> {
        self.expect(&Tok::Percent)?;
        let local = LocalId(self.bare_u64()? as u32);
        let mut proj = Vec::new();
        while self.eat(&Tok::Dot) {
            let kind = self.expect_ident()?;
            match kind.as_str() {
                "field" => {
                    self.expect(&Tok::LParen)?;
                    let i = self.bare_u64()? as u32;
                    self.expect(&Tok::RParen)?;
                    proj.push(Proj::Field(i));
                }
                "index" => {
                    self.expect(&Tok::LParen)?;
                    let op = self.operand()?;
                    self.expect(&Tok::RParen)?;
                    proj.push(Proj::Index(op));
                }
                "deref" => proj.push(Proj::Deref),
                "downcast" => {
                    self.expect(&Tok::LParen)?;
                    let v = self.bare_u64()? as u32;
                    self.expect(&Tok::RParen)?;
                    proj.push(Proj::Downcast(v));
                }
                other => return Err(self.err(&format!("unknown projection `.{other}`"))),
            }
        }
        Ok(Place { local, proj })
    }

    fn const_val(&mut self) -> Result<Const, ParseError> {
        if self.eat_kw("true") {
            return Ok(Const::Bool(true));
        }
        if self.eat_kw("false") {
            return Ok(Const::Bool(false));
        }
        if self.eat_kw("ghosted") {
            return Ok(Const::Ghosted);
        }
        if self.eat(&Tok::At) {
            let name = self.expect_ident()?;
            let f = *self
                .func_ids
                .get(&name)
                .ok_or_else(|| self.err(&format!("unknown function `{name}`")))?;
            return Ok(Const::FnRef(f));
        }
        match self.bump() {
            Some(Tok::Str(s)) => Ok(Const::Str(s)),
            Some(Tok::Num(s)) => self.number_const(&s),
            other => Err(self.err(&format!("expected a constant, found {other:?}"))),
        }
    }

    fn number_const(&mut self, s: &str) -> Result<Const, ParseError> {
        let (num, suffix) = split_num(s);
        match suffix {
            None => Err(self.err(&format!("constant `{s}` needs a type suffix"))),
            Some(suf) if suf == "f32" || suf == "f64" => {
                let v: f64 = num
                    .parse()
                    .map_err(|_| self.err(&format!("bad float `{num}`")))?;
                let ty = self.m.intern_ty(if suf == "f32" {
                    TyKind::F32
                } else {
                    TyKind::F64
                });
                Ok(Const::Float(v, ty))
            }
            Some(suf) => {
                let (width, signed) = parse_int_suffix(&suf)
                    .ok_or_else(|| self.err(&format!("bad suffix `{suf}`")))?;
                let v: i128 = num
                    .parse()
                    .map_err(|_| self.err(&format!("bad integer `{num}`")))?;
                let ty = self.m.intern_ty(TyKind::Int { width, signed });
                Ok(Const::Int(v, ty))
            }
        }
    }

    fn bare_u64(&mut self) -> Result<u64, ParseError> {
        match self.bump() {
            Some(Tok::Num(s)) => {
                let (num, suffix) = split_num(&s);
                if suffix.is_some() {
                    return Err(self.err(&format!("expected a bare integer, found `{s}`")));
                }
                num.parse()
                    .map_err(|_| self.err(&format!("bad integer `{num}`")))
            }
            other => Err(self.err(&format!("expected integer, found {other:?}"))),
        }
    }

    fn local_index(&mut self) -> Result<u32, ParseError> {
        self.expect(&Tok::Percent)?;
        Ok(self.bare_u64()? as u32)
    }

    fn block_id(&mut self) -> Result<BlockId, ParseError> {
        let id = self.expect_ident()?;
        let n = id
            .strip_prefix("bb")
            .and_then(|r| r.parse::<u32>().ok())
            .ok_or_else(|| self.err(&format!("bad block label `{id}`")))?;
        Ok(BlockId(n))
    }

    // --- types ---

    fn param_types(&mut self) -> Result<Vec<TyId>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut tys = Vec::new();
        while !self.eat(&Tok::RParen) {
            if !tys.is_empty() {
                self.expect(&Tok::Comma)?;
            }
            tys.push(self.ty()?);
        }
        Ok(tys)
    }

    /// A return spec: `void` (no values), a single type, or `(t, u, ..)`.
    fn ret_types(&mut self) -> Result<Vec<TyId>, ParseError> {
        if self.peek_kw("void") {
            self.pos += 1;
            return Ok(Vec::new());
        }
        if self.eat(&Tok::LParen) {
            let mut tys = Vec::new();
            while !self.eat(&Tok::RParen) {
                if !tys.is_empty() {
                    self.expect(&Tok::Comma)?;
                }
                tys.push(self.ty()?);
            }
            return Ok(tys);
        }
        Ok(vec![self.ty()?])
    }

    fn ty(&mut self) -> Result<TyId, ParseError> {
        // bracket-led: slice `[]T` or array `[T; N]`
        if self.eat(&Tok::LBracket) {
            if self.eat(&Tok::RBracket) {
                let e = self.ty()?;
                return Ok(self.m.intern_ty(TyKind::Slice(e)));
            }
            let e = self.ty()?;
            self.expect(&Tok::Semi)?;
            let n = self.bare_u64()?;
            self.expect(&Tok::RBracket)?;
            return Ok(self.m.intern_ty(TyKind::Array(e, n)));
        }
        // paren-led tuple (or grouping)
        if self.eat(&Tok::LParen) {
            let first = self.ty()?;
            if self.eat(&Tok::RParen) {
                return Ok(first);
            }
            let mut elems = vec![first];
            while self.eat(&Tok::Comma) {
                elems.push(self.ty()?);
            }
            self.expect(&Tok::RParen)?;
            return Ok(self.m.intern_ty(TyKind::Tuple(elems)));
        }
        let name = self.expect_ident()?;
        match name.as_str() {
            "bool" => Ok(self.m.intern_ty(TyKind::Bool)),
            "f32" => Ok(self.m.intern_ty(TyKind::F32)),
            "f64" => Ok(self.m.intern_ty(TyKind::F64)),
            "str" => Ok(self.m.intern_ty(TyKind::Str)),
            "void" => Ok(self.m.intern_ty(TyKind::Void)),
            "rawptr" => Ok(self.m.intern_ty(TyKind::RawPtr)),
            "tag" => {
                let e = self.ty()?;
                Ok(self.m.intern_ty(TyKind::Tag(e)))
            }
            "crib" => {
                let e = self.ty()?;
                Ok(self.m.intern_ty(TyKind::Crib(e)))
            }
            "ref" => {
                let e = self.ty()?;
                Ok(self.m.intern_ty(TyKind::Ref(e)))
            }
            "map" => {
                self.expect(&Tok::LBracket)?;
                let k = self.ty()?;
                self.expect(&Tok::Comma)?;
                let v = self.ty()?;
                self.expect(&Tok::RBracket)?;
                Ok(self.m.intern_ty(TyKind::Map(k, v)))
            }
            "vec" => {
                self.expect(&Tok::LBracket)?;
                let e = self.ty()?;
                self.expect(&Tok::RBracket)?;
                Ok(self.m.intern_ty(TyKind::Vec(e)))
            }
            "fn" => {
                let params = self.param_types()?;
                self.expect(&Tok::Arrow)?;
                let rets = self.ret_types()?;
                let sig = self.m.intern_sig(Sig { params, rets });
                Ok(self.m.intern_ty(TyKind::FnPtr(sig)))
            }
            _ => {
                if let Some((width, signed)) = parse_int_suffix(&name) {
                    return Ok(self.m.intern_ty(TyKind::Int { width, signed }));
                }
                if let Some(&s) = self.struct_ids.get(&name) {
                    return Ok(self.m.intern_ty(TyKind::Struct(s)));
                }
                if let Some(&s) = self.sum_ids.get(&name) {
                    return Ok(self.m.intern_ty(TyKind::Sum(s)));
                }
                Err(self.err(&format!("unknown type `{name}`")))
            }
        }
    }
}

/// Split a numeric literal into its numeric part and an optional type suffix.
fn split_num(s: &str) -> (&str, Option<String>) {
    // suffix starts at the first ascii-alphabetic char (a leading '-' and digits/'.'
    // precede it).
    match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (&s[..i], Some(s[i..].to_string())),
        None => (s, None),
    }
}
