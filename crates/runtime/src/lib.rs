//! `runtime` — bet's real runtime (the differentiator): crib arenas, slot generations,
//! per-frame scratch, allocator context, the `slide` scheduler, and the per-OS syscall
//! layer. Implements every `rt-abi` entry point for real; it is a **drop-in replacement**
//! for `rt-stub` — the `extern "C"` signatures are byte-for-byte identical, so swapping the
//! bootstrap `librt_stub.a` for `libruntime.a` is a pure link-line change.
//!
//! ## What is "real" here (vs the naive `rt-stub`)
//!
//! * **Bump cribs are growable, capacity-retaining arenas.** A bump crib is a list of
//!   pointer-stable chunks; when the active chunk fills we advance to (or append) another
//!   chunk instead of aborting the way the stub does. `evict` rewinds to the first chunk in
//!   O(1) *without freeing the chunks*, so the second frame's allocations reuse the memory
//!   the first frame grew into — the whole point of an arena.
//! * **Typed cribs use a free list** (`cop` is O(1) instead of the stub's O(n) linear scan)
//!   backed by **per-slot generation counters**. `evict` bumps every slot's generation, so a
//!   stale [`Tag`] naming a reused slot fails its `holla` check and resolves as *ghosted*
//!   rather than reading the new occupant (spec §7.2–§7.4). This is the load-bearing
//!   generational-handle correctness property.
//! * **Scratch** is the same growable bump arena, per-thread, reset (not freed) at frame end.
//! * **Allocator context** is a real thread-local stack; **recover boundaries** (`sheesh`)
//!   are tracked in a real thread-local LIFO instead of being pure no-ops.
//!
//! ## What is still minimal (headless), matching `rt-stub`
//!
//! The `slide` scheduler maps tasks onto OS threads (no M:N runtime yet) and the `gg`
//! platform layer is headless (Null-RHI: `present`/`audio` are no-ops, `poll` returns no
//! events, `ticks` is a real monotonic clock). Those are the two pieces the Step-3 brief
//! explicitly allows to stay minimal; they are the next real-runtime milestones.

// The whole crate is inherently unsafe: it defines the FFI boundary, dereferences raw crib
// handles, and hands out raw slab pointers. The `unsafe_code` lint stays at `warn`
// workspace-wide for visibility; opt in explicitly (see CLAUDE.md).
#![allow(unsafe_code)]
// Every entry point is an `unsafe extern "C"` trampoline; safety is documented at the ABI
// level in `rt-abi`, not repeated per-function here.
#![allow(clippy::missing_safety_doc)]

use core::ffi::c_void;
use std::alloc::{self, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
// `Instant` backs only the headless `bet_gg_ticks`; the desktop build gets its clock from
// `gg-backend`, so gate the import to avoid an unused-import warning under `gg-desktop`.
#[cfg(not(feature = "gg-desktop"))]
use std::time::Instant;

/// The base name of this crate's static library (`libruntime.a` → `-lruntime`). The driver
/// uses it to construct the link line for compiled `bet` programs; it is the drop-in swap
/// for [`rt-stub`'s `rt_stub`](../rt_stub/fn.staticlib_link_name.html). Keeping the name
/// next to the crate that produces the archive ties the two together.
pub fn staticlib_link_name() -> &'static str {
    "runtime"
}

// Re-export the shared ABI types so downstream (and tests) can name them via `runtime`.
// Only the *types* are re-exported; the entry-point declarations in `rt-abi` are not (they
// would collide with the definitions below).
pub use rt_abi::{
    AllocCtx, CribHandle, Event, FrameBuffer, MapHandle, RngHandle, RngState, Tag, TaskHandle,
    VecHandle, event_kind,
};

// ---------------------------------------------------------------------------
// Low-level allocation helpers.
// ---------------------------------------------------------------------------

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size.max(1), align.max(1)).expect("invalid (size, align)")
}

/// Round `addr` up to the next multiple of `align` (a power of two, as the ABI guarantees).
#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    let align = align.max(1);
    (addr + align - 1) & !(align - 1)
}

