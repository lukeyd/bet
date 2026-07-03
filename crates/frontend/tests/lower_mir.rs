//! AST→`midir` lowering tests.
//!
//! `frontend::compile` parses, lowers, and **validates** — so every program these tests
//! compile is well-formed by construction. Two nets:
//!
//! * `snapshot_lower!` pins the emitted `.mir` text (via `midir::print`) for a diverse slice
//!   of the ready subset — arithmetic/overflow modes, control flow, short-circuit booleans,
//!   functions, structs, `vibe`/`moods`, and the memory model. Sources are minimal programs
//!   that mirror the corpus's computational shapes; the corpus programs whose bodies only
//!   *print computed values* can't round-trip yet (the compiled path has no int-format
//!   primitive), so their library functions are exercised directly here.
//! * `ready_corpus_programs_lower` is the regression guard: the corpus programs that lower end
//!   to end today must keep compiling to a validated module.

use std::fs;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .canonicalize()
        .expect("corpus dir should exist")
}

/// Compile a source string and render the resulting module to canonical `.mir` text.
fn lower_to_mir(src: &str) -> String {
    let m = frontend::compile(src).unwrap_or_else(|e| panic!("should lower: {e}"));
    midir::print(&m)
}

macro_rules! snapshot_lower {
    ($($name:ident => $src:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                insta::assert_snapshot!(lower_to_mir($src));
            }
        )*
    };
}

snapshot_lower! {
    // --- scalars, arithmetic, overflow modes (§2.4) ---
    lower_arith => "finna add(a: int, b: int) -> int { bet a + b }\n\
                    finna umath(x: u32, y: u32) -> u32 { bet x + y }",
    lower_fixed_point => "facts FRACBITS: int = 16\n\
                          finna fixedMul(a: int, b: int) -> int { bet (a * b) >> FRACBITS }",
    lower_bit_ops => "finna bits(a: u8, b: u8) -> u8 { bet (a & b) | (a ^ b) }\n\
                      finna shifts(a: u8) -> u8 { bet (a << 1) >> 2 }",
    lower_wrapping => "finna wrap_add(a: i8, b: i8) -> i8 { bet math.lap(a, b) }",
    lower_casts => "finna narrow(x: int) -> u8 { bet x as u8 }\n\
                    finna widen(x: u8) -> int { bet x as int }\n\
                    finna to_int(f: f64) -> int { bet f as int }",
    lower_unary => "finna negate(x: int) -> int { bet -x }\n\
                    finna complement(x: u32) -> u32 { bet ~x }\n\
                    finna notb(b: bool) -> bool { bet !b }",

    // --- control flow ---
    lower_if_chain => "finna classify(n: int) -> int {\n\
                         fr n < 0 { bet 0 } naw fr n == 0 { bet 1 } naw { bet 2 }\n\
                       }",
    lower_while => "finna sumto(n: int) -> int {\n\
                      lowkey i = 1\n lowkey total = 0\n\
                      vibin i <= n { total = total + i\n i = i + 1 }\n bet total\n}",
    lower_loop_control => "finna scan(n: int) -> int {\n\
                             lowkey sum = 0\n lowkey i = 0\n\
                             vibin i < n {\n i = i + 1\n\
                               fr sum > 20 { dip }\n fr i % 2 == 0 { skip }\n\
                               sum = sum + i\n }\n bet sum\n}",
    lower_short_circuit => "finna both(a: bool, b: bool) -> bool { bet a && b }\n\
                            finna either(a: bool, b: bool) -> bool { bet a || b }\n\
                            finna chain(x: int) -> bool { bet 3 < x && x <= 5 }",
    lower_compound_assign => "finna acc() -> int {\n\
                                lowkey n = 10\n n += 5\n n *= 2\n n <<= 1\n n &= 60\n bet n\n}",

    // --- functions ---
    lower_first_class => "finna dub(x: int) -> int { bet x * 2 }\n\
                          finna apply(f: finna(int) -> int, x: int) -> int { bet f(x) }\n\
                          finna go() -> int { bet apply(dub, 21) }",
    lower_multi_return => "finna divmod(a: int, b: int) -> (int, int) { bet a / b, a % b }\n\
                           finna go() -> int { lowkey q, r = divmod(17, 5)\n bet q + r }",
    lower_extern_call => "extern \"C\" finna abs(x: i32) -> i32\n\
                          finna magnitude(x: i32) -> i32 { bet abs(x) }",

    // --- structs & receiver methods ---
    lower_struct => "drip Counter { flex n: int }\n\
                     finna (c: Counter) bump(by: int) -> int { bet c.n + by }\n\
                     finna go() -> int { lowkey c = Counter{ n: 10 }\n bet c.bump(5) }",
    lower_field_mutation => "drip Point { flex x: int\n flex y: int }\n\
                             finna move_x(p: Point, dx: int) -> int {\n\
                               lowkey q = p\n q.x = q.x + dx\n bet q.x + q.y\n}",

    // --- sum types & vibe matching ---
    lower_vibe => "moods Shape { Circle(int), Rect(int, int), Dot }\n\
                   finna area(s: Shape) -> int {\n\
                     vibe s { Circle(r) { bet 3 * r * r } Rect(w, h) { bet w * h } Dot { bet 0 } }\n}\n\
                   finna go() -> int { bet area(Rect(3, 4)) }",
    lower_vibe_naw => "moods Token { Num(int), Plus, Minus, Times }\n\
                       finna arity(t: Token) -> int {\n\
                         vibe t { Num(n) { bet n } Plus { bet 2 } naw { bet 0 } }\n}",
    lower_sum_in_field => "moods Op { Add, Sub }\n\
                           drip Calc { flex op: Op\n flex lhs: int\n flex rhs: int }\n\
                           finna eval(c: Calc) -> int {\n\
                             vibe c.op { Add { bet c.lhs + c.rhs } Sub { bet c.lhs - c.rhs } }\n}",

    // --- memory model: cop / holla / trust / evict / crib decl ---
    lower_holla => "drip Enemy { flex hp: int }\n\
                    finna idOf(e: tag Enemy, arena: crib Enemy) -> int {\n\
                      holla r = e in arena { bet r.hp } ghosted { bet -1 }\n}",
    lower_cop_trust => "drip Enemy { flex hp: int }\n\
                        finna peek(arena: crib Enemy) -> int {\n\
                          lowkey e = cop Enemy{ hp: 77 } in arena\n\
                          lowkey r = e.trust() in arena\n bet r.hp\n}",
    lower_local_crib => "drip Enemy { flex hp: int }\n\
                         finna go() -> int {\n\
                           crib arena: Enemy[8]\n\
                           lowkey e = cop Enemy{ hp: 5 } in arena\n\
                           holla r = e in arena { bet r.hp } ghosted { bet -1 }\n}",
    lower_evict => "drip Node { flex v: int }\n\
                    finna clear(arena: crib Node) { evict arena }",
}

