# Plan Amendment 02: SP0 — Critical-Path Freeze Decisions (pulled forward)

> **Amends:** Language Specification (Draft v0.1) §12, and Plan Amendment 01 §8.
> **Status:** Proposed working resolutions. These are **ratified at Milestone 7 (language
> freeze)**; they are pulled forward now so the parallel tracks in the "Open Doom's Gate"
> plan don't stall on undecided design questions. On ratification, fold into the spec.
> **Summary:** Closes the five §12/§8 questions that sit on the critical path —
> allocator-context mechanism, `str.fromBytes` validity, `rawptr` conversion rules, `gg`
> surface scope, and wrapping-arithmetic naming. The remaining open questions stay open
> until the M7 freeze gate (they are not on any track's critical path).

Rationale for pulling these forward (SP0): per amendment-01 §7, "a self-hosted compiler
makes every language change ~2× more expensive." But three of the five below are *already
half-built into the contracts* (`rt-abi` ships the allocator-context and platform-layer
entry points), and the other two block Track 1/Track 2 the moment they start. Deciding them
now — as working resolutions, ratified later — costs nothing and unblocks the fan-out. None
of these decisions changes a `midir`/`rt-abi` signature; they resolve *surface* and *policy*.

---

## SP0.1 — Allocator-context mechanism → **implicit ambient context**

**§12 question:** "Allocator-context mechanism (implicit param vs. context struct)."

**Decision:** An **implicit, ambient, thread-local allocator context** (Odin-inspired), with
an explicit **`in: <crib>`** override that scopes a single allocation (or block) to a named
allocator. Not a threaded explicit parameter; not a context struct passed by hand.

**Why (this ratifies what is already built):**
- `crates/rt-abi/src/lib.rs` already ships `AllocCtx(*mut c_void)` + `bet_ctx_current()` /
  `bet_ctx_push(ctx)` / `bet_ctx_pop()` (lib.rs:58, 113–117), and `crates/runtime/src/lib.rs`
  already implements a per-thread context stack. The mechanism exists; this decision names it.
- Spec §7.2 already states functions "implicitly receive the current allocator." Implicit
  ambient context is the spec's own model.
- Cross-thread safety falls out for free: each thread owns its context stack, so ambient
  context never crosses a `slide` boundary implicitly (it collides with nothing in the
  still-open cross-thread-crib question — that question stays open, this doesn't force it).

**Surface / lowering:**
- Default allocation (`cop`, `stash.new`, slice/`str` growth) targets `bet_ctx_current()`.
- `stash.new[K,V](in: astCrib)` and `cop X{…} in someCrib` lower to: `bet_ctx_push(astCrib)`
  → run the constructor/allocation → `bet_ctx_pop()` (or the allocation primitive takes the
  crib handle directly where one already does, e.g. `cop … in <typed crib>`). The `in:` form
  is the *only* way to redirect; there is no ambient mutation from bet source without it.
- Implementation lands in `frontend/src/lower.rs` (the `in:`-scoped push/pop) as part of the
  `fnd-alloc` foundation task; no ABI change.

**Closes:** §12 "allocator-context mechanism." (Heterogeneous/`tag any` cribs remain a
separate open question — untouched here.)

---

## SP0.2 — `str.fromBytes` UTF-8 validity → **checked by default, greppable unchecked escape**

**§8 question (amendment-01):** "`str` byte-slicing validity rules (UTF-8 enforcement at the
`fromBytes` boundary?)."

**Decision:**
- `str` is **guaranteed valid UTF-8** internally. `str.bytes()` is always zero-copy (the safe
  direction) and returns `[]u8`.
- `str.fromBytes(b: []u8) -> (str, yikes)` **validates** and returns an error (Go-style, per
  spec §6) on invalid UTF-8. This is the default, safe constructor.
- `str.fromBytesTrust(b: []u8) -> str` is the **unchecked** escape hatch — deliberately ugly
  and greppable, gated exactly like `trust()` (debug builds validate and `yeet` on violation;
  release compiles to a zero-copy reinterpret). For the lexer / WAD hot paths that already
  hold the invariant.

**Why:** matches bet's existing two-tier safety philosophy (`holla`/`trust`, bounds-checked
`bytes.cast` vs. its unsafe variant). The safe path keeps `str` honest; the greppable
unchecked path keeps the "a lexer must not allocate per token" requirement (amendment-01
§2.7) satisfiable without a validation pass per token.

**Surface / lowering:** lands in the `bytes-io` task (`frontend/src/lower.rs` + interp
`call_str`), which already has partial `str.*`/`bytes.*` handling in the interpreter.

**Closes:** §8 "`str` byte-slicing validity rules."

---

## SP0.3 — `rawptr` conversion rules → **explicit, greppable, extern-only**

**§8 question:** "`rawptr` conversion rules at the FFI boundary — exactly how ugly, exactly
how greppable."

**Decision:**
- `rawptr` is **produced and consumed only in `extern`-adjacent code**. It is not a
  general-purpose pointer; you cannot form one from a bet value with an `as` cast.
- Crossing between `rawptr` and a bet value is always an **explicit, greppable call**, never
  an implicit coercion:
  - `bytes.fromRaw(p: rawptr, len: int) -> []u8` (bounds are the caller's assertion — gated
    like `trust()`; debug builds cannot verify length, so this is the unsafe surface).
  - `bytes.toRaw(b: []u8) -> rawptr` (safe direction — a slice already has a base+len).
  - No `rawptr` → `tag`/`drip`/`ref` conversion exists at all. Handles never come from FFI;
    they come from `cop`. This keeps the generational-safety story airtight.
