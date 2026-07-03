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
//! - [`ast`]    — pending; the format is co-designed with its `bet` reproducer at the C3 parser
//!   port, so it is not frozen here yet.

use crate::CompileError;
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

/// Dump the parsed AST. Not yet implemented — see the module docs.
pub fn ast(_src: &str) -> Result<String, CompileError> {
    Err(CompileError::Lower(
        "--emit=ast is not implemented yet; the dump format is co-designed with the bet \
         reproducer at the C3 parser port (self-host roadmap)"
            .into(),
    ))
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
}
