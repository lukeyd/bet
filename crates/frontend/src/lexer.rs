//! The lexer: source text → a stream of [`Spanned`] tokens.
//!
//! Covers the whole of `spec/grammar.ebnf` Part L: every L2 keyword, the numeric tower
//! (dec/hex/bin ints, floats), string + byte literals with escapes, all L4 operators and
//! compound-assignment forms, and both comment forms. After the raw `logos` scan, an
//! [`apply_asi`] pass implements the L6 Go-style automatic statement termination (ASI): it
//! turns statement-ending newlines into [`Token::Semi`] separators and drops the rest.

use crate::ast::Span;
use logos::Logos;

/// A token together with its byte span in the source.
#[derive(Clone, PartialEq, Debug)]
pub struct Spanned {
    pub tok: Token,
    pub span: Span,
}

/// A lexical token. `Newline` is produced by the raw scan and consumed by ASI; the parser
/// only ever sees `Semi` (never `Newline`).
#[derive(Logos, Clone, PartialEq, Debug)]
#[logos(skip r"[ \t]+")] // spaces / tabs (newlines are significant — see ASI)
#[logos(skip r"//[^\n]*")] // line comments (the trailing newline survives for ASI)
#[logos(skip r"/\*([^*]|\*[^/])*\*/")] // non-nesting block comments
pub enum Token {
    // --- statement termination ---
    #[regex(r"\r?\n")]
    Newline,
    #[token(";")]
    Semi,

    // --- L2 keywords ---
    #[token("finna")]
    Finna,
    #[token("bet")]
    Bet,
    #[token("lowkey")]
    Lowkey,
    #[token("facts")]
    Facts,
    #[token("drip")]
    Drip,
    #[token("moods")]
    Moods,
    #[token("pull")]
    Pull,
    #[token("extern")]
    Extern,
    #[token("fr")]
    Fr,
    #[token("naw")]
    Naw,
    #[token("vibin")]
    Vibin,
    #[token("squad")]
    Squad,
    #[token("dip")]
    Dip,
    #[token("skip")]
    Skip,
    #[token("vibe")]
    Vibe,
    #[token("slide")]
    Slide,
    #[token("yeet")]
    Yeet,
    #[token("sheesh")]
    Sheesh,
    #[token("bounce")]
    Bounce,
    #[token("nocap")]
    Nocap,
    #[token("cap")]
    Cap,
    #[token("ghosted")]
    Ghosted,
    #[token("flex")]
    Flex,
    #[token("hush")]
    Hush,
    #[token("crib")]
    Crib,
    #[token("soa")]
    Soa,
    #[token("cop")]
    Cop,
    #[token("evict")]
    Evict,
    #[token("tag")]
    Tag,
    #[token("holla")]
    Holla,
    #[token("trust")]
    Trust,
    #[token("in")]
    In,
    #[token("as")]
    As,

    // --- identifiers ---
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*", |lex| lex.slice().to_string())]
    Ident(String),

    // --- numeric tower (L3). Floats first so `3.0` beats `3` + `.` + `0`. ---
    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?", |lex| parse_float(lex.slice()))]
    #[regex(r"[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*", |lex| parse_float(lex.slice()))]
    Float(f64),
    #[regex(r"0x[0-9a-fA-F][0-9a-fA-F_]*", |lex| parse_radix(lex.slice(), 16))]
    #[regex(r"0b[01][01_]*", |lex| parse_radix(lex.slice(), 2))]
    #[regex(r"[0-9][0-9_]*", |lex| parse_dec(lex.slice()))]
    Int(i128),

    // --- string / byte literals (L3) ---
    #[regex(r#""([^"\\\n]|\\.)*""#, |lex| unescape_str(lex.slice()))]
    Str(String),
    #[regex(r#"'([^'\\\n]|\\.)'"#, |lex| unescape_byte(lex.slice()))]
    Byte(u8),

