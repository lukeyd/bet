# Oregon Trail — a `bet` port

A faithful port of the original **MECC Oregon Trail** (Bill Heinemann, 1971; the 01/01/78
CDC CYBER BASIC listing) to `bet`. It runs on the interpreter and compiles to native machine
code, playing byte-for-byte identically on both.

```sh
# interpreter
cargo build -p driver
target/debug/bet run ports/oregon-trail/oregon.bet

# native machine code (needs LLVM 18; see the repo README / selfhost/README.md)
cargo build -p driver -p rt-stub --features llvm
target/debug/bet build ports/oregon-trail/oregon.bet -o oregon
./oregon
```

You steer a wagon 2040 miles from Independence, Missouri to Oregon City in 1847: buy oxen,
food, ammunition, clothing, and supplies with $700; then each two-week turn you stop at forts,
hunt, eat, fight off riders, and survive random misfortune, broken axles, illness, snakebite,
river crossings, mountain blizzards, until you arrive, or don't.

Input is line-based (menu numbers, dollar amounts). Running non-interactively is fine: when
input runs out (EOF) every menu falls back to a safe default, so a piped run always terminates.

## The port

The original is ~790 lines of `GOTO`/`GOSUB` BASIC over a bank of global variables. This port
keeps the mechanics, text, event table, and endings, but restructures the spaghetti into
functions over a single **value-threaded `Trip` struct** (`bet` structs are value-copy, so each
phase takes the state and returns it: `s = phase(s, g)`; a `done` field signals death/arrival).

Two adaptations are forced by the platform, and are the reason this port exists — it drove two
new language features into the `bet` compiler:

- **Randomness → a seeded PRNG.** BASIC's `RND(-1)` becomes `math.cook(seed)` +
  `g.upTo(n)` / `g.frac()`. `math.cook` is a new stdlib intrinsic: a seedable, non-cryptographic
  generator (xoshiro256\*\* seeded by SplitMix64) whose algorithm is shared between the
  interpreter and the runtime, so a given seed replays the exact same trip on both. The seed is
  fixed (`1847`) for reproducibility. The original's floating-point event formulas are rendered
  in integer arithmetic.
- **Keyboard input → `sys.peep()`.** BASIC's `INPUT` becomes `sys.peep()`, another new
  intrinsic: read one line from stdin (empty at EOF). Numbers are parsed by a small in-language
  `parseInt`.
- **The timed-typing shoot mechanic** (`CLK`, "type BANG faster than the varmint") can't time
  real keystrokes portably, even the 1978 listing notes this is system-specific. Shot quality is
  instead drawn from your claimed marksman skill plus luck, feeding the same
  nice-shot / slow / knifed buckets.

## Fidelity notes

Preserved: the $700 outfitting with re-buy on overspend; two-week date progression (April 12 …);
the fort / hunt / continue action menu (fort offered every other turn); fort shopping; hunting;
the three eating levels; rider attacks with all four tactics and the hostility flip; the full
15-entry random-event table; the South Pass / Blue Mountains passes; blizzards and the illness
sub-routine; and every ending, starvation, illness, injury, snakebite, massacre, and the
President Polk victory epilogue.

Not identical to the original's exact numbers: the float formulas are integer approximations and
randomness comes from a different generator, so a run won't match a 1978 CYBER run value-for-value.
The gameplay, difficulty, and text are faithful.
