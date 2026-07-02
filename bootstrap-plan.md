# Bootstrap Plan: Parallel-Ready Project Structure

> **Companion to:** Language Specification (Draft v0.1)
> **Purpose:** Defines what gets built first so every workstream in spec §11.3 can proceed in parallel with no unbuilt dependencies, plus the repository layout and the Rust toolchain we rely on.

---

## 1. Core Principle

**Build the contracts as code, not as documents.** A spec document unblocks nobody until it is executable; a stub crate unblocks everyone. The Phase 0 contracts from spec §11.2 (language spec, mid-level IR, runtime ABI) are each delivered as a working artifact — types, stubs, and test corpora — before any team fans out.

The goal state: after the bootstrap phase, the **only** coordination surface between agents is two contract crates (`midir` and the runtime ABI) plus the golden test corpus. Everything else is independently buildable and testable.

---

## 2. Bootstrap Sequence

### Step 0 — Workspace Skeleton (1 agent, ~1 day)

The true first dependency. Before any real code:

- Cargo workspace with **every crate stubbed** and compiling empty.
- CI matrix green on all 6 targets: Linux / macOS / Windows × x86-64 / ARM64.
- Dependency-graph enforcement in CI (see §5) so no crate can bypass the contracts.
- Repo conventions: rustfmt config, clippy config, PR template.

Every agent from this point lands PRs against passing CI from their first commit. Setting up cross-platform CI *after* code exists is always worse.

### Step 1 — The Three Contract Artifacts (3 agents, parallel)

**Ordering caveat:** the grammar/corpus (1c) should lead by a few days — the IR design needs to see what surface constructs it must represent (especially `holla` blocks and multi-value returns).

#### 1a. The `midir` crate — highest-leverage single artifact

Not a design doc. Deliverables:

- Actual Rust types for the mid-level IR.
- A builder API for constructing IR programmatically.
- A **textual serialization format** (`.mir` files) with parser and printer.
- A validator (well-formedness checks).

This is what makes frontend and backend genuinely parallel: the backend team starts immediately from **hand-written `.mir` test files** (as anticipated in spec §11.3), and the frontend has a concrete target instead of a moving one. The textual format later becomes the interchange for differential testing.

#### 1b. Runtime ABI as a stub implementation

A crate defining every `extern "C"` entry point — allocation, crib push/evict, holla check intrinsics, task spawn/yield — **plus a naive malloc-backed implementation of all of them.** No arenas, no real scheduler; just correct semantics.

- The backend links against it and produces running binaries before the real runtime exists.
- The runtime team's job becomes "replace the stub without changing the signatures" — cleanly parallel.
- The interpreter calls the same stub for memory operations, keeping semantics aligned across both execution paths.

#### 1c. Frozen grammar + golden corpus

- The EBNF grammar, formalized.
- **30–50 small programs** in the language with expected outputs, checked into `/tests/corpus`.

The corpus is the underrated artifact — it simultaneously unblocks:

- the interpreter (run these),
- the frontend (parse these),
- the formatter (round-trip these),
- the conformance suite (it *is* the seed of it).

Writing 50 real programs also forces the open syntax questions (ASI rules, default visibility, numeric spellings) to be settled faster than debate would.

### Step 2 — Tracer Bullet (1 agent, before fan-out)

Thread the tiniest possible program — literally `spill.it("hi")` — through:

```
frontend → midir → backend → stub runtime → running binary on all 3 OSes
```

This is spec milestone 2 pulled as early as possible. It exists to catch contract mistakes while they are cheap: an ABI or IR design flaw should be found by one tracer-bullet agent in week 3, not by five agents' merge conflicts in week 10.

### Step 3 — Full Parallel Fan-Out

Once the tracer bullet lands, every workstream in spec §11.3 is unblocked simultaneously:

