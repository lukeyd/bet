//! `rt-stub` — a naive, malloc-backed implementation of every `rt-abi` entry point.
//!
//! Correct semantics, no performance: typed cribs are heap slabs with per-slot generations,
//! bump cribs are a reserved buffer + offset, the allocator is the system heap, tasks are OS
//! threads, and the **platform layer is headless** (Null-RHI style — `present`/`audio` are
//! no-ops, `poll` returns no events, `ticks` is a real monotonic clock). Bootstrap-only; the
//! real `runtime` replaces this crate without changing a single signature.
//!
//! The load-bearing behavior is generational access: `bet_evict` marks every slot free *and
//! bumps its generation*, so a stale [`Tag`] naming a reused slot resolves as ghosted rather
//! than reading the new occupant (spec §7.2–§7.4).

// rt-stub is inherently unsafe (it defines the FFI boundary and dereferences raw handles).
// The `unsafe_code` lint stays at `warn` workspace-wide for visibility; opt in explicitly.
#![allow(unsafe_code)]
// Every entry point is an `unsafe extern "C"` trampoline; safety is documented at the ABI
// level in `rt-abi`, not per-function here.
#![allow(clippy::missing_safety_doc)]

use core::ffi::c_void;
use std::alloc::{self, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

/// The base name of this crate's static library (`librt_stub.a` → `-lrt_stub`). The driver
/// uses it to construct the link line for compiled `bet` programs. Keeping it here ties the
/// name to the crate that produces the archive.
pub fn staticlib_link_name() -> &'static str {
    "rt_stub"
}

// Re-export the shared ABI types so downstream (and tests) can name them via `rt_stub`.
// Only the *types* are re-exported; the entry-point declarations in `rt-abi` are not (they
// would collide with the definitions below).
pub use rt_abi::{
    AllocCtx, CribHandle, Event, FrameBuffer, MapHandle, Tag, TaskHandle, event_kind,
};

