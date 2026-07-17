# fireworks — the per-frame scratch arena, measured

A small `bet` demo that exercises **`mem.scratch()`**, the per-frame arena, end to end.

Every frame a handful of overlapping firework bursts *bloom* into scratch: each burst expands
into a **variable** number of `Spark` structs `cop`ped into the frame arena and drawn with
`gg.rect`, and then the whole cloud is reclaimed in O(1) by `gg.flush()`, which resets scratch at
the frame boundary. No per-spark bookkeeping, no fixed `MAXSPARKS` ceiling, no GC: allocate
freely, reclaim for free.

The point is that it's **measured, not asserted**. `mem.receipts()` reports live scratch bytes,
so the trace prints the arena filling each frame (a count that varies with the spark count) and
dropping back to `0` after flush — the frame-arena cycle, observable:

```
frame 12 | bursts 1 | sparks 24 | scratch 384 B  -> flush ->  0 B
```

## Run it

```sh
cargo xtask run fireworks                    # windowed — watch the fireworks
BET_GG_HEADLESS=1 cargo xtask run fireworks  # headless — prints the trace, no window
```

That builds the compiler (`driver --features llvm`) and the windowed runtime
(`runtime --features gg-desktop`), compiles the demo, and runs it. LLVM 18 is discovered
automatically — nothing to export. The window opens at 2x the 640×480 logical frame; override
with `GG_SCALE=1 cargo xtask run fireworks`.

It **self-terminates after 300 frames** (`MAXFRAMES`) or on `Esc` / window close, which is what
lets the headless invocation run to completion unattended.

## Notes

- **`cop ... in mem.scratch()` is the load-bearing part.** A plain `cop` into the enclosing crib
  would *not* be reclaimed by `gg.flush()` — the scratch-back only happens for allocations placed
  in the frame arena (`squadops in crib` does not scratch-back).
- Burst centers are derived arithmetically from the launch epoch rather than from an RNG, so a
  run is deterministic and the arithmetic stays well inside `i64` (no overflow trap).
- **Not in the golden corpus.** Like Pong and gg-demo, a live window is non-deterministic, so
  this is run manually rather than gated in CI — the headless mode exists so a future CI job
  *could* smoke-test it.
