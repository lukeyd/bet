# RENDER-LOG.md — W3-render pixel-parity grind (doom-w3rend)

Goal: drive the port's per-frame 8-bit framebuffer to match id's reference oracle
(doomgeneric, `doom-oracle/`) pixel-for-pixel at tics where the simulation already agrees
(demo3 = E1M7, sim matches tics 0–30). Success = the sync stream's `C` (frame CRC) field
matches the oracle for those tics.

## Frame-comparison infrastructure (Step 1)

Built a tic-aligned 8-bit-frame dump on both sides plus a pixel differ:

- **Port** (`d/d_main.bet`, `defs/gs.bet`): new `-dumpframe <tic> -dumpout <path>` flags. In
  `D_Display`, at `gametic == dumpTic`, `fs.drop`s the finished `g.vid.scr0` (raw 64000-byte
  8-bit frame). This is exactly the frame whose crc becomes the sync `C` at line `T=gametic`,
  so it lines up tic-for-tic with the oracle. Example:
  `BET_GG_HEADLESS=1 /tmp/doom -iwad doom1.wad -timedemo demo3 -dumpframe 10 -dumpout /tmp/ours.raw -maxframes 20`
- **Oracle** (`goldens/oracle.patch` → `doomgeneric_dump.c`): `DG_DrawFrame` now writes
  `I_VideoBuffer` when `gametic == DOOM_FRAME_TIC` (env), to `DOOM_FRAME_OUT`. `DOOM_FRAME_CRC=<hex>`
  matches by framebuffer crc instead (for startup frames whose gametic is aliased), and
  `DOOM_FRAME_SKIP=<n>` steps past fade-in/attract frames. `DG_SyncTick` additionally dumps the
  exact `C`-labelled frame via `DOOM_SYNCFRAME_OUT`/`DOOM_SYNCFRAME_TIC`. All env-gated — the
  normal `demo3.oracle.sync` output is byte-identical (verified). Example:
  `DOOM_FRAME_OUT=/tmp/oracle.raw DOOM_FRAME_TIC=10 ./doomgeneric_dump -iwad doom1.wad -timedemo demo3`
- **Differ** (`tools/framediff.py`, stdlib-only, no PIL/numpy): reads both 8-bit frames + the WAD
  `PLAYPAL` (lump 0, palette 0), writes side-by-side + diff-mask PNGs, and prints the mismatch
  count, the `(ours_idx → oracle_idx)` index-pair histogram at mismatches, and row/col diff
  distributions. `python3 tools/framediff.py ours.raw oracle.raw --out /tmp`.

Tic alignment: dumping at `gametic == N` on both sides yields the frame whose crc is `C` at
sync line `T=N` (verified: oracle gametic 3/4/10/… → `C@T3/4/10`). Startup tics 1–2 are aliased
by a doomgeneric render-stall at `gametic==2` and don't align (use `DOOM_FRAME_CRC`/`SYNCFRAME`).

## Bugs fixed

### 1. `centery`/`viewheight` — whole 3D view shifted 16 rows (tic 10: 45630 → 646 mismatches)
`r/r_main.bet` `rInit` called `setViewSize(gt, 11, 0)` — screenblocks **11** = fullscreen, 200-row
3D view, `centery = 100`. The oracle's default is screenblocks **10** (`m_menu.c screenblocks = 10`)
= full-width 168-row view with the status bar, `centery = 84`. `100 − 84 = 16`: every textured
pixel sampled `texturemid + (y − centery)·iscale` was off by a constant 16-row screen offset (found
by cross-correlating wall columns: a clean `d = −16` aligned every column at every distance).
**Fix:** `setViewSize(gt, 10, 0)`. Top half went to 0 mismatches; only the floor remained.

### 2. `R_DrawSpan` fixed-point precision — floor sub-texel drift (tic 10: 646 → 0 mismatches)
The remaining 646 were isolated single-pixel floor speckle (±1/±2, increasing toward the near
floor). Ruled out lighting (isolated, not per-span runs), FixedDiv (int64 vs double gave identical
crc), and the coordinate tables (`yslope`/`distscale`/`basexscale`/`baseyscale` + all span
`xfrac/yfrac/xstep/ystep` dumped bit-identical to the oracle). Root cause: `r/r_draw.bet`'s
`drawSpan` implemented linuxdoom's **`#if 0` (unused)** separate-`xfrac`/`yfrac` span at full 16.16
precision, but doomgeneric's **active** `R_DrawSpan` packs x (top 16 bits) and y (bottom 16 bits)
into one 32-bit `position` — 6 integer + 10 fractional bits per axis — and steps it with a single
`position += step`. The reduced 10-bit fractional precision + x←y carry coupling is the exact walk
id's oracle takes; full-precision independent stepping drifts a scatter of ±1-texel pixels.
**Fix:** rewrote `drawSpan` to the packed-`position` algorithm. Tic 10 → **0 mismatches**
(crc `0x5341cbff` == oracle `C@T10`).

