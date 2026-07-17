# CLAUDE.md — working rules for the `bet` repo

`bet` is a compiled, statically-typed language with slang-keyword vocabulary and an
arena/`tag`/`holla` memory model, implemented in Rust over an LLVM backend. The
normative reference is `language-spec.md`, with the frozen contracts in `spec/`
(`grammar.ebnf`, `midir.md`, `runtime-abi.md`, `semantics.md`). The original
bootstrap plan and its amendments finished and were deleted; `git log` has them,
and comments citing `bootstrap-plan.md §N` / `plan-amendment-0N §N` are provenance
pointers into that history, not live documents.

The compiler pipeline is complete and self-hosted: `selfhost/betfe.bet` re-emits its own
MIR byte-identically (Milestone 8 fixpoint), and `ports/doom` plays real DOOM with
byte-exact sim parity. Current work is port-driven hardening (Frozen Bubble, the M:N
scheduler, corpus parity) coordinated through the `midir`/`rt-abi` contract crates
and the golden corpus.

## Time tracking: retired — do NOT log time

Agent time-tracking is **over**. `timelog/` is a closed historical record of the
bootstrap effort (see `timelog/README.md`); nothing new gets written to it.

- Do **not** run `scripts/timelog.sh in/switch/out`, and do not add timelog entries
  to your commits. There is no `PostToolUse` heartbeat hook any more — don't re-add one.
- Do **not** add `chore(timelog)` clock-punching commits. 12% of this repo's history
  is already that, and it buys nothing.
- `timelog/tasks.toml` is likewise frozen. Track live work in GitHub issues.

The read-side tooling (`cargo xtask timelog report` / `eta`, `scripts/timelog.sh`)
still works against the historical data if you're curious what the build cost.

## Repo conventions

- **One Cargo workspace.** Members are `crates/*` only. `std/`, `tests/`, `selfhost/`,
  `ports/` are NOT crates (they hold `bet` source / data / future harnesses).
- **Contracts are code.** `crates/midir` (IR) and `crates/rt-abi` (runtime ABI) are the
  only cross-team coordination surface. The allowed dependency graph is enforced by
  `cargo xtask graph-check` against `graph-allowlist.toml` — if you add an internal
  dependency edge, update the allowlist in the same change (and expect review).
- **Green from the first commit.** Before you push: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask graph-check`,
  `cargo xtask corpus --check`, `cargo nextest run --workspace --no-tests=pass`.
- **No LLVM needed for the default build.** `backend`'s `inkwell` dependency is optional
  behind a non-default `llvm` feature; never pass `--all-features` in CI (it would pull LLVM).
- Keep the keyword joke in the language; keep the Rust implementation boring and solid.

## Toolchain

Pinned in `rust-toolchain.toml`. `cargo xtask <cmd>` runs repo automation
(`graph-check`, `timelog`, `setup-llvm`, and Step-1+ stubs `corpus`/`dist`).
