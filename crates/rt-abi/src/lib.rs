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
    /// Submit `count` interleaved stereo audio frames.
    pub fn bet_gg_audio(frames: *const i16, count: usize);
    /// Poll the next input event into `out`; returns `false` when the queue is empty.
    pub fn bet_gg_poll(out: *mut Event) -> bool;
    /// A monotonic high-resolution timer, in nanoseconds.
    pub fn bet_gg_ticks() -> u64;
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
}
