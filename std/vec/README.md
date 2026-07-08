# `vec` — first-class SIMD vector types

**Implemented** (compiler intrinsics, not importable source — like the rest of the stdlib). Fixed-width
vector types lower to LLVM `<N x T>` with single-instruction lane ops (guaranteed SIMD, not dependent
on auto-vectorization):

- **Types:** `<elem>x<N>` for a scalar `elem` (`f32x4`, `i32x4`, `i64x2`, `u32x4`, `f64x2`, …), plus the
  float aliases `vec2` = `f32x2`, `vec3` = `f32x3`, `vec4` = `f32x4`. Lane counts 2–4 in v1.
- **Construct / splat:** `f32x4(a, b, c, d)` builds it lane-by-lane; `f32x4(x)` broadcasts one scalar.
- **Element-wise:** `+ - * /` and (integer) `>> <<` — a scalar operand is broadcast (`v >> 16`, `v * s`).
  Integer lanes wrap; float lanes compute in their real width.
- **Lanes:** `v.x` / `v.y` / `v.z` / `v.w` read a lane (read-only in v1).
- **Reductions / ops:** `v.dot(w)`, `v.sum()`, `v.min(w)`, `v.max(w)`, `v.abs()`, `v.scale(s)`, and — for
  float vectors — `v.length()` and `v.norm()` (via `sqrt`).

Interpreter parity: `Value::Simd` holds lanes in their true element type, so `bet run` and a compiled
binary agree bit-for-bit (integer exactly; `f32` because the interpreter computes in real `f32`).

Still planned: `mat4` (4×4 matrix construct + `mat*vec` / `mat*mat`), lane writes (`v.x = …`), wider
lane counts (8/16), and swizzle patterns (`.xy`, `.xxzz`).
