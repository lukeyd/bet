# Language Specification (Draft v0.1)

> **Working name:** TBD — needs a name from the same slang register before repo creation.
> **Status:** Pre-implementation draft. Captures all design decisions to date. Open questions are flagged inline and collected in §12.

---

## 1. Vision & Goals

A compiled, statically typed, general-purpose programming language whose keyword and standard-library vocabulary is built entirely from durable internet slang. Inspired by Go in spirit (simple, compiled, batteries-included runtime) but **not** transpiled to Go — it compiles directly to native machine code with the runtime statically linked into every binary.

**Targets:** Linux, macOS, Windows on x86-64 and ARM64 (6 platform combos).

**Primary differentiators:**

1. **The bit, fully committed.** Slang keywords *and* slang standard library. The vocabulary is a real design surface with rules, not random renaming.
2. **Game-development-first memory management.** No tracing GC in the hot path. Arena ("crib") allocation as a first-class language concept, plus native generational handles (`tag` / `holla`) — the pattern every serious game codebase hand-rolls, promoted to a language feature.

**Non-goals (for v1):** exceptions, inheritance/classes, a tracing GC as the default memory strategy.

---

## 2. Design Principles

1. **Slang with staying power.** Prefer terms that have survived 5+ years and crossed into general vocabulary (`bet`, `cap`, `lowkey`, `ghost`, `flex`, `fam`, `sus`). Avoid meme-cycle slang that will age badly (`rizz`, `skibidi`, `gyat`).
2. **Semantic mapping, not random assignment.** Every slang keyword must genuinely describe its concept (`ghost` = disappear = deallocate/absent). If no good mapping exists, use a plain word.
3. **Everything else is boring.** C-family braces, conventional operators, conventional literals. The keywords are the joke; the rest must be rock solid.
4. **One concept, one word, everywhere.** If `peep` means "read," it means read in every module. Consistency makes the vocabulary learnable.
5. **Escape hatches stay conventional.** Domain math (`sin`, `cos`, `dot`, `cross`) keeps standard names — game math is ported from reference material constantly.

---

## 3. Core Keyword Vocabulary

| Concept | Keyword | Rationale |
|---|---|---|
| function declaration | `finna` | "finna do something" — about to do it |
| return | `bet` | affirmative; hands back the answer |
| mutable variable | `lowkey` | unassuming declaration: `lowkey x = 5` |
| constant | `facts` | it's facts — it doesn't change |
| if | `fr` | "for real?" — test whether something is real |
| else | `naw` | the negative branch |
| while loop | `vibin` | keep vibing while the condition holds |
| for-each loop | `squad` | iterate over the whole squad |
| break | `dip` | dip out of the loop |
| continue | `skip` | plain, reads naturally |
| boolean true | `nocap` | no lie |
| boolean false | `cap` | a lie |
| nil / none / no-error | `ghosted` | the value never showed up |
| struct | `drip` | your data's outfit |
| import | `pull` | "pull up" — bring the module through |
| panic | `yeet` | throw it away hard |
| recover (panic boundary) | `sheesh` | exclamation at the catch site |
| spawn concurrent task | `slide` | slide into another thread |
| public visibility | `flex` | exposed for everyone to see |
| private visibility | `hush` | keep it quiet |
| error type | `yikes` | self-explanatory |

