# bet — formalized semantics (placeholder)

> **Status:** Pre-implementation. The normative decisions to date live in
> `language-spec.md` (types §5, error handling §6, memory model §7, concurrency §8).
> This document formalizes them (evaluation order, the `holla`/`ghosted` desugaring,
> generation-check semantics, overflow/wrapping rules) during Phase 0. See
> `language-spec.md §11.2` (Phase 0 contracts) and `§12` (open questions).

Load-bearing semantics to formalize here:

- **Memory model** — cribs (arenas), `cop ... in <crib>`, `evict` (O(1) mass-free that
  bumps every slot generation), `tag T` (8-byte slot+generation handle), `holla`/`ghosted`
  checked access, `trust()` unchecked (debug-checked). language-spec.md §7.
- **Error handling** — `yikes` values, `.tea()` wrapping, `ghosted` nil-error, `bounce`
  early-return sugar (adopted, amendment §2.8). language-spec.md §6.
- **Overflow** — unsigned wraps (defined), signed traps in debug / wraps in release;
  explicit `math.lap` wrapping ops; explicit casts required. amendment §2.4.
