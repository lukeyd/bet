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

---
