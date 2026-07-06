# bet — dynamic semantics

> **Status:** Normative for **runtime (dynamic) semantics.** Surface *syntax* is frozen in
> `grammar.ebnf` and `syntax-decisions.md`; the *static* decisions (types, visibility) live in
> `language-spec.md §5–§8` and `plan-amendment-01.md §2`. This document fixes how a well-typed
> `bet` program **behaves** — evaluation order, the numeric/overflow rules, the memory model
> (`cop`/`evict`/`tag`/`holla`), error propagation (`bounce`, `yeet`/`sheesh`), pattern
> dispatch (`vibe`), and concurrency.
>
> **The implementation is the ground truth.** Every rule below is realized by two tested
> contract crates and cites them: **`crates/midir`** (how the construct lowers in the IR) and
> **`crates/rt-abi`/`crates/rt-stub`** (the runtime entry point it calls). Where this document
> and the code disagree, that is a bug in one of them — report it. Where this document and the
> *grammar* appear to disagree, the grammar wins on syntax and this document wins on behavior;
> neither restates the other.
>
> Open questions are collected in §8 and are **not** resolved here.

---

## 1. Evaluation model

1. **Eager, left-to-right.** Operands, call arguments, and the elements of an `exprList` (in
   multi-value binds `a, y = f()`, returns `bet x, y`, and assignments) are evaluated fully,
   in source order, before the enclosing operation. There are no lazy values in v1.

2. **Value semantics (spec §5).** A `drip` is a value: binding, assignment, argument passing,
   and returning all **copy** it. Fixed arrays `T[N]` and tuples copy likewise. The only
   reference forms are the generational handle `tag T` (itself an 8-byte *value* — see §3) and
   the scoped `ref` a `holla` binds; there are no free pointers outside the FFI `rawptr`
   boundary (§6, spec §7.5). Assignment through a place (`p.field = v`, `xs[i] = v`) mutates in
   place.

2a. **Struct-literal zero-defaults.** A struct literal (bare `T{…}` or `cop T{…} in c`) may
   omit any subset of fields; each omitted field takes its type's **zero value**: integers `0`,
   floats `0.0`, `bool` `cap`, `str` `""`, nested drips recursively zeroed, fixed arrays
   element-wise zeroed, `tag T` the null tag (always ghosted under `holla`, §3.4), slices the
   empty `{ null, 0 }` fat value, and handle-shaped fields (fn values, `vec`, `stash`, `rng`,
   `rawptr`, `crib`) the null handle — safe to hold and to overwrite with a real handle later; a
   *use* before that (calling / pushing / indexing) is a crash, exactly like a zeroed handle in
   C. A `moods` field has no zero value and must be initialized explicitly. `cop` writes every
   declared field into the fresh slot (given or defaulted), so a reused slot never leaks its
   previous occupant's bytes. `12-doom/gamestate-crib` pins the whole rule.

3. **Short-circuit `&&` / `||`.** Logical operators short-circuit and therefore desugar to
   control flow, not to a binary operator (this is why `midir` has bitwise `BitAnd`/`BitOr`
   but **no** logical binop — it lowers these to `Branch`):

   ```text
   a && b   ≡   fr a { b } naw { cap }        (b evaluated only when a is nocap)
   a || b   ≡   fr a { nocap } naw { b }       (b evaluated only when a is cap)
   ```

4. **Statement termination** is Go-style ASI, already fixed in `syntax-decisions.md §2`; not
   restated here.

---

## 2. Types, casts, and numeric behavior

References: spec §5, amendment §2.4, `midir::CastKind` / `midir::ArithMode`.

1. **Defaults & tower.** An untyped integer literal is `int` (= `i64`); an untyped float is
   `float` (= `f64`). The sized tower is `i8 i16 i32 i64 u8 u16 u32 u64 f32 f64`.

2. **No implicit narrowing, ever.** Every cross-type numeric conversion is written `x as T`.
   Each legal `as` lowers to exactly one `midir::CastKind`:

   | Conversion | `CastKind` |
   |---|---|
   | narrower int → wider, unsigned source | `IntZext` |
   | narrower int → wider, signed source | `IntSext` |
   | wider int → narrower | `IntTrunc` (keeps the low bits) |
   | int → float | `IntToFloat` |
   | float → int | `FloatToInt` (truncates toward zero) |
   | `f32` ↔ `f64` | `FloatResize` |
   | same-size reinterpret (FFI / `bytes.cast`) | `Bitcast` |

