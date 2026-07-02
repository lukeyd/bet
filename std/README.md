# `std` — the bet standard library

Written **in bet itself**, so it stays empty scaffolding until the compiler exists
(these directories are NOT Cargo crates; the workspace excludes them). Each module
below has a placeholder README naming its intended API. Naming rules are binding
(language-spec.md §9.1): module names are slang nouns; function names are slang verbs
where a good mapping exists; one concept, one word, everywhere; domain math keeps
conventional names. Every stdlib API is allocator-aware.

| Module | Domain | Sample API |
|---|---|---|
| `spill` | print / format | `spill.it(x)`, `spill.f("hp: {}", hp)` |
| `fs` | file system | `fs.peep` (read, `[]u8`) · `fs.peepText` · `fs.drop` write · `fs.yeet` delete · `fs.pullUp` list |
| `str` | strings | `snip`, `glow` (upper), `chill` (lower), `split`, `slaps` (eq); byte len/index/slice, `bytes()`/`fromBytes()` |
| `math` | math | `clout` pow · `root` · `cook` RNG · `sin/cos/...` · `lap` wrapping-arith family |
| `squadops` | collections | `.stack` push · `.pop` · `.snatch` remove-at · `.gang` len · `.vibeCheck` filter · `.glowUp` map |
| `time` | time | `time.rn()` now · `time.chill(ms)` sleep |
| `net` | networking | `slideInto` connect · `yap` send · `peep` receive · `ghost` disconnect |
| `mem` | memory | `mem.crib(size)` · `mem.evict` · `mem.scratch()` · `mem.receipts()` |
| `gg` | game loop / input / platform | `frame()` · `dt()` · `keys.pressed(k)` · `blit(fb)` · `audio(ring)` · `poll()->Event` · `ticks()` |
| `vec` | vec2/3/4, mat4, SIMD | `add`, `scale`, `dot`, `cross`, `norm` |
| `stash` | hash maps (NEW) | `stash.new[K,V]()` · `.put` · `.peep` · `.yeet` · `.gang` |
| `bytes` | binary I/O (NEW) | `readU32le(buf,off)` · `slice` · `cast[T]` (bounds-checked) |

`stash` and `bytes` are added by plan-amendment-01 (§2.3, §2.7).
