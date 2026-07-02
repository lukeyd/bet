# `bytes` — binary I/O (NEW, amendment §2.7)

Written in bet (implementation pending the compiler). Endian-explicit reads and packed-
struct reinterpretation for WAD parsing and fast lexing. API: `bytes.readU32le(buf, off)`
(and family), `bytes.slice`, `bytes.cast[T]` (bounds-checked; unsafe variant gated like
`trust()`). `[]u8` is the byte-slice type.
