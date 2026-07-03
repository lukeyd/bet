//! A tiny recursive-descent parser + AST. Tracer-bullet scope: a program is a sequence of
//! `pull` imports and zero-argument `finna` functions whose bodies are `spill.it("…")`
//! print statements. Everything else in `spec/grammar.ebnf` is a clean "not yet" error.

use crate::lexer::Token;

/// A parsed program: its functions (imports are recognized and discarded for now).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub funcs: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `spill.it("text")` — print the literal (a trailing newline is added when lowering).
    Print(String),
}

pub fn parse(tokens: &[Token]) -> Result<Program, String> {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
    };
    p.program()
}

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Token) -> Result<(), String> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("expected {t:?}, found {:?}", self.peek()))
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.bump() {
            Some(Token::Ident(s)) => Ok(s),
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }

    fn program(&mut self) -> Result<Program, String> {
        let mut funcs = Vec::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::Pull => {
                    self.bump();
                    match self.bump() {
                        Some(Token::Str(_)) => {} // import recognized; resolution is a no-op for now
                        other => {
                            return Err(format!(
                                "expected a module-name string after `pull`, found {other:?}"
                            ));
                        }
                    }
                }
                Token::Finna => funcs.push(self.function()?),
                other => return Err(format!("unexpected top-level token {other:?}")),
            }
        }
        Ok(Program { funcs })
    }

    fn function(&mut self) -> Result<Function, String> {
        self.expect(&Token::Finna)?;
        let name = self.ident()?;
        self.expect(&Token::LParen)?;
        // Tracer-bullet functions take no parameters.
        self.expect(&Token::RParen)?;
        self.expect(&Token::LBrace)?;
        let mut body = Vec::new();
        while !self.eat(&Token::RBrace) {
            if self.peek().is_none() {
                return Err("unexpected end of input inside a function body".into());
            }
            body.push(self.stmt()?);
        }
        Ok(Function { name, body })
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        // Only `spill.it("literal")`.
        let noun = self.ident()?;
        if noun != "spill" {
            return Err(format!(
                "only `spill.it(\"…\")` statements are supported yet, found `{noun}`"
            ));
        }
        self.expect(&Token::Dot)?;
        let method = self.ident()?;
        if method != "it" {
            return Err(format!(
                "only `spill.it` is supported yet, found `spill.{method}`"
            ));
        }
        self.expect(&Token::LParen)?;
        let text = match self.bump() {
            Some(Token::Str(s)) => s,
            other => {
                return Err(format!(
                    "`spill.it` takes a string literal in this frontend, found {other:?}"
                ));
            }
        };
        self.expect(&Token::RParen)?;
        Ok(Stmt::Print(text))
    }
}
