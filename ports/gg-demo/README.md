# gg-demo — the platform-layer shakeout (DOOM-raise edition)

A small `bet` program that exercises the **newest** `gg` platform surface — relative mouse,
per-voice stereo pan, fixed-canvas presentation, streaming-audio backpressure, the extended
keycode table, and the `BET_GG_HEADLESS` CI switch — on top of the amendment-03 mixer. Where
[Pong](../pong/README.md) software-renders at the live window size (`gg.blit` + `gg.size`),
`gg-demo` renders a **fixed 320×200 logical framebuffer** (DOOM's resolution) and presents it
with `gg.show`: `gg.blit`'s input model with `gg.flush`'s scaling — integer nearest-neighbor
aspect-fit upscale, centered with black letterbox bars.

## Run it

`gg`'s windowed backend (minifb + cpal) is gated behind an off-by-default cargo feature, so the
default build stays dependency-free. Both build paths open the same window.

```sh
# --- native (the real deliverable) ---
cargo xtask run gg-demo                       # builds the compiler + runtime + demo, then runs it

# --- headless (CI): the SAME binary, no window, no audio device ---
BET_GG_HEADLESS=1 cargo xtask run gg-demo     # runs its 600 frames and exits cleanly

# --- interpreter (same window, quick iteration; render-bound, so it paces below 60fps) ---
cargo run -p driver --features "llvm,gg-desktop" -- run ports/gg-demo/gg-demo.bet
```

`cargo xtask run` discovers LLVM 18 itself (`cargo xtask setup-llvm` reports what it found), so
there is nothing to export. The interpreter path needs LLVM in the environment the normal way —
`eval "$(cargo xtask setup-llvm | sed -n 's/^  export /export /p')"` prints the right exports for
your machine.

The demo **self-terminates after 600 frames (~10s)** — or on `Esc` / window close — which is
what lets the headless CI invocation run to completion unattended. It prints the audio device
spec at startup, the raw-ring backpressure once a second, and a final state line.

## Controls / what you see

| input               | on screen                                                        |
|---------------------|------------------------------------------------------------------|
| move the mouse      | the cyan square integrates **`gg.mouseDelta()`** (relative mouse) |
| (automatic)         | the white bar sweeps with the tone's **`gg.tune`** stereo pan     |
| hold `Ctrl`         | red rect (keycodes 260/261 — the new modifier block)             |
| hold `F1`           | green rect (keycode 280 — the new F-key block)                   |
| hold `Tab`          | yellow rect (keycode 9 — ASCII printables)                       |
| `Esc` / close       | quit early                                                       |

The looping 440Hz tone pans full-left → full-right and back every ~8.5s. Pan is linear:
left gain `vol·(255−pan)/255`, right gain `vol·pan/255` (0 = full left, 128 = center,
255 = full right).

## What it exercises (the new `gg` surface)

| bet call                        | primitive                                  | rt-abi symbol        |
|---------------------------------|--------------------------------------------|----------------------|
| `gg.show(fb, w, h)`             | fixed-canvas present (aspect-fit, letterbox) | `bet_gg_show`      |
| `gg.mouseDelta() -> (dx, dy)`   | relative mouse (signed, drained per call)  | `bet_gg_mouse_delta` |
| `gg.tune(voice, vol, pan)`      | live per-voice volume + stereo pan         | `bet_gg_tune`        |
| `gg.audioSpec() -> (rate, ch)`  | audio device output config                 | `bet_gg_audio_spec`  |
| `gg.pending() -> int`           | raw `gg.audio` ring depth (backpressure)   | `bet_gg_pending`     |

…plus `gg.sound`/`gg.play`/`gg.stop`, `gg.poll`, and `gg.ticks` from the existing set. The
extended keycode table (Ctrl/Shift/Alt pairs 260–265, Pause 266, F1–F12 280–291, Tab/Backspace
and the punctuation keys at their ASCII codes) is documented in `crates/gg-backend/src/lib.rs`'s
`mod key` — the values are contractual.

## Headless mode (`BET_GG_HEADLESS=1`)

Set once in the environment (checked at the first gg call), a **desktop-featured** build runs
fully headless: no window opens and no audio device is touched. `gg.poll` always reports NONE,
`gg.show`/`gg.blit`/`gg.flush` discard, `gg.ticks` stays real (so frame pacing still paces),
`gg.audioSpec` reports the fixed `(48000, 2)` default, and `gg.pending` reports `0` (instant
drain). This is how CI runs a compiled gg game to completion.

## How it works (bet's memory/value model)

- **The framebuffer, tone, and keystate are `mem.slab[T](n)` heap `[]T` buffers**, and ALL
  buffer writes stay inline in `main`: a slice shares its backing pointer when compiled but is
  deep-copied in the interpreter, so passing a buffer to a mutating helper would diverge the two
  paths (same discipline as Pong).
- **`gg.mouseDelta` drains** — each call returns the raw window-pixel deltas accumulated since
  the previous call (sign-preserving; sub-pixel remainders carry over) and resets the
  accumulator. minifb has no pointer lock, so deltas clamp once the cursor pins a window edge.
- **`gg.tune` retargets a playing voice** — the mixer reads each voice's volume/pan per sample,
  so a sweep is click-free. Voices start at pan 128 (center).
- **`gg.pending` reads the raw `gg.audio` ring** (Pong's path), not the mixer; this demo plays
  through the mixer, so it prints 0. A streaming music synth submits with `gg.audio` and uses
  `pending` to pace itself against the device.

## Fidelity / platform notes

- **Not in the golden corpus.** A live window is non-deterministic (real time + input), so —
  like Pong and the Oregon Trail port — `gg-demo` lives in `ports/` and is run manually, not
  gated in CI. (The headless mode exists precisely so a future CI job CAN run compiled gg
  binaries deterministically enough to smoke-test them.)
- **macOS** is the validated target (same backend crates as Pong: minifb + cpal). Linux would
  need the X11/ALSA link libraries wired into `crates/driver/src/link.rs`.
