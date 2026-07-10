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

#[test]
fn crib_global_mir_compiles() {
    compile_fixture("crib_global.mir");
}

// --- Step-3 gap-fill: sums, tuples/multi-return, arrays. ---

#[test]
fn sum_mir_compiles() {
    compile_fixture("sum.mir");
}

#[test]
fn vibe_mir_compiles() {
    compile_fixture("vibe.mir");
}

#[test]
fn tuple_mir_compiles() {
    compile_fixture("tuple.mir");
}

#[test]
fn array_mir_compiles() {
    compile_fixture("array.mir");
}

#[test]
fn slice_mir_compiles() {
    compile_fixture("slice.mir");
}

// --- Track C: the scalar `spill` print primitives (bet_print_i64/u64/f64 + the bool branch),
// including the sign/zero-extend and fpext coercions the frontend emits. ---

#[test]
fn print_mir_compiles() {
    compile_fixture("print.mir");
}

// --- security-hardening batch: the memory-safety guards emitted by codegen must all produce a
// module that still passes LLVM verification (`compile_fixture` verifies via `compile`). These
// exercise the guard/mask/saturation codegen and the 16-byte Tag ABI (issues #32, #34, #36). ---

/// Array/slice indexing emits a bounds-check guard (Access `idx < len`, Addr `idx <= len` for a
/// one-past-the-end address) that branches to `bet_panic` and splits the block. (Issue #32.)
#[test]
fn oob_index_mir_compiles() {
    compile_fixture("oob_index.mir");
}

/// Div/rem guard a zero divisor (and, when signed, `INT_MIN / -1`) with a branch to `bet_panic`.
/// (Issue #36.)
#[test]
fn div_guard_mir_compiles() {
    compile_fixture("div_guard.mir");
}

/// `shl`/`shr` mask the shift amount to `[0, bit_width)` before shifting. (Issue #36.)
#[test]
fn shift_mask_mir_compiles() {
    compile_fixture("shift_mask.mir");
}

/// `trap`-mode add/sub/mul emit `llvm.{s,u}{add,sub,mul}.with.overflow` + a branch on the
/// overflow bit. (Issue #32.)
#[test]
fn overflow_trap_mir_compiles() {
    compile_fixture("overflow_trap.mir");
}

/// Float→int casts lower to the saturating `llvm.fpto{s,u}i.sat` intrinsics with a NaN→0 select.
/// (Issue #36.)
#[test]
fn ftoi_sat_mir_compiles() {
    compile_fixture("ftoi_sat.mir");
}

/// The 16-byte `{ i32 slot, i64 generation }` tag flows consistently through `cop`,
/// `holla_check`, `trust`, `evictslot`, and `ghosted`. (Issue #34, backend half.)
#[test]
fn tag16_mir_compiles() {
    compile_fixture("tag16.mir");
}