3. **Overflow — the load-bearing numeric rule.** Integer arithmetic carries an *overflow
   mode* (`midir::ArithMode`):
   - **Unsigned arithmetic wraps** modulo `2^width` — this is *defined* behavior, not an
     error. `ArithMode::Wrap`. Load-bearing for BAM angles: `09-bit-math/bam-angles` relies on
     `u32` addition wrapping (`ANG270 + ANG180 == ANG90`), and `09-bit-math/wrapping` shows
     `(250 + 10): u8 == 4`.
   - **Signed arithmetic traps** on overflow **in debug** builds (a `yeet`) and **wraps in
     release**. `ArithMode::Trap`. Because signed `+` would trap, a program that *wants* signed
     wrapping asks for it explicitly.
   - **`math.lap(a, b)`** (and its family) is explicit wrapping in *any* build, for either
     signedness — `ArithMode::Wrap`. `09-bit-math/wrapping` uses `math.lap(100i8, 100i8) ==
     -56` (200 wraps in `i8`).
   - Float and boolean/bitwise/comparison operations carry `ArithMode::Na`.

4. **Result types.** Comparisons (`== != < <= > >=`) yield `bool`. Bitwise (`& | ^ ~`) and
   shifts (`<< >>`) yield the operand integer type. Division/remainder by zero is a `yeet`
   (checked in every build; it is not "defined" like overflow).

---

## 3. Memory model — cribs, tags, `holla`, `evict`, `trust`

The differentiator (spec §7). This section gives an abstract-machine state and
**state-transition rules**; each maps to a `rt-abi` entry point (`crates/rt-stub` is the
reference implementation, `crates/rt-abi` the contract).

### 3.1 State

A **typed crib** `c` of element type `T` and capacity `N` is an array of slots:

```text
c.slot[i] = (occ_i : bool, gen_i : u32, store_i : T)        for i in 0 .. N
```

An **untyped bump crib** is a pair `(buffer, offset)`. `mem.scratch()` is a thread-local bump
crib, auto-reset at the frame boundary. Cribs are created by `bet_crib_new` (typed) /
`bet_crib_new_bump` (untyped) and destroyed by `bet_crib_free`.

A **`tag T`** is an 8-byte value `{ slot : u32, generation : u32 }` (`rt_abi::Tag`). It is
plain, copyable data — storable in any `drip`, passable anywhere, held for any duration. It is
*not* a pointer and cannot be dereferenced directly; that is a compile error (spec §7.4).

### 3.2 `cop` — allocate

```text
cop v in c        (typed crib c)
  choose the least i with ¬occ_i
  occ_i := true ;  store_i := v
  ⟹ value  tag{ slot = i, generation = gen_i }        [midir Cop → rt-abi bet_cop + write]
```

If no slot is free, the result is the sentinel `Tag::NULL` (`{ u32::MAX, 0 }`), which is
*always* ghosted (§3.4). A typed crib is a fixed slab, so a well-formed program sizes it to
its working set (as Doom sizes its mobj array); allocating past capacity is a program error
surfaced as an always-dead handle, not memory corruption. `cop … in <bump crib>` instead bumps
`offset` and yields a `rawptr`-flavored slot (`bet_bump_alloc`); bump allocations have no
generation and are freed only en masse.

### 3.3 `evict` — O(1) mass-free that invalidates every tag

```text
evict c           (typed crib c)
  for every i:  occ_i := false ;  gen_i := gen_i + 1   (mod 2^32)     [rt-abi bet_evict]
```

Bumping every generation is the whole trick: it is O(1), and it makes **every tag handed out
before the evict stale** (§3.4) without touching those tags. For a bump crib, `evict` resets
`offset := 0`.

### 3.3a `evict tag in crib` — per-slot free

The single-slot form (the operation Doom performs every time a mobj dies):

```text
evict t in c      (typed crib c)
  fr valid(c, t):   occ_{t.slot} := false ;  gen_{t.slot} := gen_{t.slot} + 1  (mod 2^32)
  naw:              no-op                                        [rt-abi bet_evict_slot]
```

Guarded by the same `valid` rule as `holla` (§3.4), so it is **idempotent and alias-safe**: a
stale tag, the null tag, an already-freed slot, and a double evict are all no-ops, and evicting
through one copy of a tag ghosts every other copy (the generation bump). The freed slot returns
to the crib's free pool, so a later `cop` reuses it — at the **new** generation, which is why
the stale tag can never resolve to the slot's next occupant (`12-doom/evict-slot` pins this).
On a bump crib the statement is a no-op (bump allocations are freed only en masse).

### 3.4 `holla` / `ghosted` — checked access

