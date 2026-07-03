//! LLVM-only backend tests: `.mir` text → native object bytes. Gated behind `--features
//! llvm`, so they compile out (and `nextest --no-tests=pass` covers the empty set) in the
//! default LLVM-free build. This isolates code generation from linking.

#![cfg(feature = "llvm")]

use backend::{EmitOptions, compile_mir_source};
use std::path::Path;

#[test]
fn hello_mir_compiles_to_object() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mir/hello.mir");
    let src = std::fs::read_to_string(&path).expect("read hello.mir");
    let opts = EmitOptions {
        entry: Some("main".into()),
        ..Default::default()
    };
    let obj = compile_mir_source(&src, &opts).expect("hello.mir should compile to an object");
    assert!(!obj.is_empty(), "emitted object should be non-empty");

    // A valid native object: Mach-O (arm64/x86_64, little-endian) or ELF.
    let magic = &obj[..4.min(obj.len())];
    let is_macho = magic == [0xCF, 0xFA, 0xED, 0xFE] || magic == [0xCE, 0xFA, 0xED, 0xFE];
    let is_elf = magic == [0x7F, b'E', b'L', b'F'];
    assert!(
        is_macho || is_elf,
        "unexpected object magic bytes: {magic:02X?}"
    );
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
