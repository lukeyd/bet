# `str` — strings

Written in bet (implementation pending the compiler). API: `str.snip`, `str.glow`
(upper), `str.chill` (lower), `str.split`, `str.slaps(a, b)` equals. Plus byte-length,
byte indexing, non-copying slicing, and `bytes()` / `fromBytes()` zero-copy conversion
(amendment §2.7) — a lexer must not allocate per token.
