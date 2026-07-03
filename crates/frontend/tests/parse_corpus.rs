//! Corpus-driven parser tests.
//!
//! `parse_all_corpus` is the broad regression net: every golden `.bet` program in
//! `tests/corpus/**` must parse cleanly into an `ast::Program`. A curated handful are then
//! snapshotted with `insta` so the exact tree shape is reviewed and pinned.

use std::fs;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .canonicalize()
        .expect("corpus dir should exist")
}

/// Recursively collect every `.bet` file under `dir`, sorted for determinism.
fn bet_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).expect("readable corpus dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("bet") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn parse_all_corpus() {
    let root = corpus_dir();
    let files = bet_files(&root);
    assert!(!files.is_empty(), "expected corpus programs to exist");

    let mut failures = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).expect("readable .bet");
        if let Err(e) = frontend::parse(&src) {
            let rel = path.strip_prefix(&root).unwrap_or(path);
            failures.push(format!("{}: {e}", rel.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "failed to parse {} of {} corpus programs:\n  {}",
        failures.len(),
        files.len(),
        failures.join("\n  ")
    );
}

/// Snapshot the parsed AST of a representative, diverse slice of the corpus.
macro_rules! snapshot_corpus {
    ($($name:ident => $rel:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let path = corpus_dir().join($rel);
                let src = fs::read_to_string(&path).expect("readable .bet");
                let program = frontend::parse(&src)
                    .unwrap_or_else(|e| panic!("parse {}: {e}", $rel));
                insta::assert_debug_snapshot!(program);
            }
        )*
    };
}

snapshot_corpus! {
    snap_hello         => "01-basics/hello.bet",
    snap_comments      => "01-basics/comments.bet",
    snap_arithmetic    => "02-values/arithmetic.bet",
    snap_numeric_tower => "02-values/numeric-tower.bet",
    snap_compound      => "02-values/compound-assign.bet",
    snap_fr_naw        => "03-control/fr-naw.bet",
    snap_squad         => "03-control/squad.bet",
    snap_multi_return  => "04-functions/multi-return.bet",
    snap_generics_fn   => "04-functions/generics-fn.bet",
    snap_first_class   => "04-functions/first-class-fn.bet",
    snap_drip_basics   => "05-structs/drip-basics.bet",
    snap_drip_generic  => "05-structs/drip-generic.bet",
    snap_moods_basics  => "06-sumtypes/moods-basics.bet",
    snap_bounce        => "07-errors/bounce.bet",
    snap_bit_ops       => "09-bit-math/bit-ops.bet",
    snap_bam_angles    => "09-bit-math/bam-angles.bet",
    snap_holla_ghosted => "08-memory/holla-ghosted.bet",
    snap_scratch       => "08-memory/scratch.bet",
    snap_trust         => "08-memory/trust.bet",
    snap_stash         => "10-stdlib/stash.bet",
    snap_extern_abs    => "12-ffi/extern-abs.bet",
    snap_slide_spawn   => "13-concurrency/slide-spawn.bet",
    snap_mini_compiler => "11-reference/mini-compiler.bet",
}