| Workstream | Builds against | Independent because |
|---|---|---|
| Frontend | grammar + `midir` | emits IR, never sees backend |
| Backend | `midir` + runtime ABI | consumes hand-written and frontend-emitted `.mir` |
| Runtime | ABI signatures | swaps stubs for real cribs/scheduler, no signature changes |
| Interpreter | grammar + corpus + ABI stubs | races ahead on corpus; differential-test partner |
| Stdlib | ABI allocator context | allocator-aware from day one |
| Tooling (fmt, LSP) | frontend library | consumes the frontend as a crate |
| Conformance/CI | corpus | grows corpus; runs differential tests interpreter-vs-compiled |

The only standing coordination cost is changes to the two contract crates — a narrow, visible channel where cross-team churn happens, instead of churn everywhere.

---

## 3. Repository Structure

One Cargo workspace. Contract crates are marked ★.

```
lang/                          # working name TBD (spec §12)
├── Cargo.toml                 # workspace root: members, shared deps, lints
├── Cargo.lock                 # single lockfile for the whole tree
├── rust-toolchain.toml        # pins the exact Rust version for all agents & CI
├── .github/workflows/         # CI: 3 OS × 2 arch matrix, graph check, fmt, clippy
│
├── spec/
│   ├── grammar.ebnf           # ★ frozen surface grammar
│   ├── semantics.md           # formalized semantics
│   ├── midir.md               # IR design rationale (the code in crates/midir is normative)
│   └── runtime-abi.md         # ABI rationale (the code in crates/rt-abi is normative)
│
├── crates/
│   ├── midir/                 # ★ IR types, builder, textual format, validator
│   ├── rt-abi/                # ★ extern "C" signatures + shared ABI types
│   ├── rt-stub/               # naive malloc-backed impl of rt-abi (bootstrap only)
│   ├── runtime/               # real runtime: cribs, generations, scheduler, OS layer
│   ├── frontend/              # lexer → parser → AST → typecheck → midir (a LIBRARY)
│   ├── backend/               # midir → LLVM IR (inkwell), pass pipeline, lld linking
│   ├── interp/                # tree-walking interpreter over the AST
│   ├── fmt/                   # canonical formatter (consumes frontend)
│   ├── lsp/                   # tower-lsp server (consumes frontend)
│   ├── driver/                # the CLI users invoke: compile / run / fmt / test
│   └── xtask/                 # repo automation (see §4.3)
│
├── std/                       # the slang stdlib, written in the language itself
│   ├── spill/
│   ├── fs/
│   ├── mem/
│   └── ...                    # per spec §9.2 module map
│
└── tests/
    ├── corpus/                # ★ golden programs + expected outputs
    ├── mir/                   # hand-written .mir files for backend-only testing
    ├── conformance/           # harness: runs corpus via interp AND compiled, diffs
    └── bench/                 # pause-time & allocation benchmarks (memory regressions)
```

### Allowed dependency graph (CI-enforced)

```
frontend  ──►  midir
backend   ──►  midir, rt-abi
runtime   ──►  rt-abi
rt-stub   ──►  rt-abi
interp    ──►  frontend, rt-abi
fmt       ──►  frontend
lsp       ──►  frontend
driver    ──►  frontend, backend, interp, fmt
```

Notably **forbidden:** `backend → frontend` (the backend may only know the IR), and anything → `runtime` internals (only the ABI). The monorepo's danger is that "everything can see everything" erodes the contracts; the graph check keeps them honest mechanically, not socially.

---

## 4. Rust Toolchain: Yes, Rely On It Fully

Since the compiler, runtime, and all tooling are written in Rust (spec §11.1), we get Rust's mature toolchain for free — and we should lean on all of it rather than building any bespoke build machinery.

**One clarification of layers:** these tools build *the implementation* (the compiler itself). Programs written in *our language* are built by our own `driver` CLI and, later, our own package manager (spec §11.3, project 6). Cargo is the house we build the language in; our driver is what users of the language get.

### 4.1 Core tools (required)

