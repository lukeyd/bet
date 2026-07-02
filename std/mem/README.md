# `mem` — memory

Written in bet (implementation pending the compiler). API: `mem.crib(size)` make arena,
`mem.evict` O(1) mass-free, `mem.scratch()` per-frame auto-evicted arena, `mem.receipts()`
allocation stats. The crib/tag/holla memory model is the language's differentiator
(language-spec.md §7).
