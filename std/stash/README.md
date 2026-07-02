# `stash` — hash maps (NEW, amendment §2.3)

Written in bet (implementation pending the compiler). Where you keep the goods.
API: `stash.new[K, V]()`, `.put(k, v)`, `.peep(k) -> (V, bool)`, `.yeet(k)`, `.gang()`.
Allocator-aware: `stash.new[str, Symbol](in: astCrib)`. Blocking for symbol tables,
string interning, scope resolution, WAD lump directories.
