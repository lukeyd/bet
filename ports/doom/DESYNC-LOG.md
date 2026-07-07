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
