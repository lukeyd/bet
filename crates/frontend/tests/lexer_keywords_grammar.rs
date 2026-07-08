//! Drift guard: every reserved keyword in the lexer must appear in `spec/grammar.ebnf`.
//!
//! The lexer's alphabetic `#[token("word")]` attributes ARE the reserved-keyword set — the
//! single source of truth (`crates/frontend/src/lexer.rs`). This test extracts them from the
//! lexer source and checks each is present in the frozen grammar, so adding a keyword to the
//! lexer without recording it in the grammar fails CI instead of silently drifting (as `soa`
//! and `bounce` once did). It intentionally does NOT police the friendly prose table in
//! `language-spec.md` — the lexer + grammar are the normative surface.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The alphabetic `#[token("...")]` keywords declared in the lexer. Symbolic operator tokens
/// (`->`, `==`, `;`, …) are excluded — only `[a-z]+` content counts as a keyword.
fn lexer_keywords() -> Vec<String> {
    let src = fs::read_to_string(repo_file("src/lexer.rs")).expect("read lexer.rs");
    src.lines()
        .filter_map(|line| line.trim().strip_prefix("#[token(\""))
        .filter_map(|rest| rest.split('"').next())
        .filter(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_lexer_keyword_is_in_the_grammar() {
    let grammar =
        fs::read_to_string(repo_file("../../spec/grammar.ebnf")).expect("read grammar.ebnf");
    // Whole-word set (split on any non-lowercase char) so `in` matches the `"in"` token and
    // the reserved list, but is not a false substring of `finna`.
    let words: HashSet<&str> = grammar
        .split(|c: char| !c.is_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let keywords = lexer_keywords();
    assert!(
        keywords.len() >= 30,
        "expected the full lexer keyword set, found only {}: {keywords:?}",
        keywords.len()
    );

    let missing: Vec<&String> = keywords
        .iter()
        .filter(|kw| !words.contains(kw.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "lexer keywords missing from spec/grammar.ebnf (add them to the §L2 reserved list \
         and, if they introduce syntax, a production): {missing:?}"
    );
}