### 3. "Blue nukage" nit — NOT a bug (misdiagnosis; port is correct)
E1M1's visible start pool renders in the blue palette ramp (indices 242/207/206/241). Verified
flat-by-flat against `doom1.wad`: that pool sector's floor flat is **`FLAT14`**, which is a
genuinely **blue** flat in the WAD (top indices 242/207/206/241) — *not* NUKAGE. The port renders
`FLAT14` faithfully. Cross-checks: `R_FlatNumForName` resolves correctly (`NUKAGE3→53`, `F_SKY1→54`);
the port renders actual `NUKAGE3` **green** matching the oracle **exactly** in demo3/E1M7 (which uses
NUKAGE3 in 8 sectors — 14 green px both sides at tic 25, 0 blue). No render change needed; the pool
is blue because the map says so.

## Result

At every tic where the sim agrees (demo3 tics 2–30), the **3D view is pixel-perfect** (0 mismatches
in rows 0–167). Full-frame CRC matches the oracle for tics **2–17** (16 consecutive). Residual
frame-CRC differences, both **outside the renderer's r_*/v_video lane**:

- **Tic 1**: the oracle presents an all-black initial framebuffer at `T=1` (its first gameplay render
  lands at `T=2`); the port presents gameplay at `T=1`. A d_main main-loop presentation-timing
  artifact — content re-aligns from `T=2`. Not a render-pixel bug.
- **Tics 18–30**: 34 pixels, entirely the status-bar doomguy **face** (x143–176), driven by
  `M_Random` (`ST_updateFaceWidget`). `M_Random`'s `rndindex` isn't in the sync stream, so it can
  diverge silently. This is `st_stuff` + `m_random`, not the renderer.

## Render smoke goldens (regenerated — justified)

`renderworld.golden` and `renderthings.golden` both changed and were regenerated, because fix #1
correctly shrinks the 3D view from 200 to 168 rows (screenblocks 10):

- `renderworld.golden`: `scr0NonzeroPixels 60335 → 50968`, `scr0Crc 0xc8f1a3ff → 0xed06526d`.
  The ~9.4k fewer nonzero pixels ≈ the bottom 32 rows (32×320 = 10240) no longer covered by the
  168-row 3D view. `numTextures=125` unchanged.
- `renderthings.golden`: `scr0NonzeroPixels 60294 → 50907`, `scr0Crc 0x98df0334 → 0xdb3028aa`.
  The vissprite projection (`visspriteP`, per-sprite `scale/patch/x1/x2`) is **unchanged** — only
  the composited pixels moved (centery 100→84 + the packed span walk).

These goldens are the port's own render regression baselines (not oracle truth); the new values
reflect the now-oracle-correct render. Both smokes pass against the regenerated goldens.


## W3-residuals — closing the full-frame-CRC gaps (doom-resid)

Picking up from "full-frame CRC matches tics 2–17; residual = the status-bar face". The 3D view
was 0-diff at sim-correct tics, so the whole-frame CRC was chased down to three status-bar / main-
loop parity gaps. Diagnosed by temporarily adding `rndindex` (the M_Random stream index, `RI=`) to
both the port sync line and the oracle `DG_SyncTick` (reverted after — the committed fingerprint
format and `demo{1,2,3}.oracle.sync` are unchanged).

### 1. Doomguy face — the level-start melt wipe advances `rndindex` (the main win)
`M_Random` (face RNG, `st_randomnumber = M_Random()` in `ST_Ticker`) uses `g.tick.rndindex`, a
separate stream from the sim's `prndindex`. It was *ordered and counted* right per-tic, but the
absolute `rndindex` drifted: the reference's `D_Display` runs the level-start **melt wipe**
(`wipe_ScreenWipe`→`wipe_initMelt`) even under `-timedemo`, and `wipe_initMelt` draws
**SCREENWIDTH (320) `M_Random()`** values (`y[0]` + 319 column deltas). The port skipped the whole
wipe under `-timedemo` (`wipe && !timingdemo`), so its `rndindex` was 320 behind. The straight-ahead
face is re-rolled every `ST_STRAIGHTFACECOUNT` (17) tics as `ST_calcPainOffset() + st_randomnumber%3`,
so the first re-roll (tic 18) picked a different face → the 34-px face diff at x143–176. **Fix**
(`d/d_main.bet` `D_Display`): under `-timedemo`, still call `fwipe.initMelt` on the wipe frame for
its RNG side-effect (it only advances `rndindex` and fills `g.wipe.y`; it never touches `scr0`), just
skip the blocking melt animation. `RI` then matched tic-for-tic.

