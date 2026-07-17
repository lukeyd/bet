//! `gg-backend` — bet's native `gg` platform backend (the SDL-equivalent layer).
//!
//! One small API — [`present`], [`audio`], [`poll`], [`ticks`] — shared by BOTH the real
//! `runtime` (via the frozen `rt-abi` `bet_gg_*` entry points) and the tree-walking
//! `interp` (via its `gg.*` intrinsics). Keeping the platform code in one crate means the
//! compiled path and `bet run` present pixels, drain audio, and report input through the
//! exact same window/stream/queue.
//!
//! ## Two builds from one source
//!
//! * **`desktop` feature ON** — a real window (`minifb`), a real audio output stream
//!   (`cpal`) draining a wait-free SPSC sample ring, and a synthesized input-event queue. All
//!   of it hangs off a single process-global singleton created lazily on first use.
//!
//! ## Threads
//!
//! There are exactly TWO: the game thread, which owns the singleton and every allocation, and
//! the `cpal` audio callback, which is REAL-TIME and shares no state with it — only the two
//! wait-free SPSC rings reach across. Nothing in the callback locks or allocates; see [`mixer`].
//! * **`desktop` feature OFF (default)** — headless no-ops with ZERO heavy dependencies:
//!   [`present`]/[`audio`] do nothing, [`poll`] always reports [`event_kind::NONE`], and
//!   [`ticks`] is a monotonic `Instant` (a plain monotonic counter on `wasm32`, which has no
//!   std clock). This is what the default `cargo build` compiles, so neither `minifb` nor `cpal`
//!   is ever pulled in unless a caller explicitly opts in.
//!
//! ## Keycode convention
//!
//! [`Event::code`] values are defined by [`mod@key`] and are STABLE — the ports depend on the
//! exact numbers (letters are ASCII-uppercase, arrows are 256..=259, etc.). The `desktop`
//! backend maps `minifb::Key` onto them.
//!
//! ## `BET_GG_HEADLESS`
//!
//! Setting `BET_GG_HEADLESS=1` in the environment makes a `desktop`-featured build run fully
//! headless — no window, no audio device — behaving exactly like the default headless build
//! ([`poll`] ⇒ NONE, presents/audio discarded, [`ticks`] stays real, [`audio_spec`] reports the
//! fixed `(48000, 2)` default, [`pending`] reports `0`). The switch is read once at gg init
//! (the first gg call) and lets CI run a compiled gg game to completion.

// The desktop singleton stores a `minifb::Window` and a `cpal::Stream`, neither of which is
// `Send`. We keep them in a process-global `Mutex` and force `Send` on the wrapper: every `gg`
// entry point runs on the one game-loop thread, so the singleton is only ever touched from
// there. (The audio callback is a second thread, but it shares nothing with the singleton — see
// the `unsafe impl Send` for the real argument.) This is the one place the crate needs `unsafe`.
#![allow(unsafe_code)]

use rt_abi::{Event, event_kind};

/// Stable [`Event::code`] keycodes. The ports reference these by value, so the numbers are
/// part of the contract, not an implementation detail. Printable keys map to their (unshifted,
/// US-layout) ASCII code — letters ASCII uppercase, digits `'0'..='9'`, punctuation its symbol —
/// and non-printable keys to a private block above ASCII: arrows 256..=259, modifiers/Pause
/// 260..=266, F1..=F12 at 280..=291.
pub mod key {
    // Letters A..=Z → ASCII uppercase (so `W == 87`, `S == 83`, ...).
    pub const A: u32 = b'A' as u32;
    pub const B: u32 = b'B' as u32;
    pub const C: u32 = b'C' as u32;
    pub const D: u32 = b'D' as u32;
    pub const E: u32 = b'E' as u32;
    pub const F: u32 = b'F' as u32;
    pub const G: u32 = b'G' as u32;
    pub const H: u32 = b'H' as u32;
    pub const I: u32 = b'I' as u32;
    pub const J: u32 = b'J' as u32;
    pub const K: u32 = b'K' as u32;
    pub const L: u32 = b'L' as u32;
    pub const M: u32 = b'M' as u32;
    pub const N: u32 = b'N' as u32;
    pub const O: u32 = b'O' as u32;
    pub const P: u32 = b'P' as u32;
    pub const Q: u32 = b'Q' as u32;
    pub const R: u32 = b'R' as u32;
    pub const S: u32 = b'S' as u32;
    pub const T: u32 = b'T' as u32;
    pub const U: u32 = b'U' as u32;
    pub const V: u32 = b'V' as u32;
    pub const W: u32 = b'W' as u32;
    pub const X: u32 = b'X' as u32;
    pub const Y: u32 = b'Y' as u32;
    pub const Z: u32 = b'Z' as u32;

    // Digits '0'..='9' → ASCII.
    pub const D0: u32 = b'0' as u32;
    pub const D1: u32 = b'1' as u32;
    pub const D2: u32 = b'2' as u32;
    pub const D3: u32 = b'3' as u32;
    pub const D4: u32 = b'4' as u32;
    pub const D5: u32 = b'5' as u32;
    pub const D6: u32 = b'6' as u32;
    pub const D7: u32 = b'7' as u32;
    pub const D8: u32 = b'8' as u32;
    pub const D9: u32 = b'9' as u32;

    /// Space bar (ASCII 32).
    pub const SPACE: u32 = b' ' as u32;
    /// Enter / Return. `10` (`\n`), matching the ASCII line feed.
    pub const ENTER: u32 = 10;
    /// Escape (ASCII 27). Delivered as a normal KEY_DOWN/KEY_UP like every other key — games
    /// with menus need to see Esc presses. QUIT (kind 4) is reserved for actual window close.
    pub const ESC: u32 = 27;
    /// Backspace (ASCII 8).
    pub const BACKSPACE: u32 = 8;
    /// Tab (ASCII 9, `\t`).
    pub const TAB: u32 = 9;

    // Punctuation → the key's unshifted ASCII code (US layout), like letters/digits.
    /// `'` (ASCII 39).
    pub const APOSTROPHE: u32 = b'\'' as u32;
    /// `,` (ASCII 44).
    pub const COMMA: u32 = b',' as u32;
    /// `-` (ASCII 45).
    pub const MINUS: u32 = b'-' as u32;
    /// `.` (ASCII 46).
    pub const PERIOD: u32 = b'.' as u32;
    /// `/` (ASCII 47).
    pub const SLASH: u32 = b'/' as u32;
    /// `;` (ASCII 59).
    pub const SEMICOLON: u32 = b';' as u32;
    /// `=` (ASCII 61).
    pub const EQUALS: u32 = b'=' as u32;
    /// `[` (ASCII 91).
    pub const LBRACKET: u32 = b'[' as u32;
    /// `\` (ASCII 92).
    pub const BACKSLASH: u32 = b'\\' as u32;
    /// `]` (ASCII 93).
    pub const RBRACKET: u32 = b']' as u32;
    /// `` ` `` (ASCII 96).
    pub const BACKTICK: u32 = b'`' as u32;

    /// Arrow keys — a private block just above ASCII so they never collide with letters/digits.
    pub const UP: u32 = 256;
    pub const DOWN: u32 = 257;
    pub const LEFT: u32 = 258;
    pub const RIGHT: u32 = 259;

    // Modifiers + Pause — the non-printable block continues straight after the arrows.
    pub const LCTRL: u32 = 260;
    pub const RCTRL: u32 = 261;
    pub const LSHIFT: u32 = 262;
    pub const RSHIFT: u32 = 263;
    pub const LALT: u32 = 264;
    pub const RALT: u32 = 265;
    pub const PAUSE: u32 = 266;

    // Function keys — F1..=F12 at 280..=291 (267..=279 stay reserved for future non-printables).
    pub const F1: u32 = 280;
    pub const F2: u32 = 281;
    pub const F3: u32 = 282;
    pub const F4: u32 = 283;
    pub const F5: u32 = 284;
    pub const F6: u32 = 285;
    pub const F7: u32 = 286;
    pub const F8: u32 = 287;
    pub const F9: u32 = 288;
    pub const F10: u32 = 289;
    pub const F11: u32 = 290;
    pub const F12: u32 = 291;
}

/// The empty-queue sentinel returned by [`poll`].
const NONE_EVENT: Event = Event {
    kind: event_kind::NONE,
    code: 0,
    x: 0,
    y: 0,
};

/// The window size `size()` reports before any frame is presented (no window yet), and the size
/// a headless build always reports. Kept in sync with `rt-stub`'s `bet_gg_size` default so the
/// two headless paths stay byte-identical.
pub const DEFAULT_W: u32 = 960;
pub const DEFAULT_H: u32 = 640;

/// The audio output config `audio_spec()` reports when there is no real device — a headless
/// build, `BET_GG_HEADLESS=1`, or a desktop build that failed to open an output stream. Kept in
/// sync with `rt-stub`'s `bet_gg_audio_spec` so the headless paths stay byte-identical.
pub const DEFAULT_AUDIO_RATE: u32 = 48_000;
pub const DEFAULT_AUDIO_CHANNELS: u32 = 2;

/// Pack a `(width, height)` into the `u64` the `bet_gg_size` ABI returns (`w << 32 | h`).
const fn pack_size(w: u32, h: u32) -> u64 {
    ((w as u64) << 32) | h as u64
}

// ===========================================================================
// Public API — the calls the runtime and interp share.
// ===========================================================================

/// Present a framebuffer. `pixels` points to at least `stride * height` `u32`s (row-major,
/// `0x00RR_GGBB`); each of the `height` rows is `stride` pixels wide but only the first
/// `width` are shown. With `desktop`, copies the pixels into the window and pumps input; the
/// headless build ignores everything.
pub fn present(pixels: *const u32, width: u32, height: u32, stride: u32) {
    imp::present(pixels, width, height, stride);
}

/// Submit interleaved audio samples, pushed onto the output ring for the `cpal` callback to
/// drain. Headless: a no-op.
pub fn audio(samples: &[i16]) {
    imp::audio(samples);
}

/// Pop the next queued input event, or an [`event_kind::NONE`] [`Event`] when the queue is
/// empty. Headless: always `NONE`.
pub fn poll() -> Event {
    imp::poll()
}

/// A monotonic high-resolution timer, in nanoseconds.
///
/// Native (desktop + headless): real wall-clock via `std::time::Instant`. On
/// `wasm32-unknown-unknown` there is no std clock source — `Instant::now` panics — and the browser
/// playground is text-output only (`interp::run_to_string` runs to completion; no frame loop), so
/// the wasm build returns a strictly-increasing, non-panicking counter instead. No new dependency.
#[cfg(not(target_arch = "wasm32"))]
pub fn ticks() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// See [`ticks`]. wasm has no std clock; return a monotonic counter so timing code that computes
/// deltas gets a sane, ever-increasing value instead of a panic (~1ms per call; `fetch_add` returns
/// the previous value, so successive reads strictly increase).
#[cfg(target_arch = "wasm32")]
pub fn ticks() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NS: AtomicU64 = AtomicU64::new(0);
    NS.fetch_add(1_000_000, Ordering::Relaxed)
}

/// The current window `(width, height)` packed as `w << 32 | h` — the size the framebuffer
/// should match for 1:1 dynamic resolution. Tracks live resizes; returns the default until the
/// first frame opens the window, and always the default in a headless build.
pub fn size() -> u64 {
    imp::size()
}

// ---------------------------------------------------------------------------
// gg compositor / mixer / mouse (amendment §SP0.4 raise). All share the singleton the four
// original primitives use; the audio mixer is owned outright by the cpal callback and reached
// only through a wait-free command ring, so the real-time thread takes no lock at all.
// ---------------------------------------------------------------------------

/// Upload an RGBA8 texture (`w * h` pixels, 4 bytes `R,G,B,A` each) and return its 1-based id
/// (`0` = invalid). Headless: `0`.
pub fn tex(rgba: &[u8], w: u32, h: u32) -> u32 {
    imp::tex(rgba, w, h)
}

/// Begin a frame: (re)size the logical canvas to `w * h` and clear it to `clear_argb`
/// (`0x00RR_GGBB`). Headless: a no-op.
pub fn frame(w: u32, h: u32, clear_argb: u32) {
    imp::frame(w, h, clear_argb);
}