| Tool | Role | Why we rely on it |
|---|---|---|
| **`rustup`** | Toolchain manager | Installs pinned compiler version + all 6 cross-compilation target triples per agent machine. `rust-toolchain.toml` makes this automatic — every agent and CI runner gets the identical toolchain with zero setup drift. |
| **`cargo`** | Build system + package manager | The workspace feature *is* our monorepo tooling. Incremental cross-crate builds, one lockfile, `cargo build/test/run -p <crate>`. No Bazel/Nx needed. |
| **`cargo fmt`** (rustfmt) | Formatter | Canonical formatting, enforced in CI. (Also the philosophical model for our own language's formatter — spec cites Go's lesson.) |
| **`cargo clippy`** | Linter | Catches bug classes at review time; enforced in CI at `-D warnings`. |
| **`cargo test`** | Test runner | Unit + integration tests per crate; the conformance harness is itself a cargo test target. |

### 4.2 Strongly recommended additions

| Tool | Role | Why |
|---|---|---|
| **`cargo nextest`** | Faster test runner | Better parallelism and per-test isolation; matters once the conformance corpus grows into hundreds of programs. |
| **`cargo deny`** | Dependency audit | License + advisory + duplicate-version checks; cheap insurance for a long-lived project. |
| **`cargo bench` / criterion** | Benchmarking | Backs the pause-time and allocation benchmarks the spec requires to catch memory regressions. |
| **`cargo doc`** | API docs | The `midir` and `rt-abi` contract crates should have first-class rustdoc — that documentation *is* the inter-agent interface spec. |
| **`insta`** (crate) | Snapshot testing | Ideal for parser/AST and IR-printer golden tests; review diffs instead of hand-editing expected output. |

### 4.3 The `xtask` pattern for repo automation

Rust projects conventionally avoid Makefiles/shell scripts by putting repo automation in a small Rust crate invoked as `cargo xtask <command>`. Ours should own:

- `cargo xtask corpus` — run the golden corpus through interpreter and compiled path, diff results (the differential tester).
- `cargo xtask graph-check` — verify the dependency graph in §3 (parse `cargo metadata`, fail on undeclared edges).
- `cargo xtask dist` — build release artifacts for all 6 targets.

This keeps automation cross-platform (it must run on Windows CI) and in the same language agents already work in.

### 4.4 The two places plain Cargo isn't enough

1. **LLVM.** `inkwell` needs an LLVM installation at build time; Cargo won't install it. Pin the LLVM version (spec §12) and handle per-OS setup in CI config + a documented `cargo xtask setup-llvm` helper. This is the single biggest build-environment complication — solve it in Step 0, not when the backend team hits it.
2. **Cross-compilation of the runtime.** The `no_std`-flavored runtime static library must build for all 6 triples. `rustup target add` covers the Rust side; linking on foreign targets is handled by bundling `lld` (already planned, spec §11.1) rather than depending on host linkers.

---

## 5. CI Definition (Step 0 deliverable)

Every PR, on the full 3-OS matrix (ARM64 via native runners or cross-compile + emulation where needed):

1. `cargo fmt --check`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo xtask graph-check` — dependency-graph enforcement
4. `cargo nextest run --workspace`
5. `cargo xtask corpus` — differential testing (once tracer bullet lands)
6. Benchmarks on main-branch merges, with regression thresholds (post-milestone 3)

---

## 6. Milestone Alignment

| Bootstrap step | Feeds spec milestone (§11.4) |
|---|---|
| Step 0 skeleton + CI | Milestone 1 (Phase 0 frozen, interpreter + CI spun up) |
| Contract artifacts | Milestone 1 |
| Tracer bullet | Milestone 2 ("hello world" on all 3 OSes) — pulled as early as possible |
| Fan-out: runtime replaces stubs | Milestones 3–4 (cribs, then tag/holla end-to-end) |
| Fan-out: fmt on frontend library | Milestone 5 (self-hosted formatter) |
