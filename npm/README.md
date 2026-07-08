# npm/ — the `betlang` npm distribution

The "try it in one command" channel: `npx betlang demo pong`. One tiny user-facing
package (`betlang`, a Node shim) plus one prebuilt-binary package per platform
(`@betlang/<platform>-<arch>`), wired as `optionalDependencies` so npm downloads
only the binary matching the user's machine — the esbuild pattern, with no
postinstall scripts.

## Layout

```
npm/betlang/          the published `betlang` package (shim + demos + README)
  bin/bet.js           resolves the platform binary and execs it; adds `demo` + `--version`
  demos/               STAGED, gitignored — populated by stage.mjs from ports/
npm/demos-extra/       demo sources that live only in the npm package (hello)
npm/stage.mjs          stages demos and assembles the platform packages
npm/dist/              STAGED, gitignored — the per-platform packages ready to publish
```

The binary staged into the platform packages is the **default (LLVM-free) driver
with the desktop window**: `cargo build --release -p driver --features gg-desktop`.
That gives `run` / `fmt` / `--emit` and windowed games in a ~2 MB binary; native
`bet build` intentionally stays out of the npm channel (it needs LLVM + a system C
compiler — see the package README).

## Publishing

Releases are built by `.github/workflows/release-npm.yml` (tag `npm-v<version>` or
run it manually). It builds the binary on native runners for all five platforms,
stages everything, and publishes the five platform packages followed by `betlang`.

One-time setup before the first release:

1. Create the `betlang` **org** on npmjs.com (the platform packages live under the
   `@betlang` scope; the org name must match).
2. Add an npm **granular access token** with publish rights to the repo's secrets
   as `NPM_TOKEN`.
3. Bump `version` in `npm/betlang/package.json` **and** its `optionalDependencies`
   pins (stage.mjs refuses to run if they drift), commit, tag `npm-v<version>`, push
   the tag.

## Local development / testing

```sh
cargo build --release -p driver --features gg-desktop
node npm/stage.mjs --demos
node npm/stage.mjs --platform darwin-arm64 --binary target/release/bet   # your platform
cd npm/betlang && npm pack && cd ../dist/betlang-darwin-arm64 && npm pack
# install both tarballs into a scratch dir and run `npx bet demo`
```

Or skip packing entirely: `BET_BIN=target/release/bet node npm/betlang/bin/bet.js run <file.bet>`.
