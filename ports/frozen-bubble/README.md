# Frozen Bubble — `bet` port

A port of [Frozen Bubble](https://github.com/kthakore/frozen-bubble) (GPL-2) to the `bet`
language, running on the extended `gg` game platform (sprite compositor + audio mixer + mouse;
see `plan-amendment-03.md`).

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

## Build & run (macOS)

```sh
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18
export LIBRARY_PATH="/opt/homebrew/lib:$LIBRARY_PATH"
cargo build -p driver  --features llvm
cargo build -p runtime --features gg-desktop
target/debug/bet build ports/frozen-bubble/frozen-bubble.bet --runtime real -o fb && ./fb   # native
# or interpreter:
cargo run -p driver --features "llvm,gg-desktop" -- run ports/frozen-bubble/frozen-bubble.bet
```
