# Plan Amendment 03: `gg` platform surface raise — compositor + mixer + mouse

> **Amends:** Plan Amendment 02 §SP0.4 (`gg` surface scope for v1), which set the
> "framebuffer + audio ring + input + timing" floor as the ceiling, and its own post-M6
> amendment that added `gg.size()` as a sanctioned fifth primitive.
> **Status:** Working resolution, ratified with the rest of SP0 at Milestone 7 (language
> freeze); folded into the spec on ratification.
> **Summary:** Deliberately raises the SP0.4 `gg` ceiling to include a **textured, alpha
> sprite compositor**, a **voice-mixing audio path**, and **mouse input + position**. Nine
> new `rt-abi` `bet_gg_*` entry points join the frozen platform block. GPU/shaders/multi-window
> remain out.

---

## SP0.4′ — `gg` surface raise → **compositor + mixer + mouse (real game platform)**

**Prior decision (amendment-02 §SP0.4).** "The floor *is* the ceiling for v1": `gg` was to be
exactly the four reserved primitives — `gg.blit` (present a software-rendered `FrameBuffer`),
`gg.audio` (an interleaved audio ring), `gg.poll` (keyboard + mouse-move + quit events), and
`gg.ticks` (monotonic timing) — later joined by `gg.size()` for dynamic resolution (the fifth,
justified because a native-resolution renderer *must* learn the drawable size). Doom
software-renders to a framebuffer, so the floor was declared sufficient for the oracle.

**Decision — raise the ceiling.** `gg` now also offers a small retained-mode 2D layer on top of
the framebuffer, a mixer above the raw audio ring, and mouse position/buttons:

- `gg.tex(buf, byteOff, w, h) -> int` — upload an RGBA8 texture; returns a 1-based id.
  (`bet_gg_tex`)
- `gg.frame(w, h, color)` — begin a frame: (re)size a fixed **logical canvas** and clear it to
  `0x00RRGGBB`. (`bet_gg_frame`)
- `gg.sprite(tex, x, y)` — premultiplied src-over blit of a texture, clipped. (`bet_gg_sprite`)
- `gg.rect(x, y, w, h, color)` — src-over rectangle fill (`0xAARRGGBB`). (`bet_gg_rect`)
- `gg.flush()` — present the composited canvas, **aspect-fit + letterboxed** into the window,
  and pump input. (`bet_gg_flush`)
- `gg.sound(buf, byteOff, byteLen, channels, rate) -> int` — register a PCM sound; 1-based id.
  (`bet_gg_sound`)
- `gg.play(sound, loop, volume) -> int` — start a mixer voice (Q8 volume); 1-based voice id.
  (`bet_gg_play`)
- `gg.stop(voice)` — stop a voice. (`bet_gg_stop`)
- `gg.mouse() -> (x, y)` — the cursor in logical-canvas coordinates. (`bet_gg_mouse`)

Two new `event_kind`s join the frozen set: `MOUSE_DOWN = 5` and `MOUSE_UP = 6` (with `code`
0 = left, 1 = right). Mouse **buttons** arrive through the unchanged `gg.poll() -> (kind, code)`;
mouse **position** comes from `gg.mouse()`. `gg.poll`'s arity is intentionally untouched, so the
Pong port keeps working verbatim.

**Why (user-directed).** The floor was sized for Doom, which is a single software-rendered
framebuffer. The next port target — **Frozen Bubble** — is a sprite game: dozens of alpha-blended
bubbles composited per frame, a mixed soundtrack of overlapping effects, and a mouse-aimed
launcher. Expressing that pixel-by-pixel on a bare framebuffer (as Pong does) is possible but
punishing, and a per-frame full-canvas software blit in `bet` source is the wrong altitude for a
platform layer that already owns the window. Making `gg` a *real* game platform — one that can
carry a sprite game without the program re-implementing a compositor and a mixer each time — is a
direct, sanctioned expansion. The `gg-demo` port (`ports/gg-demo/`) is the shakeout.

**Design guarantees kept:**
- **The freeze discipline holds.** These are additions to the `rt-abi` platform block, implemented
  behind the same headless-stub / real-backend split; the four (now five) original primitives are
  byte-for-byte unchanged, and `gg.audio`'s raw ring still drains 1:1 *through* the new mixer.
- **`gg` still lives inside the runtime.** The compositor and mixer are pure software in
  `gg-backend` (minifb + cpal), behind the off-by-default `desktop`/`gg-desktop` features. The
  default build pulls no window/audio dependency.
- **No deadlock regressions.** The mixer sits behind its **own** mutex so the cpal audio callback
  never locks the outer `gg` state — preserving the single-lock invariant the original backend
  established.

**Explicitly still OUT of the ceiling:** GPU/3D acceleration, shaders, render-to-texture,
multiple windows, gamepad, and networking. The new ceiling is "framebuffer + audio + input +
timing + size + **textured alpha compositing + voice mixer + mouse**." Anything past that is a
future amendment, not v1.

**Closes:** the Frozen-Bubble-shaped gap in amendment-02 §8's "how much SDL-equivalent surface
`gg` absorbs" — the answer is now "enough to carry a sprite game," not "just a framebuffer."