// ---------------------------------------------------------------------------
// Allocator + allocator-context stack.
// ---------------------------------------------------------------------------
//
// The current allocator *context* is a real thread-local stack (Odin-style). `bet_alloc`
// et al. still route to the system heap regardless of the top context: routing allocation
// through a context-selected arena is a deliberate follow-up (it needs the context handle to
// name a real allocator, and the ABI's `AllocCtx` is intentionally opaque for now). Keeping
// the default on the heap preserves drop-in parity with `rt-stub`.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_alloc(size: usize, align: usize) -> *mut u8 {
    unsafe { alloc::alloc(layout(size, align)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_alloc_zeroed(size: usize, align: usize) -> *mut u8 {
    unsafe { alloc::alloc_zeroed(layout(size, align)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_free(ptr: *mut u8, size: usize, align: usize) {
    if !ptr.is_null() {
        unsafe { alloc::dealloc(ptr, layout(size, align)) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_realloc(
    ptr: *mut u8,
    old_size: usize,
    new_size: usize,
    align: usize,
) -> *mut u8 {
    if ptr.is_null() {
        return unsafe { bet_alloc(new_size, align) };
    }
    unsafe { alloc::realloc(ptr, layout(old_size, align), new_size.max(1)) }
}

thread_local! {
    /// The allocator-context stack. Empty => the default (system heap), reported as a null
    /// `AllocCtx`.
    static CTX_STACK: RefCell<Vec<AllocCtx>> = const { RefCell::new(Vec::new()) };
    /// The per-thread scratch crib, created lazily on first `bet_scratch`.
    static SCRATCH: RefCell<Option<CribHandle>> = const { RefCell::new(None) };
    /// The active `sheesh` recovery boundaries, innermost last. Each entry is the token id
    /// handed back by `bet_recover_begin`.
    static RECOVER: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_ctx_current() -> AllocCtx {
    CTX_STACK.with(|s| {
        s.borrow()
            .last()
            .copied()
            .unwrap_or(AllocCtx(std::ptr::null_mut()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_ctx_push(ctx: AllocCtx) {
    CTX_STACK.with(|s| s.borrow_mut().push(ctx));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_ctx_pop() {
    CTX_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

// ---------------------------------------------------------------------------
// Cribs.
// ---------------------------------------------------------------------------

struct Crib {
    kind: CribKind,
}

enum CribKind {
    Typed(TypedCrib),
    Bump(BumpCrib),
}

/// A typed slab: `capacity` fixed-size slots, each with a generation counter, plus a free
/// list so `cop` is O(1). The ABI has no per-object free — slots are reclaimed only en masse
/// by `evict` — but the free list is the honest slab structure and is ready for a future
/// `uncop`.
struct TypedCrib {
    elem_size: usize,
    elem_align: usize,
    base: *mut u8,
    slots: Vec<SlotMeta>,
    /// Indices of currently-free slots (used as a LIFO stack).
    free: Vec<u32>,
}

#[derive(Clone, Copy)]
struct SlotMeta {
    occupied: bool,
    generation: u32,
}

/// A single pointer-stable region of a bump crib.
struct Chunk {
    base: *mut u8,
    cap: usize,
    /// The alignment this chunk was allocated with (needed to free it).
    align: usize,
}

/// A growable, capacity-retaining bump arena: a list of pointer-stable [`Chunk`]s. Allocation
/// bumps within the active chunk and advances to (or appends) the next one on overflow, so
/// existing pointers never move. `evict` rewinds to the first chunk in O(1) and keeps every
/// chunk, so subsequent frames reuse the grown capacity.
struct BumpCrib {
    chunks: Vec<Chunk>,
    /// Index of the active chunk within `chunks`.
    current: usize,
    /// Bump offset within the active chunk (bytes from `chunks[current].base`).
    offset: usize,
    /// Size hint for the next freshly-allocated chunk (grows geometrically).
    next_reserve: usize,
}

impl BumpCrib {
    fn new(reserve: usize) -> BumpCrib {
        let cap = reserve.max(16);
        let align = 16usize;
        let base = unsafe { alloc::alloc(layout(cap, align)) };
        BumpCrib {
            chunks: vec![Chunk { base, cap, align }],
            current: 0,
            offset: 0,
            next_reserve: cap,
        }
    }

    /// Bump-allocate `size` bytes at `align`, growing the arena (never moving existing
    /// pointers) if the active chunk is full.
    fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        let align = align.max(1);
        loop {
            let chunk = &self.chunks[self.current];
            let start = chunk.base as usize + self.offset;
            let aligned = align_up(start, align);
            if aligned + size <= chunk.base as usize + chunk.cap {
                self.offset = aligned + size - chunk.base as usize;
                return aligned as *mut u8;
            }

            // The active chunk is full. Reuse the next retained chunk if it can hold the
            // request; otherwise splice in a fresh, big-enough chunk right after it.
            let reuse = self.chunks.get(self.current + 1).is_some_and(|next| {
                align_up(next.base as usize, align) + size <= next.base as usize + next.cap
            });
            if !reuse {
                let want = self.next_reserve.max(size.saturating_add(align));
                let chunk_align = align.max(16);
                let base = unsafe { alloc::alloc(layout(want, chunk_align)) };
                self.chunks.insert(
                    self.current + 1,
                    Chunk {
                        base,
                        cap: want,
                        align: chunk_align,
                    },
                );
                self.next_reserve = self.next_reserve.saturating_mul(2).max(want);
            }
            self.current += 1;
            self.offset = 0;
        }
    }

    /// O(1) rewind: reset to the first chunk, keeping every chunk's memory for reuse.
    fn reset(&mut self) {
        self.current = 0;
        self.offset = 0;
    }

    /// Free every chunk's backing memory.
    fn destroy(&mut self) {
        for c in self.chunks.drain(..) {
            if !c.base.is_null() {
                unsafe { alloc::dealloc(c.base, layout(c.cap, c.align)) };
            }
        }
    }
}

/// Reborrow a crib handle as `*mut Crib`. Caller guarantees the handle is live and that the
/// crib is not being touched from another thread (a crib is single-threaded, spec §8 open Q).
unsafe fn crib<'a>(h: CribHandle) -> &'a mut Crib {
    unsafe { &mut *(h.0 as *mut Crib) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_crib_new(
    elem_size: usize,
    elem_align: usize,
    capacity: u32,
) -> CribHandle {
    let cap = capacity as usize;
    let total = elem_size.saturating_mul(cap);
    let base = if total == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { alloc::alloc(layout(total, elem_align.max(1))) }
    };
    let slots = vec![
        SlotMeta {
            occupied: false,
            generation: 0,
        };
        cap
    ];
    // Free list holds every slot initially. Popping from the back hands out the highest
    // index first; no test relies on a particular order, only that init and `evict` rebuild
    // the list identically so a reused slot matches the one first handed out.
    let free: Vec<u32> = (0..capacity).collect();
    let boxed = Box::new(Crib {
        kind: CribKind::Typed(TypedCrib {
            elem_size,
            elem_align: elem_align.max(1),
            base,
            slots,
            free,
        }),
    });
    CribHandle(Box::into_raw(boxed) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_crib_new_bump(reserve: usize) -> CribHandle {
    let boxed = Box::new(Crib {
        kind: CribKind::Bump(BumpCrib::new(reserve)),
    });
    CribHandle(Box::into_raw(boxed) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_crib_free(handle: CribHandle) {
    if handle.0.is_null() {
        return;
    }
    let mut boxed = unsafe { Box::from_raw(handle.0 as *mut Crib) };
    match &mut boxed.kind {
        CribKind::Typed(t) => {
            let total = t.elem_size.saturating_mul(t.slots.len());
            if !t.base.is_null() && total > 0 {
                unsafe { alloc::dealloc(t.base, layout(total, t.elem_align)) };
            }
        }
        CribKind::Bump(b) => b.destroy(),
    }
    // `boxed` drops here, releasing the `Crib` control block.
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_evict(handle: CribHandle) {
    let c = unsafe { crib(handle) };
    match &mut c.kind {
        // The load-bearing operation: free every slot AND bump its generation, so every
        // outstanding tag into this crib becomes ghosted. The free list is rebuilt so the
        // slab hands slots out again from a known state.
        CribKind::Typed(t) => {
            t.free.clear();
            for (i, m) in t.slots.iter_mut().enumerate() {
                m.occupied = false;
                m.generation = m.generation.wrapping_add(1);
                t.free.push(i as u32);
            }
        }
        // O(1) rewind; keeps chunks so the next frame reuses the grown capacity.
        CribKind::Bump(b) => b.reset(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_evict_slot(handle: CribHandle, tag: Tag) {
    let c = unsafe { crib(handle) };
    // Per-slot free is a typed-crib operation; a bump crib has no slots to free one of.
    let CribKind::Typed(t) = &mut c.kind else {
        return;
    };
    // Only a LIVE tag frees its slot: occupied and generation-matching (the same validity
    // rule as `bet_holla_check`). A stale/null/out-of-range tag is a no-op, so double-evict
    // and evict-after-mass-evict are safe. Bumping the generation ghosts every outstanding
    // copy of the tag; pushing the index returns the slot to the free list, so the very
    // next `cop` reuses it (LIFO) at the new generation.
    if let Some(m) = t.slots.get_mut(tag.slot as usize)
        && m.occupied
        && m.generation == tag.generation
    {
        m.occupied = false;
        m.generation = m.generation.wrapping_add(1);
        t.free.push(tag.slot);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_bump_alloc(handle: CribHandle, size: usize, align: usize) -> *mut u8 {
    let c = unsafe { crib(handle) };
    match &mut c.kind {
        CribKind::Bump(b) => b.alloc(size, align),
        CribKind::Typed(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_scratch() -> CribHandle {
    SCRATCH.with(|s| {
        let mut slot = s.borrow_mut();
        if slot.is_none() {
            *slot = Some(unsafe { bet_crib_new_bump(64 * 1024) });
        }
        slot.unwrap()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_scratch_reset() {
    SCRATCH.with(|s| {
        if let Some(h) = *s.borrow() {
            unsafe { bet_evict(h) };
        }
    });
}

// ---------------------------------------------------------------------------
// Tags / holla (generational access).
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_cop(handle: CribHandle) -> Tag {
    let c = unsafe { crib(handle) };
    let CribKind::Typed(t) = &mut c.kind else {
        return Tag::NULL;
    };
    match t.free.pop() {
        Some(slot) => {
            let m = &mut t.slots[slot as usize];
            m.occupied = true;
            Tag {
                slot,
                generation: m.generation,
            }
        }
        None => Tag::NULL,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_holla_check(handle: CribHandle, tag: Tag) -> *mut u8 {
    let c = unsafe { crib(handle) };
    let CribKind::Typed(t) = &c.kind else {
        return std::ptr::null_mut();
    };
    let slot = tag.slot as usize;
    match t.slots.get(slot) {
        // Occupied AND the generation matches: a live handle. Anything else — a freed slot,
        // a stale generation after `evict`, or an out-of-range slot — is ghosted (null).
        Some(m) if m.occupied && m.generation == tag.generation && !t.base.is_null() => unsafe {
            t.base.add(slot * t.elem_size)
        },
        _ => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_slot_ptr(handle: CribHandle, slot: u32) -> *mut u8 {
    let c = unsafe { crib(handle) };
    let CribKind::Typed(t) = &c.kind else {
        return std::ptr::null_mut();
    };
    let slot = slot as usize;
    if slot < t.slots.len() && !t.base.is_null() {
        unsafe { t.base.add(slot * t.elem_size) }
    } else {
        std::ptr::null_mut()
    }
}

// ---------------------------------------------------------------------------
// Strings.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_str_concat(
    a_ptr: *const u8,
    a_len: usize,
    b_ptr: *const u8,
    b_len: usize,
) -> *mut u8 {
    let mut buf: Vec<u8> = Vec::with_capacity(a_len + b_len);
    unsafe {
        buf.extend_from_slice(std::slice::from_raw_parts(a_ptr, a_len));
        buf.extend_from_slice(std::slice::from_raw_parts(b_ptr, b_len));
    }
    Box::into_raw(buf.into_boxed_slice()) as *mut u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_str_upper(ptr: *const u8, len: usize) -> *mut u8 {
    let src = unsafe { std::slice::from_raw_parts(ptr, len) };
    let buf: Vec<u8> = src.iter().map(|b| b.to_ascii_uppercase()).collect();
    Box::into_raw(buf.into_boxed_slice()) as *mut u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_str_eq(
    a_ptr: *const u8,
    a_len: usize,
    b_ptr: *const u8,
    b_len: usize,
) -> bool {
    let a = unsafe { std::slice::from_raw_parts(a_ptr, a_len) };
    let b = unsafe { std::slice::from_raw_parts(b_ptr, b_len) };
    a == b
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_str_valid(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    usize::from(std::str::from_utf8(bytes).is_ok())
}

// ---------------------------------------------------------------------------
// Filesystem.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_fs_read(
    path_ptr: *const u8,
    path_len: usize,
    out_len: *mut usize,
) -> *mut u8 {
    let path_bytes = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    let Ok(path) = std::str::from_utf8(path_bytes) else {
        unsafe { *out_len = 0 };
        return std::ptr::null_mut();
    };
    match std::fs::read(path) {
        Ok(bytes) => {
            unsafe { *out_len = bytes.len() };
            Box::into_raw(bytes.into_boxed_slice()) as *mut u8
        }
        Err(_) => {
            unsafe { *out_len = 0 };
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_fs_write(
    path_ptr: *const u8,
    path_len: usize,
    data_ptr: *const u8,
    data_len: usize,
) -> bool {
    let path_bytes = unsafe { std::slice::from_raw_parts(path_ptr, path_len) };
    let Ok(path) = std::str::from_utf8(path_bytes) else {
        return false;
    };
    let data: &[u8] = if data_len == 0 {
        // A zero-length write may arrive with a null data pointer (an empty `[]u8`).
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
    };
    std::fs::write(path, data).is_ok()
}

// ---------------------------------------------------------------------------
// Process arguments. Captured once from the synthesized `main`; `sys.arg`/`sys.argc` read them.
// ---------------------------------------------------------------------------

static ARGS: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_args_init(argc: i32, argv: *const *const u8) {
    let mut args = Vec::new();
    if !argv.is_null() && argc > 0 {
        for i in 0..argc as isize {
            let p = unsafe { *argv.offset(i) };
            if p.is_null() {
                args.push(Vec::new());
            } else {
                args.push(
                    unsafe { std::ffi::CStr::from_ptr(p.cast()) }
                        .to_bytes()
                        .to_vec(),
                );
            }
        }
    }
    // `set` fails only on a second init; the first-captured args win, which is what we want.
    let _ = ARGS.set(args);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_arg_count() -> usize {
    ARGS.get().map_or(0, Vec::len)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_arg_get(i: usize, out_len: *mut usize) -> *const u8 {
    match ARGS.get().and_then(|a| a.get(i)) {
        Some(bytes) => {
            unsafe { *out_len = bytes.len() };
            bytes.as_ptr()
        }
        None => {
            unsafe { *out_len = 0 };
            std::ptr::null()
        }
    }
}

// ---------------------------------------------------------------------------
// Stdin (line reader). Backs `sys.peep`.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_read_line(out_len: *mut usize) -> *mut u8 {
    use std::io::BufRead as _;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            unsafe { *out_len = 0 };
            std::ptr::null_mut()
        }
        Ok(_) => {
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            let bytes = line.into_bytes().into_boxed_slice();
            unsafe { *out_len = bytes.len() };
            Box::into_raw(bytes) as *mut u8
        }
    }
}

// ---------------------------------------------------------------------------
// Stash (hash maps). A type-erased byte-key/blob-value map; one implementation backs every
// `stash[K, V]` (the frontend serializes keys/values to bytes per operation).
// ---------------------------------------------------------------------------

struct StashMap {
    val_size: usize,
    entries: HashMap<Vec<u8>, Vec<u8>>,
}

unsafe fn stash_ref<'a>(map: MapHandle) -> &'a mut StashMap {
    unsafe { &mut *(map.0 as *mut StashMap) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_new(val_size: usize) -> MapHandle {
    let m = Box::new(StashMap {
        val_size,
        entries: HashMap::new(),
    });
    MapHandle(Box::into_raw(m) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_put(
    map: MapHandle,
    key_ptr: *const u8,
    key_len: usize,
    val_ptr: *const u8,
) {
    let m = unsafe { stash_ref(map) };
    let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) }.to_vec();
    let val = unsafe { std::slice::from_raw_parts(val_ptr, m.val_size) }.to_vec();
    m.entries.insert(key, val);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_get(
    map: MapHandle,
    key_ptr: *const u8,
    key_len: usize,
    out_val: *mut u8,
) -> bool {
    let m = unsafe { stash_ref(map) };
    let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
    match m.entries.get(key) {
        Some(v) => {
            unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out_val, m.val_size) };
            true
        }
        None => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_del(map: MapHandle, key_ptr: *const u8, key_len: usize) -> bool {
    let m = unsafe { stash_ref(map) };
    let key = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
    m.entries.remove(key).is_some()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_len(map: MapHandle) -> usize {
    unsafe { stash_ref(map) }.entries.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_map_free(map: MapHandle) {
    if !map.0.is_null() {
        drop(unsafe { Box::from_raw(map.0 as *mut StashMap) });
    }
}

// ---------------------------------------------------------------------------
// Vec (growable array). Type-erased flat byte buffer of fixed-width elements; one
// implementation backs every `vec[T]` (matches rt-stub, same semantics).
// ---------------------------------------------------------------------------

struct DynVec {
    elem_size: usize,
    data: Vec<u8>,
}

impl DynVec {
    fn len(&self) -> usize {
        // `checked_div` yields `None` for a zero-size element type, which we treat as length 0.
        self.data.len().checked_div(self.elem_size).unwrap_or(0)
    }
}

unsafe fn vec_ref<'a>(vec: VecHandle) -> &'a mut DynVec {
    unsafe { &mut *(vec.0 as *mut DynVec) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_new(elem_size: usize) -> VecHandle {
    let v = Box::new(DynVec {
        elem_size,
        data: Vec::new(),
    });
    VecHandle(Box::into_raw(v) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_push(vec: VecHandle, elem_ptr: *const u8) {
    let v = unsafe { vec_ref(vec) };
    let bytes = unsafe { core::slice::from_raw_parts(elem_ptr, v.elem_size) };
    v.data.extend_from_slice(bytes);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_pop(vec: VecHandle, out_elem: *mut u8) -> bool {
    let v = unsafe { vec_ref(vec) };
    let n = v.len();
    if n == 0 {
        return false;
    }
    let start = (n - 1) * v.elem_size;
    unsafe { core::ptr::copy_nonoverlapping(v.data[start..].as_ptr(), out_elem, v.elem_size) };
    v.data.truncate(start);
    true
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_get(vec: VecHandle, idx: usize, out_elem: *mut u8) -> bool {
    let v = unsafe { vec_ref(vec) };
    if idx >= v.len() {
        return false;
    }
    let start = idx * v.elem_size;
    unsafe { core::ptr::copy_nonoverlapping(v.data[start..].as_ptr(), out_elem, v.elem_size) };
    true
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_set(vec: VecHandle, idx: usize, elem_ptr: *const u8) -> bool {
    let v = unsafe { vec_ref(vec) };
    if idx >= v.len() {
        return false;
    }
    let start = idx * v.elem_size;
    let bytes = unsafe { core::slice::from_raw_parts(elem_ptr, v.elem_size) };
    v.data[start..start + v.elem_size].copy_from_slice(bytes);
    true
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_len(vec: VecHandle) -> usize {
    unsafe { vec_ref(vec) }.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_data(vec: VecHandle) -> *const u8 {
    unsafe { vec_ref(vec) }.data.as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_extend(vec: VecHandle, ptr: *const u8, len: usize) {
    let v = unsafe { vec_ref(vec) };
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    v.data.extend_from_slice(bytes);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_vec_free(vec: VecHandle) {
    if !vec.0.is_null() {
        drop(unsafe { Box::from_raw(vec.0 as *mut DynVec) });
    }
}

// ---------------------------------------------------------------------------
// Rng (seedable PRNG). The algorithm lives in `rt-abi` ([`RngState`]); these entry points box
// the state and delegate, so the interpreter and compiled code share one implementation.
// ---------------------------------------------------------------------------

unsafe fn rng_ref<'a>(rng: RngHandle) -> &'a mut RngState {
    unsafe { &mut *(rng.0 as *mut RngState) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rng_new(seed: u64) -> RngHandle {
    RngHandle(Box::into_raw(Box::new(RngState::new(seed))) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rng_next(rng: RngHandle) -> u64 {
    unsafe { rng_ref(rng) }.next_u64()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rng_frac(rng: RngHandle) -> f64 {
    unsafe { rng_ref(rng) }.frac()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rng_upto(rng: RngHandle, n: u64) -> u64 {
    unsafe { rng_ref(rng) }.up_to(n)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rng_free(rng: RngHandle) {
    if !rng.0.is_null() {
        drop(unsafe { Box::from_raw(rng.0 as *mut RngState) });
    }
}

// ---------------------------------------------------------------------------
// Concurrency (slide) — minimal: OS threads + a registry (matches rt-stub). A real M:N
// cooperative scheduler is the next milestone.
// ---------------------------------------------------------------------------

struct TaskReg {
    next: u64,
    handles: HashMap<u64, JoinHandle<()>>,
}

fn tasks() -> &'static Mutex<TaskReg> {
    static TASKS: OnceLock<Mutex<TaskReg>> = OnceLock::new();
    TASKS.get_or_init(|| {
        Mutex::new(TaskReg {
            next: 1,
            handles: HashMap::new(),
        })
    })
}

/// A raw pointer we promise to move to exactly one spawned task.
struct SendPtr(usize);
// SAFETY: the argument pointer is handed to a single task; the caller owns the aliasing
// contract, exactly as with the real scheduler.
unsafe impl Send for SendPtr {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_slide(entry: extern "C" fn(*mut u8), arg: *mut u8) -> TaskHandle {
    let sp = SendPtr(arg as usize);
    let handle = std::thread::spawn(move || {
        entry(sp.0 as *mut u8);
    });
    let mut reg = tasks().lock().unwrap();
    let id = reg.next;
    reg.next += 1;
    reg.handles.insert(id, handle);
    TaskHandle(id)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_yield() {
    std::thread::yield_now();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_task_join(task: TaskHandle) {
    let handle = tasks().lock().unwrap().handles.remove(&task.0);
    if let Some(h) = handle {
        let _ = h.join();
    }
}

// ---------------------------------------------------------------------------
// Panic / recover (yeet / sheesh).
// ---------------------------------------------------------------------------

fn die(msg: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "bet: panic: {msg}");
    std::process::abort();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_panic(msg: *const u8, len: usize) -> ! {
    let text = if msg.is_null() {
        "yeet".to_string()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(msg, len) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    die(&text)
}

// Recovery (`sheesh`) is rare and discouraged. `bet_panic` aborts today (an `extern "C"`
// boundary cannot unwind), so an open boundary cannot yet *catch* — but unlike the stub's
// pure no-ops we track the boundary nesting in a real thread-local LIFO. That makes the
// structure real (mismatched begin/end are detectable) and is the hook a longjmp- or
// landing-pad-based recover will fill in once the backend emits the boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_recover_begin() -> *mut c_void {
    RECOVER.with(|r| {
        let mut stack = r.borrow_mut();
        // Token ids start at 1 so a valid token is never null.
        let id = stack.last().map_or(1, |top| top + 1);
        stack.push(id);
        id as *mut c_void
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_recover_end(token: *mut c_void) {
    RECOVER.with(|r| {
        let mut stack = r.borrow_mut();
        // Pop only if it matches the innermost boundary; a mismatched/stale token is ignored
        // rather than corrupting the stack.
        if stack.last() == Some(&(token as usize)) {
            stack.pop();
        }
    });
}

// ---------------------------------------------------------------------------
// Lifecycle + stdout.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rt_init() {
    // Touch the per-thread scratch and the monotonic clock so their lazy statics are warm
    // before the first frame. Idempotent — safe to call once at program start.
    let _ = unsafe { bet_scratch() };
    let _ = unsafe { bet_gg_ticks() };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rt_shutdown() {
    // Free the per-thread scratch crib if one was created on this thread.
    SCRATCH.with(|s| {
        if let Some(h) = s.borrow_mut().take() {
            unsafe { bet_crib_free(h) };
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_print(ptr: *const u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    write_stdout(bytes);
}

/// Write `bytes` to stdout and flush — the shared tail of every `bet_print_*` entry point.
fn write_stdout(bytes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Format a signed integer the way the interpreter prints it: plain decimal, no newline.
/// Factored out so the test asserts the exact bytes without capturing process stdout.
/// Byte-for-byte identical to `rt-stub`'s `fmt_i64` (drop-in parity).
pub fn fmt_i64(v: i64) -> String {
    format!("{v}")
}

/// Format an unsigned integer the way the interpreter prints it: plain decimal, no newline.
/// Byte-for-byte identical to `rt-stub`'s `fmt_u64`.
pub fn fmt_u64(v: u64) -> String {
    format!("{v}")
}

/// Format a float to match the interpreter's `display_float`: a finite integral value gets a
/// single trailing `.0` (e.g. `3.0`); everything else uses the default `f64` formatting.
/// Byte-for-byte identical to `rt-stub`'s `fmt_f64`.
pub fn fmt_f64(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_print_i64(v: i64) {
    write_stdout(fmt_i64(v).as_bytes());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_print_u64(v: u64) {
    write_stdout(fmt_u64(v).as_bytes());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_print_f64(v: f64) {
    write_stdout(fmt_f64(v).as_bytes());
}

// ---------------------------------------------------------------------------
// Platform layer. Headless by default (Null-RHI style, matches rt-stub); with the
// `gg-desktop` feature, the four entry points delegate to `gg-backend`'s real
// minifb window + cpal audio + input queue. The `extern "C"` signatures (frozen in
// rt-abi) are byte-for-byte identical in both builds — the feature only changes the
// body, so swapping a headless `libruntime.a` for a desktop one is a pure rebuild.
// ---------------------------------------------------------------------------

// --- headless (default: no `gg-desktop`) — the exact prior Null-RHI behavior. ---

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_present(_fb: *const FrameBuffer) {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_audio(_frames: *const i16, _count: usize) {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_poll(out: *mut Event) -> bool {
    if !out.is_null() {
        unsafe {
            *out = Event {
                kind: event_kind::NONE,
                code: 0,
                x: 0,
                y: 0,
            };
        }
    }
    false
}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_ticks() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

// gg compositor / mixer / mouse — headless (matches rt-stub): id-returning entry points yield
// `0`, the rest are no-ops. `bet_gg_mouse` is NOT cfg-split (it delegates to gg-backend below).

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_tex(_rgba: *const u8, _w: u32, _h: u32) -> u32 {
    0
}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_frame(_w: u32, _h: u32, _clear_argb: u32) {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sprite(_tex: u32, _dx: i32, _dy: i32) {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sprite_sub(
    _tex: u32,
    _sx: i32,
    _sy: i32,
    _sw: u32,
    _sh: u32,
    _dx: i32,
    _dy: i32,
) {
}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_rect(_dx: i32, _dy: i32, _w: u32, _h: u32, _argb: u32) {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_flush() {}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sound(
    _pcm: *const u8,
    _byte_len: usize,
    _channels: u32,
    _rate: u32,
) -> u32 {
    0
}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_play(_sound: u32, _loop: u32, _vol_q8: u32) -> u32 {
    0
}

#[cfg(not(feature = "gg-desktop"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_stop(_voice: u32) {}

// --- desktop (`gg-desktop`) — delegate to the real gg-backend. ---

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_present(fb: *const FrameBuffer) {
    if fb.is_null() {
        return;
    }
    // SAFETY: the frozen contract says `fb` (when non-null) points to a valid `FrameBuffer`
    // whose `pixels` addresses `stride * height` readable `u32`s.
    let fb = unsafe { &*fb };
    gg_backend::present(fb.pixels as *const u32, fb.width, fb.height, fb.stride);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_audio(frames: *const i16, count: usize) {
    if frames.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller guarantees `frames` addresses `count` readable `i16`s.
    let samples = unsafe { std::slice::from_raw_parts(frames, count) };
    gg_backend::audio(samples);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_poll(out: *mut Event) -> bool {
    let ev = gg_backend::poll();
    if !out.is_null() {
        // SAFETY: `out` (when non-null) points to a writable `Event`.
        unsafe {
            *out = ev;
        }
    }
    ev.kind != event_kind::NONE
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_ticks() -> u64 {
    gg_backend::ticks()
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_tex(rgba: *const u8, w: u32, h: u32) -> u32 {
    if rgba.is_null() || w == 0 || h == 0 {
        return 0;
    }
    let len = (w as usize) * (h as usize) * 4;
    // SAFETY: the contract says `rgba` addresses `w * h * 4` readable bytes (RGBA8).
    let px = unsafe { std::slice::from_raw_parts(rgba, len) };
    gg_backend::tex(px, w, h)
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_frame(w: u32, h: u32, clear_argb: u32) {
    gg_backend::frame(w, h, clear_argb);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sprite(tex: u32, dx: i32, dy: i32) {
    gg_backend::sprite(tex, dx, dy);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sprite_sub(
    tex: u32,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    dx: i32,
    dy: i32,
) {
    gg_backend::sprite_sub(tex, sx, sy, sw, sh, dx, dy);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
    gg_backend::rect(dx, dy, w, h, argb);
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_flush() {
    gg_backend::flush();
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_sound(
    pcm: *const u8,
    byte_len: usize,
    channels: u32,
    rate: u32,
) -> u32 {
    if pcm.is_null() || byte_len == 0 {
        return 0;
    }
    // SAFETY: the contract says `pcm` addresses `byte_len` readable bytes (interleaved LE i16).
    let bytes = unsafe { std::slice::from_raw_parts(pcm, byte_len) };
    gg_backend::sound(bytes, channels, rate)
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_play(sound: u32, loop_: u32, vol_q8: u32) -> u32 {
    gg_backend::play(sound, loop_, vol_q8)
}

#[cfg(feature = "gg-desktop")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_stop(voice: u32) {
    gg_backend::stop(voice);
}

/// `bet_gg_size` — the 5th gg primitive (dynamic resolution). Not cfg-split: `gg_backend::size`
/// reports the live window size under `gg-desktop` and the fixed headless default otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_size() -> u64 {
    gg_backend::size()
}

/// `bet_gg_mouse` — the mouse position in logical-canvas coordinates, packed `x << 32 | y`. Not
/// cfg-split (mirrors `bet_gg_size`): `gg_backend::mouse` reports the live cursor under
/// `gg-desktop` and `0` otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_mouse() -> u64 {
    gg_backend::mouse()
}

// --- gg relative mouse / voice retune / fixed-canvas present / streaming audio. None are
// cfg-split (mirroring `bet_gg_size`/`bet_gg_mouse`): gg-backend itself is headless without
// its `desktop` feature, so each call below is a real backend call under `gg-desktop` and the
// documented headless default otherwise. ---

/// `bet_gg_mouse_delta` — drain the accumulated raw mouse `(dx, dy)`, packed
/// `(dx as u32) << 32 | (dy as u32)` (sign-preserving `i32` halves). Headless: `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_mouse_delta() -> u64 {
    gg_backend::mouse_delta()
}

/// `bet_gg_tune` — live-update a playing voice's volume (Q8) and stereo pan (0 = left,
/// 128 = center, 255 = right). Headless: a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_tune(voice: u32, vol_q8: u32, pan_q8: u32) {
    gg_backend::tune(voice, vol_q8, pan_q8);
}

/// `bet_gg_show` — present a tightly packed `w * h` framebuffer aspect-fit (integer
/// nearest-neighbor upscale, centered letterbox) into the live window. Headless: a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_show(pixels: *const u32, w: u32, h: u32) {
    // Null/zero-size guarding happens inside gg-backend (mirrors its `present`).
    gg_backend::show(pixels, w, h);
}

/// `bet_gg_audio_spec` — the audio device's output config, packed `rate << 32 | channels`.
/// Headless (or no usable device): the fixed `(48000, 2)` default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_audio_spec() -> u64 {
    gg_backend::audio_spec()
}

/// `bet_gg_pending` — interleaved `i16` samples still queued in the raw `bet_gg_audio` ring.
/// Headless: `0` (instant drain).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_pending() -> u64 {
    gg_backend::pending()
}

/// `bet_gg_title` — set the window title to the `len` UTF-8 bytes at `title`. Empty/absent or
/// non-UTF-8 is ignored. Headless: a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_title(title: *const u8, len: usize) {
    if title.is_null() || len == 0 {
        return;
    }
    // SAFETY: the contract says `title` addresses `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(title, len) };
    if let Ok(name) = std::str::from_utf8(bytes) {
        gg_backend::title(name);
    }
}