A tag `t` is **valid against** crib `c` iff its slot is occupied *and* the generations agree:

```text
valid(c, t)  ≝  occ_{t.slot} ∧ (gen_{t.slot} = t.generation)
```

The only checked way to reach the data behind a tag desugars to:

```text
holla x = t in c { L } ghosted { G }
  ≡   fr valid(c, t) {
          let x = &store_{t.slot}     // x : ref T — a scoped, NON-ESCAPING borrow
          L
      } naw {
          G
      }
```

`x` is live for exactly the `L` block and must not escape it (stored into a longer-lived place,
returned, etc.) — enforced statically. This lowers to a single `midir::Terminator::HollaCheck`
(which binds the resolved `ref` on the live edge) over `rt_abi::bet_holla_check`, whose one
job is to return the element pointer when `valid`, else null. Cost: one indexed load + one
integer compare.

**Generation-reuse guarantee.** After `evict c`, any tag `t` produced before the evict has
`t.generation < gen_{t.slot}`, so `¬valid(c, t)` — it ghosts. A slot reused by a later `cop`
carries the *new* generation, so only the fresh tag resolves. `08-memory/generation-reuse`
pins this exactly: id `7` (fresh) → `evict` → id `42` (reused slot, new gen) → the stale tag
resolves to `-1` (ghosted), never to the new occupant. The `rt-stub` `generation_reuse` test
reproduces the same `7 / 42 / -1`.

### 3.5 `trust()` — unchecked escape hatch

`t.trust() in c` skips the generation check. It is deliberately ugly and greppable. In **debug**
builds it performs `valid(c, t)` anyway and `yeet`s on violation; in **release** builds it
compiles to a raw indexed slot load (`rt_abi::bet_slot_ptr`, no check). Use only where liveness
is structurally guaranteed.

### 3.6 Allocator context

Every stdlib allocation is allocator-aware: it uses the **current allocator context**, a
thread-local the runtime exposes as `bet_ctx_current` / `bet_ctx_push` / `bet_ctx_pop`
(Odin-style). The *surface* mechanism by which a function receives it (implicit parameter vs.
explicit context struct) is **open** (§8); the ABI-level stack is fixed.

---

## 4. Error handling

References: spec §6, amendment §2.8.

1. **Errors are values.** A fallible function returns `(T…, yikes)` — the error is the last
   result. `ghosted` is the nil error. The canonical check is `fr y != ghosted { … }`.