// --- corpus programs that lower end to end today (regression guard) ---

#[test]
fn ready_corpus_programs_lower() {
    // Programs whose every function lowers with today's subset. (Most corpus `main`s print
    // computed integers, which needs the not-yet-built int-format primitive; those are covered
    // per-function by the synthetic snapshots above.)
    let ready = [
        "01-basics/hello.bet",
        "01-basics/comments.bet",
        "01-basics/spill-format.bet",
        "03-control/fr-naw.bet",
        "04-functions/multi-return.bet",
    ];
    for rel in ready {
        let src = fs::read_to_string(corpus_dir().join(rel)).expect("readable .bet");
        let m = frontend::compile(&src).unwrap_or_else(|e| panic!("{rel} should lower: {e}"));
        assert!(!m.funcs().is_empty(), "{rel} lowered to no functions");
    }
}

/// Whatever the corpus lowers, it must lower to a *validated* module: `compile` runs
/// `midir::validate`, so a successful compile is a clean module. This asserts the coverage
/// count does not silently regress and that no lowered program is ill-formed.
#[test]
fn lowered_corpus_is_valid_and_covers_the_expected_set() {
    let root = corpus_dir();
    let mut stack = vec![root.clone()];
    let mut lowered = 0usize;
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).expect("readable corpus dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("bet") {
                let src = fs::read_to_string(&path).expect("readable .bet");
                // A parse failure would be a different bug; only count lowering successes.
                if frontend::compile(&src).is_ok() {
                    lowered += 1;
                }
            }
        }
    }
    // Five whole programs lower end to end today (see `ready_corpus_programs_lower`). This is a
    // floor: it may only go up as the frontier shrinks.
    assert!(
        lowered >= 5,
        "expected at least 5 corpus programs to lower, got {lowered}"
    );
}