/// Blit texture `tex` onto the canvas at `(dx, dy)` with premultiplied src-over alpha, clipped.
/// Headless: a no-op.
pub fn sprite(tex: u32, dx: i32, dy: i32) {
    imp::sprite(tex, dx, dy);
}

/// Blit the source sub-rectangle `(sx, sy, sw, sh)` of texture `tex` to `(dx, dy)` with
/// premultiplied src-over alpha — the same blit math as [`sprite`], windowed to the source rect
/// and clipped to both the texture and the canvas. Headless: a no-op.
#[allow(clippy::too_many_arguments)]
pub fn sprite_sub(tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
    imp::sprite_sub(tex, sx, sy, sw, sh, dx, dy);
}

/// Fill a `w * h` rectangle at `(dx, dy)` with `argb` (`0xAARR_GGBB`), src-over, clipped.
/// Headless: a no-op.
pub fn rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
    imp::rect(dx, dy, w, h, argb);
}

/// Present the composited canvas (aspect-fit letterbox) and pump input. Headless: a no-op.
pub fn flush() {
    imp::flush();
}

/// Register a PCM sound (`channels` interleaved little-endian `i16` channels at `rate` Hz) with
/// the mixer and return its 1-based id (`0` = invalid). Headless: `0`.
pub fn sound(pcm: &[u8], channels: u32, rate: u32) -> u32 {
    imp::sound(pcm, channels, rate)
}

/// Start playing `sound` on a voice at `vol_q8` (Q8; `256` = unity), looping if `loop_ != 0`, and
/// return the 1-based voice id (`0` = could not play). Headless: `0`.
pub fn play(sound: u32, loop_: u32, vol_q8: u32) -> u32 {
    imp::play(sound, loop_, vol_q8)
}

/// Stop voice `voice` (from [`play`]). Headless: a no-op.
pub fn stop(voice: u32) {
    imp::stop(voice);
}

/// The mouse position in logical-canvas coordinates, packed `x << 32 | y`. Headless: `0`.
pub fn mouse() -> u64 {
    imp::mouse()
}

// ---------------------------------------------------------------------------
// gg relative mouse / voice retune / fixed-canvas present / streaming audio (the DOOM-motivated
// platform raise). Additive: nothing above changes shape.
// ---------------------------------------------------------------------------

/// Drain the raw mouse movement accumulated since the previous call, as a sign-preserving
/// `(dx, dy)` pair of `i32`s packed `(dx as u32) << 32 | (dy as u32)`. Deltas are in window
/// pixels, accumulated at every input pump (present/flush/show); the fractional sub-pixel
/// remainder carries over to the next call. Headless: always `0`.
pub fn mouse_delta() -> u64 {
    imp::mouse_delta()
}

/// Update a PLAYING voice's volume (`vol_q8`, Q8: `256` = unity) and stereo pan (`pan_q8`:
/// `0` = full left, `128` = center, `255` = full right; see the mixer's linear pan law).
/// Unknown or finished voice ids are ignored. Headless: a no-op.
pub fn tune(voice: u32, vol_q8: u32, pan_q8: u32) {
    imp::tune(voice, vol_q8, pan_q8);
}

/// Present a tightly packed `w * h` framebuffer (`0x00RR_GGBB`, stride == `w`) with the
/// compositor's presentation — integer nearest-neighbor aspect-fit upscale, centered with black
/// letterbox bars — into the live window, then pump input. [`present`]'s input model with
/// [`flush`]'s scaling. Headless: a no-op.
pub fn show(pixels: *const u32, width: u32, height: u32) {
    imp::show(pixels, width, height);
}

/// The audio device's output config, packed `rate << 32 | channels`. Opens the output stream on
/// first use (like [`play`]); headless — or no usable device — reports the fixed
/// ([`DEFAULT_AUDIO_RATE`], [`DEFAULT_AUDIO_CHANNELS`]) default.
pub fn audio_spec() -> u64 {
    imp::audio_spec()
}

/// The number of interleaved `i16` samples currently queued in the raw [`audio`] ring
/// (submitted but not yet consumed by the device callback) — streaming backpressure.
/// Headless: always `0` (instant drain).
pub fn pending() -> u64 {
    imp::pending()
}

/// Set the window title, applied to the live window immediately and remembered for a later window
/// (re)creation. An empty title is ignored. Headless: a no-op.
pub fn title(name: &str) {
    imp::title(name);
}

// ===========================================================================
// The mixer core — compiled in BOTH builds, so the default `cargo nextest run --workspace`
// gate exercises it. Nothing here knows about `cpal`: the real-time side is expressed as the
// two `PcmSource`/`CmdSource` traits, which the `desktop` build satisfies with lock-free
// `rtrb` SPSC consumers and the tests satisfy with plain slices/`VecDeque`s.
// ===========================================================================

/// A counting global allocator, installed ONLY in this crate's own test binary.
///
/// The claims this crate makes — "the audio callback never allocates", "the present path is
/// zero-allocation per frame" — are exactly the kind that rot into a false line in a Markdown
/// table. So they are measured, against the real allocator, rather than asserted.
///
/// Both `alloc` AND `dealloc` are counted: a `free()` on the real-time thread misses the
/// deadline just as surely as a `malloc`, and it is the subtler hazard (an `Arc` whose refcount
/// happens to hit zero inside the callback). See [`mixer::Mixer::apply`].
#[cfg(test)]
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        /// Allocator operations performed by THIS thread. `const`-initialized and `Drop`-free,
        /// so touching it from inside the allocator cannot recurse through TLS destructors.
        static OPS: Cell<usize> = const { Cell::new(0) };
    }

    fn bump() {
        // `try_with`: during thread teardown the TLS may be gone — never panic in the allocator.
        let _ = OPS.try_with(|c| c.set(c.get() + 1));
    }

    pub(crate) struct Counting;

    // SAFETY: every method forwards verbatim to `System`, which is a correct `GlobalAlloc`; the
    // only addition is a thread-local counter bump that allocates nothing itself.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            bump();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            bump();
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    /// Run `f` and report how many allocator operations it performed on this thread. Per-thread
    /// (not global) so it stays exact whether the harness runs tests threaded or process-per-test.
    pub(crate) fn count_allocator_ops(f: impl FnOnce()) -> usize {
        let before = OPS.with(Cell::get);
        f();
        OPS.with(Cell::get) - before
    }
}

/// The lock-free voice mixer shared by the `desktop` audio callback and the unit tests.
///
/// ## Why this shape (cwage #96)
///
/// The audio callback is a REAL-TIME thread: it must never block and never allocate. The
/// previous design had it take two `Mutex`es that the game thread also held while doing a
/// `Vec<i16>` resample — textbook priority inversion, and an audible click whenever a sound
/// registered mid-frame. So:
///
/// * the game thread OWNS the sound registry ([`SoundRegistry`]) and does every allocation
///   (registration, resampling) itself, before handing anything over;
/// * the callback OWNS the [`Mixer`] outright — no sharing, therefore no lock;
/// * the only channel between them is a wait-free SPSC ring of [`MixCmd`]s;
/// * the voice pool is PRE-ALLOCATED ([`MAX_VOICES`] slots), so starting a voice writes into
///   an existing slot rather than growing a `Vec`.
mod mixer {
    use std::sync::Arc;

    /// The pre-allocated voice pool size. Fixed so `play` never grows a `Vec` (the old
    /// `voices: Vec<Voice>` also grew without bound — every `play` leaked a slot forever).
    /// Beyond this, the oldest slot is stolen, which is what every real game mixer does.
    pub(crate) const MAX_VOICES: usize = 64;

    /// Voice ids pack `generation` and `slot` so a `stop`/`tune` racing a stolen slot is a
    /// no-op instead of retuning whatever voice recycled it. 25 generation bits keeps
    /// `generation * MAX_VOICES + slot + 1` inside `u32` (max id `0x8000_0000`).
    const VOICE_GEN_MASK: u32 = 0x01FF_FFFF;

    /// Pack a `(slot, generation)` into the public 1-based voice id (`0` = invalid).
    pub(crate) fn voice_id(slot: usize, generation: u32) -> u32 {
        ((generation & VOICE_GEN_MASK) * MAX_VOICES as u32) + slot as u32 + 1
    }

    /// Unpack a public voice id back into `(slot, generation)`; `0` (and only `0`) is invalid.
    pub(crate) fn decode_voice(id: u32) -> Option<(usize, u32)> {
        let i = id.checked_sub(1)?;
        Some((i as usize % MAX_VOICES, i / MAX_VOICES as u32))
    }

    /// A message from the game thread to the audio callback. Every variant is plain data or an
    /// already-built `Arc` — applying one never allocates and never blocks.
    pub(crate) enum MixCmd {
        /// Start (or steal) `slot` with an ALREADY-RESAMPLED buffer. The game thread resampled
        /// it; the callback only reads it.
        Play {
            slot: usize,
            generation: u32,
            pcm: Arc<Vec<i16>>,
            vol_q8: u32,
            looping: bool,
        },
        Stop {
            slot: usize,
            generation: u32,
        },
        Tune {
            slot: usize,
            generation: u32,
            vol_q8: u32,
            pan_q8: u32,
        },
    }

    /// A pre-allocated voice slot. `pcm` is a REFERENCE to registry-owned audio, never an owned
    /// buffer — see [`Mixer::apply`] for why that matters in a real-time callback.
    pub(crate) struct Voice {
        pcm: Option<Arc<Vec<i16>>>,
        /// The generation this slot was last started at; a command for any other generation is
        /// stale and ignored.
        generation: u32,
        pos: usize,
        vol_q8: u32,
        /// Stereo pan: `0` = full left, `128` = center, `255` = full right (see [`Mixer::mix_sample`]).
        pan_q8: u32,
        looping: bool,
        active: bool,
    }

    impl Voice {
        const fn idle() -> Voice {
            Voice {
                pcm: None,
                generation: 0,
                pos: 0,
                vol_q8: 0,
                pan_q8: 128,
                looping: false,
                active: false,
            }
        }
    }

    /// The interleaved raw-[`crate::audio`] sample stream the callback drains 1:1. Abstracted so
    /// the mixer core compiles (and is tested) without `rtrb`.
    pub(crate) trait PcmSource {
        /// The next queued sample, or `0` when the stream has run dry (silence, not a stall).
        fn next_sample(&mut self) -> i16;
    }

    /// The [`MixCmd`] stream the callback drains at the top of each block.
    pub(crate) trait CmdSource {
        fn next_cmd(&mut self) -> Option<MixCmd>;
    }

