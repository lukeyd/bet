# `selfhost` — the self-hosted `bet` frontend

`bet`'s frontend (lex → parse → typecheck-fold → lower → emit `.mir`) is written **in `bet`**, in
[`betfe.bet`](./betfe.bet). It lowers its own source **byte-identically** to the reference Rust
frontend — the self-hosting fixpoint, enforced by `cargo xtask selfhost`. The LLVM backend and the
runtime stay in Rust (self-hosting means the frontend/middle-end, per convention — Go's runtime is
still substantially not-Go). So a compile is:

```
your.bet ──[ betfe : the bet language frontend ]──▶ .mir ──[ bet : Rust backend + linker ]──▶ binary
```

Status: **done** — Milestone 7.1 (self-host stage 1) and Milestone 8 (fixpoint) are complete.
`betfe --emit mir <file>` is byte-identical to `bet build --emit mir <file>` for all 57 lowerable
corpus programs and for betfe's own 27k-line source; the stage-2/stage-3 rebuild is stable.

---

## Build the self-hosted CLI — one command

Prereqs: the pinned Rust toolchain (automatic) + **LLVM 18** (the backend's codegen), needed only to
*build*:

| Platform | install | env to export |
|---|---|---|
| macOS (Homebrew) | `brew install llvm@18 zstd` | `LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18`  `LIBRARY_PATH=/opt/homebrew/lib` |
| Debian/Ubuntu | `apt install llvm-18-dev` | `LLVM_SYS_180_PREFIX=/usr/lib/llvm-18` |

```sh
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18     # your LLVM 18 prefix
export LIBRARY_PATH=/opt/homebrew/lib                    # macOS: where libzstd lives
scripts/betself --setup
```

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
