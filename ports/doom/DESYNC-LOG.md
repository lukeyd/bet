# DOOM port — demo-sync desync log (W3-sim)

Every entry is one root-caused simulation divergence between the bet port and id's
reference oracle (`ports/doom/goldens/demo{1,2,3}.oracle.sync`). The sim fields compared
are `R X Y Z A MX MY H S LT` (the `C` framebuffer crc is the renderer's job — see
`--sim-only`). Format per entry: **tic**, **symptom** (which field diverged first),
**root cause**, **fix**.

Reproduce a run:
```
target/debug/bet build ports/doom/doom.bet --runtime real -o /tmp/doom
BET_GG_HEADLESS=1 /tmp/doom -iwad .../doom1.wad -timedemo demo3 -sync /tmp/ours_demo3.sync -maxframes 3000
cargo xtask doom-verify --sim-only --ours /tmp/ours_demo3.sync --theirs ports/doom/goldens/demo3.oracle.sync
```

---

## Tooling: `doom-verify --sim-only`

`--sim-only` now strips the `C=` framebuffer-crc field before comparing fingerprint
lines, so the first reported divergence is always a *sim* field (R/X/Y/Z/A/MX/MY/H/S/LT).
Previously the diff stopped on any line difference including `C`, which masked the sim
picture behind renderer noise. Implemented in `crates/xtask/src/doom_verify.rs`
(`sync_diff_opt(ours, theirs, sim_only)` + `strip_c`), with unit tests.

Baseline at start of this grind: demo3 sim matched tics 0..30; first sim divergence at
**tic 31 (X/Y)** with R identical — a fixed-point / geometry precision error, not an RNG
ordering bug.

---

## demo3 tic 31 — P_SlideMove "continue along the wall" used stale position

- **symptom**: first sim divergence at gametic 31, fields **X/Y**. R/A/MX/MY all identical
  at the divergence (so not RNG order, not thrust). Ours barely moved (X +0x44bb, Y +0),
  oracle moved a lot (X +0x1c040, Y -0x67513). Exact match through tic 30, sudden large
  divergence at tic 31 = a categorical difference, not accumulated drift.
- **root cause**: gametic 31 is the player's **first wall contact** in demo3 — the first
  time `P_SlideMove` runs. Instrumenting `xyMovement`/`slideMove` showed both sides pick
  the same blocking line (947) and the same `bestslidefrac` (0xd733) and slide momentum
  (`nmx=19131, nmy=0`). But our final position moved by only `nmx` (the along-wall step),
  while the oracle's moved by the "move up to the wall" displacement (~+0x17b45, -0x676b4)
  **plus** the along-wall step. In `P_SlideMove`, after the "move up to the wall"
  `P_TryMove` succeeds it advances `mo->x/mo->y`; the reference's final "continue along the
  wall" call is `P_TryMove(mo, mo->x+tmxmove, mo->y+tmymove)` — reading the **already-
  advanced** position. The bet port instead used the loop-entry `mx`/`my` (captured before
  the move-up), so the along-wall `tryMove` overwrote the moved-up position with
  `(mx+nmx, my+nmy)`, discarding the move-up displacement.
- **fix**: in `ports/doom/p/p_map.bet` `slideMove`, re-read the mobj's current x/y (`curx`,
  `cury`) after the move-up `tryMove` and call `tryMove(gt, mo, curx+nmx, cury+nmy)`. The
  loop-entry `mx`/`my` are still correct for the move-up call (nothing has moved yet that
  iteration) and for the retry path (the loop top re-reads x/y each iteration).
- **result**: demo3 sim now matches **all 2134 tics** (2402/2402 stream lines under
  `--sim-only`). The remaining full-diff divergence is the `C` framebuffer crc (renderer).

### follow-up: the fix must NOT re-read the mobj through a fresh `holla`

The first cut of this fix re-read the advanced position with
`holla t = mo in gs.thinkers { curx = t.mobj.x; cury = t.mobj.y }` right after the
"move up to the wall" `tryMove`. That is the *nested-`holla` diverges in native* gotcha:
a fresh `holla` read of a field that a prior nested `holla` (inside `tryMove` →
`P_SetThingPosition`) just wrote is **not reliably ordered** in native lowering — the
read can observe the stale (pre-move-up) value. It happened to pass with debug
instrumentation present (the extra `holla` blocks perturbed codegen) and regress once the
instrumentation was removed; demo2/demo3 flapped between line-357/300 and full match
depending on the build. Root fix: don't re-read at all. `P_TryMove` sets `mo->x/mo->y` to
its target exactly on success, so the advanced position is *known* to be `(mx+newx,
my+newy)`; track it in locals `ux`/`uy` and pass `ux+nmx, uy+nmy` to the along-move
`tryMove`. Deterministic, and exact per the reference.
- **result**: demo1 advances 278 → 2966, **demo2 and demo3 match ALL tics** (4147/4147
  and 2402/2402), stable across repeated runs.

---

## demo1 tic 60 — P_TryMove wrote mo->x/y inside a NESTED `holla` (lost write)

- **symptom**: after the slide fixes, demo1 still diverged in X/Y, at a P_SlideMove tic —
  but the divergence *floated* between builds (tic 60 / line 278 vs. tic ~2748 / line
  2966) and only ever affected ~2 isolated tics, with every later tic re-aligning. Adding
  ANY debug read to `emitSyncTic`/`slideMove` made it vanish. Classic non-determinism.
- **root cause**: the actual mobj store was correct (the *next* tic read the right
  position, matching the oracle) — only the fingerprint's read was intermittently stale by
  exactly the "move up to the wall" displacement. The write itself was the problem:
  `P_TryMove` set `t.mobj.x = x; t.mobj.y = y` **inside a nested `holla`**
  (`holla t = thing in gs.thinkers` nested inside `holla g = gt in gs.gsc`). Per the
  repo's known bet gotcha, a store to a `tag` field inside a nested `holla` does not
  reliably commit in native lowering, so a later read (e.g. `emitSyncTic` reading
  `players[0].mo`) could observe the pre-move value. demo2/demo3 only "passed" because
  their reads happened to land after the write committed.
- **fix**: un-nest the write in `tryMove` (`ports/doom/p/p_map.bet`) — read `tmfloorz`/
  `tmceilingz` into locals inside the `gs.gsc` holla, close it, then write
  `t.mobj.x/y/floorz/ceilingz` through a **top-level** `holla t = thing in gs.thinkers`.
- **result**: the P_SlideMove position divergences are gone and *deterministic*. demo2 and
  demo3 still match ALL tics; demo1 advances to tic 2748 (line 2966), where the remaining
  divergence is an **RNG-order** issue (R off by one) — a different class, logged next.

---