    /// A `PcmSource` over a plain slice — the test stand-in for the `rtrb` consumer.
    #[cfg(test)]
    pub(crate) struct SlicePcm<'a>(pub &'a [i16], pub usize);
    #[cfg(test)]
    impl PcmSource for SlicePcm<'_> {
        fn next_sample(&mut self) -> i16 {
            let s = self.0.get(self.1).copied().unwrap_or(0);
            self.1 += 1;
            s
        }
    }

    /// Silence — the `PcmSource` for tests that only care about voices.
    #[cfg(test)]
    pub(crate) struct Silence;
    #[cfg(test)]
    impl PcmSource for Silence {
        fn next_sample(&mut self) -> i16 {
            0
        }
    }

    /// A `CmdSource` over a `Vec`, used by tests; `None` once drained.
    #[cfg(test)]
    pub(crate) struct VecCmds(pub std::collections::VecDeque<MixCmd>);
    #[cfg(test)]
    impl CmdSource for VecCmds {
        fn next_cmd(&mut self) -> Option<MixCmd> {
            self.0.pop_front()
        }
    }

    /// No commands at all.
    #[cfg(test)]
    pub(crate) struct NoCmds;
    #[cfg(test)]
    impl CmdSource for NoCmds {
        fn next_cmd(&mut self) -> Option<MixCmd> {
            None
        }
    }

    /// The voice mixer, owned OUTRIGHT by the audio callback (never shared, never locked).
    pub(crate) struct Mixer {
        voices: [Voice; MAX_VOICES],
        device_channels: u16,
    }

    impl Mixer {
        pub(crate) fn new(device_channels: u16) -> Mixer {
            const IDLE: Voice = Voice::idle();
            Mixer {
                voices: [IDLE; MAX_VOICES],
                device_channels,
            }
        }

        /// Apply one command. Allocation-free by construction:
        ///
        /// * `Play` stores an `Arc` CLONE; the drop of the slot's previous `Arc` can only
        ///   decrement a refcount, never free, because [`SoundRegistry`] holds a strong
        ///   reference to every resampled buffer for the life of the process. That invariant is
        ///   what keeps `free()` out of the real-time thread — do not "tidy" the registry into
        ///   dropping entries.
        /// * `Stop`/`Tune` only touch scalars.
        ///
        /// Stale commands (a `stop`/`tune` for a slot that has since been stolen) are ignored.
        pub(crate) fn apply(&mut self, cmd: MixCmd) {
            match cmd {
                MixCmd::Play {
                    slot,
                    generation,
                    pcm,
                    vol_q8,
                    looping,
                } => {
                    let Some(v) = self.voices.get_mut(slot) else {
                        return;
                    };
                    // A `Play` always wins its slot — that IS the steal.
                    v.pcm = Some(pcm);
                    v.generation = generation;
                    v.pos = 0;
                    v.vol_q8 = vol_q8;
                    v.pan_q8 = 128; // center — `tune` moves it
                    v.looping = looping;
                    v.active = true;
                }
                MixCmd::Stop { slot, generation } => {
                    if let Some(v) = self.voices.get_mut(slot)
                        && v.generation == generation
                    {
                        v.active = false;
                    }
                }
                MixCmd::Tune {
                    slot,
                    generation,
                    vol_q8,
                    pan_q8,
                } => {
                    if let Some(v) = self.voices.get_mut(slot)
                        && v.generation == generation
                    {
                        v.vol_q8 = vol_q8;
                        v.pan_q8 = pan_q8.min(255);
                    }
                }
            }
        }

        /// Drain every pending command. Bounded by the ring's capacity, and each `apply` is O(1)
        /// — so the callback's worst case stays a fixed, small amount of work.
        pub(crate) fn drain(&mut self, cmds: &mut impl CmdSource) {
            while let Some(cmd) = cmds.next_cmd() {
                self.apply(cmd);
            }
        }

        /// Produce one output sample for channel `chan` (the slot's index within its interleaved
        /// frame): the raw ring (kept 1:1 and unpanned for `gg.audio` / Pong) plus every active
        /// voice, summed and clipped to `i16`. Advances each voice's `pos`; a one-shot voice
        /// deactivates at its end, a looping voice wraps.
        ///
        /// Per-voice stereo pan (linear law): with `vol = vol_q8` and `pan = pan_q8` (0 = full
        /// left, 128 = center, 255 = full right),
        ///   * even channels (left)  get gain `vol * (255 - pan) / 255`,
        ///   * odd  channels (right) get gain `vol * pan / 255`,
        ///   * a mono device applies the plain unpanned `vol`.
        ///
        /// Center is therefore ~half gain per side (127/255 and 128/255), the standard linear
        /// halving that keeps a hard-panned voice at unity on its side.
        pub(crate) fn mix_sample(&mut self, pcm: &mut impl PcmSource, chan: usize) -> i16 {
            let mut acc = i32::from(pcm.next_sample());
            let mono = self.device_channels <= 1;
            for v in self.voices.iter_mut() {
                if !v.active {
                    continue;
                }
                let res = match v.pcm.as_ref() {
                    Some(r) if !r.is_empty() => r,
                    _ => {
                        v.active = false;
                        continue;
                    }
                };
                let vol = v.vol_q8 as i32;
                let gain = if mono {
                    vol
                } else if chan.is_multiple_of(2) {
                    vol * (255 - v.pan_q8 as i32) / 255
                } else {
                    vol * v.pan_q8 as i32 / 255
                };
                acc += (i32::from(res[v.pos]) * gain) >> 8;
                v.pos += 1;
                if v.pos >= res.len() {
                    if v.looping {
                        v.pos = 0;
                    } else {
                        v.active = false;
                    }
                }
            }
            acc.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        }

        /// The whole real-time callback body, in one allocation-free, lock-free call: drain the
        /// command ring, then mix `data.len()` sample slots. Shared by the I16 and F32 stream
        /// formats (and driven directly by the tests).
        pub(crate) fn fill(
            &mut self,
            data: &mut [i16],
            pcm: &mut impl PcmSource,
            cmds: &mut impl CmdSource,
            channels: usize,
        ) {
            self.drain(cmds);
            for (i, s) in data.iter_mut().enumerate() {
                *s = self.mix_sample(pcm, i % channels);
            }
        }

        /// Whether `slot` is currently sounding — test/instrumentation only.
        #[cfg(test)]
        pub(crate) fn is_active(&self, slot: usize) -> bool {
            self.voices[slot].active
        }
    }

    /// A registered PCM sound, owned by the GAME thread. `pcm` is the raw interleaved `i16` at
    /// its native `channels`/`rate`; `resampled` is a lazily-built copy interleaved at the DEVICE
    /// channels/rate, so the mixer advances a voice's `pos` by one per output sample slot
    /// (matching the raw ring's 1:1 drain).
    struct Sound {
        pcm: Vec<i16>,
        channels: u32,
        rate: u32,
        resampled: Option<Arc<Vec<i16>>>,
    }

    /// The game-thread-owned sound registry and voice-slot allocator. Every allocation in the
    /// audio path happens HERE, on the game thread — the callback only ever receives finished
    /// `Arc`s. Entries are never removed, which is precisely what guarantees the callback's
    /// `Arc` drops can't free (see [`Mixer::apply`]).
    pub(crate) struct SoundRegistry {
        sounds: Vec<Sound>,
        /// Per-slot generation counter, bumped on every (re)use so stale ids can be rejected.
        generations: [u32; MAX_VOICES],
        /// Round-robin cursor: voices are stolen oldest-first once the pool wraps.
        next_slot: usize,
    }

    impl SoundRegistry {
        pub(crate) fn new() -> SoundRegistry {
            SoundRegistry {
                sounds: Vec::new(),
                generations: [0; MAX_VOICES],
                next_slot: 0,
            }
        }

        /// Register interleaved little-endian `i16` PCM; returns its 1-based id.
        pub(crate) fn register(&mut self, pcm: &[u8], channels: u32, rate: u32) -> u32 {
            let samples: Vec<i16> = pcm
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            self.sounds.push(Sound {
                pcm: samples,
                channels: channels.max(1),
                rate: if rate == 0 { 44_100 } else { rate },
                resampled: None,
            });
            self.sounds.len() as u32
        }

        /// The device-format buffer for `sound` (1-based), resampling it ON THE GAME THREAD the
        /// first time. `None` for an unknown id.
        pub(crate) fn prepare(
            &mut self,
            sound: u32,
            dev_ch: u32,
            dev_rate: u32,
        ) -> Option<Arc<Vec<i16>>> {
            let idx = (sound.checked_sub(1)?) as usize;
            let s = self.sounds.get_mut(idx)?;
            if s.resampled.is_none() {
                let dst_rate = if dev_rate == 0 { s.rate } else { dev_rate };
                let res = resample(&s.pcm, s.channels, s.rate, dev_ch, dst_rate);
                s.resampled = Some(Arc::new(res));
            }
            s.resampled.clone()
        }

        /// Claim the next voice slot round-robin, bumping its generation. Returns
        /// `(slot, generation)`; the caller turns that into a public id with [`voice_id`].
        pub(crate) fn alloc_voice(&mut self) -> (usize, u32) {
            let slot = self.next_slot;
            self.next_slot = (self.next_slot + 1) % MAX_VOICES;
            self.generations[slot] = self.generations[slot].wrapping_add(1) & VOICE_GEN_MASK;
            (slot, self.generations[slot])
        }

        #[cfg(test)]
        pub(crate) fn len(&self) -> usize {
            self.sounds.len()
        }
    }

    /// Resample `pcm` (interleaved `src_ch` channels at `src_rate` Hz) to interleaved `dst_ch`
    /// channels at `dst_rate` Hz, nearest-neighbor. The result is interleaved at the DEVICE channel
    /// count so the mixer advances a voice's `pos` by one per output sample slot. Channel mapping:
    /// mono -> duplicate, multi -> mono average, else copy shared channels.
    ///
    /// This allocates — which is exactly why it runs on the game thread, in `play`, once per
    /// sound, and never in the callback.
    pub(crate) fn resample(
        pcm: &[i16],
        src_ch: u32,
        src_rate: u32,
        dst_ch: u32,
        dst_rate: u32,
    ) -> Vec<i16> {
        let src_ch = src_ch.max(1) as usize;
        let dst_ch = dst_ch.max(1) as usize;
        let src_rate = src_rate.max(1) as u64;
        let dst_rate = dst_rate.max(1) as u64;
        let src_frames = pcm.len() / src_ch;
        if src_frames == 0 {
            return Vec::new();
        }
        let dst_frames = (src_frames as u64 * dst_rate / src_rate) as usize;
        let mut out = Vec::with_capacity(dst_frames * dst_ch);
        for f in 0..dst_frames {
            let sf = ((f as u64 * src_rate / dst_rate) as usize).min(src_frames - 1);
            let base = sf * src_ch;
            if dst_ch == src_ch {
                out.extend_from_slice(&pcm[base..base + src_ch]);
            } else if src_ch == 1 {
                let s = pcm[base];
                for _ in 0..dst_ch {
                    out.push(s);
                }
            } else if dst_ch == 1 {
                let mut acc = 0i32;
                for c in 0..src_ch {
                    acc += i32::from(pcm[base + c]);
                }
                out.push((acc / src_ch as i32) as i16);
            } else {
                for c in 0..dst_ch {
                    out.push(if c < src_ch { pcm[base + c] } else { 0 });
                }
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn cmds(v: Vec<MixCmd>) -> VecCmds {
            VecCmds(v.into())
        }

        /// A registered sound resamples once, on the game thread, and hands the callback a
        /// ready-to-read `Arc`. Two `prepare`s of the same sound must return the SAME buffer —
        /// that sharing is what makes the callback's `Arc` drop a refcount decrement and never a
        /// `free()` (cwage #96).
        #[test]
        fn registry_resamples_once_and_shares_the_buffer() {
            let mut reg = SoundRegistry::new();
            // 4 mono frames of 0x0101-ish LE i16.
            let id = reg.register(&[1, 0, 2, 0, 3, 0, 4, 0], 1, 8_000);
            assert_eq!(id, 1);
            assert_eq!(reg.len(), 1);

            let a = reg.prepare(id, 2, 8_000).expect("prepared");
            let b = reg.prepare(id, 2, 8_000).expect("prepared again");
            // Same allocation, not a re-resample.
            assert!(Arc::ptr_eq(&a, &b));
            // Mono -> stereo duplicate at the same rate: 4 frames * 2 channels.
            assert_eq!(*a, vec![1, 1, 2, 2, 3, 3, 4, 4]);
            // The registry keeps its own strong ref forever — the invariant the callback relies on.
            assert_eq!(Arc::strong_count(&a), 3); // registry + a + b
            assert!(reg.prepare(0, 2, 8_000).is_none());
            assert!(reg.prepare(99, 2, 8_000).is_none());
        }

        /// The voice pool is fixed-size and round-robin. Slot reuse must bump the generation so
        /// the recycled id differs — otherwise a late `stop` would kill an unrelated voice.
        #[test]
        fn voice_ids_roundtrip_and_bump_generation_on_reuse() {
            let mut reg = SoundRegistry::new();
            let mut first = Vec::new();
            for _ in 0..MAX_VOICES {
                let (slot, generation) = reg.alloc_voice();
                first.push(voice_id(slot, generation));
            }
            // Every id in one lap round the pool is distinct...
            let mut sorted = first.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), MAX_VOICES);
            // ...and every id decodes back to what produced it.
            for (i, &id) in first.iter().enumerate() {
                assert_eq!(decode_voice(id), Some((i, 1)));
            }
            // The next lap reuses slot 0 but at generation 2 — a different id.
            let (slot, generation) = reg.alloc_voice();
            assert_eq!((slot, generation), (0, 2));
            assert_ne!(voice_id(slot, generation), first[0]);
            // `0` is the reserved invalid id, and only `0`.
            assert_eq!(decode_voice(0), None);
            assert!(decode_voice(1).is_some());
        }

        /// A `stop` aimed at a slot that has since been stolen must be ignored, not applied to
        /// whatever voice now owns the slot.
        #[test]
        fn stale_stop_does_not_kill_the_voice_that_stole_the_slot() {
            let mut mix = Mixer::new(2);
            let pcm = Arc::new(vec![100i16; 64]);
            // Generation 1 takes slot 0, then generation 2 steals it.
            for generation in [1, 2] {
                mix.apply(MixCmd::Play {
                    slot: 0,
                    generation,
                    pcm: Arc::clone(&pcm),
                    vol_q8: 256,
                    looping: false,
                });
            }
            // The old owner's stop must not touch the new one.
            mix.apply(MixCmd::Stop {
                slot: 0,
                generation: 1,
            });
            assert!(mix.is_active(0), "stale stop killed the stealing voice");
            // The current owner's stop must.
            mix.apply(MixCmd::Stop {
                slot: 0,
                generation: 2,
            });
            assert!(!mix.is_active(0));
        }

        /// The command ring is the ONLY way into the mixer, so a `Play` arriving mid-block must
        /// be picked up by `fill`'s drain and be audible in that same block.
        #[test]
        fn fill_drains_commands_then_mixes_the_voice() {
            let mut mix = Mixer::new(1); // mono: unpanned, gain == vol
            let pcm = Arc::new(vec![1000i16, 2000, 3000, 4000]);
            let mut q = cmds(vec![MixCmd::Play {
                slot: 3,
                generation: 1,
                pcm,
                vol_q8: 256, // unity
                looping: false,
            }]);
            let mut out = [0i16; 4];
            mix.fill(&mut out, &mut Silence, &mut q, 1);
            assert_eq!(out, [1000, 2000, 3000, 4000]);
            // A 4-sample one-shot is finished; the next block is silence and the slot is free.
            let mut out2 = [0i16; 4];
            mix.fill(&mut out2, &mut Silence, &mut NoCmds, 1);
            assert_eq!(out2, [0, 0, 0, 0]);
            assert!(!mix.is_active(3));
        }

        /// The raw `gg.audio` ring stays 1:1 and unpanned (Pong / DOOM music depend on it), and
        /// sums with voices rather than replacing them.
        #[test]
        fn raw_ring_passes_through_and_sums_with_voices() {
            let mut mix = Mixer::new(1);
            let raw = [10i16, 20, 30, 40];
            let mut src = SlicePcm(&raw, 0);
            let mut out = [0i16; 4];
            // No voices: straight passthrough.
            mix.fill(&mut out, &mut src, &mut NoCmds, 1);
            assert_eq!(out, raw);
            // A dry ring reads as silence, not a stall.
            let mut out2 = [0i16; 2];
            mix.fill(&mut out2, &mut src, &mut NoCmds, 1);
            assert_eq!(out2, [0, 0]);
            // With a voice: the two sum.
            let mut src = SlicePcm(&raw, 0);
            let mut q = cmds(vec![MixCmd::Play {
                slot: 0,
                generation: 1,
                pcm: Arc::new(vec![5i16; 4]),
                vol_q8: 256,
                looping: false,
            }]);
            let mut out3 = [0i16; 4];
            mix.fill(&mut out3, &mut src, &mut q, 1);
            assert_eq!(out3, [15, 25, 35, 45]);
        }

        /// The linear pan law, at the three points that matter. Center is ~half gain per side.
        #[test]
        fn pan_law_splits_stereo_channels() {
            let pcm = Arc::new(vec![1000i16; 16]);
            let play = |pan_q8: u32| MixCmd::Tune {
                slot: 0,
                generation: 1,
                vol_q8: 256,
                pan_q8,
            };
            // Center's slight left/right asymmetry (496 vs 500) is the pan law's integer
            // 127/255-vs-128/255 split, not a bug: 128 is the only exact center an odd-width
            // 0..=255 range can't have.
            for (pan, want_l, want_r) in [(0u32, 1000i16, 0i16), (255, 0, 1000), (128, 496, 500)] {
                let mut mix = Mixer::new(2);
                mix.apply(MixCmd::Play {
                    slot: 0,
                    generation: 1,
                    pcm: Arc::clone(&pcm),
                    vol_q8: 256,
                    looping: true,
                });
                mix.apply(play(pan));
                let mut out = [0i16; 2];
                mix.fill(&mut out, &mut Silence, &mut NoCmds, 2);
                assert_eq!(out, [want_l, want_r], "pan {pan}");
            }
        }

        /// A looping voice wraps forever; a one-shot stops exactly at its end.
        #[test]
        fn looping_voice_wraps_and_one_shot_ends() {
            let pcm = Arc::new(vec![1i16, 2]);
            for looping in [true, false] {
                let mut mix = Mixer::new(1);
                mix.apply(MixCmd::Play {
                    slot: 0,
                    generation: 1,
                    pcm: Arc::clone(&pcm),
                    vol_q8: 256,
                    looping,
                });
                let mut out = [0i16; 6];
                mix.fill(&mut out, &mut Silence, &mut NoCmds, 1);
                if looping {
                    assert_eq!(out, [1, 2, 1, 2, 1, 2]);
                    assert!(mix.is_active(0));
                } else {
                    assert_eq!(out, [1, 2, 0, 0, 0, 0]);
                    assert!(!mix.is_active(0));
                }
            }
        }

        /// An empty sound must deactivate its voice instead of indexing an empty buffer.
        #[test]
        fn empty_sound_deactivates_rather_than_panicking() {
            let mut mix = Mixer::new(2);
            mix.apply(MixCmd::Play {
                slot: 0,
                generation: 1,
                pcm: Arc::new(Vec::new()),
                vol_q8: 256,
                looping: true,
            });
            let mut out = [0i16; 4];
            mix.fill(&mut out, &mut Silence, &mut NoCmds, 2);
            assert_eq!(out, [0; 4]);
            assert!(!mix.is_active(0));
        }

        /// Summing voices clip to `i16` rather than wrapping (a wrap is a loud, ugly click).
        #[test]
        fn mixed_voices_clip_instead_of_wrapping() {
            let mut mix = Mixer::new(1);
            for slot in 0..8 {
                mix.apply(MixCmd::Play {
                    slot,
                    generation: 1,
                    pcm: Arc::new(vec![i16::MAX; 4]),
                    vol_q8: 256,
                    looping: true,
                });
            }
            let mut out = [0i16; 4];
            mix.fill(&mut out, &mut Silence, &mut NoCmds, 1);
            assert_eq!(out, [i16::MAX; 4]);
        }

        /// THE cwage #96 regression test, and the one that is measured rather than asserted: a
        /// real counting global allocator watches `Mixer::fill` — the entire body of the cpal
        /// callback — and it must perform ZERO allocator operations, including frees.
        ///
        /// The interesting case is the third block: it carries a `Play` that STEALS a live slot,
        /// which drops that slot's `Arc<Vec<i16>>`. That drop is a `free()` on the real-time
        /// thread unless the registry still holds a strong reference. This test is what stops
        /// someone "cleaning up" the registry and silently reintroducing the click.
        #[test]
        fn callback_body_performs_no_allocator_operations() {
            let mut mix = Mixer::new(2);
            let pcm = Arc::new(vec![500i16; 8192]);
            let mut block = vec![0i16; 1024];

            // Fill the voice pool first, so later steals have a live Arc to displace.
            for slot in 0..MAX_VOICES {
                mix.apply(MixCmd::Play {
                    slot,
                    generation: 1,
                    pcm: Arc::clone(&pcm),
                    vol_q8: 200,
                    looping: true,
                });
            }

            // 1. Steady state: no commands, 64 voices, 1024 sample slots.
            let ops = crate::alloc_probe::count_allocator_ops(|| {
                mix.fill(&mut block, &mut Silence, &mut NoCmds, 2);
            });
            assert_eq!(ops, 0, "steady-state mix performed {ops} allocator ops");

            // 2. With the raw `gg.audio` ring also feeding it.
            let raw = vec![7i16; 1024];
            let ops = crate::alloc_probe::count_allocator_ops(|| {
                let mut src = SlicePcm(&raw, 0);
                mix.fill(&mut block, &mut src, &mut NoCmds, 2);
            });
            assert_eq!(
                ops, 0,
                "mix with a live raw ring performed {ops} allocator ops"
            );

            // 3. The hard case: commands arriving mid-block, including a slot steal whose
            //    displaced `Arc` must NOT be the last reference.
            let mut q = cmds(vec![
                MixCmd::Play {
                    slot: 0,
                    generation: 2,
                    pcm: Arc::clone(&pcm),
                    vol_q8: 256,
                    looping: false,
                },
                MixCmd::Tune {
                    slot: 1,
                    generation: 1,
                    vol_q8: 128,
                    pan_q8: 20,
                },
                MixCmd::Stop {
                    slot: 2,
                    generation: 1,
                },
            ]);
            let ops = crate::alloc_probe::count_allocator_ops(|| {
                mix.fill(&mut block, &mut Silence, &mut q, 2);
            });
            assert_eq!(ops, 0, "command drain performed {ops} allocator ops");
        }

        /// The probe itself must be able to see an allocation — otherwise the test above would
        /// pass just as happily if the counter were broken.
        #[test]
        fn alloc_probe_actually_counts() {
            let ops = crate::alloc_probe::count_allocator_ops(|| {
                let v: Vec<i16> = Vec::with_capacity(1024);
                std::hint::black_box(&v);
            });
            assert!(
                ops > 0,
                "the allocation probe counted nothing; it is broken"
            );
        }

        #[test]
        fn resample_rate_and_channel_conversions() {
            // Mono 8k -> stereo 16k: 2x the frames, each duplicated across channels.
            let out = resample(&[1, 2], 1, 8_000, 2, 16_000);
            assert_eq!(out, vec![1, 1, 1, 1, 2, 2, 2, 2]);
            // Stereo -> mono averages the pair.
            assert_eq!(
                resample(&[10, 20, 30, 40], 2, 8_000, 1, 8_000),
                vec![15, 35]
            );
            // Downsampling halves the frame count.
            assert_eq!(resample(&[1, 2, 3, 4], 1, 16_000, 1, 8_000), vec![1, 3]);
            // Degenerate inputs don't divide by zero or panic.
            assert!(resample(&[], 1, 8_000, 2, 8_000).is_empty());
            assert!(resample(&[1, 2], 0, 0, 0, 0).len() <= 2);
        }
    }
}

