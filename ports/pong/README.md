# Pong — the `gg` platform-layer shakeout

A `bet` port of PONG. Where the Oregon Trail port is text/stdin only, Pong is the first
program to exercise **`gg`**, `bet`'s platform layer (its SDL-equivalent) — real **video**,
**input**, **audio**, and **timing** in a windowed game loop. It's the gamedev shakeout for
`gg` before Doom.

## Run it

```sh
cargo xtask run pong
```

That builds the compiler (`driver --features llvm`) and the windowed runtime
(`runtime --features gg-desktop`), compiles the game, and runs it. LLVM 18 is discovered
automatically — nothing to export. Pong needs no assets, which is why it's the first port to
reach for.

`gg`'s windowed backend (minifb + cpal) is gated behind an off-by-default cargo feature, so the
default build stays dependency-free. The interpreter opens the same window, for quick iteration:

```sh
cargo run -p driver --features "llvm,gg-desktop" -- run ports/pong/pong.bet
```

A **resizable** window opens (default 960×640). It runs at **true dynamic resolution**: each
frame the program asks `gg.size()` for the live window size, and **whenever that size changes**
it reallocates its framebuffer to match 1:1 — so the field **fills the whole window** and gains
real detail as you enlarge or maximize it. Paddle/ball/speed are derived from the current size,
so the whole board scales with the window.

The window presents at 60 FPS; `GG_FPS` overrides the cap (`GG_FPS=0` to uncap).

## Source layout

`pong.bet` is the entry; it pulls three sibling modules (`bet`'s multi-file `pull` — only
`flex` items cross a file boundary, referenced qualified, e.g. `game.step(...)`, the type
`dims.Dims`, `consts.WHITE`):

| file | responsibility |
|------|----------------|
| `consts.bet` | colors, keycodes, event kinds, and audio params |
| `dims.bet`   | per-frame geometry: the `Dims` struct, `dimsFor`, `clampPaddle` |
| `game.bet`   | the `Game` state + the pure physics/AI `step` (pulls `dims`) |
| `pong.bet`   | **entry** — `main` owns the platform loop and **all rendering** |

Rendering is deliberately **not** split into a module: every framebuffer write must stay
inline in `main` (see the memory/value note below), so only the pure simulation is modularized.

## Controls

| key            | action                    |
|----------------|---------------------------|
| `W` / `S`      | left paddle up / down     |
| `↑` / `↓`      | right paddle up / down    |
| *(no arrows)*  | right paddle tracks the ball (simple AI, so one player can rally solo) |
| `Esc` / close  | quit (prints the final score) |

Beeps on every wall bounce, paddle hit, and score.

## What it exercises (the `gg` surface)

`pong.bet` builds its own frame loop and keystate on top of the `gg` primitives. Amendment-02
SP0.4 originally reserved exactly four; dynamic resolution adds a **fifth, `gg.size()`** (the
program must learn the drawable size), amending that decision:

| bet call                    | primitive         | rt-abi symbol       |
|-----------------------------|-------------------|---------------------|
| `gg.blit(fb, w, h)`         | video (present)   | `bet_gg_present`    |
| `gg.audio(samples, n)`      | audio (i16)       | `bet_gg_audio`      |
| `gg.poll() -> (kind, code)` | keyboard/quit     | `bet_gg_poll`       |
| `gg.ticks() -> int`         | timing (ns)       | `bet_gg_ticks`      |
| `gg.size() -> (w, h)`       | window size       | `bet_gg_size`       |

## How it works (bet's memory/value model)

- **Framebuffer + keystate are `mem.slab[T](n)` heap `[]T` buffers.** `bet` has no runtime-sized
  zero-init array literal, `vec` is append-only, and `crib` slabs aren't `[]`-indexable — so a
  random-access mutable buffer (the framebuffer) needs `mem.slab`, whose slice elements are
  writable (`fb[i] = color`) on both the interpreter and compiled paths.
- **Dynamic resolution.** Each frame reads `gg.size()`; on a change, the framebuffer is
  reallocated to the new `w*h` and positions are rescaled proportionally. The window uses
  `Scale::X1` (1:1, no upscaling) so the framebuffer maps to pixels exactly.
- **All framebuffer writes stay inline in `main`.** A slice shares its backing pointer when
  compiled but is deep-copied in the interpreter, so passing `fb` to a mutating helper would
  make the two paths diverge. Game state is value-threaded through a small `Game` struct (which
  stores the ball **direction ±1**, not a velocity, so speed auto-scales with resolution) and
  updated by the pure `step()` function; per-frame geometry flows through a `Dims` struct.
- **Frame pacing** is a busy-wait on `gg.ticks()` to ~16.6 ms/frame (≈60 fps). Since cwage #97
  `gg` itself also sleeps to a 60 FPS floor inside the present call, so the busy-wait usually
  finds its budget already spent and spins for close to nothing. A port written today should
  lean on `gg`'s cap (and `GG_FPS`) rather than spin.

## Fidelity / platform notes

- **Not in the golden corpus.** A live window is non-deterministic (real time + input), so —
  like the Oregon Trail port — Pong lives in `ports/` and is run manually, not gated in CI.
- **HiDPI/Retina:** `gg.size()` returns logical points, not physical pixels, so on a Retina
  display the framebuffer tracks points (≈½ the physical resolution). It still gains real detail
  and scales with the window; true physical-pixel resolution would need a different windowing
  layer. The old framebuffer is leaked on each resize (`mem.slab` has no free) — negligible here.
- **macOS** is the validated target. The backend crates (minifb + cpal) are cross-platform, and
  the runtime compiles for Linux too, but the driver's executable link step only adds the macOS
  frameworks so far (Cocoa/Carbon/Metal/MetalKit + AudioUnit/CoreAudio/CoreFoundation); Linux
  would need the X11/ALSA link libraries wired into `crates/driver/src/link.rs`.
