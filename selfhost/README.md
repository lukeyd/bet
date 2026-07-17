# `selfhost` — the self-hosted `bet` frontend

`bet`'s frontend (lex → parse → typecheck-fold → lower → emit `.mir`) is written **in `bet`**, in
[`betfe.bet`](./betfe.bet). It lowers its own source **byte-identically** to the reference Rust
frontend — the self-hosting fixpoint, enforced by `cargo xtask selfhost`. The LLVM backend and the
runtime stay in Rust (self-hosting means the frontend/middle-end, per convention — Go's runtime is
still substantially not-Go). So a compile is:

```
your.bet ──[ betfe : the bet language frontend ]──▶ .mir ──[ bet : Rust backend + linker ]──▶ binary
```

Status: **done** — Milestone 7.1 (self-host stage 1) and Milestone 8 (fixpoint) are complete, and
betfe has since been brought to **full parity** with the reference frontend: multi-file modules
(`pull`), first-class SIMD, `soa`, every recent stdlib intrinsic, and the `gg` platform module all
lower. `betfe --emit mir <file>` is byte-identical to `bet build --emit mir <file>` for **81 of 83**
corpus programs (the other two are interp-only — the reference can't lower them either) and for
betfe's own source; the stage-2/stage-3 rebuild is stable. As the capstone, `betfe --emit mir
ports/doom/doom.bet` is **byte-identical across all 137,429 lines** of the real 97-file DOOM port,
and the betfe-built DOOM binary runs the headless `demo3` timedemo with a demo-sync stream
bit-identical to the reference-built binary's. (`betfe --emit failall <entry>` lists every function
that fails to lower — the gap enumerator used to drive large multi-file ports.)

---

## Build the self-hosted CLI — one command

Prereqs: the pinned Rust toolchain (automatic) + **LLVM 18** (the backend's codegen), needed only to
*build*:

| Platform | install |
|---|---|
| macOS (Homebrew) | `brew install llvm@18 zstd` |
| Debian/Ubuntu | `apt install llvm-18-dev` |

```sh
cargo xtask setup-llvm                    # confirms LLVM 18 is present, or says how to install it
eval "$(cargo xtask setup-llvm | sed -n 's/^  export /export /p')"   # put it in this shell
scripts/betself --setup
```

There is no path to copy: `setup-llvm` **finds** your install (`$LLVM_SYS_180_PREFIX`, a
`llvm-config` on PATH, Homebrew, or the usual system paths) and prints the exports for *your*
machine — the `eval` just applies them to the current shell, which is what `betself` needs. If
LLVM lives somewhere unusual, `export LLVM_SYS_180_PREFIX=/path/to/llvm-18` overrides the probe.

That builds `target/release/bet` (the Rust backend) and `./betfe` (the self-hosted frontend).
By hand it's just:

```sh
cargo build --release -p driver -p rt-stub --features llvm   # -> target/release/bet
target/release/bet build selfhost/betfe.bet -o betfe         # -> ./betfe
```

## Use it

`scripts/betself` is a drop-in `build`/`run` driven by the **self-hosted frontend**, no env needed:

```sh
scripts/betself run   tests/corpus/06-sumtypes/moods-basics.bet   # compile via betfe, then run
scripts/betself build myprogram.bet -o myprogram                 # standalone native binary
```

All 57 lowerable corpus programs produce their golden `.expected` output through this pipeline. A
program using a construct `betfe` can't lower yet exits with a clear message instead of a wrong
binary. (The 2 non-lowerable corpus programs, `yeet-sheesh` and `squadops`, are interp-only — the
*reference* `bet build --emit mir` can't lower them either.)

## Verify the self-host property

```sh
cargo run -p xtask -- selfhost   # asserts, among other things, the fixpoint below
```

## The bootstrap loop (why it's genuinely self-hosting)

```sh
bet build selfhost/betfe.bet -o betfe                  # stage 2: betfe built by the Rust toolchain
betfe --emit mir selfhost/betfe.bet > betfe.mir        # stage 3: betfe's OWN frontend output …
bet build betfe.mir -o betfe3                          #          … built by the Rust backend
# betfe3 reproduces the corpus and re-emits its own source identically → stable fixpoint
```

The self-hosted compiler is also a permanent design oracle: a brutally honest general-purpose test
(graphs, symbol tables, string processing, sum-typed trees). Any language decision that makes it
unwritable is wrong.