// ===========================================================================
// Headless implementation (default: `desktop` OFF). Zero heavy dependencies.
// ===========================================================================

#[cfg(not(feature = "desktop"))]
mod imp {
    use super::{Event, NONE_EVENT};

    pub(super) fn present(_pixels: *const u32, _width: u32, _height: u32, _stride: u32) {}

    pub(super) fn audio(_samples: &[i16]) {}

    pub(super) fn poll() -> Event {
        NONE_EVENT
    }

    pub(super) fn size() -> u64 {
        super::pack_size(super::DEFAULT_W, super::DEFAULT_H)
    }

    pub(super) fn tex(_rgba: &[u8], _w: u32, _h: u32) -> u32 {
        0
    }

    pub(super) fn frame(_w: u32, _h: u32, _clear_argb: u32) {}

    pub(super) fn sprite(_tex: u32, _dx: i32, _dy: i32) {}

    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_sub(
        _tex: u32,
        _sx: i32,
        _sy: i32,
        _sw: u32,
        _sh: u32,
        _dx: i32,
        _dy: i32,
    ) {
    }

    pub(super) fn rect(_dx: i32, _dy: i32, _w: u32, _h: u32, _argb: u32) {}

    pub(super) fn flush() {}

    pub(super) fn sound(_pcm: &[u8], _channels: u32, _rate: u32) -> u32 {
        0
    }

    pub(super) fn play(_sound: u32, _loop_: u32, _vol_q8: u32) -> u32 {
        0
    }

