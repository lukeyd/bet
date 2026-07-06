# `ports/doom` — the DOOM port (W0 harness landed)

The gate (plan-amendment-01.md §7.2) is met and the port has begun. **W0** (this layer)
is the data-table codegen + verification-oracle harness; the engine port itself is the
W1+ workstreams. The reference source (id's GPL linuxdoom-1.10) and the shareware
`doom1.wad` live OUTSIDE the repo at `doom-reference/` (never edited; new files only in
`doom-reference/headless/`).

**Design:** engine code translated from the GPL Doom source, targeting the `gg` platform
layer (framebuffer, audio ring, input, hi-res timing). The second permanent design oracle
(amendment §1): a brutally honest systems/gamedev test (bit manipulation, FFI, byte-level
I/O, function tables, allocation pressure).

## Layout

| path | what |
|---|---|
| `defs/*.bet` | GENERATED leaf modules (`cargo xtask doom-gen`): `info.bet` (states[]/mobjinfo[]/sprnames + S_/MT_/SPR_/AID_ ordinal facts), `tables.bet` (finesine/finetangent/tantoangle VERBATIM + `slopeDiv`), `sounds.bet` (S_sfx[] + SFX_/MUS_ ordinals). Do not edit; regenerate and commit. `doom-gen --check` byte-diffs them in CI style. |
| `tools/gen_smoke.bet` | interp smoke: pulls all three defs, fills the vecs/slabs, checks ~40 hand-extracted values (`cargo run -p driver -- run ports/doom/tools/gen_smoke.bet`). |
| `tools/goldens.bet` | (W1, not yet written) the bet twin that reprints the goldens, section per `#GOLDEN <name>` marker, for `doom-verify --goldens`. |
| `goldens/gen/*.c` | committed C golden generators — they `#include` the reference m_fixed.c / m_random.c / tables.c so id's own code computes every value (`cargo xtask doom-golden-gen`). |
| `goldens/*.golden` | committed outputs: FixedMul/FixedDiv grid, rndtable + 2000 P_Random, SlopeDiv + 8-octant point-to-angle sweep, table CRCs + spot values. |
| `goldens/inventory.txt` | FROZEN census of all 720 function definitions in the reference tree (`doom-gen --inventory`); consumed by `cargo xtask doom-coverage` with `// PORTED:` / `// PORTED-PARTIAL:` / `// SKIPPED:` markers placed above definitions in the reference checkout. |
| `goldens/oracle.patch` | headless dump platform for doomgeneric (the primary oracle), applied by `cargo xtask doom-oracle --setup` to the pinned clone at `doom-oracle/`. |
| `goldens/demo3.oracle.sync` | committed reference sync stream: `doom1.wad -timedemo demo3` through the patched doomgeneric (2402 lines, deterministic). |

## The verification pipeline

- **CRC-32 everywhere:** IEEE 802.3 reflected, poly `0xEDB88320`, init `0xFFFFFFFF`,
  final XOR `0xFFFFFFFF`; 32-bit table entries are fed little-endian. Implementations:
  `crates/xtask/src/doom.rs::crc32`, `goldens/gen/gen_tables.c`, the oracle patch — the
  bet twin must copy it bit-for-bit.
- **Sync-stream format** (oracle now, bet port from W2/W3): per-tic fingerprint lines
  `T= R= X= Y= Z= A= MX= MY= H= S= LT= C=` (all `%08x`; player-0 mobj state, prndindex,
  leveltime, framebuffer crc — the crc of the frame as last PRESENTED, one behind the
  sim) plus per-level `SETUP sectors= lines= things=` and `SETUP T i= type= x= y= a=`
  spawn-order blocks. Diff: `cargo xtask doom-verify --ours <mine> --theirs <oracle>` —
  first divergence, 3 lines of context, per-field diff, and triage (SETUP → loader;
  C-only → renderer; anything else → sim).
- **Tie-breaker:** `doom-reference/headless/` builds linuxdoom-1.10 itself with modern
  clang (see its README for hook-point caveats) when doomgeneric's testimony is in doubt.

## Licensing & assets

- **The port is GPL** (it derives from id Software's GPL source). The bet language and
  toolchain are unaffected — keep this GPL code isolated in this directory.
- **No copyrighted WADs.** Test against the shareware WAD or **Freedoom**.
- **Correctness oracle:** deterministic demo-lump playback diffed against a reference port
  (differential testing against id's 1993 behavior; amendment §6.3).
