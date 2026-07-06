//! `rt-abi` — bet's runtime ABI: the `extern "C"` contract between generated code (and the
//! interpreter) and the runtime.
//!
//! This crate is the **header**: `#[repr(C)]` shared types plus the `unsafe extern "C"`
//! declarations for every entry point — allocation & allocator context, cribs (arenas),
//! `tag`/`holla` generational access, `slide` tasks, `yeet`/`sheesh` panic/recover, stdout,
//! and the **platform layer** (framebuffer / audio / input / timing, plan-amendment-01 §6.1).
//! The naive implementation lives in `rt-stub`; the real `runtime` replaces it without
//! changing a single signature. `spec/runtime-abi.md` is rationale; **this crate is
//! normative.**
//!
//! The platform-layer entry points are deliberately present from the start: they shape the
//! ABI the same way the allocation intrinsics do and cannot be retrofitted cheaply.

// This crate is inherently unsafe: it declares the FFI boundary. The lint stays at `warn`
// workspace-wide so unsafe is visible; here we opt in explicitly (see CLAUDE.md / the
// Step-0 plan's note on `unsafe_code`).
#![allow(unsafe_code)]

use core::ffi::c_void;

// ---------------------------------------------------------------------------
// Shared ABI types.
// ---------------------------------------------------------------------------

/// A generational handle into a typed crib: an 8-byte `(slot, generation)` pair (spec §7.3).
/// Plain, copyable data — storable anywhere, valid for any duration; validity is checked at
/// access time via [`bet_holla_check`].
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tag {
    pub slot: u32,
    pub generation: u32,
}

impl Tag {
    /// The sentinel returned when a `cop` cannot allocate; always resolves as ghosted.
    pub const NULL: Tag = Tag {
        slot: u32::MAX,
        generation: 0,
    };
}

/// An opaque handle to a crib (arena). Created by [`bet_crib_new`] / [`bet_crib_new_bump`].
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CribHandle(pub *mut c_void);

/// An opaque handle to a spawned task (`slide`).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskHandle(pub u64);

/// An opaque handle to a `stash` (hash map). Created by [`bet_map_new`]. Keys are arbitrary
/// byte sequences (a `str`'s bytes, an integer's little-endian bytes, ...) and values are
/// fixed-size blobs whose width is fixed at creation — the map is intentionally type-erased at
/// the ABI so one runtime implementation backs every `stash[K, V]` monomorphization.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapHandle(pub *mut c_void);

/// An opaque handle to a `vec` (growable array). Created by [`bet_vec_new`]. Elements are
/// fixed-size blobs whose width is fixed at creation — the vec is intentionally type-erased at
/// the ABI so one runtime implementation backs every `vec[T]` monomorphization (exactly like
/// [`MapHandle`]). The frontend copies each element to/from a stack slot at the call site.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VecHandle(pub *mut c_void);

/// An opaque handle to a seedable PRNG (`math.cook`). Created by [`bet_rng_new`]; its state is
/// an [`RngState`]. The generator itself lives in this crate (see [`RngState`]) so the runtime
/// and the interpreter share one implementation and produce byte-identical streams.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RngHandle(pub *mut c_void);

/// The canonical seedable PRNG backing `math.cook` — **xoshiro256\*\*** seeded by **SplitMix64**.
///
/// This is the *single source of truth* for the algorithm. Both runtimes (`rt-stub`, `runtime`)
/// and the tree-walking interpreter (`interp`) all depend on `rt-abi` and call these methods, so
/// a program's random stream is identical whether run via `bet run` or a compiled binary — which
/// the corpus differential-tests byte-for-byte. Uses only fixed-width `u64` wrapping ops, so it
/// is portable across targets. **Non-cryptographic**: reproducible pseudo-randomness, never a
/// CSPRNG.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RngState {
    s: [u64; 4],
}

