# Frozen Bubble — `bet` port

A port of [Frozen Bubble](https://github.com/kthakore/frozen-bubble) (GPL-2) to the `bet`
language, running on the extended `gg` game platform (sprite compositor + audio mixer + mouse).

## Assets are NOT committed (GPL-2)

Frozen Bubble's graphics and audio are GPL-2. This repo ships only the **baker**, never the
baked data. `assets.dat` (~170 MB of pre-decoded RGBA + PCM) and the generated `assets_gen.bet`
index are `.gitignore`d. To build the assets locally:

```sh
# 1. Get a Frozen Bubble checkout (GPL-2):
git clone --depth 1 https://github.com/kthakore/frozen-bubble /path/to/frozen-bubble

# 2. Bake its share/ assets into the packed .dat + generated index:
cargo xtask bake-frozen-bubble --src /path/to/frozen-bubble --out ports/frozen-bubble/assets.dat
```

The baker decodes PNG/GIF/BMP → RGBA8888 (via `image`) and Ogg Vorbis → i16 PCM (via `lewton`),
and emits a packed little-endian `FBD1` file (header + entry table + string table + blob region)
plus `assets_gen.bet` mapping asset names → entry indices. The game reads `assets.dat` at runtime
with `fs.peep` and uploads textures/sounds via `gg.tex`/`gg.sound`.

## Build & run

Bake the assets first (above) — there is no way around it, and `cargo xtask run frozen-bubble`
will tell you so rather than failing obscurely if `assets.dat` is missing. Then:

```sh
cargo xtask run frozen-bubble     # builds the compiler + runtime + port, then runs it

# or interpreter:
cargo run -p driver --features "llvm,gg-desktop" -- run ports/frozen-bubble/frozen-bubble.bet
```

`cargo xtask run` discovers LLVM 18 itself — nothing to export (`cargo xtask setup-llvm` reports
what it found). The interpreter path needs LLVM in the environment the normal way.