// ---------------------------------------------------------------------------
// Allocator.
// ---------------------------------------------------------------------------

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size.max(1), align.max(1)).expect("invalid (size, align)")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_alloc(size: usize, align: usize) -> *mut u8 {
    unsafe { alloc::alloc(layout(size, align)) }
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
    static CTX_STACK: RefCell<Vec<AllocCtx>> = const { RefCell::new(Vec::new()) };
    static SCRATCH: RefCell<Option<CribHandle>> = const { RefCell::new(None) };
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

struct TypedCrib {
    elem_size: usize,
    elem_align: usize,
    base: *mut u8,
    meta: Vec<SlotMeta>,
}

#[derive(Clone, Copy)]
struct SlotMeta {
    occupied: bool,
    generation: u32,
}

struct BumpCrib {
    base: *mut u8,
    cap: usize,
    align: usize,
    offset: usize,
}

/// Reborrow a crib handle as a `&mut Crib`. Caller guarantees the handle is live and not
/// aliased (the stub is single-threaded per crib).
unsafe fn crib(h: CribHandle) -> *mut Crib {
    h.0 as *mut Crib
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
        unsafe { alloc::alloc(layout(total, elem_align)) }
    };
    let meta = vec![
        SlotMeta {
            occupied: false,
            generation: 0,
        };
        cap
    ];
    let boxed = Box::new(Crib {
        kind: CribKind::Typed(TypedCrib {
            elem_size,
            elem_align,
            base,
            meta,
        }),
    });
    CribHandle(Box::into_raw(boxed) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_crib_new_bump(reserve: usize) -> CribHandle {
    let align = 16usize;
    let cap = reserve.max(1);
    let base = unsafe { alloc::alloc(layout(cap, align)) };
    let boxed = Box::new(Crib {
        kind: CribKind::Bump(BumpCrib {
            base,
            cap,
            align,
            offset: 0,
        }),
    });
    CribHandle(Box::into_raw(boxed) as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_crib_free(handle: CribHandle) {
    if handle.0.is_null() {
        return;
    }
    let boxed = unsafe { Box::from_raw(crib(handle)) };
    match boxed.kind {
        CribKind::Typed(t) => {
            let total = t.elem_size.saturating_mul(t.meta.len());
            if !t.base.is_null() && total > 0 {
                unsafe { alloc::dealloc(t.base, layout(total, t.elem_align)) };
            }
        }
        CribKind::Bump(b) => {
            if !b.base.is_null() {
                unsafe { alloc::dealloc(b.base, layout(b.cap, b.align)) };
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_evict(handle: CribHandle) {
    let c = unsafe { &mut *crib(handle) };
    match &mut c.kind {
        // The load-bearing operation: free every slot AND bump its generation, so every
        // outstanding tag into this crib becomes ghosted.
        CribKind::Typed(t) => {
            for m in &mut t.meta {
                m.occupied = false;
                m.generation = m.generation.wrapping_add(1);
            }
        }
        CribKind::Bump(b) => b.offset = 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_bump_alloc(handle: CribHandle, size: usize, align: usize) -> *mut u8 {
    let c = unsafe { &mut *crib(handle) };
    let CribKind::Bump(b) = &mut c.kind else {
        return std::ptr::null_mut();
    };
    let align = align.max(1);
    let aligned = (b.offset + align - 1) & !(align - 1);
    if aligned.saturating_add(size) > b.cap {
        // A bump arena must hand out stable pointers, so we do not grow-by-move; reserve
        // enough up front. Exhaustion is a hard error in the stub.
        die("bump crib exhausted (reserve more up front)");
    }
    b.offset = aligned + size;
    unsafe { b.base.add(aligned) }
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
// Tags / holla.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_cop(handle: CribHandle) -> Tag {
    let c = unsafe { &mut *crib(handle) };
    let CribKind::Typed(t) = &mut c.kind else {
        return Tag::NULL;
    };
    for (i, m) in t.meta.iter_mut().enumerate() {
        if !m.occupied {
            m.occupied = true;
            return Tag {
                slot: i as u32,
                generation: m.generation,
            };
        }
    }
    Tag::NULL
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_holla_check(handle: CribHandle, tag: Tag) -> *mut u8 {
    let c = unsafe { &*crib(handle) };
    let CribKind::Typed(t) = &c.kind else {
        return std::ptr::null_mut();
    };
    let slot = tag.slot as usize;
    match t.meta.get(slot) {
        Some(m) if m.occupied && m.generation == tag.generation && !t.base.is_null() => unsafe {
            t.base.add(slot * t.elem_size)
        },
        _ => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_slot_ptr(handle: CribHandle, slot: u32) -> *mut u8 {
    let c = unsafe { &*crib(handle) };
    let CribKind::Typed(t) = &c.kind else {
        return std::ptr::null_mut();
    };
    let slot = slot as usize;
    if slot < t.meta.len() && !t.base.is_null() {
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
    // Leak the buffer: `str` values are immutable and freed at program exit (the stub does not
    // track string lifetimes — the real runtime's allocator context will).
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

// ---------------------------------------------------------------------------
// Stash (hash maps).
// ---------------------------------------------------------------------------

/// A type-erased map: byte-sequence keys to fixed-width value blobs. One implementation backs
/// every `stash[K, V]` — the frontend serializes keys/values to bytes at each call site.
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
// Concurrency (slide).
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
// Panic / recover.
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

// Recovery (`sheesh`) is rare and discouraged. The stub's panic aborts (an `extern "C"`
// boundary cannot unwind), so these boundaries are no-ops; real recover lands with the
// runtime. The symbols exist so the ABI is complete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_recover_begin() -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_recover_end(_token: *mut c_void) {}

// ---------------------------------------------------------------------------
// Lifecycle + stdout.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rt_init() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_rt_shutdown() {}

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
pub fn fmt_i64(v: i64) -> String {
    format!("{v}")
}

/// Format an unsigned integer the way the interpreter prints it: plain decimal, no newline.
pub fn fmt_u64(v: u64) -> String {
    format!("{v}")
}

/// Format a float to match the interpreter's `display_float`: a finite integral value gets a
/// single trailing `.0` (e.g. `3.0`); everything else uses the default `f64` formatting.
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
// Platform layer — headless (Null-RHI style).
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_present(_fb: *const FrameBuffer) {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_audio(_frames: *const i16, _count: usize) {}

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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bet_gg_ticks() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_read_round_trips_a_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("bet_fs_read_test_{}.txt", std::process::id()));
        let contents = b"hello from fs.peep\nsecond line\n";
        std::fs::write(&path, contents).unwrap();

        let path_str = path.to_str().unwrap();
        let mut out_len: usize = 0;
        let ptr = unsafe {
            bet_fs_read(
                path_str.as_ptr(),
                path_str.len(),
                &mut out_len as *mut usize,
            )
        };
        assert!(!ptr.is_null(), "reading an existing file should succeed");
        assert_eq!(out_len, contents.len());
        let read = unsafe { std::slice::from_raw_parts(ptr, out_len) };
        assert_eq!(read, contents);
        // Reclaim the leaked buffer so the test does not leak.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, out_len)) });
    }

    #[test]
    fn fs_read_missing_file_returns_null() {
        let missing = "/nonexistent/bet/path/does/not/exist.xyz";
        let mut out_len: usize = 7;
        let ptr =
            unsafe { bet_fs_read(missing.as_ptr(), missing.len(), &mut out_len as *mut usize) };
        assert!(ptr.is_null());
        assert_eq!(out_len, 0);
    }

    #[test]
    fn str_ops_concat_upper_eq() {
        let a = b"foo";
        let b = b"bar";
        let cat = unsafe { bet_str_concat(a.as_ptr(), a.len(), b.as_ptr(), b.len()) };
        let joined = unsafe { std::slice::from_raw_parts(cat, a.len() + b.len()) };
        assert_eq!(joined, b"foobar");

        let up = unsafe { bet_str_upper(a.as_ptr(), a.len()) };
        assert_eq!(unsafe { std::slice::from_raw_parts(up, a.len()) }, b"FOO");

        assert!(unsafe { bet_str_eq(a.as_ptr(), a.len(), a.as_ptr(), a.len()) });
        assert!(!unsafe { bet_str_eq(a.as_ptr(), a.len(), b.as_ptr(), b.len()) });
    }
}
