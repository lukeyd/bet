//! The lexer. Tracer-bullet scope: just the tokens `hello.bet` needs (`pull`, `finna`,
//! identifiers, string literals, and the bracket/`.` punctuation). Newlines are treated as
//! whitespace here; the full Go-style ASI rules (`spec/grammar.ebnf` §L6) arrive with the
//! real frontend, along with the rest of the token set.

use logos::Logos;

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")] // whitespace, including newlines (no ASI yet)
#[logos(skip r"//[^\n]*")] // line comments
#[logos(skip r"/\*([^*]|\*[^/])*\*/")] // non-nesting block comments
pub enum Token {
    #[token("pull")]
    Pull,
    #[token("finna")]
    Finna,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(".")]
    Dot,
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*", |lex| lex.slice().to_string())]
    Ident(String),
    #[regex(r#""([^"\\]|\\.)*""#, |lex| unescape(lex.slice()))]
    Str(String),
}

/// Tokenize a whole source string, or fail at the first unrecognized input.
pub fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut lex = Token::lexer(src);
    while let Some(res) = lex.next() {
        match res {
            Ok(tok) => out.push(tok),
            Err(()) => {
                return Err(format!(
                    "unexpected character(s) `{}` at byte {}",
                    lex.slice(),
                    lex.span().start
                ));
            }
        }
    }
    Ok(out)
}

/// Decode a quoted string literal's escapes (the surrounding quotes are still attached).
/// Matches the escape set in `spec/grammar.ebnf` §L3; unknown escapes pass their character
/// through (lenient — the full frontend rejects them).
fn unescape(raw: &str) -> String {
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}
