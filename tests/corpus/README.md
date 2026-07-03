# `tests/corpus` — golden programs ★

Real `bet` programs with expected stdout, checked in as `name.bet` + `name.expected`
pairs. This is the ★ contract artifact from bootstrap-plan.md §1c: it simultaneously seeds
the interpreter (run these), the frontend (parse these), the formatter (round-trip these),
and the conformance suite (it *is* the seed). Per plan-amendment-01 §6.1 it grows a targeted
program for every language feature.

**No interpreter exists yet** (Step 1c predates the frontend/interp). These programs are
authored against the frozen `spec/grammar.ebnf` and hand-computed `.expected` outputs. They
are guarded structurally today by `cargo xtask corpus --check` (pairing + feature coverage);
they become *executed* golden tests in Step 2, when `cargo xtask corpus` runs them through
the interpreter and the compiled path and diffs the results.

Not a Cargo crate — bet source + data, driven by `cargo xtask corpus`.

## Layout

```
NN-category/name.bet        the program (entry point: finna main())
NN-category/name.expected   its exact stdout, byte-for-byte
MANIFEST.toml               program -> features-covered map + the coverage universe
```

Categories: `01-basics 02-values 03-control 04-functions 05-structs 06-sumtypes
07-errors 08-memory 09-bit-math 10-stdlib 11-reference 12-ffi 13-concurrency`.

## Authoring rules

1. **Entry point** is `finna main()`. The `.expected` file is the program's exact stdout,
   including trailing newlines. Every program must be **deterministic** — no wall-clock time,
   no unseeded RNG. Seed any randomness with a literal.
2. **Grammar-conformant.** Every construct must parse under `spec/grammar.ebnf`. Prefer the
   idiomatic form; parenthesize where a human reader would want it (see decision 3/7 in
   `spec/syntax-decisions.md`).
3. **One concept, one word.** Use the spec vocabulary consistently (spec §2.4, §3, §9.1):
   `peep` = read everywhere, `yeet` = discard/panic, etc. New helper names follow §9.1.
4. **Small and legible.** Each program isolates a feature (or, in `11-reference`, combines
   many on purpose). A short `//` header comment states what it demonstrates.
5. **Register each program in `MANIFEST.toml`** with the feature keys it covers.

## Display rules (how values print — a corpus convention, not yet a spec)

`spill.it(x)` prints `x`'s default display followed by `\n`. `spill.f(fmt, args…)` substitutes
each `{}` with the next argument's default display; `{{`/`}}` are literal braces; it adds no
trailing newline of its own. Default displays:

| Type | Prints as | Example |
|---|---|---|
| `int` / sized ints | decimal, `-` for negative, no separators | `42`, `-7`, `1000000` |
| `bool` | the slang literal | `nocap` / `cap` |
| `str` | the raw characters, no quotes | `hi` |
| `ghosted` / nil | `ghosted` | `ghosted` |
| `yikes` (error) | its message text | `couldn't load save: no such file` |

**Floats are never printed bare** in a program that has an `.expected` file (round-trip
float formatting is unsettled). Verify float logic by reducing to an int or bool before
printing (e.g. `spill.it(x > 0.5)` or `spill.it(x as int)`).
