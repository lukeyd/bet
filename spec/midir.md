# bet — mid-level IR (design rationale)

> **The code in `crates/midir` is normative.** This document is rationale only.

The mid-level IR (à la Rust MIR / Swift SIL) sits between the frontend and LLVM. It hosts
bet-specific passes — arena lifetime analysis, allocator inlining, hoisting `holla`
generation checks out of loops, fusing repeated `holla`s — and isolates
the frontend from LLVM API churn (making Cranelift pluggable). Delivered as a real crate,
not a doc (bootstrap-plan.md §1a): Rust types + builder API + textual `.mir` format
(parser + printer) + validator.

Per plan-amendment-01 §6.1, `midir` must represent:
- `moods` construction and `vibe` dispatch (switch-on-tag),
- monomorphized generic instantiations (post-expansion — the IR never sees type params),
- function-pointer values and indirect calls,
- the full sized-integer tower with explicit wrap/trap arithmetic ops,
- the `soa` struct-of-arrays layout, carried as a single `TyKind::Soa(inner)` wrapper
  (see below).
- first-class SIMD vectors, carried as `TyKind::Simd { elem, lanes }` (see below).

## `soa` — struct-of-arrays layout

The surface `soa` keyword (a container-of-`drip` laid out as parallel per-field arrays) is
carried in the IR as **one wrapper type, `TyKind::Soa(inner)`**, where `inner` is
`Array(Struct,N)`, `Slice(Struct)`, or `Vec(Struct)`. This is deliberately minimal: rather
than a pass that rewrites places or a family of layout-tagged variants, the layout rides on
the type and is *materialized only in the backend*. Consequences the code makes normative:

- **Access is layout-agnostic in the IR.** `soa[i].field` is the ordinary place projection
  `[Index(i), Field(j)]` — identical to array-of-structs. The backend's `place_ptr` fuses the
  `Index`+`Field` pair and picks the transposed address (field-array first, then index) for a
  `Soa` base; every other layer treats the projection structurally. So a `Soa`-typed value
  needs no new `Rvalue`/`Proj`.
- **Construction is per-field.** A `soa T[N]` is built by scattering into the transposed
  local; `soa []T`/`soa vec[T]` are bundles (`{ {ptr,len} × k }` / `{ handle × k }`) whose
  per-field slots are addressed with a bare `Field(j)` on the `Soa` type. The validator types
  that `Field(j)` structurally as `Slice(field_j)` / `Vec(field_j)` (no interning needed).
- **Whole-element ops are unrepresentable and rejected in the frontend** (a `soa` element has
  no single address); `place_ptr` and the validator are the soundness backstop. `soa vec`
  reuses the `bet_vec_*` ABI unchanged (one handle per field) — no `rt-abi` change.

## SIMD — first-class fixed-width vectors

The surface vector types (`f32x4`, `i64x2`, `vec2..4`) are carried as **`TyKind::Simd { elem, lanes }`**
(a scalar element type + a lane count), lowered to an LLVM `<lanes x elem>` vector. The contract is
kept minimal — one type variant plus one aggregate and one rvalue:

- **Element-wise arithmetic/shift reuses `Rvalue::BinOp`.** Two `Simd`-typed operands make the backend
  emit the element-wise vector instruction (`fadd <4 x float>`, `mul <2 x i64>`, …); the frontend
  broadcasts a scalar operand to a vector (`SimdOp::Splat`) so the IR only ever sees `BinOp(vec, vec)`.
  No new binary op. Integer lanes are wrapping (no per-lane trap).
- **Construction is `AggKind::Simd(elem)`** — `N` lane operands (`N` = lanes), mirroring `AggKind::Array`.
- **Everything else is one rvalue, `Rvalue::Simd { op, args, ty }`**, with
  `SimdOp ∈ { Splat, Lane(i), Min, Max, Abs, Dot, Sum, Length, Norm }`. `ty` is the result type
  (vector for construct-shaped ops, the scalar element for `Lane`/`Dot`/`Sum`/`Length`). The reductions
  fold lanes in order 0→N-1 (the interpreter matches the order, so float reductions are bit-identical);
  `Length`/`Norm` are the sole users of an LLVM intrinsic (`llvm.sqrt`). Min/max/abs lower to
  compare + select (no intrinsic). **No `rt-abi`/runtime change** — SIMD values are pure LLVM.

Stub in Step 0 (`crates/midir` compiles empty); real IR lands in Step 1a.