    // --- L4 operators & punctuation (longest-match wins, so `<<=` beats `<<` beats `<`) ---
    #[token("->")]
    Arrow,
    #[token("==")]
    EqEq,
    #[token("!=")]
    Ne,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("<<=")]
    ShlEq,
    #[token(">>=")]
    ShrEq,
    #[token("<<")]
    Shl,
    #[token(">>")]
    Shr,
    #[token("&&")]
    AndAnd,
    #[token("||")]
    OrOr,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("&=")]
    AmpEq,
    #[token("|=")]
    PipeEq,
    #[token("^=")]
    CaretEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("=")]
    Eq,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("&")]
    Amp,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("~")]
    Tilde,
    #[token("!")]
    Bang,
    #[token(".")]
    Dot,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
}

impl Token {
    /// Whether a newline immediately after this token TERMINATES the current statement
    /// (`spec/grammar.ebnf` §L6): an identifier, a literal, a closing bracket, or one of the
    /// statement keywords `bet` / `dip` / `skip` / `bounce`.
    fn ends_statement(&self) -> bool {
        matches!(
            self,
            Token::Ident(_)
                | Token::Int(_)
                | Token::Float(_)
                | Token::Str(_)
                | Token::Byte(_)
                | Token::Nocap
                | Token::Cap
                | Token::Ghosted
                | Token::RParen
                | Token::RBracket
                | Token::RBrace
                | Token::Bet
                | Token::Dip
                | Token::Skip
                | Token::Bounce
        )
    }
}

/// Tokenize a whole source string, or fail at the first unrecognized input.
pub fn tokenize(src: &str) -> Result<Vec<Spanned>, String> {
    let mut raw = Vec::new();
    let mut lex = Token::lexer(src);
    while let Some(res) = lex.next() {
        let span = lex.span();
        match res {
            Ok(tok) => raw.push(Spanned {
                tok,
                span: Span::new(span.start as u32, span.end as u32),
            }),
            Err(()) => {
                return Err(format!(
                    "unexpected character(s) `{}` at byte {}",
                    lex.slice(),
                    span.start
                ));
            }
        }
    }
    Ok(apply_asi(raw))
}