- **No pointer arithmetic on `rawptr` in bet source.** Offset math is done in `[]u8` space via
  `bytes.slice`/`bytes.readU32le`. `rawptr` is opaque between the `extern` call and the
  `bytes.fromRaw` that immediately domesticates it.

**Why:** same philosophy as `trust()` — the escape hatch is conventional (design principle 5),
ugly, and `grep rawptr` / `grep fromRaw` finds every unsafe boundary. Confining `rawptr` to
`extern` adjacency means the memory model's safety guarantees hold everywhere else by
construction. `midir` already has `TyKind::RawPtr` and the backend maps it to an LLVM `ptr`;
no IR/ABI change.

**Surface / lowering:** lands in the `ffi-rawptr` task alongside the interp FFI shim.

**Closes:** §8 "`rawptr` conversion rules."

---

## SP0.4 — `gg` surface scope for v1 → **framebuffer + audio ring + input + timing; GPU out**

**§8 question:** "How much of SDL-equivalent surface `gg` absorbs in v1 (framebuffer + audio
ring + input is the floor; GPU is explicitly out)."

**Decision — the floor *is* the ceiling for v1.** `gg` v1 is exactly the four entry points
already reserved in `rt-abi` (lib.rs:184–190), no more:
- `gg.blit(fb)` — present a **software-rendered 32-bit RGBA `FrameBuffer`** (the existing
  `FrameBuffer{pixels,width,height,stride}`), single window.
- `gg.audio(ring)` — push interleaved audio frames to a ring buffer.
- `gg.poll() -> Event` — pump **keyboard + mouse-move + quit** events (the existing
  `Event`/`event_kind::{KEY_DOWN,KEY_UP,MOUSE_MOVE,QUIT}` set).
- `gg.ticks() -> u64` — monotonic hi-res timing (already a real clock in both stub + runtime).

**Explicitly OUT of v1:** GPU/3D acceleration, shaders, multiple windows, window resizing
semantics beyond a fixed backbuffer, gamepad, networking. Doom software-renders to a
framebuffer — the floor is sufficient for the oracle.

**Why:** the ABI shape is already frozen to exactly this; v1 is an *implementation* of the
reserved surface (the `gg-platform` task: real macOS + Linux backends behind the headless
stub), not an expansion of it. Keeping the surface minimal protects the "gg implemented
inside the runtime" promise and the freeze.

**Closes:** §8 "how much SDL-equivalent surface `gg` absorbs."

**Amendment (post-M6, Pong dynamic resolution):** a **fifth** primitive, `gg.size() -> (w, h)`
(`bet_gg_size`, packed `w<<32|h`), was added. A program that renders at the window's native
resolution (fills/resizes the window, no upscaling) must learn the drawable size, which the
original four cannot report. This is the one deliberate expansion of the SP0.4 floor; the
"framebuffer + audio + input + timing + size" set is the new ceiling. GPU/shaders/multi-window
remain out.

---

## SP0.5 — Wrapping-arithmetic naming → **`math.wrapAdd` / `wrapSub` / `wrapMul`**

**§8 question:** "Wrapping-arithmetic naming (`math.lap` is weak)." (Amendment-01 §5 marks
`math.lap` "weak — candidates welcome.")

**Decision:** Retire `math.lap`. The explicit wrapping-arithmetic family is
**`math.wrapAdd(a, b)`, `math.wrapSub(a, b)`, `math.wrapMul(a, b)`** — one honest name per
operation, conventional (these are escape-hatch ops under design principle 5, not slang),
and greppable. Available in every build and for every integer signedness, lowering to
`midir` `BinOp` with `ArithMode::Wrap`.

**Why:** `lap` obscured which operation wrapped and read as slang where the amendment says
these must stay conventional. `wrapAdd`/`wrapSub`/`wrapMul` mirror Rust's `wrapping_add`
family that every systems programmer already knows — no surprises, exactly the intent of the
numeric-tower resolution (amendment-01 §2.4).

**Migration:** `frontend/src/lower.rs:1463-1466` currently wires only `("math","lap")`. Land
the `math.wrap*` intrinsics there (and add the missing `math` module to the interpreter —
tracked in `compiled-path`), then update the corpus program `09-bit-math/wrapping.bet` and
its `.expected`. Part of the `compiled-path` task's §2.4 slice.

**Closes:** §8 "wrapping-arithmetic naming."

---

## Updated open-question tracker (supersedes amendment-01 §8 for these five)

**Now closed (SP0 working resolutions, ratified at M7):** allocator-context mechanism →
implicit ambient context (SP0.1); `str.fromBytes` validity → checked default + greppable
unchecked (SP0.2); `rawptr` conversion → explicit/greppable/extern-only (SP0.3); `gg` v1
surface → framebuffer+audio+input+timing, GPU out (SP0.4); wrapping-arith naming →
`math.wrap*` (SP0.5).

**Still open until the M7 freeze gate (deliberately — not on any track's critical path):**
`moods`/`vibe` naming & exact pattern syntax, closure capture (v2/never), interfaces/traits
story, long-lived shared objects (RC vs. budgeted GC), heterogeneous/`tag any` cribs,
cross-thread crib semantics, ASI fine print, package-manager model, LLVM version pin,
ECS-flavored `squad` queries, and **the language name** (the repo is already `bet`; treat
"keep `bet`" as the presumptive resolution to ratify at freeze).