impl RngState {
    /// Seed with a single `u64` (C#-`new Random(seed)` style). SplitMix64 expands the seed into
    /// the four 64-bit state words; an all-zero state (which xoshiro cannot escape) is nudged off.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut s = [0u64; 4];
        for slot in &mut s {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *slot = x ^ (x >> 31);
        }
        if s == [0, 0, 0, 0] {
            s[0] = 1;
        }
        RngState { s }
    }

    /// The raw next 64-bit draw (xoshiro256\*\*). Backs `rng.roll`.
    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// A uniform `f64` in `[0.0, 1.0)` from the top 53 bits — the BASIC `RND(-1)` analog. Backs
    /// `rng.frac`.
    pub fn frac(&mut self) -> f64 {
        // 2^-53 is exact in f64; the product is a uniform dyadic rational in [0, 1).
        (self.next_u64() >> 11) as f64 * (1.0 / ((1u64 << 53) as f64))
    }

    /// A uniform integer in `[0, n)`, unbiased via Lemire's multiply-high method. `n == 0` → 0.
    /// Backs `rng.upTo`.
    pub fn up_to(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let mut m = (self.next_u64() as u128).wrapping_mul(n as u128);
        let mut low = m as u64;
        if low < n {
            let threshold = n.wrapping_neg() % n; // 2^64 mod n
            while low < threshold {
                m = (self.next_u64() as u128).wrapping_mul(n as u128);
                low = m as u64;
            }
        }
        (m >> 64) as u64
    }
}

/// An opaque allocator-context handle (the Odin-style "current allocator"). The stub's
/// current allocator is always the system heap; the handle exists so the contract is stable.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AllocCtx(pub *mut c_void);

/// A framebuffer descriptor handed to [`bet_gg_present`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FrameBuffer {
    pub pixels: *mut u32,
    pub width: u32,
    pub height: u32,
    /// Row stride in pixels (>= `width`).
    pub stride: u32,
}

/// An input event from [`bet_gg_poll`]. `kind` is one of the [`event_kind`] constants.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub kind: u32,
    /// A key code (for key events) or button (for mouse events).
    pub code: u32,
    pub x: i32,
    pub y: i32,
}

