# bet

A compiled, statically-typed, general-purpose programming language whose keyword and
standard-library vocabulary is built entirely from durable internet slang, with a
game-development-first memory model (arena "cribs" + generational `tag`/`holla` handles,
no tracing GC in the hot path). Compiles to native machine code via LLVM, with the runtime
statically linked into every binary. Implemented in Rust.

## Play something

```sh
cargo xtask setup-llvm        # checks for LLVM 18 (needed to compile to native code)
cargo xtask run pong          # no assets needed — start here
cargo xtask run doom          # auto-fetches freedoom1.wad if no WAD present
```

Yes, really: `bet` runs [real DOOM](ports/doom/README.md) — ~30,000 lines of id's C translated
into ~90 `bet` modules, with **byte-exact simulation parity** verified tic-by-tic against id's
own behavior. `cargo xtask run` builds the compiler, the runtime, and the port, and sorts out
assets; `cargo xtask run` with no port lists all six.

> **Status: Milestone 8 (self-hosting fixpoint) complete; port-driven hardening.** The
> frontend, LLVM backend, real runtime, interpreter, and formatter are built and gated by
> an interp-vs-compiled differential over the golden corpus. The frontend is self-hosted:
> [`selfhost/betfe.bet`](selfhost/README.md) re-emits its own MIR byte-identically. A full
> [DOOM port](ports/doom/README.md) (~90 modules) plays with byte-exact simulation parity
> against id's behavior; further ports (Frozen Bubble et al.) and the M:N scheduler are in
> flight — see [`timelog/tasks.toml`](timelog/tasks.toml) for live per-workstream status.
> Design docs: [`language-spec.md`](language-spec.md) and [`spec/`](spec/) (grammar,
> semantics, IR & ABI rationale).

## Layout

```
crates/     the Rust implementation (the ONLY Cargo workspace members)
  midir ★   mid-level IR (frontend<->backend contract)
  rt-abi ★  runtime ABI (codegen<->runtime contract) + platform layer
  rt-stub   naive malloc-backed rt-abi impl (bootstrap only)
  runtime   real runtime: cribs, generations, scheduler, OS layer
  frontend  lexer->parser->AST->typecheck->midir (a LIBRARY)
  backend   midir->LLVM IR (inkwell), passes, lld linking
  interp    tree-walking interpreter / differential-test partner
  gg-backend native gg platform layer (window/audio/input; `desktop` feature)
  playground wasm-bindgen shim: the interpreter in the browser (`cargo xtask wasm`)
  fmt lsp   tooling (consume the frontend library; lsp is still a stub)
  driver    the `bet` CLI (compile / run / fmt / test)
  xtask     repo automation (graph-check, timelog, corpus, selfhost, wasm, doom-*)
std/        the bet stdlib, written in bet (not Cargo crates)
spec/       grammar (EBNF), semantics, IR & ABI rationale
tests/      corpus (golden programs), mir, conformance, bench
timelog/    committed time & velocity tracking (see timelog/README.md)
selfhost/   the self-hosted frontend, betfe.bet (fixpoint reached; see its README)
ports/      DOOM (done), pong, fireworks, gg-demo, frozen-bubble, oregon-trail
            (ports/doom is GPL, isolated to that directory)
```

## Build & checks

```sh
cargo build --workspace                 # the Rust implementation (LLVM-free: see below)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask graph-check                 # enforce the dependency graph
cargo nextest run --workspace --no-tests=pass
cargo xtask timelog report              # active build effort, by activity/task
```

**On LLVM:** the *default* workspace build needs none — `backend`'s `inkwell` dependency sits
behind a non-default `llvm` feature, so these checks (and CI) stay LLVM-free, and the
interpreter (`bet run`) works without it. But **emitting native code needs LLVM 18**, so every
game does: `cargo xtask run <port>` builds `--features llvm` and finds your install itself
(`$LLVM_SYS_180_PREFIX`, `llvm-config`, Homebrew, or the usual system paths — no path is
hardcoded). `cargo xtask setup-llvm` reports what it found, or how to install it.

The `bet` CLI (from `crates/driver`) is what *users of the language* get; Cargo is the
house the language is built in. See [`CLAUDE.md`](CLAUDE.md) for working rules, including
**mandatory time tracking**.

## Licensing

The language and toolchain are **Apache-2.0** ([`LICENSE`](LICENSE)). The one exception is
[`ports/doom`](ports/doom/README.md), which is derived from id Software's DOOM source and is
therefore **GPL-2.0-only**; it is isolated to that directory and is not part of the toolchain.
[`LICENSING.md`](LICENSING.md) states the boundary in full.

No game assets are committed. `cargo xtask run doom` fetches Freedoom, which is BSD-licensed and
freely redistributable.