2. **`yikes` construction & wrapping.** `yikes.new(msg)` builds an error. `y.tea(ctx)` returns
   a *new* `yikes` that wraps `y` with added context — the chain is what actually happened
   (mirrors Go's `%w`). A `yikes`'s default display (what `spill.it` prints) is its message
   chain; e.g. `07-errors/bounce` prints `negative` for `yikes.new("negative")`.

3. **`bounce y` desugaring.** For a function whose declared return is `(T_1, …, T_k, yikes)`:

   ```text
   bounce y   ≡   fr y != ghosted { bet zero(T_1), …, zero(T_k), y }
   ```

   where `zero(T)` is the zero value of `T`: `0` for integers, `0.0` for floats, `cap` for
   `bool`, `""` for `str`, `ghosted` for any `tag`/`ref`/`yikes`, and field-wise `zero` for a
   `drip`. It lowers to a `Branch` plus a `Return`. `07-errors/bounce` (and
   `11-reference/mini-compiler`) exercise it: `pipeline(3)` threads through to `12`, while
   `pipeline(-1)` bounces the error out and prints `negative`.

4. **`yeet` / `sheesh`.** `yeet(msg)` is an unrecoverable panic → `rt_abi::bet_panic`
   (`midir::Terminator::Panic`, followed by `Unreachable`). `sheesh { … } naw e { … }` is a
   recovery boundary → `bet_recover_begin` / `bet_recover_end`. Recovery is rare and
   discouraged (Go's `recover`); **in `rt-stub` `bet_panic` aborts and the boundary is a
   no-op** — real recovery lands with the `runtime` crate. The `sheesh` binding syntax is still
   provisional (§8).

---

## 5. Sum types & pattern matching (`moods` / `vibe`)

Reference: amendment §2.1; `midir` `Discriminant` / `Switch` / `Downcast`+`Field`.

1. **Construction.** A `moods` value is a variant tag plus that variant's payload. Building one
   (`Add(l, r)`, `Lit(2)`) sets the discriminant and stores the payload; when built with `cop`
   it lives in a crib and is reached by `tag` (as in `11-reference/mini-compiler`).

2. **`vibe` dispatch.** `vibe e { V₁(a…) { … } … [ naw { … } ] }` reads `e`'s discriminant and
   branches to the matching arm, binding that variant's payload fields for the arm body:

   ```text
   vibe e { … }   ⟶   switch discriminant(e) [ tag(Vᵢ) -> armᵢ … ] else naw-arm
                       (in armᵢ, each payload binder = e downcast to Vᵢ, field j)
   ```

   This is a `midir::Terminator::Switch` on `Rvalue::Discriminant`; each arm reads payloads via
   a `Downcast` + `Field` place projection.

3. **Exhaustiveness is mandatory.** A `vibe` that omits a variant is a **compile error** unless
   a `naw { … }` wildcard arm is present (`06-sumtypes/moods-exhaustive`). This is a static
   check; at runtime a complete `switch` always selects an arm.

---

## 6. First-class functions

Reference: amendment §2.5; `midir::TyKind::FnPtr` + `Callee::Indirect`.

A function value is a **plain code pointer** — v1 has **no environment capture**. Function
types (`finna(tag Mobj) -> void`) are usable as field, parameter, and element types. Calling a
function value is an indirect call (`Callee::Indirect`), semantically identical to a direct call
except the callee is computed. `11-reference/doom-thinker` stores a `think` function pointer in
a `drip` and dispatches it each tic. Closures that capture environment are out of scope for v1
(§8).

---

## 7. Concurrency

Reference: spec §8; `rt_abi::bet_slide`.

`slide f(args)` spawns a lightweight task running `f`; the runtime owns the scheduler. Because a
`tag` is plain data, tags cross threads freely. **Which thread owns a crib, and whether
`holla` is sound across threads, is open** (§8) — v1 programs should treat a crib as owned by a
single thread.

---

## 8. Open semantic questions

Carried forward from amendment §8 / spec §12 and **not resolved by this document**:

- **Closure capture** — v1 ships plain function values only; capture is v2-or-never.
- **`str` byte-slice validity** — whether `str.fromBytes` enforces UTF-8 at the boundary.
- **Allocator-context surface mechanism** — implicit parameter vs. explicit context struct
  (the ABI stack in §3.6 is fixed; the surface sugar is not).
- **Long-lived shared objects** — reference counting vs. an opt-in budgeted GC, and whether in
  v1 at all; never in the default path.
- **Cross-thread crib semantics** — ownership and `holla` soundness across threads (§7).
- **Wrapping-arithmetic naming** — `math.lap` is provisional.
- **`sheesh` recovery-binding syntax** — provisional in the grammar; the stub aborts.

---

## Appendix — surface → `midir` → `rt-abi` map

The chain each dynamic construct travels, for cross-referencing:

| Surface | `midir` node | `rt-abi` entry point |
|---|---|---|
| `cop v in c` | `Rvalue::Cop` | `bet_cop` (+ write) / `bet_bump_alloc` |
| `evict c` | `Stmt::Evict` | `bet_evict` |
| `evict t in c` | `Stmt::EvictSlot` | `bet_evict_slot` |
| `holla x = t in c {…} ghosted {…}` | `Terminator::HollaCheck` | `bet_holla_check` |
| `t.trust() in c` | `Rvalue::Trust` | `bet_slot_ptr` (release) |
| `mem.scratch()` | — | `bet_scratch` / `bet_scratch_reset` |
| `vibe e { … }` | `Switch(Discriminant)` + `Downcast`/`Field` | — |
| `a && b` / `a \|\| b` | `Terminator::Branch` | — |
| `x as T` | `Rvalue::Cast(_, _, CastKind)` | — |
| signed `+`, unsigned `+`, `math.lap` | `BinOp(_, _, _, ArithMode)` | — |
| `bounce y` | `Branch` + `Return` | — |
| `yeet(msg)` | `Terminator::Panic` | `bet_panic` |
| `sheesh {…}` | (call-bracketed region) | `bet_recover_begin` / `bet_recover_end` |
| `f(args)` value call | `Rvalue::Call(Callee::Indirect, …)` | — |
| `slide f(args)` | (call) | `bet_slide` |
| `spill.it(x)` | (call) | `bet_print` |

`spill.it` is **println-like**: it appends a trailing `\n` to its argument's display before the
`bet_print` call (so `01-basics/hello` prints `hi\n`). For a string literal the lowering is
`str_ptr`/`str_len` over the newline-terminated bytes (see `tests/mir/hello.mir`); the general
`x` display path is a stdlib concern that arrives with the formatter.
