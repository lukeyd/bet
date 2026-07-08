//! The recursive-descent parser: a token stream → [`ast::Program`].
//!
//! Covers `spec/grammar.ebnf` Part S: `pull`, the top-level declarations (`finna`, `drip`,
//! `moods`, `crib`, `facts`, `lowkey`, `extern`), the full statement set, the §E1 expression
//! precedence ladder (note the Go rule — bitwise `& ^ |` bind tighter than the comparisons),
//! and the §S3 type grammar. Statement boundaries are the `Semi` tokens produced by the
//! lexer's ASI pass; the parser is lenient about runs of them.

use crate::ast::*;
use crate::lexer::{Spanned, Token};

/// Parse a token stream into a [`Program`], or report the first syntax error.
pub fn parse(tokens: &[Spanned]) -> Result<Program, String> {
    let eof = tokens.last().map(|s| s.span.end).unwrap_or(0);
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        eof,
        no_struct: false,
    };
    p.program()
}

struct Parser<'a> {
    toks: &'a [Spanned],
    pos: usize,
    eof: u32,
    /// When set, `Name {` is NOT parsed as a struct literal (the `{` starts a block). Applies
    /// only at the top level of an `fr`/`vibin`/`squad`/`vibe`/`holla` header (§L6 rule 3);
    /// any nested `(...)`/`[...]`/`{...}` re-enables struct literals.
    no_struct: bool,
}

type PResult<T> = Result<T, String>;

