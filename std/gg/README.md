# `gg` — game loop / input / platform layer

Written in bet, backed by the runtime's platform layer (amendment §2.6). API:
`gg.frame()`, `gg.dt()`, `gg.keys.pressed(k)`, plus the runtime-backed platform surface
`gg.blit(fb)` framebuffer present, `gg.audio(ring)` audio, `gg.poll() -> Event` input,
`gg.ticks()` hi-res timing. Doom targets `gg` directly; `extern` exists for everyone
else's SDL/OpenGL. GPU is explicitly out of v1.