    pub(super) fn stop(_voice: u32) {}

    pub(super) fn mouse() -> u64 {
        0
    }

    pub(super) fn mouse_delta() -> u64 {
        0
    }

    pub(super) fn tune(_voice: u32, _vol_q8: u32, _pan_q8: u32) {}

    pub(super) fn show(_pixels: *const u32, _width: u32, _height: u32) {}

    pub(super) fn audio_spec() -> u64 {
        super::pack_size(super::DEFAULT_AUDIO_RATE, super::DEFAULT_AUDIO_CHANNELS)
    }

    pub(super) fn pending() -> u64 {
        0
    }

    pub(super) fn title(_name: &str) {}
}

// ===========================================================================
// Desktop implementation (`desktop` ON): minifb window + cpal audio + input queue.
// ===========================================================================

#[cfg(feature = "desktop")]
mod imp {
    use super::{Event, NONE_EVENT, event_kind, key, mixer};
    use std::collections::VecDeque;
    use std::sync::{Mutex, OnceLock};

    /// The default window title, used until a game sets its own via [`title`]. Kept generic (not
    /// per-game) so a gg program that never calls `gg.title` doesn't inherit another game's name.
    const WINDOW_TITLE: &str = "bet";
    /// Cap the audio ring so a program that outruns the audio callback can't grow it forever.
    /// ~10s of 48kHz stereo — far more headroom than any port uses.
    const RING_CAP: usize = 1 << 20;
    /// Cap the mixer command ring. Commands are tiny and the callback drains the whole ring every
    /// block (~10ms), so this is orders of magnitude more than a frame can ever produce.
    const CMD_CAP: usize = 256;

    /// `minifb::Key` → keycode pairs used to diff the down-key set each frame. `Escape` is a
    /// normal key here (KEY_DOWN/KEY_UP code 27); QUIT is emitted only for window close.
    const KEY_TABLE: &[(minifb::Key, u32)] = &[
        (minifb::Key::A, key::A),
        (minifb::Key::B, key::B),
        (minifb::Key::C, key::C),
        (minifb::Key::D, key::D),
        (minifb::Key::E, key::E),
        (minifb::Key::F, key::F),
        (minifb::Key::G, key::G),
        (minifb::Key::H, key::H),
        (minifb::Key::I, key::I),
        (minifb::Key::J, key::J),
        (minifb::Key::K, key::K),
        (minifb::Key::L, key::L),
        (minifb::Key::M, key::M),
        (minifb::Key::N, key::N),
        (minifb::Key::O, key::O),
        (minifb::Key::P, key::P),
        (minifb::Key::Q, key::Q),
        (minifb::Key::R, key::R),
        (minifb::Key::S, key::S),
        (minifb::Key::T, key::T),
        (minifb::Key::U, key::U),
        (minifb::Key::V, key::V),
        (minifb::Key::W, key::W),
        (minifb::Key::X, key::X),
        (minifb::Key::Y, key::Y),
        (minifb::Key::Z, key::Z),
        (minifb::Key::Key0, key::D0),
        (minifb::Key::Key1, key::D1),
        (minifb::Key::Key2, key::D2),
        (minifb::Key::Key3, key::D3),
        (minifb::Key::Key4, key::D4),
        (minifb::Key::Key5, key::D5),
        (minifb::Key::Key6, key::D6),
        (minifb::Key::Key7, key::D7),
        (minifb::Key::Key8, key::D8),
        (minifb::Key::Key9, key::D9),
        (minifb::Key::Space, key::SPACE),
        (minifb::Key::Enter, key::ENTER),
        (minifb::Key::Escape, key::ESC),
        (minifb::Key::Backspace, key::BACKSPACE),
        (minifb::Key::Tab, key::TAB),
        (minifb::Key::Apostrophe, key::APOSTROPHE),
        (minifb::Key::Comma, key::COMMA),
        (minifb::Key::Minus, key::MINUS),
        (minifb::Key::Period, key::PERIOD),
        (minifb::Key::Slash, key::SLASH),
        (minifb::Key::Semicolon, key::SEMICOLON),
        (minifb::Key::Equal, key::EQUALS),
        (minifb::Key::LeftBracket, key::LBRACKET),
        (minifb::Key::Backslash, key::BACKSLASH),
        (minifb::Key::RightBracket, key::RBRACKET),
        (minifb::Key::Backquote, key::BACKTICK),
        (minifb::Key::Up, key::UP),
        (minifb::Key::Down, key::DOWN),
        (minifb::Key::Left, key::LEFT),
        (minifb::Key::Right, key::RIGHT),
        (minifb::Key::LeftCtrl, key::LCTRL),
        (minifb::Key::RightCtrl, key::RCTRL),
        (minifb::Key::LeftShift, key::LSHIFT),
        (minifb::Key::RightShift, key::RSHIFT),
        (minifb::Key::LeftAlt, key::LALT),
        (minifb::Key::RightAlt, key::RALT),
        (minifb::Key::Pause, key::PAUSE),
        (minifb::Key::F1, key::F1),
        (minifb::Key::F2, key::F2),
        (minifb::Key::F3, key::F3),
        (minifb::Key::F4, key::F4),
        (minifb::Key::F5, key::F5),
        (minifb::Key::F6, key::F6),
        (minifb::Key::F7, key::F7),
        (minifb::Key::F8, key::F8),
        (minifb::Key::F9, key::F9),
        (minifb::Key::F10, key::F10),
        (minifb::Key::F11, key::F11),
        (minifb::Key::F12, key::F12),
    ];

