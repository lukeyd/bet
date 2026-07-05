# gg-demo — the compositor + mixer + mouse shakeout

A small `bet` program that exercises the **new** `gg` platform primitives — textured/alpha
sprite compositing, a voice-mixing audio path, and mouse input/position — added in
`plan-amendment-03.md` (the deliberate raise of the amendment-02 SP0.4 ceiling). Where
[Pong](../pong/README.md) software-renders a 32-bit framebuffer with the original five
primitives, `gg-demo` drives the higher-level compositor directly: it uploads a sprite once,
pre-renders a tone once, then each frame clears a fixed logical canvas, draws a translucent
rectangle and the sprite at the cursor, and presents.

## Run it

`gg`'s windowed backend (minifb + cpal) is gated behind an off-by-default cargo feature, so the
default build stays dependency-free. Both build paths open the same window.

```sh
# one-time env for the LLVM codegen path (this machine)
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18
export LIBRARY_PATH="/opt/homebrew/lib:$LIBRARY_PATH"

# --- native (the real deliverable) ---
cargo build -p driver  --features llvm           # the bet compiler
cargo build -p runtime --features gg-desktop     # the windowed runtime (libruntime.a)
target/debug/bet build ports/gg-demo/gg-demo.bet --runtime real -o gg-demo
./gg-demo

# --- interpreter (same window, quick iteration) ---
cargo run -p driver --features "llvm,gg-desktop" -- run ports/gg-demo/gg-demo.bet
```

A window opens onto a fixed **320×240 logical canvas**. Unlike Pong's dynamic resolution, the
canvas is a fixed size and is **aspect-fit** into the window at `gg.flush()` — nearest-neighbor
upscaled by an integer factor and centered with black letterbox bars — so the sprite stays crisp
at any window size. `gg.mouse()` reports the cursor already mapped back into logical-canvas
coordinates.

## Controls

| input          | action                                   |
|----------------|------------------------------------------|
| mouse move     | the sprite + box follow the cursor       |
| left click     | play the 440Hz tone (a mixer voice)      |
| `Esc` / close  | quit (prints the click count)            |

## What it exercises (the `gg` surface)

`gg-demo.bet` uses the compositor/mixer/mouse primitives added in amendment-03, plus `gg.poll`
and `gg.ticks` from the original set:

| bet call                              | primitive              | rt-abi symbol     |
|---------------------------------------|------------------------|-------------------|
| `gg.tex(buf, off, w, h) -> int`       | upload RGBA8 texture   | `bet_gg_tex`      |
| `gg.frame(w, h, color)`               | begin/clear canvas     | `bet_gg_frame`    |
| `gg.sprite(tex, x, y)`                | alpha sprite blit      | `bet_gg_sprite`   |
| `gg.rect(x, y, w, h, color)`          | translucent fill       | `bet_gg_rect`     |
| `gg.flush()`                          | present + pump input   | `bet_gg_flush`    |
| `gg.sound(buf, off, len, ch, rate) -> int` | register PCM sound | `bet_gg_sound`    |
| `gg.play(sound, loop, vol) -> int`    | start a mixer voice    | `bet_gg_play`     |
| `gg.stop(voice)`                      | stop a voice           | `bet_gg_stop`     |
| `gg.mouse() -> (x, y)`                | cursor position        | `bet_gg_mouse`    |
| `gg.poll() -> (kind, code)`           | input (incl. mouse)    | `bet_gg_poll`     |
| `gg.ticks() -> int`                   | timing (ns)            | `bet_gg_ticks`    |

Mouse buttons arrive through `gg.poll()` as `MOUSE_DOWN` (kind 5) / `MOUSE_UP` (kind 6) with
`code` 0 = left, 1 = right; the cursor **position** comes from `gg.mouse()`.

## How it works (bet's memory/value model)

- **Sprite + tone buffers are `mem.slab[T](n)` heap `[]T` buffers**, built once inline in `main`.
  The RGBA sprite is a 16×16 soft-edged circle written a channel at a time; the tone is a 440Hz
  square wave written a sample at a time.
- **`gg.tex` / `gg.sound` take a byte offset** into the buffer. The demo passes `0`; the compiled
  path converts the byte offset to an element index (`byteOff / sizeof(T)`) since midir indexes
  element-granularly, and the interpreter serializes its `[]i16` tone to interleaved little-endian
  bytes to mirror the compiled path's raw byte view.
- **All buffer writes stay inline in `main`.** A slice shares its backing pointer when compiled but
  is deep-copied in the interpreter, so passing a buffer to a mutating helper would diverge the two
  paths (same discipline as Pong).
- **The mixer** resamples each sound to the audio device's rate/channels once, then sums active
  voices (plus Pong's raw `gg.audio` ring) in the cpal callback. The mixer lives behind its own
  lock, separate from the window/input state, so the audio callback never blocks the game loop.

## Fidelity / platform notes

- **Not in the golden corpus.** A live window is non-deterministic (real time + input), so — like
  Pong and the Oregon Trail port — `gg-demo` lives in `ports/` and is run manually, not gated in CI.
- **macOS** is the validated target (same backend crates as Pong: minifb + cpal). Linux would need
  the X11/ALSA link libraries wired into `crates/driver/src/link.rs`.
