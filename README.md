# bet

A compiled, statically-typed, general-purpose programming language whose keyword and
standard-library vocabulary is built entirely from durable internet slang, with a
game-development-first memory model (arena "cribs" + generational `tag`/`holla` handles,
no tracing GC in the hot path). Compiles to native machine code via LLVM, with the runtime
statically linked into every binary. Implemented in Rust.

> **Status: Step 3 — the parallel fan-out.** Steps 0–2 are done: the workspace skeleton
> + CI, the three contract artifacts (`midir`, `rt-abi`/`rt-stub`, frozen grammar + golden
> corpus), and the `spill.it("hi")` tracer bullet running end-to-end. Now the frontend,
> backend, runtime, and interpreter build out concurrently, coordinating only through the
> `midir`/`rt-abi` contracts and the corpus. Design docs:
> [`language-spec.md`](language-spec.md), [`bootstrap-plan.md`](bootstrap-plan.md),
> [`plan-amendment-01.md`](plan-amendment-01.md).

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
  fmt lsp   tooling (consume the frontend library)
  driver    the `bet` CLI (compile / run / fmt / test)
  xtask     repo automation (graph-check, timelog, setup-llvm)
std/        the bet stdlib, written in bet (not Cargo crates)
spec/       grammar (EBNF), semantics, IR & ABI rationale
tests/      corpus (golden programs), mir, conformance, bench
timelog/    committed time & velocity tracking (see timelog/README.md)
selfhost/   self-hosted compiler (empty until milestone 7)
ports/doom/ Doom port oracle (empty until its gate; GPL, isolated)
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
