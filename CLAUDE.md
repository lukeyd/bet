# CLAUDE.md — working rules for the `bet` repo

`bet` is a compiled, statically-typed language with slang-keyword vocabulary and an
arena/`tag`/`holla` memory model, implemented in Rust over an LLVM backend. Design
docs: `language-spec.md`, `bootstrap-plan.md`, `plan-amendment-01.md`. Steps 0–2 are
complete (skeleton, the three contract artifacts, and the `spill.it("hi")` tracer
bullet running end-to-end). The repo is now at **Step 3 — the parallel fan-out**:
frontend, backend, runtime, and interp build out concurrently, coordinating only
through the `midir`/`rt-abi` contract crates and the golden corpus. See
`timelog/tasks.toml` for live per-workstream status.

## Time tracking (MANDATORY)

Every agent — root and subagents — must log its active time. This measures real build
effort across the whole project and feeds a velocity-based ETA (`cargo xtask timelog eta`).

1. **Clock in** when you start working:
   ```sh
   scripts/timelog.sh in <activity> --task <slug>
   ```
   It prints a logfile path — **remember it for this session.**
2. **Switch** whenever your activity changes:
   ```sh
   scripts/timelog.sh switch <activity> --file <that path>
   ```
3. **Clock out** when you pause or finish:
   ```sh
   scripts/timelog.sh out --file <that path>
   ```

- **Activities** (fixed enum): `planning writing testing reviewing debugging docs research ci other`.
- **`--task`** must match a `slug` in `timelog/tasks.toml` (add a task there if you're starting new work).
- **Use your own logfile.** Each agent gets its own UUID-named file from `in`; never
  write to another agent's file. Parallel agents are safe because files never overlap.
- A `PostToolUse` hook (`.claude/settings.json`) records heartbeats automatically as a
  backstop, but it can't label your *activity* — so still clock in/switch/out.

See `timelog/README.md` for the schema and how durations are computed (idle gaps > 5 min
are not counted).

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
