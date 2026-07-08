//! `soa` (struct-of-arrays) frontend rules.
//!
//! A `soa` container transposes its storage to parallel per-field arrays, so a *whole
//! element* has no single address/value: it can't be copied, assigned as one, passed by
//! value, returned, or iterated with `squad`. Only per-field `arr[i].field` access is
//! allowed. These are compile-time (lowering) errors — the interpreter is layout-agnostic
//! and won't flag them under `bet run`, so they're tested here against `frontend::compile`.
//! The diagnostics are written in bet's internet-slang register (matching its keywords).

/// Compile a source and return the (stringified) error it must fail with.
fn err(src: &str) -> String {
    match frontend::compile(src) {
        Ok(_) => panic!("program should have failed to compile"),
        Err(e) => e.to_string(),
    }
}

const HDR: &str = r#"
pull "spill"
drip E { flex hp: int  flex x: int }
finna sink(e: E) {}
"#;

fn with_main(body: &str) -> String {
    format!(
        "{HDR}\nfinna main() {{\n  lowkey es: soa E[2] = [E{{hp:1,x:2}}, E{{hp:3,x:4}}]\n{body}\n}}\n"
    )
}

#[test]
fn whole_element_copy_is_banned() {
    let e = err(&with_main("  lowkey one = es[0]\n  sink(one)"));
    assert!(e.contains("yoink a whole soa element"), "got: {e}");
}

#[test]
fn whole_element_assign_is_banned() {
    let e = err(&with_main("  es[0] = E{hp:9,x:9}"));
    assert!(
        e.contains("slam a whole struct into a soa slot"),
        "got: {e}"
    );
}

#[test]
fn pass_element_by_value_is_banned() {
    let e = err(&with_main("  sink(es[1])"));
    assert!(e.contains("yoink a whole soa element"), "got: {e}");
}

#[test]
fn squad_over_soa_is_banned() {
    let e = err(&with_main("  squad p in es { sink(p) }"));
    assert!(e.contains("squad over a soa container"), "got: {e}");
}

#[test]
fn non_struct_element_is_rejected() {
    let e = err(
        "pull \"spill\"\nfinna main() {\n  lowkey xs: soa int[3] = [1,2,3]\n  spill.it(xs[0])\n}\n",
    );
    assert!(e.contains("soa only vibes with a drip"), "got: {e}");
}

#[test]
fn non_container_soa_is_rejected() {
    let e = err("pull \"spill\"\nfinna main() {\n  lowkey n: soa int = 0\n  spill.it(n)\n}\n");
    assert!(e.contains("soa only wraps a container"), "got: {e}");
}

#[test]
fn nested_soa_is_rejected() {
    let src = "pull \"spill\"\n\
        drip Inner { flex a: int }\n\
        drip Outer { flex slab: soa Inner[2] }\n\
        finna main() {\n  lowkey g: soa Outer[1] = [Outer{}]\n  spill.it(0)\n}\n";
    let e = err(src);
    assert!(e.contains("no soa inside soa"), "got: {e}");
}

#[test]
fn soa_vec_pop_is_banned() {
    // `pop` would hand back a whole element, which soa can't represent.
    let src = "pull \"spill\"\n\
        drip P { flex hp: int }\n\
        finna main() {\n\
        \x20 lowkey v: soa vec[P] = vec.new[P]()\n\
        \x20 v.stack(P{ hp: 1 })\n\
        \x20 lowkey e = v.pop()\n\
        \x20 spill.it(e.hp)\n}\n";
    let e = err(src);
    assert!(e.contains("soa vec pop"), "got: {e}");
}

/// The happy path must still lower + validate: per-field read and write through a `soa`
/// fixed array, plus scatter-initialization from an array literal.
#[test]
fn per_field_access_lowers() {
    let src = with_main("  es[0].hp = es[0].hp + 1\n  spill.it(es[0].hp)\n  spill.it(es[1].x)");
    let m = match frontend::compile(&src) {
        Ok(m) => m,
        Err(e) => panic!("valid soa program should lower: {e}"),
    };
    // Round-trips through the .mir text contract (the `soa` type printer/parser).
    let text = midir::print(&m);
    assert!(
        text.contains("soa [E; 2]"),
        "expected a soa type in the IR:\n{text}"
    );
}
