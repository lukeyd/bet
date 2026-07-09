# bet → npm: v1 Launch Plan

Ship the `bet` compiler as an `npm install`-able native toolchain.

**Target experience**
```sh
npm i -g @betlang/cli
bet run   hello.bet        # interpreter — every platform, zero system deps
bet fmt   hello.bet        # formatter   — every platform
bet build game.bet -o game # native exe  — needs a system C toolchain (v1)
```

## Locked decisions

- **Full native toolchain**, not interpreter-only. The npm binary bundles LLVM codegen +
  the runtime archives so `bet build` produces real native executables.
- **v1 = the "Rust model":** ship the compiler + runtime; require the user to already have a
  system C toolchain (`cc`/`clang` + linker). This is exactly what `rustc` does. The
  self-contained **"Zig model"** (bundle our own linker, need nothing installed) is the v2
  roadmap below.
- **Distribution shape = the esbuild/Biome pattern:** a scoped JS launcher `@betlang/cli`
  plus one prebuilt-binary package per platform (`@betlang/cli-<os>-<cpu>`) listed as
  `optionalDependencies` and gated by npm `os`/`cpu`. npm installs only the matching one.
  (`bet` and `betlang` are already taken on npm; a scope sidesteps that and namespaces the
  platform packages for free.)
- **`ports/` and `timelog/` stay in the repo, never in the tarball.** The npm package is a
  separate artifact from the git tree; a `files` allowlist (or a dedicated publish dir)
  ships only the launcher + binary + runtime archive. This also keeps the GPL/id-derived
  `ports/doom/` out of the Apache-licensed package.

## Target matrix (the "6 targets")

