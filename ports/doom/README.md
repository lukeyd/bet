# `ports/doom` — Doom port (empty until trigger)

**Do not start work here yet.** This directory stays empty until its gate is met
(plan-amendment-01.md §7.2): **milestones 3-4** (cribs, tag/holla through real codegen)
plus features §2.4/§2.5/§2.6/§2.7 landed (bit ops + integer semantics, first-class
function values, FFI/`gg` platform layer, bytes/binary I/O).

**Design:** engine code translated from the GPL Doom source, targeting the `gg` platform
layer (framebuffer, audio ring, input, hi-res timing). The second permanent design oracle
(amendment §1): a brutally honest systems/gamedev test (bit manipulation, FFI, byte-level
I/O, function tables, allocation pressure).

## Licensing & assets

- **The port is GPL** (it derives from id Software's GPL source). The bet language and
  toolchain are unaffected — keep this GPL code isolated in this directory.
- **No copyrighted WADs.** Test against the shareware WAD or **Freedoom**.
- **Correctness oracle:** deterministic demo-lump playback diffed against a reference port
  (differential testing against id's 1993 behavior; amendment §6.3).
