# `docker/` — reproducible build & verify environment

The whole project was built on **macOS / ARM64**. This directory pins a second, previously
unexercised platform — **Ubuntu 22.04 / x86-64** — in a disposable container with the exact
toolchain the build needs: Rust (pinned by `rust-toolchain.toml`) and **LLVM 18** (the
`backend --features llvm` dependency). It exists to answer one question: *does the `bet`
toolchain — and the DOOM port — build and verify off the author's Mac?*

Bonus: it makes LLVM 18 a no-sudo, no-host-pollution concern. Inside the image you're root, so
`apt-get install llvm-18-dev` just works; on the host it would need `sudo` against apt.llvm.org.

## Quick start

From the **repo root** (the container runs as your UID/GID, so `target/` stays yours):

```sh
export UID GID=$(id -g)                                   # if your shell doesn't export them
docker compose -f docker/compose.yaml build               # ~once: LLVM 18 + Rust (a few minutes)
docker compose -f docker/compose.yaml up -d               # start the idle builder
docker compose -f docker/compose.yaml exec bet docker/verify.sh
```

One-shot equivalent (no lingering container):

```sh
docker compose -f docker/compose.yaml run --rm bet docker/verify.sh
```

Drop into a shell to poke around:

```sh
docker compose -f docker/compose.yaml exec bet bash
```

## What `verify.sh` checks

All stages run (continue-on-error) and a PASS/FAIL summary prints at the end:

1. **Default workspace gate** — `fmt --check`, `clippy -D warnings`, `xtask graph-check`,
   `build --workspace`, `nextest run` (no LLVM: frontend, runtime, interp, tooling).
2. **LLVM 18 backend** — `cargo build -p driver --features llvm` and the headless real runtime.
3. **The DOOM port compiles** — `bet build ports/doom/doom.bet --runtime real` → a native
   x86-64 ELF. This is the headline: ~61k lines of `bet` through the compiler + LLVM on Linux.
4. **Headless demo playback** — runs the shareware DEMO1/2/3 lumps to completion via
   `BET_GG_HEADLESS=1 … -timedemo … -sync …` and reports the per-tic fingerprint count. The
   default (non-`desktop`) runtime is byte-identical to the `BET_GG_HEADLESS=1` desktop path,
   so this needs no window, GPU, minifb, or cpal.
5. **Oracle-independent golden fingerprints** — builds the native `tools/*_smoke.bet` programs
   and diffs their deterministic CRCs against the committed `ports/doom/goldens/*.golden`.

The shareware `doom1.wad` (freely redistributable) is fetched automatically and md5-checked
against the canonical v1.9 (`f0cefca…`). To use your own, drop it at `doom-reference/doom1.wad`
before running. WADs are gitignored and never committed.

## What it deliberately does NOT do

The **full doomgeneric differential oracle** (tic-by-tic parity vs id's 1993 engine) is not
reproducible here: its inputs — `ports/doom/goldens/oracle.patch` and `goldens/*.oracle.sync` —
are gitignored id-GPL-derived artifacts, absent from a clean clone. So the "99.96% vs id"
numbers in `ports/doom/README.md` can't be regenerated without the author's local files. What
*is* reproducible — the workspace gate, a native DOOM binary, real demo playback, and the
committed oracle-independent goldens — is what this environment proves on Linux/x86-64.

## Cross-platform findings surfaced while building this

This environment turned up a real compiler bug (fixed separately in #29, now on `main`) plus
some Mac-only assumptions still worth noting.

- **FIXED (#29) — O(n²) memory in aggregate codegen made `cop GameState{}` (and thus the whole
  DOOM build) run out of RAM.** On a 64 GB host the compile climbed past 45 GB and was killed before
  it finished; a *single* `cop gs.GameState{}` alone OOM'd past 24 GB, while the 16k-line tables
  module compiled in 78 MB. Root cause: `backend`'s aggregate builders lowered every `[]T` / struct
  value by chaining `insertvalue`, which on a constant aggregate re-folds the whole constant at each
  step — O(n²) in the aggregate size. GameState zero-defaults big inline arrays (`TagBox[32768]`,
  `LineLinks[8192]`, …), so this exploded. The fix (in `crates/backend/src/codegen.rs`) builds an
  all-constant aggregate as one constant (`const_array` / `const_named_struct`) and materializes a
  non-constant array (e.g. an array of zeroed structs) through a stack slot with one store per
  element — both O(n). Result: the full DOOM port compiles in **~0.9 GB** (down from >24 GB),
  `247/247` tests still pass, and `rdata_smoke` output is byte-identical to its committed golden.
  The container still keeps a hard cgroup RAM cap (`mem_limit`/`memswap_limit` in `compose.yaml`,
  default **24 GiB, no swap**) as a standing guardrail so any future runaway compile is OOM-killed
  *inside the container* (stage 3 reports exit 137) rather than endangering the host.
- **`crates/driver/src/link.rs` has no Linux system-lib handling for the `gg-desktop` runtime.**
  There's an explicit macOS-frameworks block (Cocoa/Metal/CoreAudio/…) for the minifb+cpal
  runtime, but no Linux equivalent (`-lasound`, X11/wayland). The **headless** path (what this
  env uses) is unaffected; a windowed `--runtime real` desktop build on Linux would likely fail
  to link until the ALSA/X11 libs are named there.
- **Every `ports/doom/tools/*_smoke.bet` hardcodes an absolute macOS WAD path**
  (`/Users/lukebaggett/Documents/bet/doom-reference/doom1.wad`), so they only run on the author's
  machine unedited. The image works around it by symlinking that path to the bind-mounted
  `doom-reference/` — but it really wants to be an arg or env var.
- **`cargo xtask doom-verify --goldens` is stale**: it reads `fixed/random/angle/tables.golden`,
  none of which exist in the repo. That mode is effectively dead.
- **DOOM is not covered by CI** (it lives behind xtask's non-default `doom` feature), so before
  this it had only ever built and run on one machine.

## Image notes

- `ubuntu:22.04`, LLVM 18 from `apt.llvm.org` (jammy), Rust via rustup under `/usr/local`.
- Contains **no project source** — the repo is bind-mounted at `/work`, so editing code never
  requires an image rebuild.
- The cargo download cache persists across runs via a gitignored bind mount at
  `/work/.docker-cache/cargo` (compose sets `CARGO_HOME` there) — owned by the runtime user, so
  no root-owned artifacts. Delete `.docker-cache/` to clear it.
- `LLVM_SYS_180_PREFIX=/usr/lib/llvm-18` is baked in (matches `llvm-sys 180` + the port README).
