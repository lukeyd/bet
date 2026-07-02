# Plan Amendment 01: Aspirational Test Programs (Self-Hosting & Doom)

> **Amends:** Language Specification (Draft v0.1) and Bootstrap Plan
> **Status:** Proposed. On acceptance, fold §2–§4 into the spec and §6–§7 into the bootstrap plan.
> **Summary:** Adopts two long-horizon test programs — a self-hosted compiler and a Doom port — as binding design constraints. Together they close most of the spec's §12 open questions and surface five hard feature gaps that must land in v1.

---

## 1. The Principle Being Added

Add to spec §2 (Design Principles):

> **6. The language must be able to express its own compiler and a Doom port.** These two programs are permanent design oracles. A compiler is a brutally honest general-purpose test (graphs, symbol tables, string processing, sum-typed trees); Doom is a brutally honest systems/gamedev test (bit manipulation, FFI, byte-level I/O, function tables, allocation pressure). Any v1 design decision that makes either program unwritable or miserable is wrong.

Rationale: Pong (milestone 6) is too polite a test to flush out real gaps. Languages designed against demanding reference programs (Go, Zig, Rust) end up complete; languages designed against toy demos end up with holes discovered post-1.0, when they are expensive to fix.

The two oracles are deliberately complementary — their demands barely overlap:

| Demand | Self-hosted compiler | Doom port |
|---|---|---|
| Sum types + pattern matching | **hard requirement** | useful |
| Generics (at least collections) | **hard requirement** | mild |
| Hash maps | **hard requirement** | mild |
| Deep string/byte handling | **hard requirement** | **hard requirement** |
| FFI / platform layer | mild | **hard requirement** |
| Bit operators + wrapping ints | mild | **hard requirement** |
| First-class function values | useful | **hard requirement** |
| Sized integer tower settled | useful | **hard requirement** |
| Arena/tag memory model stress | strong | **the showcase** |

---

## 2. New Language Features (spec additions)

### 2.1 Sum types & pattern matching — NEW §5 material

**Blocking for:** self-hosted compiler (AST/IR nodes are "one of N kinds"). Without this, tree code degenerates into C-style tag-field-plus-union hacks.

- Add a tagged-union declaration form. Working vocabulary proposal (subject to the same slang rules as everything else — semantic mapping or nothing):
  - `moods` — declares a sum type ("this value has moods"): `moods Expr { Lit(int), Add(tag Expr, tag Expr), Var(str) }`
  - `vibe` — pattern match: `vibe e { Lit(n) { ... } Add(l, r) { ... } Var(name) { ... } }`