**Resolved conflicts:**
- `cap` is exclusively the boolean false. `else` is `naw` (avoids parser ambiguity and user confusion).
- `sheesh` was originally sketched as try/catch; with Go-style error handling adopted, it is repurposed as the panic-recovery mechanism (rare, discouraged — mirrors Go's `recover`).

### Memory-model keywords

| Concept | Keyword | Rationale |
|---|---|---|
| declare an arena | `crib` | where your data lives |
| allocate into an arena | `cop` | `cop Player{...} in frameCrib` |
| free an entire arena (O(1)) | `evict` | everyone out at once |
| per-frame scratch arena | `scratch` | built in, auto-evicted each frame |
| generational handle type | `tag` | `tag Enemy` — a name you can call out |
| checked handle access | `holla` | call out; either they answer or you're ghosted |
| unchecked handle access | `trust()` | explicitly dangerous; greppable |

---

## 4. Syntax Overview

C-family structure: braces, semicolon-free line-oriented statements (Go-style automatic termination — **open question: confirm ASI rules**), conventional operators (`+ - * / % == != < > <= >= && || !`), conventional literals.

### 4.1 Declarations

```
lowkey x = 5              // mutable, type inferred
lowkey y: float = 1.5     // mutable, explicit type
facts MAX_HP: int = 100   // constant

drip Player {
    flex hp: int          // public field
    flex pos: vec3
    hush secrets: int     // private field
}
```

### 4.2 Functions

```
finna damage(p: Player, amt: int) -> int {
    fr p.hp - amt <= 0 {
        spill.it("player down!")
        bet 0
    } naw {
        bet p.hp - amt
    }
}
```

- `finna name(params) -> returnType { ... }`
- Multiple return values supported (required for Go-style errors): `-> (SaveData, yikes)`

### 4.3 Control flow

```
fr condition { ... } naw fr other { ... } naw { ... }

vibin condition {
    fr done { dip }
    fr irrelevant { skip }
}

squad item in collection {
    ...
}
```

### 4.4 Visibility & modules

- `pull "modname"` imports a module.
- `flex` marks declarations/fields as exported; `hush` (or no modifier — **open question: default visibility**) keeps them module-private.

---

## 5. Type System

- **Statically typed** with local type inference (`lowkey x = 5` infers `int`).
- **Value types by default.** Structs (`drip`) are values; assignment copies. Reference semantics are explicit and rare.
- Primitives: `int`, `float`, `bool`, `str`, plus sized variants (**open question: exact numeric tower — `i32`/`i64`/`f32`/`f64` spelling**).
- Slices/arrays: `[]Bullet`, `Enemy[1000]` (fixed).
- Game-math types are built in via stdlib: `vec2`, `vec3`, `vec4`, `mat4` (SIMD-friendly layout).
- `tag T` is a distinct nominal type (see §7). A `tag Enemy` is not interchangeable with a `tag Player` or a bare `Enemy`.
- **Open questions:** generics (`squad[T]`?), method syntax on `drip` (Go-style receivers vs. dot-defined), interfaces/traits story.

---

## 6. Error Handling (Go-style)

Errors are **values**, returned as the final element of a multi-value return. No exceptions.

```
finna loadSave(path: str) -> (SaveData, yikes) {
    lowkey data, y = fs.peep(path)
    fr y != ghosted {
        bet ghosted, y.tea("couldn't load save")
    }
    bet parse(data), ghosted
}
```

- **`yikes`** is the error type (analogous to Go's `error` interface). An error value carries a message.
- **`.tea()`** returns/wraps the error's message: calling `y.tea("context")` returns a new `yikes` wrapping the old one with added context (mirrors Go's `fmt.Errorf("...: %w", err)`). The tea is what actually happened.
- **`ghosted`** is the nil error. The core idiom is `fr y != ghosted { ... }`.
- **`yeet(msg)`** panics — for unrecoverable states only. **`sheesh`** recovers at a boundary (e.g., top of the frame loop). Rare and discouraged, exactly like Go's panic/recover.
- **Proposed sugar (open question):** `bounce y` — early-returns zero values plus the error, collapsing the three-line check. Motivated by Go's most common ergonomic complaint; game code checks a lot of errors.

---

## 7. Memory Model (the differentiator)

### 7.1 Motivation

- **Go's gamedev problem:** GC pauses; no way to express "logically dead" — pointers pin dead objects alive, and liveness checks are unenforceable conventions (`.Alive` flags).
- **Rust's gamedev problem:** the borrow checker forbids the dense, cyclic object graphs games are made of. The ecosystem's universal workaround is generational indices (slotmap/ECS) — rebuilt in userspace, unenforced by the compiler.
- **Unity's answer:** destroyed-object detection via overloaded `==` ("fake null") — safe but magical, expensive (engine call per check), and GC-taxed.

This language's position: **references are plain data, validity is control flow, memory is arenas.** The industry-standard pattern, absorbed into the language.

### 7.2 Cribs (arenas)

```
crib frameCrib                      // untyped arena (bump allocator)
crib enemies: Enemy[1000]           // typed crib: slotted slab of Enemies

lowkey p = cop Player{ hp: 100 } in frameCrib
evict frameCrib                     // O(1) mass free
```

- `cop ... in <crib>` allocates into a crib.
- `evict <crib>` frees everything at once and **bumps every slot generation**, so all outstanding tags into it safely become ghosted (see 7.4). O(1) mass-free *with* use-after-free safety.
- `mem.scratch()` is a built-in per-frame arena, auto-evicted at frame end — maps directly onto game loops (per-frame allocations are free to clean up).
- **Allocator context (Odin-inspired):** functions implicitly receive the current allocator so libraries respect the caller's memory strategy. (**Open question:** exact mechanism — implicit parameter vs. context struct.)

### 7.3 Tags (generational handles)

`cop` into a **typed crib** returns a `tag T`, not a pointer:

```
lowkey e: tag Enemy = cop Enemy{ hp: 50 } in enemies
```

A `tag T` is **8 bytes: slot index + generation counter.** It is plain, copyable data — storable in any struct, passable anywhere, held for any duration. No lifetimes, no borrow checking, no GC tracing. Cross-references and cycles are trivially fine because a tag is just numbers.

Generation counters exist because slots are reused: when Enemy #7 (gen 3) dies and a new enemy spawns into slot 7 (gen 4), stale tags still say gen 3 and are safely detected as dead — preventing the "attack an innocent bystander" class of bug.

### 7.4 holla / ghosted (checked access)

The **only** ways to reach the data behind a tag are `holla` (checked) or `trust()` (explicitly unchecked). Bare dereference of a tag is a compile error.

```
finna tick(b: Bullet, enemies: crib Enemy) {
    holla t = b.target in enemies {
        t.hp -= b.dmg          // t: guaranteed-live reference, scoped to this block
    } ghosted {
        b.live = cap           // target died or slot was reused; bullet fizzles
    }
}
```

Semantics: look up `tag.slot` in the named crib; if the slot is occupied **and** `slot.generation == tag.generation`, bind a scoped reference and run the first block; otherwise run the `ghosted` block. Dangling references become a normal, representable control-flow case — not UB, not a crash, not a zombie.

Desugared reference model (C#-flavored):

```csharp
ref var slot = ref enemies.slots[t.slot];
if (slot.occupied && slot.generation == t.generation) { /* holla block */ }
else { /* ghosted block */ }
```

Cost: one array index + one integer comparison. Equivalent to Unity's `if (target != null)` idiom, but honest, compiler-enforced, and nearly free.

### 7.5 trust() (unchecked escape hatch)

```
b.target.trust() in enemies      // no generation check
```

For hot inner loops where liveness is structurally known. **Debug builds check anyway and `yeet` on violation; release builds compile to a raw indexed load.** Deliberately ugly and greppable.

### 7.6 Long-lived shared objects

For the minority of data that doesn't fit arenas: reference counting with optional cycle detection, or an **opt-in** incremental GC with a pause budget — never in the default path. (**Open question:** which, and v1 or later.)

---

## 8. Concurrency

- `slide` spawns a lightweight concurrent task (goroutine-flavored); runtime owns the scheduler.
- Tags are plain data and cross threads freely; **open question:** the `holla` story across threads — which thread owns a crib, and whether cribs are single-threaded, locked, or sharded.

---

## 9. Standard Library

### 9.1 Naming conventions (binding rules)

1. Module names are slang **nouns** for the domain; short, lowercase.
2. Function names are slang **verbs** where a good mapping exists; plain short verbs otherwise. Never force it.
3. One concept, one word, everywhere (`peep` = read in `fs`, `net`, `io`; `yeet` = delete/discard; `ghost` = disconnect/absent).
4. Predicates read as slang checks: `.sus()` for validity checks.
5. Domain math keeps conventional names (`sin`, `cos`, `dot`, `cross`, `norm`).

### 9.2 Module map (draft)

| Module | Domain | Sample API |
|---|---|---|
| `spill` | print/format | `spill.it(x)`, `spill.f("hp: {}", hp)` |
| `fs` | file system | `fs.peep(path)` read · `fs.drop(path, data)` write · `fs.yeet(path)` delete · `fs.pullUp(dir)` list |
| `str` | strings | `str.snip`, `str.glow` (upper), `str.chill` (lower), `str.split`, `str.slaps(a,b)` equals |
| `math` | math | `math.clout(x,n)` pow · `math.root` · `math.cook(seed)` RNG · conventional `sin/cos/...` |
| `squadops` | collections | `.stack(x)` push · `.pop()` · `.snatch(i)` remove-at · `.gang()` length · `.vibeCheck(pred)` filter · `.glowUp(fn)` map |
| `time` | time | `time.rn()` now · `time.chill(ms)` sleep |
| `net` | networking | `net.slideInto(addr)` connect · `net.yap(msg)` send · `net.peep()` receive · `net.ghost()` disconnect |
| `mem` | memory | `mem.crib(size)` · `mem.evict` · `mem.scratch()` · `mem.receipts()` allocation stats |
| `gg` | game loop/input | `gg.frame()` · `gg.dt()` · `gg.keys.pressed(k)` |
| `vec` | vec2/3/4, mat4, SIMD | conventional: `vec.add`, `vec.scale`, `vec.dot`, `vec.cross`, `vec.norm` |

Every stdlib API is **allocator-aware** (accepts/uses the current allocator context) — mandatory, not optional.

---

## 10. Reference Examples

### 10.1 Basics

```
pull "math"
pull "spill"

drip Player {
    flex hp: int
    flex pos: vec3
}

finna damage(p: Player, amt: int) -> int {
    fr p.hp - amt <= 0 {
        spill.it("player down!")
        bet 0
    } naw {
        bet p.hp - amt
    }
}
```

### 10.2 Game loop with scratch arena

```
pull "gg"
pull "vec"

drip Bullet {
    flex pos: vec3
    flex vel: vec3
    flex live: bool
}

finna tick(bullets: []Bullet, dt: float) {
    squad b in bullets {
        fr !b.live { skip }
        b.pos = vec.add(b.pos, vec.scale(b.vel, dt))
        fr b.pos.y < 0.0 { b.live = cap }
    }
}

finna main() {
    lowkey bullets = squadops.new(Bullet, in: mem.scratch())

    vibin gg.frame() {
        fr gg.keys.pressed("space") {
            bullets.stack(cop Bullet{
                pos: vec3(0, 1, 0),
                vel: vec3(0, 0, 30),
                live: nocap
            } in mem.scratch())
        }
        tick(bullets, gg.dt())
    }
}
```

### 10.3 Tags and holla

```
crib enemies: Enemy[1000]

drip Bullet {
    flex target: tag Enemy
    flex dmg: int
    flex live: bool
}

finna tick(b: Bullet, enemies: crib Enemy) {
    holla t = b.target in enemies {
        t.hp -= b.dmg
    } ghosted {
        b.live = cap
    }
}
```

### 10.4 Errors

```
finna loadSave(path: str) -> (SaveData, yikes) {
    lowkey data, y = fs.peep(path)
    fr y != ghosted {
        bet ghosted, y.tea("couldn't load save")
    }
    bet parse(data), ghosted
}
```

---

## 11. Implementation Plan

### 11.1 Stack (decided)

- **Compiler & runtime language: Rust.** One language for both; `inkwell` for LLVM bindings; ecosystem (`logos`, `chumsky`, `salsa`, `tower-lsp`, native Cranelift) is the strongest available; painless cross-platform CI via Cargo.
- **Backend: LLVM (pinned version), via inkwell.** Release builds through LLVM's optimizer; `lld` bundled for linking; DWARF/PDB debug info; runtime statically linked into every binary.
- **Cranelift slot reserved** for fast debug builds later.
- **Own mid-level IR between frontend and LLVM** (à la Rust MIR / Swift SIL). Hosts language-specific passes — arena lifetime analysis, allocator inlining, hoisting `holla` generation checks out of loops, fusing repeated `holla`s, SoA crib layout — and isolates the frontend from LLVM API churn while making Cranelift pluggable.
- **Runtime:** Rust, `no_std`-flavored static library. Owns allocators/cribs/generations, scheduler for `slide`, per-OS syscall layer (libc on macOS/Windows; Go's raw-syscall breakage on macOS is the cautionary tale), signals, startup/shutdown.

### 11.2 Phase 0 contracts (blocking, ~2-6 weeks)

1. **Language spec** (this document, formalized: grammar in EBNF, full semantics).
2. **Mid-level IR definition** — the frontend/backend contract.
3. **Runtime ABI** — runtime entry points (allocation, crib push/evict, holla intrinsics, coroutine yield). LLVM handles per-platform calling conventions, simplifying this doc.

### 11.3 Parallel projects

| # | Project | Notes |
|---|---|---|
| 1 | **Frontend** | lexer → parser → AST → type check → mid-IR. **Built as a library** so tooling reuses it (Go's retrofit of `go/types` is the lesson). |
| 2 | **Backend/toolchain** | mid-IR → LLVM IR lowering; pass pipeline; lld linking; 6 target triples; debug info. Shrunk to 1-2 people by the LLVM decision. Can start from hand-written IR test cases immediately. |
| 3 | **Runtime & memory** | The differentiator and hardest project: cribs, generations, scratch arenas, allocator context, scheduler, OS layer. |
| 4 | **Standard library** | Allocator-aware everything; early parts in Rust, later parts self-hosted. |
| 5 | **Prototype interpreter** | Tree-walking, weeks not months; validates that the slang *feels good* before codegen exists; later the REPL. Runs fully in parallel. |
| 6 | **Tooling** | Formatter at v0.1 (Go's lesson: canonical formatting kills style wars); LSP (`tower-lsp`); build system; package manager (spec in parallel from day one). |
| 7 | **Tests & CI** | Conformance suite written from the spec by the spec authors; 3-OS × 2-arch CI matrix; **differential testing interpreter-vs-compiled** (excellent bug-finder); pause-time & allocation benchmarks to catch memory regressions. |

### 11.4 Milestones

1. Phase 0 docs frozen; interpreter + CI spun up (weeks 1-6).
2. "Hello world" compiles and runs on all three OSes (first forced integration of frontend + backend + runtime).
3. Arena allocator end-to-end (`crib`/`cop`/`evict` through real codegen).
4. `tag`/`holla` end-to-end with debug-mode `trust()` checking.
5. Self-hosted formatter.
6. Pong demo; then a demo with real allocation pressure as the true stress test.

---

## 12. Open Questions

Most of these were resolved by implementation through Milestones 3–6; the five on the
critical path were ratified early in `plan-amendment-02.md` (SP0). **RESOLVED** items are
frozen for v1; the short remaining list is what the Milestone-7 freeze still tracks.

**Syntax/semantics**
- **RESOLVED — Statement termination:** Go-style ASI; a statement ends at a newline unless an
  open bracket/binary-operator continuation is pending.
- **RESOLVED — Default visibility:** no modifier means `hush` (module-private); `flex` exports.
- **RESOLVED — Numeric tower:** `int`/`uint` (64-bit), `i8/i16/i32/i64`, `u8/u16/u32/u64`,
  `float`/`f32`/`f64`; unsigned wraps, signed traps in debug (§2.4), explicit
  `math.lap`/wrapping ops otherwise.
- **RESOLVED — Generics:** yes for v1; `finna f[T](..)` / `drip S[T]` / `stash[K,V]`, erased in
  the IR via ahead-of-time monomorphization.
- **RESOLVED — Method syntax:** dot-defined receivers (`finna (self: T) name(..)`); interfaces/
  traits are explicitly *out* for v1 (function-pointer struct fields cover the dispatch needs,
  as the doom-thinker oracle shows).
- **RESOLVED — `bounce y`:** adopted (early-returns zero values plus the error).

**Memory model**
- **RESOLVED — Typed vs. untyped cribs:** typed cribs hand back generational `tag`s reached
  through `holla`; untyped bump cribs hand back a live `ref` directly. Heterogeneous `tag any`
  is *out* for v1.
- **RESOLVED — Allocator context:** an ambient thread-local allocator context with an
  `in: <crib>` override (SP0.1; ratifies the existing `bet_ctx_*` ABI).
- *Open:* long-lived shared objects — refcounting vs. opt-in budgeted GC. Deferred past v1; the
  arena/`tag` model covers the game-loop workloads without it.
- *Open:* threading model for cribs and cross-thread `holla` (single-thread-per-crib is the
  working assumption; revisited when the M:N scheduler lands).

**Ecosystem/bit**
- **RESOLVED — Language name:** `bet`.
- **RESOLVED — LLVM pin:** LLVM 18 (`llvm-sys` 18, `inkwell` `llvm18-0`).
- *Open:* how far toward a native ECS the `squad`-over-live-slots query primitive should go
  (candidate feature #2, post-freeze).
- *Open:* package-manager model (registry vs. git-based). Out of scope for the language freeze.