/// The L6 ASI pass: collapse `Newline` tokens into statement-terminating `Semi`s.
///
/// A `Newline` becomes a `Semi` iff the previous significant token can end a statement;
/// otherwise it is dropped (the statement continues onto the next line). Explicit `Semi`
/// tokens (source `;`) are kept verbatim. Consecutive terminators are coalesced.
fn apply_asi(raw: Vec<Spanned>) -> Vec<Spanned> {
    let mut out: Vec<Spanned> = Vec::with_capacity(raw.len());
    let mut prev_ends = false;
    for sp in raw {
        match sp.tok {
            Token::Newline => {
                if prev_ends {
                    out.push(Spanned {
                        tok: Token::Semi,
                        span: sp.span,
                    });
                    prev_ends = false;
                }
            }
            Token::Semi => {
                // Explicit separator: keep one, but don't stack on an inserted Semi.
                if prev_ends {
                    prev_ends = false;
                }
                out.push(sp);
            }
            _ => {
                prev_ends = sp.tok.ends_statement();
                out.push(sp);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Literal decoders.
// ---------------------------------------------------------------------------

fn parse_dec(s: &str) -> Option<i128> {
    let cleaned: String = s.chars().filter(|&c| c != '_').collect();
    cleaned.parse().ok()
}

fn parse_radix(s: &str, radix: u32) -> Option<i128> {
    // Strip the `0x` / `0b` prefix and any `_` separators.
    let cleaned: String = s[2..].chars().filter(|&c| c != '_').collect();
    i128::from_str_radix(&cleaned, radix).ok()
}

fn parse_float(s: &str) -> Option<f64> {
    let cleaned: String = s.chars().filter(|&c| c != '_').collect();
    cleaned.parse().ok()
}

/// Decode a `"..."` literal's escapes (surrounding quotes still attached).
fn unescape_str(raw: &str) -> Option<String> {
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(decode_escape(&mut chars)?);
    }
    Some(out)
}

/// Decode a `'x'` byte literal to its `u8` value.
fn unescape_byte(raw: &str) -> Option<u8> {
    let inner = &raw[1..raw.len() - 1];
    let mut chars = inner.chars();
    let c = chars.next()?;
    let decoded = if c == '\\' {
        decode_escape(&mut chars)?
    } else {
        c
    };
    // A byte literal must fit in one byte.
    let cp = decoded as u32;
    if cp <= 0xFF { Some(cp as u8) } else { None }
}

/// Decode the character(s) following a `\` in a literal (`spec/grammar.ebnf` §L3 `escape`).
fn decode_escape(chars: &mut std::str::Chars<'_>) -> Option<char> {
    match chars.next()? {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        '\\' => Some('\\'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '0' => Some('\0'),
        'x' => {
            let hi = chars.next()?.to_digit(16)?;
            let lo = chars.next()?.to_digit(16)?;
            char::from_u32(hi * 16 + lo)
        }
        _ => None, // unknown escape — the full frontend rejects it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Token> {
        tokenize(src).unwrap().into_iter().map(|s| s.tok).collect()
    }

    #[test]
    fn numeric_tower() {
        assert_eq!(
            kinds("0x40 0b1010 42 3.5 3e8 1_000"),
            vec![
                Token::Int(0x40),
                Token::Int(0b1010),
                Token::Int(42),
                Token::Float(3.5),
                Token::Float(3e8),
                Token::Int(1000),
            ]
        );
    }

    #[test]
    fn compound_ops_are_longest_match() {
        assert_eq!(
            kinds("<<= >>= << >> <= >= == != && || += ^="),
            vec![
                Token::ShlEq,
                Token::ShrEq,
                Token::Shl,
                Token::Shr,
                Token::Le,
                Token::Ge,
                Token::EqEq,
                Token::Ne,
                Token::AndAnd,
                Token::OrOr,
                Token::PlusEq,
                Token::CaretEq,
            ]
        );
    }

    #[test]
    fn string_and_byte_escapes() {
        assert_eq!(
            kinds(r#""a\nb\x41" 'Z' '\n'"#),
            vec![
                Token::Str("a\nb\u{41}".into()),
                Token::Byte(b'Z'),
                Token::Byte(b'\n'),
            ]
        );
    }

    #[test]
    fn asi_terminates_after_value_tokens() {
        // A line ending in a value/`)` gets a Semi; a line ending in `{`/operator does not.
        assert_eq!(
            kinds("a = 1\nb = 2\n"),
            vec![
                Token::Ident("a".into()),
                Token::Eq,
                Token::Int(1),
                Token::Semi,
                Token::Ident("b".into()),
                Token::Eq,
                Token::Int(2),
                Token::Semi,
            ]
        );
    }

    #[test]
    fn asi_continues_after_open_bracket_and_operator() {
        // No Semi is inserted after `{`, `+`, or `,` at end of line.
        assert_eq!(
            kinds("finna f() {\n1 +\n2\n}\n"),
            vec![
                Token::Finna,
                Token::Ident("f".into()),
                Token::LParen,
                Token::RParen,
                Token::LBrace,
                // no Semi after `{`
                Token::Int(1),
                Token::Plus,
                // no Semi after `+`
                Token::Int(2),
                Token::Semi,
                Token::RBrace,
                Token::Semi,
            ]
        );
    }

    #[test]
    fn comments_are_skipped_but_newlines_survive() {
        assert_eq!(
            kinds("a // trailing\n/* block\n spanning */ b\n"),
            vec![
                Token::Ident("a".into()),
                Token::Semi,
                Token::Ident("b".into()),
                Token::Semi,
            ]
        );
    }
}