### 1b. Sound pitch — an *extra* `M_Random` the doomgeneric oracle never makes
After 1, `rndindex` still diverged by +1 at demo3 tic 46 (the first in-level sound). linuxdoom-1.10
`S_StartSoundAtVolume` randomizes sfx pitch with `pitch += N - (M_Random()&M)` — two draws — and the
port had ported that. The **oracle** (doomgeneric / Chocolate-Doom `s_sound.c`) removed pitch
randomization entirely (fixed `NORM_PITCH`, no `M_Random` in the sound path). Since `M_Random` shares
`rndindex` with the face, each stray sound draw desynced the face for the rest of the demo. **Fix**
(`s/s_sound.bet` `startSoundAtVolume`): drop the two pitch-jitter `M_Random` draws (keep `NORM_PITCH`).
`RI` now matches **all** tics on demo1/demo2/demo3.

### 2. Tic-1 present timing — mirror the reference pre-loop `TryRunTics`
The reference `D_DoomLoop` runs one `TryRunTics()` *before* the first `D_Display` (and before
`I_InitGraphics`), so (a) its first *composed* gameplay frame is the SECOND gametic's, (b) the frame
whose CRC lands at `C@T=1` is the freshly-allocated all-black framebuffer, and (c) the level-start
melt wipe fires between the `T=1` and `T=2` fingerprints — not `T=0`/`T=1`. The port did
`gTicker + dDisplay` from the first iteration, composing an extra tic-0 frame (so `C@T=1` was tic-0
gameplay, not black, and the melt landed one fingerprint early). **Fix** (`d/d_main.bet` `D_DoomLoop`):
a `primed` flag runs the first tic with **no** `dDisplay`, recording `crc(black scr0)` as the last-
presented frame. This fixed `C@T=1` (now `0x7dc291ab` = crc of zeros, matching the oracle) **and**
landed the melt at the correct tic in one shot (the two residuals shared this root cause).

### 3. Arms weapon widgets — diff-draw erases to STBAR, not STARMS
Exposed once the face matched: a static ~5-px diff per arms cell, appearing the tic a weapon is
picked up (demo3 tic 47 = shotgun). The digit glyph (`STYSNUM*`) matched; the differing pixels were
its *transparent* posts, showing the panel background. The reference's `st_backing_screen` is **STBAR
only**; `STARMS` is a separate overlay widget. `STlib_updateMultIcon` erases a changed digit to the
STBAR backing screen (`V_CopyRect(..., st_backing_screen, ...)`), so once a cell changes, its
transparent posts show **STBAR** — whereas the port's full-refresh always draws the **STARMS** overlay
under every digit. STBAR and STARMS have a few gray shades' difference in the arms panel. Verified by
extracting both lumps: STBAR = oracle's pixels, STARMS = the port's. **Fix** (`st/st_stuff.bet`
`drawArm` + `st/st_lib.bet` `eraseMultIcon` + `defs/gs.bet` `armsOldInum[6]`/`armsUnderStbar[6]`):
track each arms multicon's `oldinum`; once a cell's inum changes it becomes "under-STBAR" — erase that
digit box from the STBAR backing screen (BG/scr4) before drawing the digit, exactly as id's diff-draw.
`statusbar.golden` is unchanged (the smoke draws the bar once, before any pickup, so no cell is dirty).

### Result (C = full-frame CRC over the whole 320×200 frame, vs the committed oracle refs)
- **demo3** (E1M7, all 2134 tics sim-correct): C-match **17 → 2130 / 2134**.
- **demo2** (all 3836 tics sim-correct): C-match **→ 3824 / 3836**.
- `rndindex`/face is now byte-exact on all three demos; `C@T=1` matches; `statusbar.golden`,
  `renderworld.golden`, `renderthings.golden`, `sound_channels.golden` all still pass.

### Still residual (NOT fixed here — 3D-view renderer, out of the st/d/p/defs lane)
The remaining C gaps are tiny masked-column **sprite-edge** differences in the 3D view (y<168),
2–8 px each, at isolated tics: demo3 tics 196/341/685/1936 (e.g. tic 196: 8 px at x233–238 y146–147,
a ~1-column shift of part of a sprite silhouette); demo2 has 12 such tics. They are pre-existing
`r_things` masked-column / sprite-clip nuances (my status-bar + present-timing changes cannot touch
y<168) that were masked before because the face diff made every frame's CRC differ from tic 18 on.
The render workstream's earlier "3D view pixel-perfect" was verified only over tics 2–30; these
appear later, when close sprites are drawn. Left for the renderer (r_things/r_draw) workstream.
