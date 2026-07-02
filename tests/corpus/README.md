# `tests/corpus` — golden programs ★

Real bet programs with expected outputs, checked in as `name.bet` + `name.expected`.
Populated in **Step 1c** (30-50 small programs; bootstrap-plan.md §1c). The corpus
simultaneously unblocks the interpreter (run these), frontend (parse these), formatter
(round-trip these), and conformance suite (it *is* the seed of it). Per amendment §6.1 it
grows targeted programs for every new feature (sum-type interpreters, BAM-angle bit math,
byte parsing, thinker-style function tables).

Not a Cargo crate — these are bet source + data, driven by `cargo xtask corpus` (Step 2+).
No files here yet (the `.bet` extension and ASI rules are settled while writing them).
