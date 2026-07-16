# bet

A compiled, statically-typed, general-purpose programming language whose keyword and
standard-library vocabulary is built entirely from durable internet slang, with a
game-development-first memory model (arena "cribs" + generational `tag`/`holla` handles,
no tracing GC in the hot path). Compiles to native machine code via LLVM, with the runtime
statically linked into every binary. Implemented in Rust.

> **Status: Milestone 8 (self-hosting fixpoint) complete; port-driven hardening.** The
> frontend, LLVM backend, real runtime, interpreter, and formatter are built and gated by
> an interp-vs-compiled differential over the golden corpus. The frontend is self-hosted:
> [`selfhost/betfe.bet`](selfhost/README.md) re-emits its own MIR byte-identically. A full
> [DOOM port](ports/doom/README.md) (~90 modules) plays with byte-exact simulation parity
> against id's behavior; further ports (Frozen Bubble et al.) and the M:N scheduler are in
> flight — see [`timelog/tasks.toml`](timelog/tasks.toml) for live per-workstream status.
> Design docs: [`language-spec.md`](language-spec.md),
> [`bootstrap-plan.md`](bootstrap-plan.md), plus `plan-amendment-01/02/03.md`.

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
cargo build --workspace                 # everything compiles (no LLVM needed by default)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask graph-check                 # enforce the dependency graph
cargo nextest run --workspace --no-tests=pass
cargo xtask timelog report              # active build effort, by activity/task
```

The `bet` CLI (from `crates/driver`) is what *users of the language* get; Cargo is the
house the language is built in. See [`CLAUDE.md`](CLAUDE.md) for working rules, including
**mandatory time tracking**.