impl<'a> Parser<'a> {
    // --- cursor primitives ---

    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos).map(|s| &s.tok)
    }

    fn peek2(&self) -> Option<&Token> {
        self.toks.get(self.pos + 1).map(|s| &s.tok)
    }

    fn check(&self, t: &Token) -> bool {
        self.peek() == Some(t)
    }

    /// Byte offset where the current token starts (or EOF).
    fn lo(&self) -> u32 {
        self.toks
            .get(self.pos)
            .map(|s| s.span.start)
            .unwrap_or(self.eof)
    }

    /// Byte offset where the previously consumed token ended.
    fn hi(&self) -> u32 {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].span.end
        }
    }

    fn span_from(&self, lo: u32) -> Span {
        Span::new(lo, self.hi())
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.check(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Token) -> PResult<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("expected {:?}, found {:?}", t, self.peek()))
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.toks.get(self.pos).map(|s| &s.tok) {
            Some(Token::Ident(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }

    /// Consume any run of statement terminators (`Semi`).
    fn skip_semis(&mut self) {
        while self.check(&Token::Semi) {
            self.pos += 1;
        }
    }

    // --- program & top-level ------------------------------------------------

    fn program(&mut self) -> PResult<Program> {
        let mut items = Vec::new();
        loop {
            self.skip_semis();
            if self.peek().is_none() {
                break;
            }
            items.push(self.top_item()?);
        }
        Ok(Program { items })
    }

    fn top_item(&mut self) -> PResult<Item> {
        if self.check(&Token::Pull) {
            return Ok(Item::Pull(self.pull()?));
        }
        if self.check(&Token::Extern) {
            return Ok(Item::Extern(self.extern_decl()?));
        }
        let vis = if self.eat(&Token::Flex) {
            Vis::Flex
        } else {
            Vis::Hush
        };
        match self.peek() {
            Some(Token::Finna) => Ok(Item::Func(self.fn_decl(vis)?)),
            Some(Token::Drip) => Ok(Item::Drip(self.drip_decl(vis)?)),
            Some(Token::Moods) => Ok(Item::Moods(self.moods_decl(vis)?)),
            Some(Token::Crib) => Ok(Item::Crib(self.crib_decl(vis)?)),
            Some(Token::Facts) => Ok(Item::Const(self.const_decl(vis)?)),
            Some(Token::Lowkey) => Ok(Item::Var(self.var_decl(vis)?)),
            other => Err(format!("unexpected top-level token {other:?}")),
        }
    }

    fn pull(&mut self) -> PResult<Pull> {
        let lo = self.lo();
        self.expect(&Token::Pull)?;
        let module = self.str_lit()?;
        let alias = if self.eat(&Token::As) {
            Some(self.ident()?)
        } else {
            None
        };
        Ok(Pull {
            module,
            alias,
            span: self.span_from(lo),
        })
    }

    fn extern_decl(&mut self) -> PResult<ExternDecl> {
        let lo = self.lo();
        self.expect(&Token::Extern)?;
        let abi = self.str_lit()?;
        self.expect(&Token::Finna)?;
        let name = self.ident()?;
        let params = self.param_list()?;
        let ret = self.opt_ret_single()?;
        Ok(ExternDecl {
            abi,
            name,
            params,
            ret,
            span: self.span_from(lo),
        })
    }

    fn fn_decl(&mut self, vis: Vis) -> PResult<FnDecl> {
        let lo = self.lo();
        self.expect(&Token::Finna)?;
        let receiver = if self.check(&Token::LParen) {
            Some(self.receiver()?)
        } else {
            None
        };
        let name = self.ident()?;
        let generics = self.generic_params()?;
        let params = self.param_list()?;
        let ret = self.opt_ret()?;
        let body = self.block()?;
        Ok(FnDecl {
            vis,
            receiver,
            name,
            generics,
            params,
            ret,
            body,
            span: self.span_from(lo),
        })
    }

    fn receiver(&mut self) -> PResult<Receiver> {
        let lo = self.lo();
        self.expect(&Token::LParen)?;
        let name = self.ident()?;
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        self.expect(&Token::RParen)?;
        Ok(Receiver {
            name,
            ty,
            span: self.span_from(lo),
        })
    }

    /// `[ "[" ident { "," ident } "]" ]` — a generic parameter list.
    fn generic_params(&mut self) -> PResult<Vec<String>> {
        let mut out = Vec::new();
        if self.eat(&Token::LBracket) {
            out.push(self.ident()?);
            while self.eat(&Token::Comma) {
                out.push(self.ident()?);
            }
            self.expect(&Token::RBracket)?;
        }
        Ok(out)
    }

    fn param_list(&mut self) -> PResult<Vec<Param>> {
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        if !self.check(&Token::RParen) {
            params.push(self.param()?);
            while self.eat(&Token::Comma) {
                params.push(self.param()?);
            }
        }
        self.expect(&Token::RParen)?;
        Ok(params)
    }

    fn param(&mut self) -> PResult<Param> {
        let lo = self.lo();
        let name = self.ident()?;
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        Ok(Param {
            name,
            ty,
            span: self.span_from(lo),
        })
    }

    /// `[ "->" retType ]` — a full return (single or parenthesized multi-value).
    fn opt_ret(&mut self) -> PResult<RetType> {
        if !self.eat(&Token::Arrow) {
            return Ok(RetType::None);
        }
        if self.eat(&Token::LParen) {
            let mut tys = vec![self.parse_type()?];
            while self.eat(&Token::Comma) {
                tys.push(self.parse_type()?);
            }
            self.expect(&Token::RParen)?;
            Ok(RetType::Multi(tys))
        } else {
            Ok(RetType::Single(self.parse_type()?))
        }
    }

    /// `[ "->" type ]` — an extern's single return.
    fn opt_ret_single(&mut self) -> PResult<RetType> {
        if self.eat(&Token::Arrow) {
            Ok(RetType::Single(self.parse_type()?))
        } else {
            Ok(RetType::None)
        }
    }

    fn drip_decl(&mut self, vis: Vis) -> PResult<DripDecl> {
        let lo = self.lo();
        self.expect(&Token::Drip)?;
        let name = self.ident()?;
        let generics = self.generic_params()?;
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_semis();
            if self.check(&Token::RBrace) || self.peek().is_none() {
                break;
            }
            fields.push(self.field_decl()?);
            self.skip_semis();
            self.eat(&Token::Comma);
        }
        self.expect(&Token::RBrace)?;
        Ok(DripDecl {
            vis,
            name,
            generics,
            fields,
            span: self.span_from(lo),
        })
    }

    fn field_decl(&mut self) -> PResult<FieldDecl> {
        let lo = self.lo();
        let vis = if self.eat(&Token::Flex) {
            Some(Vis::Flex)
        } else if self.eat(&Token::Hush) {
            Some(Vis::Hush)
        } else {
            None
        };
        let name = self.ident()?;
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        Ok(FieldDecl {
            vis,
            name,
            ty,
            span: self.span_from(lo),
        })
    }

    fn moods_decl(&mut self, vis: Vis) -> PResult<MoodsDecl> {
        let lo = self.lo();
        self.expect(&Token::Moods)?;
        let name = self.ident()?;
        let generics = self.generic_params()?;
        self.expect(&Token::LBrace)?;
        let mut variants = Vec::new();
        loop {
            self.skip_semis();
            if self.check(&Token::RBrace) || self.peek().is_none() {
                break;
            }
            variants.push(self.variant()?);
            self.skip_semis();
            self.eat(&Token::Comma);
        }
        self.expect(&Token::RBrace)?;
        Ok(MoodsDecl {
            vis,
            name,
            generics,
            variants,
            span: self.span_from(lo),
        })
    }

    fn variant(&mut self) -> PResult<Variant> {
        let lo = self.lo();
        let name = self.ident()?;
        let mut payload = Vec::new();
        if self.eat(&Token::LParen) {
            payload.push(self.parse_type()?);
            while self.eat(&Token::Comma) {
                payload.push(self.parse_type()?);
            }
            self.expect(&Token::RParen)?;
        }
        Ok(Variant {
            name,
            payload,
            span: self.span_from(lo),
        })
    }

    fn crib_decl(&mut self, vis: Vis) -> PResult<CribDecl> {
        let lo = self.lo();
        self.expect(&Token::Crib)?;
        let name = self.ident()?;
        let ty = if self.eat(&Token::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok(CribDecl {
            vis,
            name,
            ty,
            span: self.span_from(lo),
        })
    }

    fn const_decl(&mut self, vis: Vis) -> PResult<ConstDecl> {
        let lo = self.lo();
        self.expect(&Token::Facts)?;
        let name = self.ident()?;
        let ty = if self.eat(&Token::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        let value = self.expr()?;
        Ok(ConstDecl {
            vis,
            name,
            ty,
            value,
            span: self.span_from(lo),
        })
    }

    fn var_decl(&mut self, vis: Vis) -> PResult<VarDecl> {
        let lo = self.lo();
        self.expect(&Token::Lowkey)?;
        let mut targets = vec![self.ident()?];
        while self.eat(&Token::Comma) {
            targets.push(self.ident()?);
        }
        let ty = if self.eat(&Token::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        let values = self.expr_list()?;
        Ok(VarDecl {
            vis,
            targets,
            ty,
            values,
            span: self.span_from(lo),
        })
    }

    // --- types --------------------------------------------------------------

    fn parse_type(&mut self) -> PResult<Type> {
        let lo = self.lo();
        // `[]T` — slice.
        if self.check(&Token::LBracket) {
            self.pos += 1;
            self.expect(&Token::RBracket)?;
            let inner = self.parse_type()?;
            return Ok(Type {
                kind: TypeKind::Slice(Box::new(inner)),
                span: self.span_from(lo),
            });
        }
        if self.eat(&Token::Tag) {
            let inner = self.parse_type()?;
            return Ok(Type {
                kind: TypeKind::Tag(Box::new(inner)),
                span: self.span_from(lo),
            });
        }
        if self.eat(&Token::Crib) {
            let inner = self.parse_type()?;
            return Ok(Type {
                kind: TypeKind::Crib(Box::new(inner)),
                span: self.span_from(lo),
            });
        }
        // `soa C` — struct-of-arrays layout. The inner `parse_type` picks up the container
        // form: `soa []Enemy` (slice), `soa Enemy[N]` (fixed array), `soa vec[Enemy]` (vec).
        if self.eat(&Token::Soa) {
            let inner = self.parse_type()?;
            return Ok(Type {
                kind: TypeKind::Soa(Box::new(inner)),
                span: self.span_from(lo),
            });
        }
        if self.eat(&Token::Finna) {
            // `finna ( typeList? ) -> type`.
            self.expect(&Token::LParen)?;
            let mut params = Vec::new();
            if !self.check(&Token::RParen) {
                params.push(self.parse_type()?);
                while self.eat(&Token::Comma) {
                    params.push(self.parse_type()?);
                }
            }
            self.expect(&Token::RParen)?;
            self.expect(&Token::Arrow)?;
            let ret = self.parse_type()?;
            return Ok(Type {
                kind: TypeKind::Fn(params, Box::new(ret)),
                span: self.span_from(lo),
            });
        }
        // A named type (`int`, `stash[str, i64]`, `Enemy`) or `rawptr`. A namespace-qualified
        // type `geometry.Point` is kept as the single dotted name `"geometry.Point"` (the module
        // resolver splits on the dot); `rawptr` is never qualified.
        let mut name = self.ident()?;
        if name != "rawptr" && self.eat(&Token::Dot) {
            let member = self.ident()?;
            name = format!("{name}.{member}");
        }
        let mut ty = if name == "rawptr" {
            Type {
                kind: TypeKind::RawPtr,
                span: self.span_from(lo),
            }
        } else {
            let mut generics = Vec::new();
            // `[` starting a generic arg list (not a fixed-array size, which is `[intLit]`).
            if self.check(&Token::LBracket) && !self.next_is_int() {
                self.pos += 1;
                generics.push(self.parse_type()?);
                while self.eat(&Token::Comma) {
                    generics.push(self.parse_type()?);
                }
                self.expect(&Token::RBracket)?;
            }
            Type {
                kind: TypeKind::Named(name, generics),
                span: self.span_from(lo),
            }
        };
        // Trailing fixed-array dimensions: `Enemy[1000]`.
        while self.check(&Token::LBracket) && self.next_is_int() {
            self.pos += 1;
            let size = self.int_lit()? as u64;
            self.expect(&Token::RBracket)?;
            ty = Type {
                kind: TypeKind::Array(Box::new(ty), size),
                span: self.span_from(lo),
            };
        }
        Ok(ty)
    }

    fn next_is_int(&self) -> bool {
        matches!(self.peek2(), Some(Token::Int(_)))
    }

    // --- blocks & statements ------------------------------------------------

    fn block(&mut self) -> PResult<Block> {
        let lo = self.lo();
        self.expect(&Token::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            self.skip_semis();
            if self.check(&Token::RBrace) || self.peek().is_none() {
                break;
            }
            stmts.push(self.stmt()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(Block {
            stmts,
            span: self.span_from(lo),
        })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let lo = self.lo();
        let kind = match self.peek() {
            Some(Token::Lowkey) => StmtKind::Var(self.var_decl(Vis::Hush)?),
            Some(Token::Facts) => StmtKind::Const(self.const_decl(Vis::Hush)?),
            Some(Token::Crib) => StmtKind::Crib(self.crib_decl(Vis::Hush)?),
            Some(Token::Fr) => self.fr_stmt()?,
            Some(Token::Vibin) => self.vibin_stmt()?,
            Some(Token::Squad) => self.squad_stmt()?,
            Some(Token::Vibe) => self.vibe_stmt()?,
            Some(Token::Holla) => self.holla_stmt()?,
            Some(Token::Sheesh) => self.sheesh_stmt()?,
            Some(Token::Evict) => {
                self.pos += 1;
                // `evict <crib>` frees the whole crib; `evict <tag> in <crib>` frees one slot.
                // Same shape as `holla`'s `<tag> in <crib>`: expression parsing stops at `in`
                // (only the `cop`/`.trust()` forms embed one, and neither is an evict operand).
                let first = self.expr()?;
                if self.eat(&Token::In) {
                    StmtKind::Evict {
                        crib: self.expr()?,
                        tag: Some(first),
                    }
                } else {
                    StmtKind::Evict {
                        crib: first,
                        tag: None,
                    }
                }
            }
            Some(Token::Slide) => {
                self.pos += 1;
                StmtKind::Slide(self.expr()?)
            }
            Some(Token::Bet) => {
                self.pos += 1;
                let values = if self.stmt_ends() {
                    Vec::new()
                } else {
                    self.expr_list()?
                };
                StmtKind::Bet(values)
            }
            Some(Token::Bounce) => {
                self.pos += 1;
                StmtKind::Bounce(self.expr()?)
            }
            Some(Token::Yeet) => {
                self.pos += 1;
                self.expect(&Token::LParen)?;
                let e = self.expr()?;
                self.expect(&Token::RParen)?;
                StmtKind::Yeet(e)
            }
            Some(Token::Dip) => {
                self.pos += 1;
                StmtKind::Dip
            }
            Some(Token::Skip) => {
                self.pos += 1;
                StmtKind::Skip
            }
            _ => self.expr_or_assign()?,
        };
        Ok(Stmt {
            kind,
            span: self.span_from(lo),
        })
    }

    /// True when the current position is a natural end of statement (terminator / `}` / EOF).
    fn stmt_ends(&self) -> bool {
        matches!(self.peek(), None | Some(Token::Semi) | Some(Token::RBrace))
    }

    fn fr_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Fr)?;
        let cond = self.expr_header()?;
        let then = self.block()?;
        let mut elifs = Vec::new();
        let mut els = None;
        while self.eat(&Token::Naw) {
            if self.eat(&Token::Fr) {
                let c = self.expr_header()?;
                let b = self.block()?;
                elifs.push((c, b));
            } else {
                els = Some(self.block()?);
                break;
            }
        }
        Ok(StmtKind::Fr(FrStmt {
            cond,
            then,
            elifs,
            els,
        }))
    }

    fn vibin_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Vibin)?;
        let cond = self.expr_header()?;
        let body = self.block()?;
        Ok(StmtKind::Vibin { cond, body })
    }

    fn squad_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Squad)?;
        let var = self.ident()?;
        self.expect(&Token::In)?;
        let iter = self.expr_header()?;
        let body = self.block()?;
        Ok(StmtKind::Squad { var, iter, body })
    }

    fn vibe_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Vibe)?;
        let scrutinee = self.expr_header()?;
        self.expect(&Token::LBrace)?;
        let mut arms = Vec::new();
        let mut default = None;
        loop {
            self.skip_semis();
            if self.check(&Token::RBrace) || self.peek().is_none() {
                break;
            }
            if self.eat(&Token::Naw) {
                default = Some(self.block()?);
                self.skip_semis();
                break;
            }
            arms.push(self.match_arm()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(StmtKind::Vibe {
            scrutinee,
            arms,
            default,
        })
    }

    fn match_arm(&mut self) -> PResult<MatchArm> {
        let lo = self.lo();
        // A variant pattern, possibly namespace-qualified: `geometry.Circle(r)`. The dotted head
        // is kept as one string; the module resolver splits on the dot.
        let mut variant = self.ident()?;
        if self.eat(&Token::Dot) {
            let member = self.ident()?;
            variant = format!("{variant}.{member}");
        }
        let mut bindings = Vec::new();
        if self.eat(&Token::LParen) {
            bindings.push(self.ident()?);
            while self.eat(&Token::Comma) {
                bindings.push(self.ident()?);
            }
            self.expect(&Token::RParen)?;
        }
        let body = self.block()?;
        Ok(MatchArm {
            variant,
            bindings,
            body,
            span: self.span_from(lo),
        })
    }

    fn holla_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Holla)?;
        let binding = self.ident()?;
        self.expect(&Token::Eq)?;
        let tag = self.expr()?;
        self.expect(&Token::In)?;
        let crib = self.expr_header()?;
        let live = self.block()?;
        self.expect(&Token::Ghosted)?;
        let ghosted = self.block()?;
        Ok(StmtKind::Holla {
            binding,
            tag,
            crib,
            live,
            ghosted,
        })
    }

    fn sheesh_stmt(&mut self) -> PResult<StmtKind> {
        self.expect(&Token::Sheesh)?;
        let body = self.block()?;
        let recover = if self.eat(&Token::Naw) {
            let name = self.ident()?;
            let b = self.block()?;
            Some((name, b))
        } else {
            None
        };
        Ok(StmtKind::Sheesh { body, recover })
    }

    /// An expression statement, or an assignment (`lvalue, ... op exprList`).
    fn expr_or_assign(&mut self) -> PResult<StmtKind> {
        let first = self.expr()?;
        if self.check(&Token::Comma) || self.assign_op().is_some() {
            let mut targets = vec![first];
            while self.eat(&Token::Comma) {
                targets.push(self.expr()?);
            }
            let op = self.assign_op().ok_or_else(|| {
                format!("expected an assignment operator, found {:?}", self.peek())
            })?;
            self.pos += 1;
            let values = self.expr_list()?;
            Ok(StmtKind::Assign {
                targets,
                op,
                values,
            })
        } else {
            Ok(StmtKind::Expr(first))
        }
    }

    fn assign_op(&self) -> Option<AssignOp> {
        Some(match self.peek()? {
            Token::Eq => AssignOp::Eq,
            Token::PlusEq => AssignOp::AddEq,
            Token::MinusEq => AssignOp::SubEq,
            Token::StarEq => AssignOp::MulEq,
            Token::SlashEq => AssignOp::DivEq,
            Token::PercentEq => AssignOp::RemEq,
            Token::AmpEq => AssignOp::AndEq,
            Token::PipeEq => AssignOp::OrEq,
            Token::CaretEq => AssignOp::XorEq,
            Token::ShlEq => AssignOp::ShlEq,
            Token::ShrEq => AssignOp::ShrEq,
            _ => return None,
        })
    }

    // --- expressions (§E1 precedence ladder) --------------------------------

    /// Parse a header expression (`fr`/`vibin`/`squad`/`vibe`/`holla`-crib), with
    /// unparenthesized struct literals disabled so a trailing `{` reads as a block.
    fn expr_header(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut out = vec![self.expr()?];
        while self.eat(&Token::Comma) {
            out.push(self.expr()?);
        }
        Ok(out)
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat(&Token::OrOr) {
            let rhs = self.and_expr()?;
            lhs = Self::binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.cmp_expr()?;
        while self.eat(&Token::AndAnd) {
            let rhs = self.cmp_expr()?;
            lhs = Self::binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitor_expr()?;
        loop {
            let op = match self.peek() {
                Some(Token::EqEq) => BinOp::Eq,
                Some(Token::Ne) => BinOp::Ne,
                Some(Token::Lt) => BinOp::Lt,
                Some(Token::Le) => BinOp::Le,
                Some(Token::Gt) => BinOp::Gt,
                Some(Token::Ge) => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.bitor_expr()?;
            lhs = Self::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bitor_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitxor_expr()?;
        while self.eat(&Token::Pipe) {
            let rhs = self.bitxor_expr()?;
            lhs = Self::binary(BinOp::BitOr, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bitxor_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitand_expr()?;
        while self.eat(&Token::Caret) {
            let rhs = self.bitand_expr()?;
            lhs = Self::binary(BinOp::BitXor, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bitand_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.shift_expr()?;
        while self.eat(&Token::Amp) {
            let rhs = self.shift_expr()?;
            lhs = Self::binary(BinOp::BitAnd, lhs, rhs);
        }
        Ok(lhs)
    }

    fn shift_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.add_expr()?;
        loop {
            let op = match self.peek() {
                Some(Token::Shl) => BinOp::Shl,
                Some(Token::Shr) => BinOp::Shr,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.add_expr()?;
            lhs = Self::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinOp::Add,
                Some(Token::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.mul_expr()?;
            lhs = Self::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.cast_expr()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinOp::Mul,
                Some(Token::Slash) => BinOp::Div,
                Some(Token::Percent) => BinOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.cast_expr()?;
            lhs = Self::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn cast_expr(&mut self) -> PResult<Expr> {
        let mut e = self.unary_expr()?;
        while self.eat(&Token::As) {
            let ty = self.parse_type()?;
            let span = Span::new(e.span.start, self.hi());
            e = Expr {
                kind: ExprKind::Cast(Box::new(e), ty),
                span,
            };
        }
        Ok(e)
    }

    fn unary_expr(&mut self) -> PResult<Expr> {
        let lo = self.lo();
        let op = match self.peek() {
            Some(Token::Bang) => UnOp::Not,
            Some(Token::Tilde) => UnOp::BitNot,
            Some(Token::Minus) => UnOp::Neg,
            _ => return self.postfix_expr(),
        };
        self.pos += 1;
        let operand = self.unary_expr()?;
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(operand)),
            span: self.span_from(lo),
        })
    }

    fn postfix_expr(&mut self) -> PResult<Expr> {
        let lo = self.lo();
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Token::Dot) => {
                    self.pos += 1;
                    if self.eat(&Token::Trust) {
                        // `.trust() in primary`
                        self.expect(&Token::LParen)?;
                        self.expect(&Token::RParen)?;
                        self.expect(&Token::In)?;
                        let crib = self.primary()?;
                        e = Expr {
                            kind: ExprKind::Trust {
                                tag: Box::new(e),
                                crib: Box::new(crib),
                            },
                            span: self.span_from(lo),
                        };
                    } else {
                        let name = self.ident()?;
                        let generics = self.maybe_generic_args()?;
                        if self.check(&Token::LParen) {
                            let args = self.call_args()?;
                            e = Expr {
                                kind: ExprKind::Method {
                                    receiver: Box::new(e),
                                    method: name,
                                    generics,
                                    args,
                                },
                                span: self.span_from(lo),
                            };
                        } else if !self.no_struct
                            && self.check(&Token::LBrace)
                            && matches!(&e.kind, ExprKind::Name { generics, .. } if generics.is_empty())
                        {
                            // A namespace-qualified struct literal: `geometry.Point{ ... }`. The
                            // base is a bare namespace name; keep the dotted head as one string
                            // (the module resolver splits on the dot).
                            let ns = match e.kind {
                                ExprKind::Name { name, .. } => name,
                                _ => unreachable!(),
                            };
                            let sl = self.struct_lit_body(format!("{ns}.{name}"), generics, lo)?;
                            e = Expr {
                                kind: ExprKind::Struct(sl),
                                span: self.span_from(lo),
                            };
                        } else {
                            e = Expr {
                                kind: ExprKind::Field {
                                    base: Box::new(e),
                                    name,
                                    generics,
                                },
                                span: self.span_from(lo),
                            };
                        }
                    }
                }
                Some(Token::LParen) => {
                    let args = self.call_args()?;
                    e = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                        span: self.span_from(lo),
                    };
                }
                Some(Token::LBracket) => {
                    self.pos += 1;
                    let index = self.full_expr()?;
                    self.expect(&Token::RBracket)?;
                    e = Expr {
                        kind: ExprKind::Index {
                            base: Box::new(e),
                            index: Box::new(index),
                        },
                        span: self.span_from(lo),
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let lo = self.lo();
        let kind = match self.peek() {
            Some(Token::Int(v)) => {
                let v = *v;
                self.pos += 1;
                ExprKind::Int(v)
            }
            Some(Token::Float(v)) => {
                let v = *v;
                self.pos += 1;
                ExprKind::Float(v)
            }
            Some(Token::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                ExprKind::Str(s)
            }
            Some(Token::Byte(b)) => {
                let b = *b;
                self.pos += 1;
                ExprKind::Byte(b)
            }
            Some(Token::Nocap) => {
                self.pos += 1;
                ExprKind::Bool(true)
            }
            Some(Token::Cap) => {
                self.pos += 1;
                ExprKind::Bool(false)
            }
            Some(Token::Ghosted) => {
                self.pos += 1;
                ExprKind::Ghosted
            }
            Some(Token::Cop) => return self.cop_expr(),
            Some(Token::LParen) => {
                self.pos += 1;
                let mut inner = self.full_expr()?;
                self.expect(&Token::RParen)?;
                inner.span = self.span_from(lo);
                return Ok(inner);
            }
            Some(Token::LBracket) => return self.array_lit(),
            Some(Token::Ident(_)) => {
                let name = self.ident()?;
                let generics = self.maybe_generic_args()?;
                if !self.no_struct && self.check(&Token::LBrace) {
                    let sl = self.struct_lit_body(name, generics, lo)?;
                    ExprKind::Struct(sl)
                } else {
                    ExprKind::Name { name, generics }
                }
            }
            other => return Err(format!("unexpected token in expression: {other:?}")),
        };
        Ok(Expr {
            kind,
            span: self.span_from(lo),
        })
    }

    /// Parse a sub-expression with struct literals RE-enabled (inside brackets).
    fn full_expr(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = false;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    /// `[ "[" typeList "]" ]` after a name — generic instantiation. Only consumed when the
    /// bracketed list is followed by `(` (a call) or, where struct literals are allowed, `{`.
    fn maybe_generic_args(&mut self) -> PResult<Vec<Type>> {
        if !self.check(&Token::LBracket) {
            return Ok(Vec::new());
        }
        let looks_generic = match self.peek_after_matching_bracket() {
            Some(Token::LParen) => true,
            Some(Token::LBrace) => !self.no_struct,
            _ => false,
        };
        if !looks_generic {
            return Ok(Vec::new());
        }
        self.pos += 1;
        let mut tys = vec![self.parse_type()?];
        while self.eat(&Token::Comma) {
            tys.push(self.parse_type()?);
        }
        self.expect(&Token::RBracket)?;
        Ok(tys)
    }

    /// The token immediately following the bracket group that starts at the current `[`
    /// (balanced across all bracket kinds). Used to disambiguate generics from indexing.
    fn peek_after_matching_bracket(&self) -> Option<&Token> {
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Token::LBracket | Token::LParen | Token::LBrace => depth += 1,
                Token::RBracket | Token::RParen | Token::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return self.toks.get(i + 1).map(|s| &s.tok);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn struct_lit_body(
        &mut self,
        name: String,
        generics: Vec<Type>,
        lo: u32,
    ) -> PResult<StructLit> {
        let saved = self.no_struct;
        self.no_struct = false;
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_semis();
            if self.check(&Token::RBrace) || self.peek().is_none() {
                break;
            }
            let flo = self.lo();
            let fname = self.ident()?;
            self.expect(&Token::Colon)?;
            let value = self.expr()?;
            fields.push(FieldInit {
                name: fname,
                value,
                span: self.span_from(flo),
            });
            self.skip_semis();
            if !self.eat(&Token::Comma) {
                self.skip_semis();
            }
        }
        self.expect(&Token::RBrace)?;
        self.no_struct = saved;
        Ok(StructLit {
            name,
            generics,
            fields,
            span: self.span_from(lo),
        })
    }

    fn array_lit(&mut self) -> PResult<Expr> {
        let lo = self.lo();
        let saved = self.no_struct;
        self.no_struct = false;
        self.expect(&Token::LBracket)?;
        let mut elems = Vec::new();
        if !self.check(&Token::RBracket) {
            elems.push(self.expr()?);
            while self.eat(&Token::Comma) {
                if self.check(&Token::RBracket) {
                    break;
                }
                elems.push(self.expr()?);
            }
        }
        self.expect(&Token::RBracket)?;
        self.no_struct = saved;
        Ok(Expr {
            kind: ExprKind::Array(elems),
            span: self.span_from(lo),
        })
    }

    fn cop_expr(&mut self) -> PResult<Expr> {
        let lo = self.lo();
        self.expect(&Token::Cop)?;
        let init = self.cop_init()?;
        self.expect(&Token::In)?;
        let crib = self.postfix_expr()?;
        Ok(Expr {
            kind: ExprKind::Cop {
                init: Box::new(init),
                crib: Box::new(crib),
            },
            span: self.span_from(lo),
        })
    }

    fn cop_init(&mut self) -> PResult<CopInit> {
        let lo = self.lo();
        let mut name = self.ident()?;
        // A namespace-qualified head (`cop state.GameState{..}` / `cop shapes.Circle(..)`)
        // keeps the dotted name as one string, exactly like a qualified struct literal or
        // type — the module resolver splits on the dot.
        if self.check(&Token::Dot) && matches!(self.peek2(), Some(Token::Ident(_))) {
            self.pos += 1;
            let member = self.ident()?;
            name = format!("{name}.{member}");
        }
        let generics = self.maybe_generic_args()?;
        if self.check(&Token::LBrace) {
            let sl = self.struct_lit_body(name, generics, lo)?;
            Ok(CopInit::Struct(sl))
        } else if self.check(&Token::LParen) {
            let args = self.call_args()?;
            Ok(CopInit::Variant { name, args })
        } else {
            Ok(CopInit::Variant {
                name,
                args: Vec::new(),
            })
        }
    }

    fn call_args(&mut self) -> PResult<Vec<Arg>> {
        let saved = self.no_struct;
        self.no_struct = false;
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        if !self.check(&Token::RParen) {
            args.push(self.arg()?);
            while self.eat(&Token::Comma) {
                if self.check(&Token::RParen) {
                    break;
                }
                args.push(self.arg()?);
            }
        }
        self.expect(&Token::RParen)?;
        self.no_struct = saved;
        Ok(args)
    }

    fn arg(&mut self) -> PResult<Arg> {
        // Optional `label:` — an identifier immediately followed by `:`. The `in` keyword is
        // also accepted as a label so the allocator-context override `stash.new(in: crib)`
        // (SP0.1) parses; `in` never lexes as an `Ident`, so it needs its own arm.
        let label = if matches!(self.peek(), Some(Token::Ident(_)))
            && matches!(self.peek2(), Some(Token::Colon))
        {
            let l = self.ident()?;
            self.pos += 1; // the `:`
            Some(l)
        } else if matches!(self.peek(), Some(Token::In))
            && matches!(self.peek2(), Some(Token::Colon))
        {
            self.expect(&Token::In)?;
            self.expect(&Token::Colon)?;
            Some("in".to_string())
        } else {
            None
        };
        let value = self.expr()?;
        Ok(Arg { label, value })
    }

    // --- small literal helpers ---

    fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        let span = Span::new(lhs.span.start, rhs.span.end);
        Expr {
            kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
            span,
        }
    }

    fn str_lit(&mut self) -> PResult<String> {
        match self.toks.get(self.pos).map(|s| &s.tok) {
            Some(Token::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            other => Err(format!("expected a string literal, found {other:?}")),
        }
    }

    fn int_lit(&mut self) -> PResult<i128> {
        match self.toks.get(self.pos).map(|s| &s.tok) {
            Some(Token::Int(v)) => {
                let v = *v;
                self.pos += 1;
                Ok(v)
            }
            other => Err(format!("expected an integer literal, found {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_src(src: &str) -> Program {
        let toks = tokenize(src).expect("lex");
        parse(&toks).expect("parse")
    }

    /// The Go rule: `&` binds tighter than `==`, so `flags & M == 0` is `(flags & M) == 0`.
    #[test]
    fn bitwise_binds_tighter_than_comparison() {
        let p = parse_src("finna main() {\n  bet flags & M == 0\n}\n");
        let Item::Func(f) = &p.items[0] else { panic!() };
        let StmtKind::Bet(vals) = &f.body.stmts[0].kind else {
            panic!()
        };
        // Top operator must be `==`, with a `&` on its left.
        match &vals[0].kind {
            ExprKind::Binary(BinOp::Eq, lhs, _) => {
                assert!(matches!(lhs.kind, ExprKind::Binary(BinOp::BitAnd, _, _)));
            }
            other => panic!("expected top-level ==, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic_precedence() {
        // `1 + 2 * 3` → `1 + (2 * 3)`.
        let p = parse_src("finna main() {\n  bet 1 + 2 * 3\n}\n");
        let Item::Func(f) = &p.items[0] else { panic!() };
        let StmtKind::Bet(vals) = &f.body.stmts[0].kind else {
            panic!()
        };
        match &vals[0].kind {
            ExprKind::Binary(BinOp::Add, _, rhs) => {
                assert!(matches!(rhs.kind, ExprKind::Binary(BinOp::Mul, _, _)));
            }
            other => panic!("expected top-level +, got {other:?}"),
        }
    }

    #[test]
    fn parses_multi_return_and_bind() {
        let p = parse_src(
            "finna divmod(a: int, b: int) -> (int, int) {\n  bet a / b, a % b\n}\nfinna main() {\n  lowkey q, r = divmod(17, 5)\n}\n",
        );
        assert_eq!(p.items.len(), 2);
        let Item::Func(dm) = &p.items[0] else {
            panic!()
        };
        assert!(matches!(dm.ret, RetType::Multi(_)));
        let Item::Func(m) = &p.items[1] else { panic!() };
        let StmtKind::Var(v) = &m.body.stmts[0].kind else {
            panic!()
        };
        assert_eq!(v.targets, vec!["q".to_string(), "r".to_string()]);
    }

    #[test]
    fn parses_fr_naw_chain() {
        let p = parse_src(
            "finna f(n: int) -> str {\n  fr n < 0 { bet \"neg\" } naw fr n == 0 { bet \"zero\" } naw { bet \"pos\" }\n}\n",
        );
        let Item::Func(f) = &p.items[0] else { panic!() };
        let StmtKind::Fr(fr) = &f.body.stmts[0].kind else {
            panic!()
        };
        assert_eq!(fr.elifs.len(), 1);
        assert!(fr.els.is_some());
    }

    #[test]
    fn header_brace_is_block_not_struct() {
        // `fr Player { ... }` header: the `{` starts the block, not a struct literal.
        let p = parse_src("finna main() {\n  fr ok { skip }\n}\n");
        let Item::Func(f) = &p.items[0] else { panic!() };
        assert!(matches!(f.body.stmts[0].kind, StmtKind::Fr(_)));
    }
}
