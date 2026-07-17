# Licensing

This repository carries two licenses. The split is deliberate and the boundary is
a directory.

| Scope | License | Text |
| --- | --- | --- |
| The `bet` language, compiler, runtime, and toolchain — everything outside `ports/doom` | Apache-2.0 | [`LICENSE`](LICENSE) |
| `ports/doom` — the DOOM port | GPL-2.0-only | [`ports/doom/COPYING`](ports/doom/COPYING) |

`Cargo.toml` declares `license = "Apache-2.0"`. That declaration is correct and
covers every workspace member, because every workspace member is outside
`ports/doom`.

## Why `ports/doom` is GPL

`ports/doom` is a hand-translation of id Software's `linuxdoom-1.10` into `bet`.
A translation of a program into another language is a derivative work of the
original, in the same way a translation of a novel is. id released that source
under the GNU General Public License, version 2, and GPL-2 is a copyleft
license: the derivative must be distributed under the same terms. There is no
version of this port that is not GPL. Its license is inherited, not chosen.

Full provenance — including why the C originals' per-file headers still name the
superseded 1997 "DOOM Source Code License", and how copyright is attributed
across the port's files — is in [`ports/doom/LICENSE`](ports/doom/LICENSE).

## Why the boundary holds

Apache-2.0 and GPL-2 are famously incompatible. The Free Software Foundation's
position is that Apache-2.0's patent-termination and indemnification clauses
impose requirements GPL-2 does not permit adding, so Apache-2.0 code cannot be
combined into a GPL-2 work. (GPL-3 resolved this; GPL-2 did not, and id's grant
is version 2 with no "or later" clause, so upgrading is not available.)

That incompatibility is only triggered by *combination* — linking the two bodies
of code into a single work. This repository never does that, for three
structural reasons:

1. **`ports/doom` is not a Cargo workspace member.** Per `CLAUDE.md`, workspace
   members are `crates/*` only; `ports/` holds `bet` source, not crates. No
   Rust target in this workspace compiles, links, or embeds anything in
   `ports/doom`. `cargo build` does not read it.

2. **The relationship is compiler-to-input, not library-to-caller.** The
   Apache-2.0 toolchain consumes `ports/doom/*.bet` the way GCC consumes a `.c`
   file. Running a program over data does not combine the two into one work, and
   compiling a GPL program with a non-GPL compiler has never implicated the
   compiler's license. The port depends on `bet` the tool; it does not link
   `bet` the library.

3. **Nothing flows the other way.** No crate under `crates/` reads from,
   vendors, or derives from `ports/doom`. The GPL code is a leaf. Deleting the
   directory would leave the toolchain and its test suite intact.

The one place the boundary would need care is the **compiled artifact**: a built
DOOM binary links the `bet` runtime (`crates/runtime`) into a GPL-2 work. Apache-2.0
is a permissive license, so the runtime's terms can be satisfied inside a GPL-2
distribution — but GPL-2's own view is that it cannot accept Apache-2.0's added
terms, so a distributed binary is the case that must be reasoned about rather than
assumed. If binary distribution of the port is ever contemplated, the practical
resolutions are to dual-license `crates/runtime` (adding MIT, which is
GPL-compatible), or to rely on GPL-2 §3's system-library-style carve-out. **No such
binary is distributed from this repository today.** The port is built locally by
developers who already have the source.

## Game data

No copyrighted WAD is committed here, and the GPL covers none of it. id's GPL-2
release covers the DOOM *source code* only; the game assets remain proprietary
to id/ZeniMax and are not redistributable.

`.gitignore` enforces this: `*.wad` is ignored repo-wide, alongside
`/doom-reference/` (the upstream C source and its shareware IWAD) and
`/doom-oracle/`. `ports/frozen-bubble/assets.dat` is likewise untracked via a
nested `.gitignore` — that port ships the asset baker, never the baked data.
Test the DOOM port against the shareware `doom1.wad`, a commercial IWAD you own,
or Freedoom.

## Contributing

Contributions outside `ports/doom` are under Apache-2.0, per its §5: anything you
intentionally submit for inclusion is under that license, absent a separate
agreement. Contributions to `ports/doom` are under GPL-2.

If you add a third license to this repository, add it as another directory with
its own license file and add a row to the table above. Do not introduce a second
license *within* an existing scope — the boundary above is only legible because
it is a directory, not a file-by-file judgment call.
