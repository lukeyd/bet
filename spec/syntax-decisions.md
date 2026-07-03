# bet — settled syntax decisions

> **Status:** Frozen in Step 1c (bootstrap-plan.md §1c), together with `grammar.ebnf`
> and the golden corpus (`tests/corpus/`). These close the *syntax/lexical* bullets in
> language-spec.md §12 and plan-amendment-01.md §8. The normative forms live in
> `grammar.ebnf`; this file records **what** was decided and **why**. Semantic
> resolutions (memory/generation/overflow) continue to live in `semantics.md`.

The bootstrap plan's bet (§1c) was that *writing 30–50 real programs settles the open
syntax questions faster than debate*. It did. Each decision below was forced by a corpus
program that would otherwise be unwritable or ugly; the citation names the driver.

---

### 1. File extension `.bet`

Source files use `.bet`; golden programs are checked in as `name.bet` + `name.expected`.
Matches the repo and language name (CLAUDE.md: "the `bet` repo"). No competing candidate
survived — the language name is itself still the one open *ecosystem* question (spec §12),
but the extension tracks the working name regardless.

### 2. Statement termination — Go-style ASI

Statements are newline-terminated; there are no mandatory semicolons. A newline terminates
the current statement iff the line's last token can end one — an identifier, a literal
(`int`/`float`/`str`/byte/`nocap`/`cap`/`ghosted`), a closing `)` `]` `}`, or a statement
keyword (`bet` `dip` `skip` `bounce`). A line ending in an operator, `,`, `.`, `->`, an
assignment op, or an open bracket continues. Explicit `;` is *accepted* as a separator but
non-idiomatic (the formatter strips it).
**Why:** the spec (§4) already commits to "Go-style automatic termination"; adopting Go's
exact rule verbatim is the no-surprises choice and lets multi-line calls / operator-led
continuations (all over the corpus, e.g. `04-functions`, `11-reference`) parse without
line-continuation characters. Full rule in `grammar.ebnf` §L6.

### 3. Composite literals in statement headers need parens

A struct literal may not be the *unparenthesized* operand of an `fr` / `vibin` / `squad`
header (`fr Player{...}.sus() { … }` is ambiguous with a block). Wrap it: `fr (Player{…}).sus()`.
**Why:** keeps `Ident {` unambiguous between "struct literal" and "start of block" with a
one-token rule — Go's exact resolution. Cheap, and the corpus never actually needs the
parenthesized form, which confirms it isn't a real ergonomic cost.

### 4. Comments — `//` and `/* */`