    /// CoreGraphics relative-mouse support (macOS only). Warping the cursor back to window-center
    /// near an edge is how a position-difference mouse delta avoids clamping to zero at the screen
    /// boundary; the associate call after the warp cancels the ~250ms movement-suppression interval
    /// CGWarp otherwise imposes, keeping the turn smooth. The framework link directive here does not
    /// survive into a static archive, so the compiled-program link names it explicitly too (see
    /// `driver`'s `MACOS_GG_FRAMEWORKS`).
    #[cfg(target_os = "macos")]
    mod cg {
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct CGPoint {
            pub x: f64,
            pub y: f64,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct CGSize {
            pub width: f64,
            pub height: f64,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct CGRect {
            pub origin: CGPoint,
            pub size: CGSize,
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            pub fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
            pub fn CGAssociateMouseAndMouseCursorPosition(connected: i32) -> i32;
            // The bounds (in logical points — the same space minifb sizes windows in) of the
            // main display, used to size the `GG_FULLSQUARE` max-square window.
            pub fn CGMainDisplayID() -> u32;
            pub fn CGDisplayBounds(display: u32) -> CGRect;
        }
    }

    /// The main display's logical size in points, or `(0, 0)` if it can't be determined. macOS
    /// uses CoreGraphics (the point-space bounds, so it's Retina-correct); elsewhere it's unknown
    /// and callers fall back to a frame-relative window size.
    #[cfg(target_os = "macos")]
    fn display_size() -> (usize, usize) {
        // SAFETY: both are pure CoreGraphics queries with no arguments to validate, always linked
        // on macOS (see the `cg` module's framework link).
        unsafe {
            let b = cg::CGDisplayBounds(cg::CGMainDisplayID());
            (
                b.size.width.max(0.0) as usize,
                b.size.height.max(0.0) as usize,
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    fn display_size() -> (usize, usize) {
        (0, 0)
    }

    /// An uploaded texture: premultiplied `0xAARR_GGBB` pixels, row-major `w * h`.
    struct Texture {
        w: u32,
        h: u32,
        px: Vec<u32>,
    }

    /// The process-global platform state. Created lazily on first [`present`]/[`audio`].
    struct GgState {
        /// The window, created on the first `present` (its size comes from that first frame).
        window: Option<minifb::Window>,
        /// The producing end of the wait-free SPSC sample ring drained by the audio callback.
        /// `None` until the stream opens (or forever, if no device). The game thread is the only
        /// producer and the callback the only consumer — hence SPSC, hence no lock (cwage #96).
        pcm_tx: Option<rtrb::Producer<i16>>,
        /// The producing end of the wait-free SPSC command ring: how `play`/`stop`/`tune` reach
        /// the callback-owned mixer without either side taking a lock.
        cmd_tx: Option<rtrb::Producer<mixer::MixCmd>>,
        /// The game-thread-owned sound registry. Holds every resampled buffer alive for the life
        /// of the process, which is what keeps `free()` out of the audio callback.
        sounds: mixer::SoundRegistry,
        /// The device's output format, cached here because the `Mixer` itself now lives inside
        /// the callback and the game thread cannot read it.
        device_rate: u32,
        device_channels: u16,
        /// The live output stream. Kept alive to keep audio playing; dropped ⇒ silence. `None`
        /// until the first `audio` call (or if no output device could be opened).
        stream: Option<cpal::Stream>,
        /// Whether we already tried (and either succeeded or failed) to open the audio stream.
        audio_started: bool,
        /// The synthesized input-event queue drained by [`poll`].
        events: VecDeque<Event>,
        /// Keycodes that were down at the previous `present`, for edge detection.
        down: Vec<u32>,
        /// Set once a QUIT has been enqueued, so we don't flood the queue every frame.
        quit_sent: bool,
        /// The composited logical canvas (`canvas_w * canvas_h` XRGB pixels), rebuilt by `frame`.
        canvas: Vec<u32>,
        canvas_w: u32,
        canvas_h: u32,
        /// Uploaded textures (premultiplied), indexed by `id - 1`.
        textures: Vec<Texture>,
        /// The integer upscale factor and letterbox offsets recorded at the last `flush`, used to
        /// map window pixels back to logical-canvas coordinates for the mouse.
        scale: u32,
        off_x: i32,
        off_y: i32,
        /// The last known mouse position, in logical-canvas coordinates.
        mouse_x: i32,
        mouse_y: i32,
        /// The last raw window-pixel cursor position, for relative-mouse delta accumulation
        /// (`None` until the cursor is first seen).
        last_win_mouse: Option<(f32, f32)>,
        /// Raw mouse deltas accumulated since the last `mouse_delta()` drain, in window pixels.
        /// Kept as `f32` so the sub-pixel remainder carries across drains.
        acc_dx: f32,
        acc_dy: f32,
        /// Debounced mouse-button state, for MOUSE_DOWN / MOUSE_UP edge detection.
        mouse_left: bool,
        mouse_right: bool,
        /// Relative-mouse mode: armed on the first `mouse_delta()` drain (so absolute-`gg.mouse()`
        /// games like Pong/FrozenBubble, which never drain a delta, stay unaffected). While armed
        /// the cursor is hidden and, on macOS, warped back to window-center near an edge so the raw
        /// delta never clamps to zero at the screen boundary — the mouse-look edge-clip turn bug.
        relative_mouse: bool,
        /// Whether the cursor has already been hidden for `relative_mouse` (hide once, not per pump).
        cursor_hidden: bool,
        /// The window title: `WINDOW_TITLE` until a game sets its own via `set_title`, then applied
        /// live and used as the title when the window is (re)created.
        title: String,
    }

    // SAFETY: `minifb::Window` and `cpal::Stream` are `!Send`, but every field of `GgState` is
    // reached ONLY from the game-loop thread — every `gg` entry point funnels through
    // `with_state`, and nothing else holds a reference to any of them. The `Send` claim is what
    // lets us keep the state in a `static Mutex`; that game-thread-only discipline is what makes
    // it sound.
    //
    // The audio callback is a SECOND thread, and a real-time one — it is not covered by, and
    // must not be argued from, the discipline above (an earlier revision of this comment claimed
    // a "single-thread" invariant the code did not have; see cwage #96). What actually makes the
    // callback safe is that it shares NO state with `GgState`: it owns its `Mixer` outright and
    // reaches the game thread only through the two wait-free SPSC rings, whose `Consumer` halves
    // it owns. There is nothing here for it to race, and nothing for it to lock.
    unsafe impl Send for GgState {}

    impl GgState {
        fn new() -> Self {
            GgState {
                window: None,
                pcm_tx: None,
                cmd_tx: None,
                sounds: mixer::SoundRegistry::new(),
                device_rate: 0,
                device_channels: 0,
                stream: None,
                audio_started: false,
                events: VecDeque::new(),
                down: Vec::new(),
                quit_sent: false,
                canvas: Vec::new(),
                canvas_w: 0,
                canvas_h: 0,
                textures: Vec::new(),
                scale: 1,
                off_x: 0,
                off_y: 0,
                mouse_x: 0,
                mouse_y: 0,
                last_win_mouse: None,
                acc_dx: 0.0,
                acc_dy: 0.0,
                mouse_left: false,
                mouse_right: false,
                relative_mouse: false,
                cursor_hidden: false,
                title: WINDOW_TITLE.to_string(),
            }
        }

        /// Copy `buf` (a tightly packed `w * h` frame) into the window, creating the window on
        /// the first call, then synthesize input events from the new key state.
        fn present(&mut self, buf: &[u32], w: usize, h: usize) {
            let title = self.title.clone();
            let window = self.window.get_or_insert_with(|| {
                // `X1` maps the framebuffer 1:1 to the window (no upscaling), so the program can
                // drive true dynamic resolution by sizing the framebuffer to `size()`. `resize`
                // lets the user maximize; `Stretch` only covers the single transient frame after
                // a resize, before the program reallocates its framebuffer to the new size.
                let opts = minifb::WindowOptions {
                    resize: true,
                    scale: minifb::Scale::X1,
                    scale_mode: minifb::ScaleMode::Stretch,
                    ..minifb::WindowOptions::default()
                };
                minifb::Window::new(&title, w, h, opts)
                    .expect("gg: failed to open the minifb window")
            });
            // `update_with_buffer` wants exactly `w * h` pixels; `present()` guarantees `buf`
            // is that long.
            let _ = window.update_with_buffer(buf, w, h);
            self.pump_input();
        }

        /// Diff the current key state against the previous frame, enqueueing KEY_DOWN/KEY_UP
        /// edges, and enqueue a single QUIT when the window is CLOSED. Esc is deliberately NOT
        /// a quit: it arrives as a normal key event (code 27, via `KEY_TABLE`) so games can
        /// drive pause/settings menus with it — only window close means "the player left".
        fn pump_input(&mut self) {
            // Arm relative-mouse presentation once `mouse_delta()` has been drained: hide the
            // cursor (meaningless in a mouse-look game) exactly once. The warp-to-center itself
            // lives in the delta section below, next to the position read it corrects.
            if self.relative_mouse
                && !self.cursor_hidden
                && let Some(w) = self.window.as_mut()
            {
                w.set_cursor_visibility(false);
                self.cursor_hidden = true;
            }

            let Some(window) = self.window.as_ref() else {
                return;
            };

            if !self.quit_sent && !window.is_open() {
                self.events.push_back(Event {
                    kind: event_kind::QUIT,
                    code: 0,
                    x: 0,
                    y: 0,
                });
                self.quit_sent = true;
            }

            let mut now_down: Vec<u32> = Vec::new();
            for &(mk, code) in KEY_TABLE {
                if window.is_key_down(mk) {
                    now_down.push(code);
                    if !self.down.contains(&code) {
                        self.events.push_back(Event {
                            kind: event_kind::KEY_DOWN,
                            code,
                            x: 0,
                            y: 0,
                        });
                    }
                }
            }
            for &code in &self.down {
                if !now_down.contains(&code) {
                    self.events.push_back(Event {
                        kind: event_kind::KEY_UP,
                        code,
                        x: 0,
                        y: 0,
                    });
                }
            }
            self.down = now_down;

            // Relative mouse: accumulate raw window-pixel deltas between pumps, drained by
            // `mouse_delta()`. minifb exposes only absolute positions (no pointer lock). On macOS,
            // once relative mode is armed we warp the cursor back to window-center whenever it
            // nears an edge (CoreGraphics), so the delta never clamps to zero at the screen
            // boundary — the mouse-look edge-clip turn bug. Off macOS the delta is a plain position
            // difference and still clips at the edge (the target platform is this Mac).
            // NB: the MOUSE_MOVE event kind stays reserved and unemitted — movement is polled
            // via `gg.mouse()` (absolute) / `gg.mouseDelta()` (relative), never event-queued.
            if let Some((wx, wy)) = window.get_mouse_pos(minifb::MouseMode::Pass) {
                if let Some((lx, ly)) = self.last_win_mouse {
                    self.acc_dx += wx - lx;
                    self.acc_dy += wy - ly;
                }
                self.last_win_mouse = Some((wx, wy));

                #[cfg(target_os = "macos")]
                if self.relative_mouse {
                    let (sw, sh) = window.get_size();
                    let (sw, sh) = (sw as f32, sh as f32);
                    // Re-center when inside this edge margin (a quarter of the smaller dimension,
                    // min 16px) — a wide comfort band so a fast flick can't overshoot the true
                    // edge between warps.
                    let margin = (sw.min(sh) * 0.25).max(16.0);
                    if sw > 2.0 * margin
                        && sh > 2.0 * margin
                        && (wx < margin || wy < margin || wx > sw - margin || wy > sh - margin)
                    {
                        let (cx, cy) = (sw / 2.0, sh / 2.0);
                        let (px, py) = window.get_position();
                        // CGWarpMouseCursorPosition takes a global, top-left-origin display point;
                        // minifb's get_position is the window's top-left in that same space.
                        let target = cg::CGPoint {
                            x: px as f64 + cx as f64,
                            y: py as f64 + cy as f64,
                        };
                        // SAFETY: POD-point CoreGraphics calls, always linked on macOS. The second
                        // call cancels the ~250ms post-warp suppression so turning stays smooth.
                        unsafe {
                            cg::CGWarpMouseCursorPosition(target);
                            cg::CGAssociateMouseAndMouseCursorPosition(1);
                        }
                        // The warp must not read as movement: next pump measures from center.
                        self.last_win_mouse = Some((cx, cy));
                    }
                }
            }

            // Mouse: track the cursor in logical-canvas coords, and synthesize MOUSE_DOWN/UP
            // edges for the two buttons (code 0 = left, 1 = right). Position via `gg.mouse()`.
            if let Some((mx, my)) = window.get_mouse_pos(minifb::MouseMode::Clamp) {
                let scale = self.scale.max(1) as i32;
                let lx = (mx as i32 - self.off_x) / scale;
                let ly = (my as i32 - self.off_y) / scale;
                let max_x = (self.canvas_w as i32 - 1).max(0);
                let max_y = (self.canvas_h as i32 - 1).max(0);
                self.mouse_x = lx.clamp(0, max_x);
                self.mouse_y = ly.clamp(0, max_y);
            }
            let left = window.get_mouse_down(minifb::MouseButton::Left);
            if left != self.mouse_left {
                self.events.push_back(Event {
                    kind: if left {
                        event_kind::MOUSE_DOWN
                    } else {
                        event_kind::MOUSE_UP
                    },
                    code: 0,
                    x: self.mouse_x,
                    y: self.mouse_y,
                });
                self.mouse_left = left;
            }
            let right = window.get_mouse_down(minifb::MouseButton::Right);
            if right != self.mouse_right {
                self.events.push_back(Event {
                    kind: if right {
                        event_kind::MOUSE_DOWN
                    } else {
                        event_kind::MOUSE_UP
                    },
                    code: 1,
                    x: self.mouse_x,
                    y: self.mouse_y,
                });
                self.mouse_right = right;
            }
        }

        /// Push samples onto the wait-free sample ring, opening the output stream on first use.
        ///
        /// Overflow policy CHANGED with the lock-free rewrite (cwage #96): the old
        /// `Mutex<VecDeque>` dropped the OLDEST sample to stay under [`RING_CAP`]; an SPSC
        /// producer cannot pop, so a full ring now drops the NEWEST samples instead. Both are
        /// lossy, and a full ring means the game is ~10s of audio ahead of the device — a
        /// condition `pending()` exists to let ports avoid, and which no port hits. That is a
        /// cheap price for taking the lock out of the real-time thread.
        fn push_audio(&mut self, samples: &[i16]) {
            self.ensure_stream();
            let Some(tx) = self.pcm_tx.as_mut() else {
                return; // No stream (no device) ⇒ nothing to feed; skip.
            };
            for &s in samples {
                if tx.push(s).is_err() {
                    break; // Ring full: drop the rest of this block rather than block.
                }
            }
        }

        /// Send one command to the callback-owned mixer. Never blocks: a full ring drops the
        /// command (an inaudible missed one-shot beats a stalled game thread).
        fn send_cmd(&mut self, cmd: mixer::MixCmd) {
            if let Some(tx) = self.cmd_tx.as_mut() {
                let _ = tx.push(cmd);
            }
        }

        /// Ensure the audio output stream exists (opened lazily on first audio use). Best-effort:
        /// if no device/stream can be opened, `stream` stays `None` and audio is silently dropped.
        fn ensure_stream(&mut self) {
            if self.audio_started {
                return;
            }
            self.audio_started = true;
            if let Some(open) = build_stream() {
                self.stream = Some(open.stream);
                self.pcm_tx = Some(open.pcm_tx);
                self.cmd_tx = Some(open.cmd_tx);
                self.device_rate = open.device_rate;
                self.device_channels = open.device_channels;
            }
        }

        /// Upload an RGBA8 texture (premultiplying each pixel) and return its 1-based id.
        fn tex(&mut self, rgba: &[u8], w: u32, h: u32) -> u32 {
            if w == 0 || h == 0 {
                return 0;
            }
            let n = (w as usize) * (h as usize);
            let mut px = vec![0u32; n];
            for (i, out) in px.iter_mut().enumerate() {
                let base = i * 4;
                // Bounds-guard short input by zero-filling any missing channels.
                let r = u32::from(rgba.get(base).copied().unwrap_or(0));
                let g = u32::from(rgba.get(base + 1).copied().unwrap_or(0));
                let b = u32::from(rgba.get(base + 2).copied().unwrap_or(0));
                let a = u32::from(rgba.get(base + 3).copied().unwrap_or(0));
                // Premultiply so the src-over blit is a straight `src + dst*(255-a)/255`.
                let pr = r * a / 255;
                let pg = g * a / 255;
                let pb = b * a / 255;
                *out = (a << 24) | (pr << 16) | (pg << 8) | pb;
            }
            self.textures.push(Texture { w, h, px });
            self.textures.len() as u32
        }

        /// Begin a frame: (re)size the canvas if needed and clear it to `clear_argb`'s RGB.
        fn frame(&mut self, w: u32, h: u32, clear_argb: u32) {
            let n = (w as usize) * (h as usize);
            if self.canvas_w != w || self.canvas_h != h || self.canvas.len() != n {
                self.canvas = vec![0u32; n];
                self.canvas_w = w;
                self.canvas_h = h;
            }
            let clear = clear_argb & 0x00FF_FFFF;
            for p in self.canvas.iter_mut() {
                *p = clear;
            }
        }

        /// Premultiplied src-over blit of texture `tex` at `(dx, dy)`, clipped to the canvas.
        fn sprite(&mut self, tex: u32, dx: i32, dy: i32) {
            let (tw, th) = match self.textures.get((tex.wrapping_sub(1)) as usize) {
                Some(t) => (t.w as i32, t.h as i32),
                None => return, // `tex == 0` wraps to u32::MAX; both miss and no-op.
            };
            self.blit_sub(tex, 0, 0, tw, th, dx, dy);
        }

        /// Premultiplied src-over blit of the source sub-rectangle `(sx, sy, sw, sh)` of `tex` to
        /// `(dx, dy)`, clipped to both the texture and the canvas. The shared blit core: `sprite`
        /// is this over the whole texture, `sprite_sub` over an arbitrary source window.
        #[allow(clippy::too_many_arguments)]
        fn blit_sub(&mut self, tex: u32, sx: i32, sy: i32, sw: i32, sh: i32, dx: i32, dy: i32) {
            if tex == 0 || tex as usize > self.textures.len() {
                return;
            }
            let cw = self.canvas_w as i32;
            let ch = self.canvas_h as i32;
            let cwu = self.canvas_w as usize;
            // Split the borrow: read the texture, write the canvas (distinct fields of `self`).
            let GgState {
                textures, canvas, ..
            } = self;
            let t = &textures[(tex - 1) as usize];
            let tw = t.w as i32;
            let th = t.h as i32;
            // Clip the source window to the texture; each source pixel maps to a dest offset.
            let sx0 = sx.max(0);
            let sy0 = sy.max(0);
            let sx1 = sx.saturating_add(sw).min(tw);
            let sy1 = sy.saturating_add(sh).min(th);
            for syi in sy0..sy1 {
                let y = dy + (syi - sy);
                if y < 0 || y >= ch {
                    continue;
                }
                for sxi in sx0..sx1 {
                    let x = dx + (sxi - sx);
                    if x < 0 || x >= cw {
                        continue;
                    }
                    let src = t.px[(syi as usize) * (t.w as usize) + sxi as usize];
                    let a = src >> 24;
                    if a == 0 {
                        continue;
                    }
                    let di = (y as usize) * cwu + x as usize;
                    let dst = canvas[di];
                    let inv = 255 - a;
                    let sr = (src >> 16) & 0xFF;
                    let sg = (src >> 8) & 0xFF;
                    let sb = src & 0xFF;
                    let dr = (dst >> 16) & 0xFF;
                    let dg = (dst >> 8) & 0xFF;
                    let db = dst & 0xFF;
                    // `src` is premultiplied: out = src + dst*(255-a)/255.
                    let or = sr + dr * inv / 255;
                    let og = sg + dg * inv / 255;
                    let ob = sb + db * inv / 255;
                    canvas[di] = (or << 16) | (og << 8) | ob;
                }
            }
        }

        /// Premultiplied src-over blit of the source sub-rectangle `(sx, sy, sw, sh)` of `tex`.
        #[allow(clippy::too_many_arguments)]
        fn sprite_sub(&mut self, tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
            self.blit_sub(tex, sx, sy, sw as i32, sh as i32, dx, dy);
        }

        /// Src-over fill of a rectangle with a straight (non-premultiplied) `0xAARR_GGBB` color.
        fn rect(&mut self, dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
            let a = argb >> 24;
            if a == 0 {
                return;
            }
            let cw = self.canvas_w as i32;
            let ch = self.canvas_h as i32;
            let cwu = self.canvas_w as usize;
            let sr = (argb >> 16) & 0xFF;
            let sg = (argb >> 8) & 0xFF;
            let sb = argb & 0xFF;
            let inv = 255 - a;
            for ry in 0..h as i32 {
                let y = dy + ry;
                if y < 0 || y >= ch {
                    continue;
                }
                for rx in 0..w as i32 {
                    let x = dx + rx;
                    if x < 0 || x >= cw {
                        continue;
                    }
                    let di = (y as usize) * cwu + x as usize;
                    if a == 255 {
                        self.canvas[di] = argb & 0x00FF_FFFF;
                        continue;
                    }
                    let dst = self.canvas[di];
                    let dr = (dst >> 16) & 0xFF;
                    let dg = (dst >> 8) & 0xFF;
                    let db = dst & 0xFF;
                    let or = (sr * a + dr * inv) / 255;
                    let og = (sg * a + dg * inv) / 255;
                    let ob = (sb * a + db * inv) / 255;
                    self.canvas[di] = (or << 16) | (og << 8) | ob;
                }
            }
        }

        /// Present the composited canvas: aspect-fit (integer nearest-neighbor upscale, centered
        /// with black letterbox bars) into the window, then pump input.
        fn flush(&mut self) {
            let cw = self.canvas_w as usize;
            let ch = self.canvas_h as usize;
            if cw == 0 || ch == 0 || self.canvas.len() != cw * ch {
                // Nothing to present yet (or a `show` retargeted the logical size and no `frame`
                // has rebuilt the canvas), but still pump input so quit/keys are observed.
                self.pump_input();
                return;
            }
            // Move the canvas out to split the borrow (`present_scaled` reads the source but
            // writes the window/scale fields), then put it back.
            let canvas = std::mem::take(&mut self.canvas);
            self.present_scaled(&canvas, cw, ch);
            self.canvas = canvas;
        }

        /// Present an app-owned, tightly packed `w * h` framebuffer with the very same scaler as
        /// [`Self::flush`] — `present`'s input model with `flush`'s presentation. Adopts `w * h`
        /// as the logical size so `gg.mouse()` maps into the shown frame (a later `frame` call
        /// simply re-sizes the compositor canvas if the two are mixed).
        fn show(&mut self, buf: &[u32], w: usize, h: usize) {
            if w == 0 || h == 0 {
                self.pump_input();
                return;
            }
            self.canvas_w = w as u32;
            self.canvas_h = h as u32;
            self.present_scaled(buf, w, h);
        }

        /// The shared presentation core behind [`Self::flush`] and [`Self::show`]: aspect-fit
        /// `src` (a tightly packed `cw * ch` logical frame) into the live window by an integer
        /// nearest-neighbor upscale, centered with black letterbox bars; record the
        /// window→logical mapping for the mouse; pump input.
        fn present_scaled(&mut self, src: &[u32], cw: usize, ch: usize) {
            // 1. Ensure the window (default ~2x the frame), read its live size, end that borrow.
            let title = self.title.clone();
            let window = self.window.get_or_insert_with(|| {
                let opts = minifb::WindowOptions {
                    resize: true,
                    scale: minifb::Scale::X1,
                    scale_mode: minifb::ScaleMode::Stretch,
                    ..minifb::WindowOptions::default()
                };
                // Default: ~2x the logical frame. With `GG_FULLSQUARE` set (the DOOM launcher
                // exports it) open the largest square that fits the main display instead — a ~6%
                // margin keeps it clear of the menu bar/dock — so the aspect-fit compositor shows
                // the frame as large as possible, centered in a square window. Falls back to the
                // 2x size when the display size is unknown or would be smaller than that.
                let (open_w, open_h) = if std::env::var_os("GG_FULLSQUARE").is_some() {
                    let (dw, dh) = display_size();
                    let side = ((dw.min(dh) as f64 * 0.94) as usize)
                        .max(cw * 2)
                        .max(ch * 2);
                    (side, side)
                } else {
                    (cw * 2, ch * 2)
                };
                minifb::Window::new(&title, open_w, open_h, opts)
                    .expect("gg: failed to open the minifb window")
            });
            let (win_w, win_h) = window.get_size();
            // 2. Integer aspect-fit scale + centered offsets; nearest-neighbor upscale into `present`.
            let scale = (win_w / cw).min(win_h / ch).max(1);
            let out_w = cw * scale;
            let out_h = ch * scale;
            let off_x = (win_w as i32 - out_w as i32) / 2;
            let off_y = (win_h as i32 - out_h as i32) / 2;
            let mut present = vec![0u32; win_w * win_h];
            for y in 0..out_h {
                let py = off_y + y as i32;
                if py < 0 || py >= win_h as i32 {
                    continue;
                }
                let sy = y / scale;
                let row = (py as usize) * win_w;
                for x in 0..out_w {
                    let px_out = off_x + x as i32;
                    if px_out < 0 || px_out >= win_w as i32 {
                        continue;
                    }
                    present[row + px_out as usize] = src[sy * cw + x / scale];
                }
            }
            // 3. Re-borrow the window and present the upscaled buffer.
            let _ = self
                .window
                .as_mut()
                .unwrap()
                .update_with_buffer(&present, win_w, win_h);
            // 4. Record the mapping for the mouse, then pump input.
            self.scale = scale as u32;
            self.off_x = off_x;
            self.off_y = off_y;
            self.pump_input();
        }

        /// Register a PCM sound (interleaved little-endian `i16`) with the game-thread registry;
        /// returns its 1-based id. Allocates — on the game thread, where that is fine.
        fn sound(&mut self, pcm: &[u8], channels: u32, rate: u32) -> u32 {
            self.sounds.register(pcm, channels, rate)
        }

        /// Start a voice playing `sound`. Everything expensive — the one-time resample to the
        /// device format — happens HERE, on the game thread; the callback receives only a
        /// finished `Arc` through the command ring. Returns the voice id, or `0` if there is no
        /// audio device or the sound id is invalid.
        fn play(&mut self, sound: u32, loop_: u32, vol_q8: u32) -> u32 {
            self.ensure_stream();
            if self.stream.is_none() {
                return 0;
            }
            let (dev_ch, dev_rate) = (u32::from(self.device_channels), self.device_rate);
            let Some(pcm) = self.sounds.prepare(sound, dev_ch, dev_rate) else {
                return 0;
            };
            let (slot, generation) = self.sounds.alloc_voice();
            self.send_cmd(mixer::MixCmd::Play {
                slot,
                generation,
                pcm,
                vol_q8,
                looping: loop_ != 0,
            });
            mixer::voice_id(slot, generation)
        }

        /// Stop the voice `voice`; unknown, finished, or already-stolen ids are ignored.
        fn stop(&mut self, voice: u32) {
            let Some((slot, generation)) = mixer::decode_voice(voice) else {
                return;
            };
            self.send_cmd(mixer::MixCmd::Stop { slot, generation });
        }

        /// Live-update voice `voice`'s volume and stereo pan (see the pan law on
        /// [`mixer::Mixer::mix_sample`]). Unknown, finished, or already-stolen ids are ignored,
        /// so retuning a voice that already ended is safe.
        fn tune(&mut self, voice: u32, vol_q8: u32, pan_q8: u32) {
            let Some((slot, generation)) = mixer::decode_voice(voice) else {
                return;
            };
            self.send_cmd(mixer::MixCmd::Tune {
                slot,
                generation,
                vol_q8,
                pan_q8,
            });
        }

        /// The mouse position in logical-canvas coordinates, packed `x << 32 | y`.
        fn mouse(&self) -> u64 {
            ((self.mouse_x as u32 as u64) << 32) | (self.mouse_y as u32 as u64)
        }

        /// Set the window title, applied to the live window immediately and remembered so a later
        /// window (re)creation uses it. An empty title is ignored (keeps the current/default).
        fn set_title(&mut self, name: &str) {
            if name.is_empty() {
                return;
            }
            self.title = name.to_string();
            if let Some(w) = self.window.as_mut() {
                w.set_title(name);
            }
        }

        /// Drain the accumulated raw mouse delta as a sign-preserving `(dx, dy)` `i32` pair
        /// packed `(dx as u32) << 32 | (dy as u32)`. Truncates toward zero; the fractional
        /// sub-pixel remainder stays accumulated for the next drain.
        fn mouse_delta(&mut self) -> u64 {
            // The first drain arms relative-mouse presentation (cursor hide + macOS edge-warp,
            // applied at the next pump). Lazy arming keeps absolute-`gg.mouse()` games unaffected.
            self.relative_mouse = true;
            let dx = self.acc_dx as i32;
            let dy = self.acc_dy as i32;
            self.acc_dx -= dx as f32;
            self.acc_dy -= dy as f32;
            ((dx as u32 as u64) << 32) | (dy as u32 as u64)
        }

        /// The audio device's output config, packed `rate << 32 | channels`. Opens the output
        /// stream on first use (like `play`) so a music synth can size itself before submitting
        /// any samples; no usable device ⇒ the fixed headless default.
        fn audio_spec(&mut self) -> u64 {
            self.ensure_stream();
            if self.stream.is_some() && self.device_rate != 0 && self.device_channels != 0 {
                return super::pack_size(self.device_rate, u32::from(self.device_channels));
            }
            super::pack_size(super::DEFAULT_AUDIO_RATE, super::DEFAULT_AUDIO_CHANNELS)
        }

        /// Interleaved `i16` samples currently queued in the raw `audio` ring (submitted but not
        /// yet consumed by the device callback) — the streaming backpressure signal. Read from
        /// the producer's free-slot count, so it stays lock-free too.
        fn pending(&self) -> u64 {
            match self.pcm_tx.as_ref() {
                Some(tx) => (RING_CAP - tx.slots()) as u64,
                None => 0,
            }
        }
    }

    /// The `BET_GG_HEADLESS` switch: when the env var is set to anything but `""`/`"0"`, this
    /// desktop-featured build runs fully headless — the singleton is never created, so no
    /// window opens and no audio device is touched — matching the default headless build's
    /// behavior exactly (see the crate docs). Checked once at gg init (the first gg call).
    fn headless() -> bool {
        static HEADLESS: OnceLock<bool> = OnceLock::new();
        *HEADLESS.get_or_init(|| {
            std::env::var("BET_GG_HEADLESS").is_ok_and(|v| !v.is_empty() && v != "0")
        })
    }

    /// The process-global singleton. `Mutex<GgState>: Sync` holds because we force `GgState:
    /// Send`; the game-thread-only discipline (see the `unsafe impl`) makes that sound.
    static GG: OnceLock<Mutex<GgState>> = OnceLock::new();

    fn with_state<R>(f: impl FnOnce(&mut GgState) -> R) -> R {
        let cell = GG.get_or_init(|| Mutex::new(GgState::new()));
        let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// An `rtrb` consumer of raw `gg.audio` samples, drained 1:1 by the callback.
    impl mixer::PcmSource for rtrb::Consumer<i16> {
        #[inline]
        fn next_sample(&mut self) -> i16 {
            // A dry ring is silence, not an error: `gg.audio` streaming games (Pong, DOOM's
            // music) legitimately run the ring empty between submissions.
            self.pop().unwrap_or(0)
        }
    }

    /// An `rtrb` consumer of mixer commands.
    impl mixer::CmdSource for rtrb::Consumer<mixer::MixCmd> {
        #[inline]
        fn next_cmd(&mut self) -> Option<mixer::MixCmd> {
            self.pop().ok()
        }
    }

    /// Everything `ensure_stream` needs back from a successful device open: the stream to keep
    /// alive, the two producer ends, and the device format the game thread must resample to.
    struct OpenStream {
        stream: cpal::Stream,
        pcm_tx: rtrb::Producer<i16>,
        cmd_tx: rtrb::Producer<mixer::MixCmd>,
        device_rate: u32,
        device_channels: u16,
    }

    /// Open the default output device and start a stream.
    ///
    /// The callback closure MOVES its `Mixer` and both ring `Consumer`s in, so at steady state it
    /// touches nothing any other thread can see: no `Arc`, no `Mutex`, no allocation, no
    /// unbounded work — just `Mixer::fill` (cwage #96). Returns `None` (silently) if no
    /// device/config/stream could be set up — audio is best-effort.
    fn build_stream() -> Option<OpenStream> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let (device_rate, device_channels) = (config.sample_rate.0, config.channels);
        let err_fn = |_err| { /* best-effort audio: ignore stream errors */ };
        // Interleaved-slot → channel index for the pan law: cpal hands whole frames, so slot `i`
        // is channel `i % channels`.
        let channels = usize::from(config.channels).max(1);

        let (pcm_tx, mut pcm_rx) = rtrb::RingBuffer::<i16>::new(RING_CAP);
        let (cmd_tx, mut cmd_rx) = rtrb::RingBuffer::<mixer::MixCmd>::new(CMD_CAP);
        let mut mix = mixer::Mixer::new(device_channels);

        let stream = match sample_format {
            cpal::SampleFormat::I16 => device
                .build_output_stream(
                    &config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        mix.fill(data, &mut pcm_rx, &mut cmd_rx, channels);
                    },
                    err_fn,
                    None,
                )
                .ok()?,
            cpal::SampleFormat::F32 => device
                .build_output_stream(
                    &config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        // Mix into `i16` (the mixer's native format) one slot at a time and
                        // convert in place — no scratch buffer, so still zero-allocation.
                        mix.drain(&mut cmd_rx);
                        for (i, s) in data.iter_mut().enumerate() {
                            let v = mix.mix_sample(&mut pcm_rx, i % channels);
                            *s = f32::from(v) / 32768.0;
                        }
                    },
                    err_fn,
                    None,
                )
                .ok()?,
            // Some other native format — skip audio rather than guess.
            _ => return None,
        };
        stream.play().ok()?;
        Some(OpenStream {
            stream,
            pcm_tx,
            cmd_tx,
            device_rate,
            device_channels,
        })
    }

    // Every wrapper below short-circuits under `BET_GG_HEADLESS` to the same value the default
    // headless build returns, BEFORE touching the singleton — so headless runs never open a
    // window or audio device.

    /// Pack a (possibly strided) source framebuffer into a tightly packed `w * h` buffer, so the
    /// raw-pointer read is confined here and the window only ever sees a packed frame.
    ///
    /// The per-row copy is clamped to `stride`: a `FrameBuffer` whose `width` exceeds its `stride`
    /// is malformed, and reading `width` u32s from a row only `stride` wide would run past the
    /// `stride * height` region into adjacent memory. Clamped-away columns (and the whole buffer
    /// when `pixels` is null) are left zeroed (black) rather than read out of bounds.
    ///
    /// # Safety
    /// When `pixels` is non-null it must address at least `stride * height` readable `u32`s (the
    /// frozen `FrameBuffer` contract). `w`, `h`, and `stride` are the packed frame dimensions.
    unsafe fn pack_frame(pixels: *const u32, w: usize, h: usize, stride: usize) -> Vec<u32> {
        let mut buf = vec![0u32; w * h];
        let row = w.min(stride);
        if !pixels.is_null() {
            // SAFETY: row `y` starts at `y * stride` and we read `row = min(w, stride) <= stride`
            // u32s, so the last index touched is `y * stride + row - 1 < (y + 1) * stride <=
            // stride * height` — always within the caller's guaranteed region. The destination
            // write of `row <= w` u32s stays within the `w`-wide row at `buf[y * w..]`.
            unsafe {
                for y in 0..h {
                    let src = pixels.add(y * stride);
                    std::ptr::copy_nonoverlapping(src, buf[y * w..].as_mut_ptr(), row);
                }
            }
        }
        buf
    }

    pub(super) fn present(pixels: *const u32, width: u32, height: u32, stride: u32) {
        if headless() {
            return;
        }
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        if w == 0 || h == 0 {
            return;
        }
        // SAFETY: the frozen `FrameBuffer` contract guarantees `pixels` (when non-null) addresses
        // at least `stride * height` readable `u32`s; `pack_frame` clamps each row read to
        // `stride`, so an over-wide `width > stride` frame can never read past that region.
        let buf = unsafe { pack_frame(pixels, w, h, stride) };
        with_state(|st| st.present(&buf, w, h));
    }

    pub(super) fn audio(samples: &[i16]) {
        if headless() || samples.is_empty() {
            return;
        }
        with_state(|st| st.push_audio(samples));
    }

    pub(super) fn poll() -> Event {
        if headless() {
            return NONE_EVENT;
        }
        with_state(|st| st.events.pop_front().unwrap_or(NONE_EVENT))
    }

    pub(super) fn size() -> u64 {
        if headless() {
            return super::pack_size(super::DEFAULT_W, super::DEFAULT_H);
        }
        with_state(|st| match st.window.as_ref() {
            // `get_size` is the live window size — it tracks user resizes, driving dynamic
            // resolution. Before the first `present` there's no window yet: report the default.
            Some(win) => {
                let (w, h) = win.get_size();
                super::pack_size(w as u32, h as u32)
            }
            None => super::pack_size(super::DEFAULT_W, super::DEFAULT_H),
        })
    }

    pub(super) fn tex(rgba: &[u8], w: u32, h: u32) -> u32 {
        if headless() {
            return 0;
        }
        with_state(|st| st.tex(rgba, w, h))
    }

    pub(super) fn frame(w: u32, h: u32, clear_argb: u32) {
        if headless() {
            return;
        }
        with_state(|st| st.frame(w, h, clear_argb));
    }

    pub(super) fn sprite(tex: u32, dx: i32, dy: i32) {
        if headless() {
            return;
        }
        with_state(|st| st.sprite(tex, dx, dy));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_sub(tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
        if headless() {
            return;
        }
        with_state(|st| st.sprite_sub(tex, sx, sy, sw, sh, dx, dy));
    }

    pub(super) fn rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
        if headless() {
            return;
        }
        with_state(|st| st.rect(dx, dy, w, h, argb));
    }

    pub(super) fn flush() {
        if headless() {
            return;
        }
        with_state(|st| st.flush());
    }

    pub(super) fn sound(pcm: &[u8], channels: u32, rate: u32) -> u32 {
        if headless() {
            return 0;
        }
        with_state(|st| st.sound(pcm, channels, rate))
    }

    pub(super) fn play(sound: u32, loop_: u32, vol_q8: u32) -> u32 {
        if headless() {
            return 0;
        }
        with_state(|st| st.play(sound, loop_, vol_q8))
    }

    pub(super) fn stop(voice: u32) {
        if headless() {
            return;
        }
        with_state(|st| st.stop(voice));
    }

    pub(super) fn mouse() -> u64 {
        if headless() {
            return 0;
        }
        with_state(|st| st.mouse())
    }

    pub(super) fn mouse_delta() -> u64 {
        if headless() {
            return 0;
        }
        with_state(|st| st.mouse_delta())
    }

    pub(super) fn tune(voice: u32, vol_q8: u32, pan_q8: u32) {
        if headless() {
            return;
        }
        with_state(|st| st.tune(voice, vol_q8, pan_q8));
    }

    pub(super) fn show(pixels: *const u32, width: u32, height: u32) {
        if headless() {
            return;
        }
        let (w, h) = (width as usize, height as usize);
        if pixels.is_null() || w == 0 || h == 0 {
            return;
        }
        // Copy the tightly packed source up front (stride == w), mirroring `present`, so the
        // raw-pointer read is confined to here.
        let mut buf = vec![0u32; w * h];
        // SAFETY: the caller guarantees `pixels` addresses `w * h` readable `u32`s (the
        // `bet_gg_show` contract).
        unsafe {
            std::ptr::copy_nonoverlapping(pixels, buf.as_mut_ptr(), w * h);
        }
        with_state(|st| st.show(&buf, w, h));
    }

    pub(super) fn audio_spec() -> u64 {
        if headless() {
            return super::pack_size(super::DEFAULT_AUDIO_RATE, super::DEFAULT_AUDIO_CHANNELS);
        }
        with_state(|st| st.audio_spec())
    }

    pub(super) fn pending() -> u64 {
        if headless() {
            return 0;
        }
        with_state(|st| st.pending())
    }

    pub(super) fn title(name: &str) {
        if headless() {
            return;
        }
        with_state(|st| st.set_title(name));
    }

    #[cfg(test)]
    mod tests {
        use super::pack_frame;

        // cwage #43: a `FrameBuffer` presenting with `width > stride` is malformed. `pack_frame`
        // must clamp the per-row read to `stride` so it never touches memory past the
        // `stride * height` region the source actually backs. We size the source at EXACTLY
        // `stride * height` u32s (no slack): the pre-fix code read `width` per row and ran off the
        // end, and would have filled the clamped-away columns with those out-of-bounds reads —
        // this asserts they are instead left zeroed.
        #[test]
        fn present_clamps_overwide_width_to_stride() {
            let (stride, h, w) = (2usize, 3usize, 5usize); // width (5) > stride (2)
            let src: Vec<u32> = (1..=(stride * h) as u32).collect(); // [1,2, 3,4, 5,6]
            let buf = unsafe { pack_frame(src.as_ptr(), w, h, stride) };

            assert_eq!(buf.len(), w * h);
            for y in 0..h {
                for x in 0..w {
                    let want = if x < stride { src[y * stride + x] } else { 0 };
                    assert_eq!(buf[y * w + x], want, "pixel ({x},{y})");
                }
            }
        }

        // A null source reads nothing and yields a zeroed frame of the requested size.
        #[test]
        fn present_null_source_is_zeroed() {
            let buf = unsafe { pack_frame(std::ptr::null(), 4, 2, 4) };
            assert_eq!(buf, vec![0u32; 8]);
        }

        // The normal tightly-packed case (width == stride) is copied verbatim.
        #[test]
        fn present_packs_tight_frame_verbatim() {
            let (w, h) = (3usize, 2usize);
            let src: Vec<u32> = (10..16).collect();
            let buf = unsafe { pack_frame(src.as_ptr(), w, h, w) };
            assert_eq!(buf, src);
        }
    }
}
