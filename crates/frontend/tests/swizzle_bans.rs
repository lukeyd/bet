//! Multi-lane swizzle (`v.yx` / `v.wzy` / `v.xxww`) frontend rules.
//!
//! A swizzle reads 2-4 named lanes into a new vector, in letter order. Every letter must be
//! `x`/`y`/`z`/`w` AND in range for the source vector's arity — all validated before any MIR
//! is emitted, so a bad swizzle never leaves a half-built vector behind. Duplicate letters
//! and re-broadening a narrower vector are legal; a single letter stays a scalar lane read,
//! and 5+ letters are not a swizzle at all. These are compile-time (lowering) errors, so
//! they're tested against `frontend::compile`.

/// Compile a source and return the (stringified) error it must fail with.
fn err(src: &str) -> String {
    match frontend::compile(src) {
        Ok(_) => panic!("program should have failed to compile"),
        Err(e) => e.to_string(),
    }
}

fn with_main(body: &str) -> String {
    format!("pull \"spill\"\n\nfinna main() {{\n{body}\n}}\n")
}

#[test]
fn out_of_range_lane_is_rejected() {
    // `.z`/`.w` name lanes a 2-lane vector doesn't have.
    let e = err(&with_main(
        "  lowkey v: vec2 = vec2(1.0, 2.0)\n  lowkey r: vec2 = v.zw\n  spill.it(r.x as int)",
    ));
    assert!(e.contains("out of range for a 2-lane vector"), "got: {e}");
}

#[test]
fn out_of_range_lane_is_rejected_on_vec3() {
    let e = err(&with_main(
        "  lowkey v: vec3 = vec3(1.0, 2.0, 3.0)\n  lowkey r: vec2 = v.xw\n  spill.it(r.x as int)",
    ));
    assert!(e.contains("out of range for a 3-lane vector"), "got: {e}");
}

#[test]
fn bad_lane_letter_is_rejected() {
    // `q` is not a lane letter — falls through to the scalar lane read, which rejects it.
    let e = err(&with_main(
        "  lowkey v: vec4 = vec4(1.0, 2.0, 3.0, 4.0)\n  spill.it(v.xq as int)",
    ));
    assert!(e.contains("no lane"), "got: {e}");
}

#[test]
fn over_four_letters_is_not_a_swizzle() {
    // 5 letters exceeds the widest vector, so this is not a swizzle and must not compile.
    let e = err(&with_main(
        "  lowkey v: vec4 = vec4(1.0, 2.0, 3.0, 4.0)\n  spill.it(v.xxxxx as int)",
    ));
    assert!(e.contains("no lane"), "got: {e}");
}

#[test]
fn valid_swizzles_lower() {
    // The positive cases the bans must not catch: reverse, narrow+reorder, duplicate lanes,
    // broadening a vec2, and an integer vector.
    let src = with_main(
        "  lowkey a: vec4 = vec4(1.0, 2.0, 3.0, 4.0)\n\
         \x20 lowkey r: vec2 = a.yx\n\
         \x20 lowkey b: vec3 = a.wzy\n\
         \x20 lowkey d: vec4 = a.xxww\n\
         \x20 lowkey e: vec4 = vec2(5.0, 6.0).yxyx\n\
         \x20 lowkey n: i32x4 = i32x4(10, 20, 30, 40)\n\
         \x20 lowkey m: i32x2 = n.wy\n\
         \x20 spill.it((r.x + b.x + d.x + e.x) as int)\n\
         \x20 spill.it((m.x + m.y) as int)",
    );
    assert!(
        frontend::compile(&src).is_ok(),
        "valid swizzles must compile: {:?}",
        frontend::compile(&src).err()
    );
}

#[test]
fn single_letter_stays_a_scalar_lane_read() {
    // Guards the boundary: `.x` is a scalar read, not a 1-lane vector.
    let src = with_main("  lowkey v: vec4 = vec4(1.0, 2.0, 3.0, 4.0)\n  spill.it(v.x as int)");
    assert!(frontend::compile(&src).is_ok(), "`.x` must still compile");
}