C-family line and (non-nesting) block comments. **Why:** design principle 3 ("everything
else is boring"). The `01-basics/comments` program pins both forms.

### 5. Numeric literals & the integer tower

Decimal, hex (`0x40000000`), binary (`0b1010`), `_` digit separators; floats `1.5`,
`3.0e8`. An untyped integer literal defaults to `int` (= `i64`); an untyped float to
`float` (= `f64`). Sized types (`i8 i16 i32 i64 u8 u16 u32 u64 f32 f64`) are reached by
annotation or an explicit `as` cast. **No implicit narrowing, ever** — every cross-width
conversion is written `x as u32`.
**Why:** the tower and spellings are already fixed by amendment §2.4; the corpus
(`02-values/numeric-tower`, `casts`; `09-bit-math/*`) forces the literal *forms* — hex and
binary are mandatory for BAM angles and flag fields, `_` separators for readability of
large fixed-point constants.

### 6. String & byte literals

`"..."` are UTF-8 strings (`str`); escapes `\n \t \r \\ \" \' \0 \xNN`. Single-quote
`'A'` is a **`u8` byte literal**, not a character/rune type.
**Why:** the lexer oracle (`11-reference/mini-compiler`) and WAD parsing (`10-stdlib/
bytes-parse`) need cheap byte-level constants for classifying and comparing bytes; a
distinct rune type is scope the amendment doesn't ask for. `str.bytes()` / `str.fromBytes()`
bridge `str` ↔ `[]u8` (UTF-8 validity at the `fromBytes` boundary remains open — amendment §8).

### 7. Operator precedence — C-family, with one deliberate fix

Highest→lowest: postfix (`.` call `[]`) › unary (`! ~ -`) › `as` › `* / %` › `+ -` ›
`<< >>` › `&` › `^` › `|` › comparisons › `&&` › `||`.
**Why:** amendment §2.4 asks for "conventional C-family precedence" for the bit
operators, and this is C's table for the arithmetic/shift/logical bands — **except** that
`&` `^` `|` bind *tighter* than the comparisons (Go's rule), not looser (C's). That single
deviation is **forced by the amendment's own reference code**: §10.6 writes
`fr m.flags & MF_SHOOTABLE == 0 { … }` with no parentheses, which is only correct if
`&` binds tighter than `==` — i.e. `(m.flags & MF_SHOOTABLE) == 0`. It also deletes C's
best-known footgun. `09-bit-math/*` and `11-reference/doom-thinker` depend on it.
Parenthesize when in doubt; the corpus does so where a reader might.

### 8. Program entry & output model

A program's entry point is `finna main()` (no parameters in v1; process args arrive via
stdlib later). A corpus program's `.expected` file is its **exact stdout**. `spill.it(x)`
prints `x` followed by a newline; `spill.f("{}", a)` substitutes each `{}` with the next
argument's default display (`{{`/`}}` escape a literal brace). Corpus programs are
**deterministic** — no wall-clock, no unseeded RNG in any program that has an `.expected`.
**Why:** the corpus is the seed of the conformance + differential-testing harness (Step 2);
stable, hand-computable stdout is the only thing that makes "interp vs compiled agree"
checkable. This is a corpus/harness convention, not a language restriction.

### 9. Restatements (already resolved upstream; fixed here for the grammar)

- **Default visibility is `hush`** (private). `flex` is a deliberate export. (amendment §2.8)
- **Methods use receivers**: `finna (p: Player) damage(amt: int) -> int`. (amendment §2.8)
- **`bounce y`** is adopted early-return-with-error sugar. (amendment §2.8)
- **Generics** are a single bracketed param list, monomorphized, collections-grade only.
  (amendment §2.2)
- **`scratch` is not a keyword** — the per-frame arena is reached as `mem.scratch()`; only
  `crib cop evict tag holla trust` get dedicated memory-model syntax.
- **Maps are `stash[K, V]`**, not `map[K]V`. The vestigial `map[K]V` type production was
  removed from the grammar (v0.1.1) — `map` was never a reserved keyword, and amendment §8
  settled the surface spelling as the generic `stash`. The mid-level IR keeps `TyKind::Map`
  as the lowering target for `stash[K, V]`.

### 10. Collection literals `[a, b, c]`

Slices/arrays have a literal form: `[10, 20, 30]`, `[]u8` bytes as `[0x2A, 0x00, …]`, empty
`[]` (needs a type annotation on the binding). Element type is inferred from the elements.
**Why:** discovered while writing `03-control/squad`, `10-stdlib/*`, and the reference
programs — without a literal, every iteration example degenerates into `squadops.new(...)` +
repeated `.stack(...)`, which buries the feature under allocator ceremony. A literal is
conventional (design principle 3) and allocates into the current allocator context like any
other value. Grammar: `arrayLit` in §S5.

---

### Still open (not settled by 1c — tracked in amendment §8)

`moods`/`vibe` final naming, closure capture (v1 = plain function values only), `str`
byte-slicing UTF-8 validity rules, wrapping-arithmetic naming (`math.lap` is weak),
`rawptr` conversion ugliness at the FFI boundary, `gg` platform-layer surface, interfaces/
traits, long-lived shared objects, cross-thread crib semantics, package manager, LLVM pin,
and **the language name**. `sheesh` recovery-binding syntax is provisional in the grammar.
