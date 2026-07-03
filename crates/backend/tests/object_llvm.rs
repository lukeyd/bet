//! LLVM-only backend tests: `.mir` text → native object bytes. Gated behind `--features
//! llvm`, so they compile out (and `nextest --no-tests=pass` covers the empty set) in the
//! default LLVM-free build. This isolates code generation from linking.

#![cfg(feature = "llvm")]

use backend::{EmitOptions, compile_mir_source};
use std::path::Path;

/// Read a `tests/mir/*.mir` fixture from the repo root.
fn read_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/mir")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Assert `obj` starts with a valid native object magic: Mach-O (arm64/x86_64,
/// little-endian) or ELF.
fn assert_native_object(obj: &[u8]) {
    assert!(!obj.is_empty(), "emitted object should be non-empty");
    let magic = &obj[..4.min(obj.len())];
    let is_macho = magic == [0xCF, 0xFA, 0xED, 0xFE] || magic == [0xCE, 0xFA, 0xED, 0xFE];
    let is_elf = magic == [0x7F, b'E', b'L', b'F'];
    assert!(
        is_macho || is_elf,
        "unexpected object magic bytes: {magic:02X?}"
    );
}

/// Compile a fixture that has no `main` entry (just library functions) to an object.
fn compile_fixture(name: &str) -> Vec<u8> {
    let src = read_fixture(name);
    let obj = compile_mir_source(&src, &EmitOptions::default())
        .unwrap_or_else(|e| panic!("{name} should compile to an object: {e}"));
    assert_native_object(&obj);
    obj
}

#[test]
fn hello_mir_compiles_to_object() {
    let src = read_fixture("hello.mir");
    let opts = EmitOptions {
        entry: Some("main".into()),
        ..Default::default()
    };
    let obj = compile_mir_source(&src, &opts).expect("hello.mir should compile to an object");
    assert_native_object(&obj);
}

#[test]
fn missing_entry_is_a_clean_error() {
    let src = "fn nope() -> void { bb0: return }";
    let opts = EmitOptions {
        entry: Some("main".into()),
        ..Default::default()
    };
    let err = compile_mir_source(src, &opts).expect_err("no `main` should be an error");
    assert!(matches!(err, backend::BackendError::Lower(_)), "{err:?}");
}

// --- broader lowering: one test per fixture, each asserting a valid object is emitted (and,
// via `compile`, that the generated module passes LLVM verification). ---

#[test]
fn arith_mir_compiles() {
    compile_fixture("arith.mir");
}

#[test]
fn ops_mir_compiles() {
    compile_fixture("ops.mir");
}

#[test]
fn float_mir_compiles() {
    compile_fixture("float.mir");
}

#[test]
fn casts_mir_compiles() {
    compile_fixture("casts.mir");
}

#[test]
fn control_mir_compiles() {
    compile_fixture("control.mir");
}

#[test]
fn indirect_mir_compiles() {
    compile_fixture("indirect.mir");
}

#[test]
fn holla_mir_compiles() {
    compile_fixture("holla.mir");
}

#[test]
fn thinker_mir_compiles() {
    compile_fixture("thinker.mir");
}

#[test]
fn crib_mir_compiles() {
    compile_fixture("crib.mir");
}