Authoritative list from `.github/workflows/ci.yml` and the commented `rust-toolchain.toml`
block; each built on its own native CI runner (LLVM-linked binaries don't cross-compile easily).

| npm platform pkg           | Rust target triple             | CI runner          | v1 codegen? |
|----------------------------|--------------------------------|--------------------|-------------|
| `@betlang/cli-linux-x64`   | `x86_64-unknown-linux-gnu`     | `ubuntu-24.04`     | ✅ |
| `@betlang/cli-linux-arm64` | `aarch64-unknown-linux-gnu`    | `ubuntu-24.04-arm` | ✅ |
| `@betlang/cli-darwin-x64`  | `x86_64-apple-darwin`          | `macos-13`         | ✅ |
| `@betlang/cli-darwin-arm64`| `aarch64-apple-darwin`         | `macos-14`         | ✅ |
| `@betlang/cli-win32-x64`   | `x86_64-pc-windows-msvc`       | `windows-2022`     | ⚠️ see risks |
| `@betlang/cli-win32-arm64` | `aarch64-pc-windows-msvc`      | `windows-11-arm`   | ⚠️ see risks |

## Phase 0 — Repo cleanup (unconditional, do first)

- **Untrack the stray compiled binary `betfe`** (~2.5 MB Mach-O, tracked *and* dirty; largest
  blob in the repo and its whole history). It's an accidental build output — the real thing is
  the source `selfhost/betfe.bet`, and `cargo xtask selfhost` builds to a temp dir. Fix the
  gitignore gap: `.gitignore` ignores `/pong` and `/oregon` but not `/betfe`.
  `git rm --cached betfe` + add `/betfe` next to `/pong`.
- **Add a real `LICENSE` file.** `Cargo.toml` sets `license = "Apache-2.0"` but it's marked a
  placeholder and no license text exists in the repo. Add top-level `LICENSE` (+ `NOTICE` per
  Apache convention) and drop the "placeholder — confirm before first release" comment.
- **Bump the version** from `0.0.0` → `0.1.0` (`[workspace.package]`, inherited by all 12 crates).
- **Decide `publish`**: `publish = false` is fine for npm-only; flip only if crates.io is also wanted.
- **Add a `CHANGELOG.md`** (Keep-a-Changelog) with the initial entry.
- **Rewrite `README.md` for end users.** It currently opens "Status: Step 3 — the parallel
  fan-out" and lists only `cargo`/`xtask` dev commands. Add: `npm i -g @betlang/cli`, a
  hello-world in bet, the platform matrix, the `bet build` C-toolchain prerequisite, and a
  LICENSE link. Keep the dev-facing content lower down or in `CONTRIBUTING.md`.
- **Fix the `setup-llvm` env-var typo:** `cargo xtask setup-llvm` prints `LLVM_SYS_181_PREFIX`,
  but the pinned crate is `llvm-sys 180` and everything else uses `LLVM_SYS_180_PREFIX`.
  (`crates/xtask/src/main.rs` ~line 572/574.)

## Phase 1 — Make the LLVM-linked binary self-contained (the core blocker)

The `bet --features llvm` binary is **not portable today**: `otool -L` shows it dynamically
links Homebrew/LLVM transitive C libs at hardcoded paths
(`/opt/homebrew/opt/zstd/…`, `…/llvm@18/lib/libunwind`, plus system `libxml2`/`ncurses`).
An npm user has none of those. Root cause: `llvm-sys` links LLVM's *own* libs statically but
emits `llvm-config --system-libs` (zstd, libxml2, ncurses, libedit, zlib) as **dynamic** `-l`.
No single `llvm-sys` feature flag fixes this (`force-static` only covers LLVM's own libs).

**Fix (pick one), then strip:**
- **A — provision a minimal LLVM 18** built with `-DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_ZLIB=OFF
  -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_LIBEDIT=OFF`, so
  `--system-libs` is empty and the binary is self-contained. Cleanest; makes CI reproducible.
- **B — a `crates/backend/build.rs`** that re-emits the system libs as
  `cargo:rustc-link-lib=static=zstd …` against static `.a`s staged in the LLVM prefix.
- Then `strip` (measured: 52 MB → 42 MB on this Mac).

**Definition of done:** on a machine with *no* Homebrew/LLVM installed, `bet build hello.bet -o
hello && ./hello` works (given a system `cc`). Verify with `otool -L`/`ldd` showing only OS libs.

## Phase 2 — `cargo xtask dist` (currently a stub)

Implement the stubbed `dist` subcommand (hand-rolled arg parsing like the other xtask cmds;
shells out to `cargo build`, per the existing `corpus_compiled`/`selfhost` pattern):

1. `cargo build -p driver --features llvm --release --target <triple>`.
2. `cargo build -p rt-stub --release --target <triple>` (and `-p runtime` if shipping the real
   runtime) so the `.a` is staged next to `bet` — `link.rs` finds the archive via
   `current_exe().parent()` with **no fallback**, so co-location is mandatory.
3. `strip` the binary.
4. Lay out a per-platform package dir: `{ bet[.exe], librt_stub.a, (libruntime.a) }`.
5. (Robustness) consider adding a `BET_RUNTIME_DIR` env override / search fallback in `link.rs`
   so the archive lookup can't silently break if a launcher relocates the binary.

## Phase 3 — npm package structure

Dedicated publish dir (bulletproof — the tarball physically can't contain `ports/`/`timelog/`):

```
npm/
  cli/            @betlang/cli         → JS launcher, "files":["bin/"], optionalDependencies: all platform pkgs
    bin/bet.js    resolves the platform pkg, execs its native `bet`, forwards argv/exit code
  cli-<os>-<cpu>/ @betlang/cli-…       → one per target: package.json (os/cpu) + bet[.exe] + *.a
```

- The JS shim `exec`s the **native** binary, so `current_exe()` resolves to the real `bet`
  and the co-located `.a` is found — the Phase-2 layout Just Works.
- **Preflight check:** if `bet build` is invoked and no `cc`/`clang` is on PATH, print a friendly
  slang-flavored error ("no C toolchain, fam — run `xcode-select --install`") per the repo's
  diagnostics convention.

## Phase 4 — Release CI → `npm publish`

New `.github/workflows/release.yml`, triggered on a version tag, 6-way matrix on the native
runners above. Per runner: provision LLVM 18 (apt `llvm-18-dev libpolly-18-dev libzstd-dev` on
Linux; `brew install llvm@18` on macOS — but per Phase 1 we want the *minimal* build, so CI
provisions that), run `cargo xtask dist`, upload the platform package. A final job publishes all
`@betlang/cli-*` packages then `@betlang/cli`, all at the same version.

## Known constraints & risks

- **Windows LLVM codegen isn't building yet.** `backend-llvm.yml` explicitly defers it: the
  Chocolatey/official LLVM installers lack `llvm-config` + libraries, so `llvm-sys` fails. **Open
  decision:** either solve Windows LLVM (prebuilt-with-libs or from-source) for v1, or ship
  Windows as **interpreter-only** in v1 (`bet run`/`bet fmt` work; `bet build` errors cleanly).
- **Linux is `-gnu` only (no musl).** A glibc-linked binary can fail on older/newer glibc. Decide
  whether v1 adds `*-musl` targets for a "download-and-run-anywhere" Linux binary.
- **Package size** ≈ 42 MB binary (stripped) + 17 MB `librt_stub.a` ≈ **~60 MB/platform**. In line
  with Zig/LLVM-based npm packages, but worth noting for install time.
- **`cc`/linker prerequisite** for `bet build` in v1 (Rust model). Documented + preflight-checked;
  removed entirely in the Zig-model roadmap.
- **Naming:** `@betlang/cli` assumed. Swap if a different scope/name is owned.

## Roadmap: v1 (Rust model) → v2 (Zig model, fully self-contained)

Goal: `bet build` needs **nothing** installed. Already partly planned in the repo
(`language-spec.md` §11.1 "lld bundled for linking"; `bootstrap-plan.md` "bundle lld rather than
depend on host linkers").

1. **Bundle a linker.** Replace the `cc`/`clang` shell-out in `link.rs` with `lld` — shipped
   alongside `bet`, or `rust-lld` from the toolchain, or `lld` driven via `llvm-sys`.
2. **Supply crt startup + libc**, which `cc` currently provides implicitly (the harder half —
   `lld` alone doesn't know where `crt1.o`/libc live). Bundle or locate them per target.
3. **Cross-compilation.** With a bundled linker + staged per-target runtime archives, `bet build
   --target <triple>` can emit foreign binaries from one host (Zig's headline trick). Enable the
   `rust-toolchain.toml` `targets` block at this point.
4. **Drop the C-toolchain prerequisite** from docs + the Phase-3 preflight check.

## Definition of done (v1)

- `npm i -g @betlang/cli` on a clean macOS + Linux machine (no Homebrew LLVM) →
  `bet run`, `bet fmt` work; `bet build hello.bet -o hello && ./hello` works given a system `cc`.
- Binaries carry no `/opt/homebrew` or non-OS dynamic deps (`otool -L`/`ldd` clean).
- `ports/` and `timelog/` confirmed absent from `npm pack` output.
- Repo: `betfe` untracked, `LICENSE` present, version `0.1.0`, end-user README, CHANGELOG.
- Windows story explicitly decided (full codegen vs interp-only) and documented.