- Exhaustiveness checking is **on by default** (compile error on unhandled variant); `naw { ... }` serves as the wildcard arm.
- Payload variants may hold `drip` values, primitives, and `tag T` (the compiler's AST will be `tag`-linked nodes in a crib — this combination is the load-bearing one).
- **Naming is provisional.** The concept is mandatory for v1; the slang is not frozen by this amendment.

### 2.2 Generics — RESOLVES §12 open question: **yes for v1, minimal**

- Generic `drip`s, `moods`, and `finna`s with a single bracketed parameter list: `finna glowUp[T, U](xs: []T, f: finna(T) -> U) -> []U`.
- Monomorphized (compile-time expansion), no runtime dispatch — consistent with the no-magic runtime philosophy.
- **Scope discipline:** v1 needs generics good enough for collections (`[]T`, `map[K]V`, iterators) and compiler data structures. No higher-kinded anything, no specialization, no variance. Cut scope, not the feature.

### 2.3 Hash maps — NEW stdlib module

**Blocking for:** symbol tables, string interning, scope resolution, WAD lump directories.

- Add `stash` to the §9.2 module map: `stash.new[K, V]()`, `.put(k, v)`, `.peep(k) -> (V, bool)`, `.yeet(k)`, `.gang()`.
- Allocator-aware like everything else: `stash.new[str, Symbol](in: astCrib)`.

### 2.4 Bit operators & integer semantics — RESOLVES §12 numeric-tower question

**Blocking for:** Doom (16.16 fixed-point math, BAM angle arithmetic, flag fields).

- Add to §4 operators: `& | ^ << >> ~` with conventional C-family precedence. These are **not** slang — they fall under design principle 5 (escape hatches stay conventional).
- Numeric tower settled: `i8 i16 i32 i64 u8 u16 u32 u64 f32 f64`, with `int` = `i64` and `float` = `f64` as the friendly defaults. Rust-style spellings; no surprises.
- **Overflow semantics:** unsigned types wrap (defined behavior — Doom's angle system requires u32 wraparound); signed overflow traps in debug builds (`yeet`) and wraps in release. Explicit wrapping ops available regardless of build: `math.lap(a, b)` family (**open: naming**).
- Explicit numeric casts required between sized types: `x as u32`. No implicit narrowing, ever.

### 2.5 First-class function values — NEW §5 material

**Blocking for:** Doom thinkers and action-function tables; useful everywhere (`.vibeCheck(pred)` already implies it).

- Function type syntax: `finna(Mobj) -> void` usable as a field type, parameter type, and element type.
- v1 scope: **plain function values only** (a code pointer) — no closures capturing environment. Thinkers, AI tables, and stdlib predicates all work with plain functions; closures are a v2 question. If a captureless lambda literal is cheap to add, fine; capture is out.

```
drip Thinker {
    flex fn: finna(tag Mobj) -> void
    flex mobj: tag Mobj
}
```

### 2.6 FFI & platform layer — NEW spec section

**Blocking for:** Doom (video/audio/input) and any real program. The biggest gap in the current spec.

Two layers, both v1:

1. **`extern` declarations** for calling C ABI functions, with a small marked-unsafe surface:
   ```
   extern "C" finna SDL_Init(flags: u32) -> i32
   ```
   Raw pointers exist only at the FFI boundary (`rawptr` type, unusable outside `extern`-adjacent code without explicit conversion). Deliberately ugly and greppable, same philosophy as `trust()`.
2. **`gg` grows into a real built-in platform layer** — framebuffer present, audio ring buffer, input events, high-res timing — implemented *inside the runtime* on top of per-OS backends. For a gamedev-first language this is the honest move: most users should never touch `extern`. Doom targets `gg` directly; `extern` exists for everyone else's SDL/OpenGL/etc.

**Runtime ABI impact:** the rt-abi contract crate gains the platform-layer entry points (see §6.1). This must be decided in Phase 0, not retrofitted — it shapes the ABI the same way allocation intrinsics do.

### 2.7 Bytes & binary I/O — stdlib additions

**Blocking for:** WAD parsing; also lexer performance in the self-hosted compiler.

- `[]u8` is the byte-slice type; `str` gains zero-copy conversion in both directions (`str.bytes()`, `str.fromBytes()` — validity rules **open**).
- `fs.peep` returns `[]u8`, not `str`; `fs.peepText` is the string convenience.
- New `bytes` module: `bytes.readU32le(buf, offset)` family for endian-explicit reads, `bytes.slice`, `bytes.cast[T]` for reinterpreting packed structs (**bounds-checked; unsafe variant gated like `trust()`**).
- String internals: `str` gets byte-length, byte indexing, and non-copying slicing — a lexer must not allocate per token.

### 2.8 Other §12 resolutions forced by the oracles

- **`bounce y` error sugar: adopt.** A compiler and a game engine are both wall-to-wall error checks; the three-line idiom multiplied by thousands of call sites is the strongest possible motivation.
- **Method syntax:** Go-style receivers on `drip` (`finna (p: Player) damage(amt: int) -> int`). Chosen because the frontend-as-library and LSP work want a single unambiguous "find the methods of T" rule, and receivers keep it that way.
- **Default visibility: `hush` (private).** A self-hosted compiler is a large multi-module codebase; private-by-default is the defensible default and matches the `flex` = deliberate export mental model.
- **Still open, unaffected:** interfaces/traits story (neither oracle forces it in v1 — Doom needs none, and the compiler can live on `moods` + generics), long-lived shared objects, cross-thread crib semantics, ASI fine print (to be settled by writing the corpus, per bootstrap plan Step 1c).

---

## 3. Amendment to §9.2 Module Map

| Module | Change |
|---|---|
| `stash` | **NEW** — hash maps (§2.3) |
| `bytes` | **NEW** — binary reads, endian-explicit, casts (§2.7) |
| `fs` | `peep` returns `[]u8`; add `peepText` |
| `str` | add byte-length, byte indexing, zero-copy slicing, `bytes()`/`fromBytes()` |
| `math` | add wrapping-arithmetic family (§2.4) |
| `gg` | promoted from sketch to **runtime-backed platform layer**: `gg.blit(fb)`, `gg.audio(ring)`, `gg.poll() -> Event`, `gg.ticks()` (§2.6) |

---

## 4. Two New Reference Programs (spec §10 additions)

### 10.5 Sum types in anger (compiler-shaped)

```
moods Expr {
    Lit(i64),
    Add(tag Expr, tag Expr),
    Var(str)
}

finna eval(e: tag Expr, ast: crib Expr, env: stash[str, i64]) -> (i64, yikes) {
    holla node = e in ast {
        vibe node {
            Lit(n) { bet n, ghosted }
            Add(l, r) {
                lowkey a, y = eval(l, ast, env)
                bounce y
                lowkey b, y2 = eval(r, ast, env)
                bounce y2
                bet a + b, ghosted
            }
            Var(name) {
                lowkey v, found = env.peep(name)
                fr !found { bet 0, yikes.new("undefined variable").tea(name) }
                bet v, ghosted
            }
        }
    } ghosted {
        bet 0, yikes.new("dangling AST node")
    }
}
```

### 10.6 Doom-shaped: thinkers, tags, bit math

```
facts ANG90: u32 = 0x40000000        // BAM angles: u32 wraparound is load-bearing

drip Mobj {
    flex pos: vec3
    flex angle: u32
    flex flags: u32
    flex target: tag Mobj
    flex think: finna(tag Mobj) -> void
}

finna chase(selfTag: tag Mobj) {
    holla m = selfTag in mobjs {
        fr m.flags & MF_SHOOTABLE == 0 { bet }
        holla t = m.target in mobjs {
            m.angle = faceTarget(m.pos, t.pos)      // wraps, on purpose
        } ghosted {
            m.target = findNewTarget(selfTag)        // the 1993 dangling-pointer bug, deleted
        }
    }
}
```

---

## 5. Vocabulary Additions (running table)

All provisional, all subject to spec §2 slang rules (durable, semantically honest):

| Concept | Proposed | Notes |
|---|---|---|
| sum type | `moods` | has multiple moods |
| pattern match | `vibe` | vibe-check the value |
| hash map module | `stash` | where you keep the goods |
| error-return sugar | `bounce` | bounce early with the error |
| wrapping arithmetic | `math.lap(...)` | wraps around the track; **weak — candidates welcome** |
| byte reinterpret | `bytes.cast` | conventional on purpose (escape hatch rule) |
| FFI | `extern`, `rawptr` | conventional on purpose (escape hatch rule) |

---

## 6. Bootstrap Plan Amendments

### 6.1 Step 1 changes (contract artifacts)

- **`rt-abi` gains the platform layer** (§2.6): framebuffer, audio, input, timing entry points. `rt-stub` implements them naively (blit to a window via any convenient host library — the stub is bootstrap-only and never ships). This is the single most important amendment to Step 1: FFI/platform decisions shape the ABI and cannot be retrofitted cheaply.
- **`midir` must represent:** `moods` construction and `vibe` dispatch (switch-on-tag), monomorphized generic instantiations (post-expansion — the IR never sees type parameters), function-pointer values and indirect calls, and the full sized-integer tower with explicit wrap/trap arithmetic ops.
- **Golden corpus (Step 1c) grows targeted programs:** every §2 feature gets corpus coverage from day one — sum-type interpreters, bit-math/BAM-angle tests (these double as overflow-semantics conformance tests), byte-parsing of a tiny binary format, thinker-style function tables.

### 6.2 Repository additions

```
lang/
├── crates/
│   └── ...                    # unchanged
├── std/
│   ├── stash/                 # NEW
│   ├── bytes/                 # NEW
│   └── ...
├── selfhost/                  # NEW (empty until trigger, §7.1)
│   └── README.md              # states the trigger condition so nobody starts early
└── ports/
    └── doom/                  # NEW (empty until trigger, §7.2)
        └── README.md          # GPL notice, asset policy, oracle-testing design
```

### 6.3 CI additions

- Corpus differential testing gains a third execution path once selfhost lands: **interp vs. Rust-compiled vs. self-compiled** must agree on the entire corpus.
- Doom demo-lump playback becomes a CI job post-port: deterministic demo playback diffed against a reference port — differential testing against id Software's 1993 behavior.

---

## 7. New Milestones (extends spec §11.4)

Existing milestones 1–6 unchanged. Add:

| # | Milestone | Gate / trigger |
|---|---|---|
| 7 | **Language freeze for stage 1** | All §12 questions that remain open are closed; corpus stable for N weeks. **Nothing in §7.1/§7.2 starts before this.** A self-hosted compiler makes every language change ~2× more expensive; do not pay that tax while the language is still moving. |
| 7.1 | **Self-host stage 1** | Frontend (lexer → parser → typecheck → midir emission) rewritten in the language, compiled by the Rust compiler. Backend and runtime stay Rust **permanently** — "self-hosted" means the frontend/middle-end, per convention (Go's runtime is still substantially not-Go). |
| 7.2 | **Doom port** | Requires milestones 3–4 (cribs, tag/holla through real codegen) + §2.4/§2.5/§2.6/§2.7 landed. Targets `gg` platform layer. Engine code translated from GPL source (port is GPL; the language/toolchain is unaffected); tested against shareware WAD / Freedoom; demo-lump differential testing as the correctness oracle. |
| 8 | **Self-host fixpoint** | Stage 2 (self-compiled compiler) recompiles its own source to produce stage 3; stage 2 and stage 3 binaries must be functionally identical across the full corpus. This is the proof of correct self-hosting. |

Sequencing note: 7.1 and 7.2 are **parallel** workstreams after milestone 7 — they stress disjoint feature sets (that's why they were chosen) and share only the frozen language underneath.

### What is explicitly NOT changing

- The Rust compiler is not being retired. It remains the bootstrap compiler forever (stage 0), the reference implementation, and the thing CI trusts first.
- The backend (LLVM lowering) and runtime are never rewritten in the language. No milestone proposes it; the payoff is negative.
- Pong stays as milestone 6. Doom replaces the vague "demo with real allocation pressure" — it *is* that demo, upgraded, with an external correctness oracle attached.

---

## 8. Revised Open Questions (replaces spec §12 in part)

**Closed by this amendment:** generics (yes, minimal), numeric tower (Rust spellings, `int`=`i64`), bit ops (conventional), overflow semantics (unsigned wraps; signed traps-debug/wraps-release), `bounce` (adopted), method syntax (receivers), default visibility (`hush`), `fs.peep` return type (`[]u8`).

**Newly opened:**
- `moods`/`vibe` naming and exact pattern syntax (concept mandatory; slang provisional).
- Closure capture: v2, or never? (v1 ships plain function values only.)
- `str` byte-slicing validity rules (UTF-8 enforcement at the `fromBytes` boundary?).
- Wrapping-arithmetic naming (`math.lap` is weak).
- `rawptr` conversion rules at the FFI boundary — exactly how ugly, exactly how greppable.
- How much of SDL-equivalent surface `gg` absorbs in v1 (framebuffer + audio ring + input is the floor; GPU is explicitly out).

**Still open, unchanged:** interfaces/traits, long-lived shared objects (RC vs. budgeted GC), cross-thread crib semantics, ASI fine print, package manager model, LLVM pin, ECS-flavored `squad` queries, **the language name** (still blocking repo creation, now blocking three more directories).
