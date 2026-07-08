# bet

A compiled, statically-typed programming language whose keywords are durable internet
slang, with a game-development-first memory model (arena "cribs" + generational
`tag`/`holla` handles, no tracing GC). This npm package bundles the `bet` CLI with its
tree-walking interpreter so you can try the language with zero commitment:

```sh
npx betlang demo              # list the bundled demos
npx betlang demo oregon-trail # the 1978 MECC classic, in your terminal
npx betlang demo pong         # opens a real window, with sound
```

Or write your own:

```bet
// hi.bet
pull "spill"

finna main() {
    spill.it("hi")
}
```

```sh
npx betlang run hi.bet
```

## What's in the box

- `bet run <file.bet>` — execute a program on the interpreter (this is the path the
  compiler itself is differentially tested against).
- `bet fmt [--check] <file.bet>` — the canonical formatter.
- `bet build --emit <tokens|ast|mir> <file.bet>` — dump compiler intermediates.
- `bet demo [name]` — run a bundled example (npm-only convenience).

Programs that open a window (`pong`, `gg-demo`) work out of the box on macOS and
Windows; on Linux you need X11/ALSA client libraries at runtime (present on any
desktop distro).

## What's NOT in the box

`bet build` to a native executable requires the LLVM-enabled toolchain, which is not
bundled here (it's big, and it needs a C compiler on your machine anyway). Build it
from source: <https://github.com/lukeyd/bet>. Everything else — including the game
ports and the self-hosted frontend — is in the repo.

## Platforms

Prebuilt binaries ship for macOS (arm64, x64), Linux (x64, arm64) and Windows (x64)
as `@betlang/<platform>` optional dependencies; npm installs only the one that
matches your machine. On an unsupported platform, build `bet` from source and set
`BET_BIN=/path/to/bet`.