/// [`Event::kind`] discriminants.
pub mod event_kind {
    pub const NONE: u32 = 0;
    pub const KEY_DOWN: u32 = 1;
    pub const KEY_UP: u32 = 2;
    pub const MOUSE_MOVE: u32 = 3;
    pub const QUIT: u32 = 4;
    /// A mouse button was pressed. [`Event::code`] is the button (`0` = left, `1` = right).
    pub const MOUSE_DOWN: u32 = 5;
    /// A mouse button was released. [`Event::code`] is the button (`0` = left, `1` = right).
    pub const MOUSE_UP: u32 = 6;
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

unsafe extern "C" {
    // --- lifecycle ---

    /// Initialize the runtime (allocator context, scratch arena, stdout). Call once at
    /// program start before any other entry point.
    pub fn bet_rt_init();
    /// Tear the runtime down at program end.
    pub fn bet_rt_shutdown();

    // --- allocator + allocator context ---

    /// Allocate `size` bytes at `align` from the current allocator context.
    pub fn bet_alloc(size: usize, align: usize) -> *mut u8;
    /// Allocate `size` **zero-initialized** bytes at `align`. Backs `mem.slab[T](n)`, whose
    /// `[]T` buffers must start zeroed to match the interpreter (`Value::Array` of `0`s).
    pub fn bet_alloc_zeroed(size: usize, align: usize) -> *mut u8;
    /// Free a block previously returned by [`bet_alloc`] with the same `size`/`align`.
    pub fn bet_free(ptr: *mut u8, size: usize, align: usize);
    /// Resize a block, preserving its contents up to `min(old_size, new_size)`.
    pub fn bet_realloc(ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8;
    /// The current allocator context.
    pub fn bet_ctx_current() -> AllocCtx;
    /// Push `ctx` as the current allocator context.
    pub fn bet_ctx_push(ctx: AllocCtx);
    /// Pop the current allocator context, restoring the previous one.
    pub fn bet_ctx_pop();

    // --- cribs (arenas) ---

    /// Create a typed crib: a slab of `capacity` fixed-size (`elem_size`/`elem_align`) slots.
    pub fn bet_crib_new(elem_size: usize, elem_align: usize, capacity: u32) -> CribHandle;
    /// Create an untyped bump crib with an initial `reserve` byte capacity.
    pub fn bet_crib_new_bump(reserve: usize) -> CribHandle;
    /// Destroy a crib and release its backing memory.
    pub fn bet_crib_free(crib: CribHandle);
    /// O(1) mass-free: mark every slot free and bump every slot's generation, so all
    /// outstanding tags into this crib safely become ghosted (spec §7.2).
    pub fn bet_evict(crib: CribHandle);
    /// Bump-allocate `size` bytes at `align` from an untyped bump crib.
    pub fn bet_bump_alloc(crib: CribHandle, size: usize, align: usize) -> *mut u8;
    /// The thread's built-in per-frame scratch crib (a bump arena).
    pub fn bet_scratch() -> CribHandle;
    /// Reset the scratch crib (auto-evict at frame end).
    pub fn bet_scratch_reset();

    // --- tags / holla (generational access) ---

    /// Reserve a slot in a typed crib and return its tag. Returns [`Tag::NULL`] if full.
    pub fn bet_cop(crib: CribHandle) -> Tag;
    /// Checked resolve: return a pointer to the slot's storage iff the slot is occupied and
    /// its generation matches `tag`, else null. This is the primitive a `holla` lowers to.
    pub fn bet_holla_check(crib: CribHandle, tag: Tag) -> *mut u8;
    /// Unchecked resolve: a raw pointer to a slot's storage with no generation check (backs
    /// `trust()` in release builds).
    pub fn bet_slot_ptr(crib: CribHandle, slot: u32) -> *mut u8;

    // --- strings ---

    /// Allocate `a_len + b_len` bytes, copy the two byte runs (`a` then `b`) into it, and return
    /// the buffer. The caller owns the result and computes its length as `a_len + b_len`. Backs
    /// `str` concatenation (`.tea`, `+` on strings).
    pub fn bet_str_concat(
        a_ptr: *const u8,
        a_len: usize,
        b_ptr: *const u8,
        b_len: usize,
    ) -> *mut u8;
    /// Allocate `len` bytes and copy an ASCII-uppercased copy of `[ptr, len)` into it, returning
    /// the buffer (the length is unchanged). Backs `str.glow`.
    pub fn bet_str_upper(ptr: *const u8, len: usize) -> *mut u8;
    /// Byte-equality of two strings. Backs `str.slaps`.
    pub fn bet_str_eq(a_ptr: *const u8, a_len: usize, b_ptr: *const u8, b_len: usize) -> bool;
    /// Whether `[ptr, len)` is well-formed UTF-8, as `1` (valid) or `0` (invalid). Backs the
    /// checked `str.fromBytes` (the unchecked `str.fromBytesTrust` skips this), which multiplies
    /// the length by this bit. Returns a `usize` (not `bool`) so the frontend can arithmetic on
    /// it directly. Pure — allocates nothing.
    pub fn bet_str_valid(ptr: *const u8, len: usize) -> usize;

    // --- filesystem ---

    /// Read the whole file at the `path_len`-byte path `path_ptr` into a freshly allocated
    /// buffer, writing its length to `out_len` and returning the buffer. Returns null (and
    /// leaves `out_len` = 0) on any error. The caller owns the buffer. Backs `fs.peep`.
    pub fn bet_fs_read(path_ptr: *const u8, path_len: usize, out_len: *mut usize) -> *mut u8;

    // --- process arguments ---

    /// Capture the process's command-line arguments. Called once from the synthesized `main`,
    /// with `argc`/`argv` straight from the C entry point, before `bet`'s `main` runs. The
    /// runtime takes an owned copy, so the pointers need not outlive the call. Backs the
    /// `sys.arg` / `sys.argc` intrinsics.
    pub fn bet_args_init(argc: i32, argv: *const *const u8);
    /// The number of captured command-line arguments (0 before [`bet_args_init`] runs).
    pub fn bet_arg_count() -> usize;
    /// A pointer to the `i`-th captured argument's bytes, writing its length to `out_len`.
    /// Returns null (and leaves `out_len` = 0) if `i` is out of range. The bytes live for the
    /// rest of the process; the caller must not free them. Backs `sys.arg`.
    pub fn bet_arg_get(i: usize, out_len: *mut usize) -> *const u8;

    // --- stdin ---

    /// Read one line from stdin into a freshly allocated buffer, writing its length to `out_len`
    /// and returning the buffer. The trailing newline (`\n`, and a preceding `\r`) is stripped.
    /// Returns null (and leaves `out_len` = 0) at end-of-input or on error, so a caller reading
    /// past EOF sees an empty string. The caller owns the buffer. Backs `sys.peep`.
    pub fn bet_read_line(out_len: *mut usize) -> *mut u8;

    // --- stash (hash maps) ---

    /// Create an empty hash map whose values are `val_size`-byte blobs. Keys are arbitrary
    /// byte sequences supplied per operation.
    pub fn bet_map_new(val_size: usize) -> MapHandle;
    /// Insert or overwrite the entry for the `key_len`-byte key at `key_ptr` with the
    /// `val_size` value bytes at `val_ptr` (`val_size` fixed at [`bet_map_new`]).
    pub fn bet_map_put(map: MapHandle, key_ptr: *const u8, key_len: usize, val_ptr: *const u8);
    /// Look up the `key_len`-byte key at `key_ptr`. On a hit, copy the value bytes into
    /// `out_val` and return `true`; on a miss leave `out_val` untouched and return `false`.
    pub fn bet_map_get(
        map: MapHandle,
        key_ptr: *const u8,
        key_len: usize,
        out_val: *mut u8,
    ) -> bool;
    /// Remove the entry for the given key, returning `true` if one was present.
    pub fn bet_map_del(map: MapHandle, key_ptr: *const u8, key_len: usize) -> bool;
    /// The number of entries in the map.
    pub fn bet_map_len(map: MapHandle) -> usize;
    /// Destroy a map and release its backing memory.
    pub fn bet_map_free(map: MapHandle);

    // --- vec (growable array) ---
    /// Create an empty growable array whose elements are `elem_size`-byte blobs.
    pub fn bet_vec_new(elem_size: usize) -> VecHandle;
    /// Append a copy of the `elem_size` bytes at `elem_ptr` to the end of the vec.
    pub fn bet_vec_push(vec: VecHandle, elem_ptr: *const u8);
    /// Remove the last element. On success copy its bytes into `out_elem` and return `true`; on
    /// an empty vec leave `out_elem` untouched and return `false`.
    pub fn bet_vec_pop(vec: VecHandle, out_elem: *mut u8) -> bool;
    /// Copy element `idx` into `out_elem`, returning `true`; out-of-range returns `false` and
    /// leaves `out_elem` untouched.
    pub fn bet_vec_get(vec: VecHandle, idx: usize, out_elem: *mut u8) -> bool;
    /// Overwrite element `idx` with the bytes at `elem_ptr`, returning `true`; out-of-range
    /// returns `false` and writes nothing.
    pub fn bet_vec_set(vec: VecHandle, idx: usize, elem_ptr: *const u8) -> bool;
    /// The number of elements in the vec.
    pub fn bet_vec_len(vec: VecHandle) -> usize;
    /// A read pointer to the vec's contiguous backing buffer (`bet_vec_len(vec) * elem_size`
    /// bytes). Valid only until the next mutation — a `push`/`extend` may reallocate and
    /// invalidate it — so callers copy out immediately. Backs `vec[u8].str()` (the string-builder
    /// finalize), which reads it and copies via [`bet_str_concat`].
    pub fn bet_vec_data(vec: VecHandle) -> *const u8;
    /// Append `len` raw bytes at `ptr` to the end of the vec. Assumes a byte-wide element
    /// (`elem_size == 1`); the frontend restricts it to `vec[u8]`. A bulk `push` for the
    /// string-builder pattern (`vec[u8].append(str)`).
    pub fn bet_vec_extend(vec: VecHandle, ptr: *const u8, len: usize);
    /// Destroy a vec and release its backing memory.
    pub fn bet_vec_free(vec: VecHandle);

    // --- rng (seedable prng: math.cook) ---

    /// Create a PRNG seeded with `seed` (xoshiro256\*\*/SplitMix64; see [`RngState`]). Backs
    /// `math.cook`.
    pub fn bet_rng_new(seed: u64) -> RngHandle;
    /// The raw next 64-bit draw. Backs `rng.roll`.
    pub fn bet_rng_next(rng: RngHandle) -> u64;
    /// A uniform `f64` in `[0.0, 1.0)`. Backs `rng.frac`.
    pub fn bet_rng_frac(rng: RngHandle) -> f64;
    /// A uniform integer in `[0, n)` (unbiased). Backs `rng.upTo`.
    pub fn bet_rng_upto(rng: RngHandle, n: u64) -> u64;
    /// Destroy a PRNG and release its state.
    pub fn bet_rng_free(rng: RngHandle);

    // --- concurrency (slide) ---

    /// Spawn a task running `entry(arg)` and return its handle.
    pub fn bet_slide(entry: extern "C" fn(*mut u8), arg: *mut u8) -> TaskHandle;
    /// Cooperatively yield the current task.
    pub fn bet_yield();
    /// Wait for a task to finish.
    pub fn bet_task_join(task: TaskHandle);

    // --- panic / recover (yeet / sheesh) ---

    /// Panic with a message (`yeet`). Does not return.
    pub fn bet_panic(msg: *const u8, len: usize) -> !;
    /// Begin a recovery boundary (`sheesh`); returns a token for [`bet_recover_end`].
    pub fn bet_recover_begin() -> *mut c_void;
    /// End a recovery boundary opened by [`bet_recover_begin`].
    pub fn bet_recover_end(token: *mut c_void);

    // --- stdout (what spill lowers to) ---

    /// Write `len` bytes at `ptr` to stdout.
    pub fn bet_print(ptr: *const u8, len: usize);
    /// Write the signed decimal representation of `v` to stdout, with **no** trailing
    /// newline. The caller (frontend lowering) appends any newline.
    pub fn bet_print_i64(v: i64);
    /// Write the unsigned decimal representation of `v` to stdout, with **no** trailing
    /// newline. The caller appends any newline.
    pub fn bet_print_u64(v: u64);
    /// Write `v` to stdout, with **no** trailing newline, formatted to match the
    /// interpreter's `display_float`: a finite integral value prints with a single trailing
    /// `.0` (e.g. `3.0`), everything else uses the default `f64` formatting (e.g. `2.5`).
    pub fn bet_print_f64(v: f64);

    // --- platform layer (headless in rt-stub; amendment §2.6/§6.1) ---

    /// Present a framebuffer to the display.
    pub fn bet_gg_present(fb: *const FrameBuffer);
    /// Submit `count` interleaved `i16` audio *samples* (a stereo frame is two samples) onto the
    /// output ring for playback.
    pub fn bet_gg_audio(frames: *const i16, count: usize);
    /// Poll the next input event into `out`; returns `false` when the queue is empty.
    pub fn bet_gg_poll(out: *mut Event) -> bool;
    /// A monotonic high-resolution timer, in nanoseconds.
    pub fn bet_gg_ticks() -> u64;
    /// The current window size, packed as `width << 32 | height`. Tracks live resizes, so a
    /// program can size its framebuffer to the window for true dynamic resolution; returns a
    /// fixed default before the window exists / in a headless build. (The 5th `gg` entry point —
    /// amends amendment-02 SP0.4, which reserved exactly four.)
    pub fn bet_gg_size() -> u64;

    // --- gg compositor / mixer / mouse (amendment §SP0.4 raise; headless no-ops in rt-stub) ---

    /// Upload an RGBA8 texture (`w * h` pixels, 4 bytes each in `R,G,B,A` order) to the compositor
    /// and return its 1-based id (`0` = invalid). The pixels are premultiplied and cached, so the
    /// caller's buffer need not outlive the call. Backs `gg.tex`.
    pub fn bet_gg_tex(rgba: *const u8, w: u32, h: u32) -> u32;
    /// Begin a frame: (re)size the logical canvas to `w * h` and clear it to `clear_argb` (the low
    /// 24 bits `0x00RR_GGBB`; the canvas itself is opaque). Backs `gg.frame`.
    pub fn bet_gg_frame(w: u32, h: u32, clear_argb: u32);
    /// Blit texture `tex` (from [`bet_gg_tex`]) onto the canvas at `(dx, dy)` with premultiplied
    /// src-over alpha, clipped to the canvas. A `0`/unknown id is ignored. Backs `gg.sprite`.
    pub fn bet_gg_sprite(tex: u32, dx: i32, dy: i32);
    /// Blit the source sub-rectangle `(sx, sy, sw, sh)` of texture `tex` onto the canvas at
    /// `(dx, dy)` with premultiplied src-over alpha — the same blit math as [`bet_gg_sprite`],
    /// windowed to the source rect and clipped to both the texture bounds and the canvas. A
    /// `0`/unknown id is ignored. The glyph-blit primitive behind bitmap text. Backs `gg.spriteSub`.
    pub fn bet_gg_sprite_sub(tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32);
    /// Fill a `w * h` rectangle at `(dx, dy)` with `argb` (`0xAARR_GGBB`) using src-over alpha,
    /// clipped to the canvas. Backs `gg.rect`.
    pub fn bet_gg_rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32);
    /// Present the composited canvas — aspect-fit (letterboxed) into the window — and pump input.
    /// Backs `gg.flush`.
    pub fn bet_gg_flush();
    /// Register a PCM sound (`byte_len` bytes of interleaved little-endian `i16` samples,
    /// `channels` channels at `rate` Hz) with the mixer and return its 1-based id (`0` = invalid).
    /// The samples are copied, so the caller's buffer need not outlive the call. Backs `gg.sound`.
    pub fn bet_gg_sound(pcm: *const u8, byte_len: usize, channels: u32, rate: u32) -> u32;
    /// Start playing sound `sound` on a mixer voice at volume `vol_q8` (Q8 fixed point: `256` =
    /// unity gain), looping if `loop_ != 0`, and return the 1-based voice id (`0` = could not
    /// play). Backs `gg.play`.
    pub fn bet_gg_play(sound: u32, loop_: u32, vol_q8: u32) -> u32;
    /// Stop the voice `voice` (from [`bet_gg_play`]); an unknown or already-finished id is
    /// ignored. Backs `gg.stop`.
    pub fn bet_gg_stop(voice: u32);
    /// The current mouse position in logical-canvas coordinates, packed as
    /// `(x as u32) << 32 | (y as u32)` (clamped into the canvas). Backs `gg.mouse`.
    pub fn bet_gg_mouse() -> u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_eight_bytes() {
        // The 8-byte layout is load-bearing (spec §7.3) and shared with `midir`'s tag type.
        assert_eq!(core::mem::size_of::<Tag>(), 8);
        assert_eq!(core::mem::align_of::<Tag>(), 4);
    }

    #[test]
    fn handles_are_pointer_sized() {
        assert_eq!(
            core::mem::size_of::<CribHandle>(),
            core::mem::size_of::<*mut c_void>()
        );
    }

    #[test]
    fn rng_is_deterministic_for_a_seed() {
        // Same seed → identical stream. This is the property the corpus differential relies on.
        let mut a = RngState::new(1847);
        let mut b = RngState::new(1847);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn rng_seeds_diverge() {
        let mut a = RngState::new(1);
        let mut b = RngState::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn rng_known_values_do_not_drift() {
        // A regression guard: the exact first draws for seed 0. If the algorithm/constants ever
        // change, this fails loudly rather than silently altering every seeded program's output.
        let mut r = RngState::new(0);
        let got = [r.next_u64(), r.next_u64(), r.next_u64()];
        assert_eq!(
            got,
            [
                0x99EC_5F36_CB75_F2B4,
                0xBF6E_1F78_4956_452A,
                0x1A5F_849D_4933_E6E0
            ]
        );
    }

    #[test]
    fn rng_frac_is_in_unit_interval() {
        let mut r = RngState::new(42);
        for _ in 0..10_000 {
            let f = r.frac();
            assert!((0.0..1.0).contains(&f), "frac out of range: {f}");
        }
    }

    #[test]
    fn rng_up_to_is_in_range_and_handles_zero() {
        let mut r = RngState::new(7);
        assert_eq!(r.up_to(0), 0);
        assert_eq!(r.up_to(1), 0);
        for _ in 0..10_000 {
            assert!(r.up_to(6) < 6);
        }
    }
}
