//! ABI conformance tests for the real `runtime`. The first block is a **parity oracle**:
//! it mirrors `rt-stub/tests/abi.rs` so the real runtime is proven to be observably
//! interchangeable with the bootstrap stub. The second block pins the *real-arena* properties
//! the stub doesn't have — O(1) capacity-retaining `evict`, growable bump cribs, free-list
//! reuse, generational rejection of stale tags, deep allocator-context nesting, and tracked
//! recover boundaries (spec §7.2–§7.4).

// These tests drive the raw ABI directly, so they are wall-to-wall `unsafe`. The
// `unsafe_code` lint is `warn` workspace-wide; opt in for this test crate.
#![allow(unsafe_code)]

use runtime::*;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

/// Mirror of the corpus `idOf`: resolve a tag, read its `i64` id, or `-1` if ghosted.
fn id_of(crib: CribHandle, tag: Tag) -> i64 {
    let p = unsafe { bet_holla_check(crib, tag) };
    if p.is_null() {
        -1
    } else {
        unsafe { *(p as *const i64) }
    }
}

// ===========================================================================
// Parity block — must match rt-stub's observable behavior exactly.
// ===========================================================================

/// The memory-model showcase: `08-memory/generation-reuse.bet` must print `7 / 42 / -1`.
/// After `evict`, a reused slot carries a newer generation, so the stale tag ghosts.
#[test]
fn generation_reuse() {
    unsafe {
        // A crib of 4 `Enemy { id: i64 }` slots (8 bytes, 8-aligned).
        let crib = bet_crib_new(8, 8, 4);

        let first = bet_cop(crib);
        assert_ne!(first, Tag::NULL);
        *(bet_holla_check(crib, first) as *mut i64) = 7;
        assert_eq!(id_of(crib, first), 7); // 7

        bet_evict(crib); // frees the slab; bumps every slot's generation

        // A new enemy reuses the same physical slot, at a newer generation.
        let second = bet_cop(crib);
        assert_eq!(second.slot, first.slot, "slot should be reused");
        assert_ne!(
            second.generation, first.generation,
            "generation must advance"
        );
        *(bet_holla_check(crib, second) as *mut i64) = 42;
        assert_eq!(id_of(crib, second), 42); // 42

        // The OLD tag still names that slot, but its generation is stale.
        assert_eq!(id_of(crib, first), -1); // -1 — safely ghosted

        bet_crib_free(crib);
    }
}

/// Per-slot free (`evict tag in crib`, rt-abi `bet_evict_slot`): the freed tag ghosts, other
/// tags stay live, the freed slot is reused by the next `cop` at a bumped generation, and
/// stale/null/double evicts are safe no-ops. (Parity oracle: mirrors rt-stub's test.)
#[test]
fn evict_slot_frees_one_slot_and_bumps_generation() {
    unsafe {
        let crib = bet_crib_new(8, 8, 2);

        // Fill the crib completely so slot reuse is forced (implementation-independent).
        let a = bet_cop(crib);
        let b = bet_cop(crib);
        *(bet_holla_check(crib, a) as *mut i64) = 7;
        *(bet_holla_check(crib, b) as *mut i64) = 8;
        assert_eq!(bet_cop(crib), Tag::NULL, "both slots occupied");

        bet_evict_slot(crib, a);
        assert_eq!(id_of(crib, a), -1, "the freed tag ghosts");
        assert_eq!(id_of(crib, b), 8, "other slots are untouched");

        // Double-evict and the null tag are safe no-ops.
        bet_evict_slot(crib, a);
        bet_evict_slot(crib, Tag::NULL);
        assert_eq!(id_of(crib, b), 8);

        // The freed slot is the only free one, so the next cop MUST reuse it, at a newer gen.
        let c = bet_cop(crib);
        assert_eq!(c.slot, a.slot, "the freed slot is reused");
        assert_ne!(c.generation, a.generation, "generation must advance");
        *(bet_holla_check(crib, c) as *mut i64) = 9;
        assert_eq!(id_of(crib, c), 9);
        assert_eq!(
            id_of(crib, a),
            -1,
            "the stale tag never sees the new occupant"
        );

        // A per-slot evict on a bump crib is a harmless no-op.
        let bump = bet_crib_new_bump(64);
        bet_evict_slot(bump, c);
        bet_crib_free(bump);

        bet_crib_free(crib);
    }
}

#[test]
fn cop_fills_then_returns_null_when_full() {
    unsafe {
        let crib = bet_crib_new(8, 8, 2);
        let a = bet_cop(crib);
        let b = bet_cop(crib);
        assert_ne!(a, Tag::NULL);
        assert_ne!(b, Tag::NULL);
        assert_ne!(a.slot, b.slot);
        assert_eq!(bet_cop(crib), Tag::NULL, "a full crib yields the null tag");
        bet_crib_free(crib);
    }
}

#[test]
fn bump_crib_and_scratch() {
    unsafe {
        let bc = bet_crib_new_bump(1024);
        let a = bet_bump_alloc(bc, 8, 8);
        let b = bet_bump_alloc(bc, 8, 8);
        assert!(!a.is_null() && !b.is_null());
        assert_eq!(b as usize - a as usize, 8, "8-byte allocs are contiguous");

        bet_evict(bc); // resets the bump offset
        let a2 = bet_bump_alloc(bc, 8, 8);
        assert_eq!(
            a2, a,
            "after reset, the first alloc reuses the same address"
        );
        bet_crib_free(bc);

        // The per-thread scratch crib is stable and resettable.
        let s = bet_scratch();
        assert_eq!(s, bet_scratch());
        bet_scratch_reset();
    }
}

/// `mem.receipts()` is the per-frame arena's usage gauge: it climbs as scratch is allocated into
/// and returns to zero after `bet_scratch_reset` — the frame-arena cycle the `fireworks` port
/// demonstrates (fill each frame, reclaim in O(1) at the frame boundary, never grow).
#[test]
fn mem_receipts_tracks_scratch_and_resets() {
    unsafe {
        // Untouched scratch reads zero (no arena yet).
        assert_eq!(bet_mem_receipts(), 0, "receipts start at zero");

        let s = bet_scratch();
        assert_eq!(
            bet_mem_receipts(),
            0,
            "a fresh scratch has nothing allocated"
        );

        // Allocating into scratch climbs the gauge...
        let _a = bet_bump_alloc(s, 64, 8);
        let after_first = bet_mem_receipts();
        assert!(
            after_first >= 64,
            "receipts reflect the bytes handed out ({after_first})"
        );
        let _b = bet_bump_alloc(s, 128, 8);
        assert!(
            bet_mem_receipts() >= after_first + 128,
            "receipts keep climbing as the frame allocates"
        );

        // ...and the frame-end reset returns it to zero (the per-frame cycle).
        bet_scratch_reset();
        assert_eq!(bet_mem_receipts(), 0, "reset reclaims the whole arena");
    }
}

#[test]
fn alloc_realloc_free() {
    unsafe {
        let p = bet_alloc(16, 8);
        assert!(!p.is_null());
        *(p as *mut u64) = 0xdead_beef;
        let p2 = bet_realloc(p, 16, 64, 8);
        assert_eq!(
            *(p2 as *const u64),
            0xdead_beef,
            "realloc preserves contents"
        );
        bet_free(p2, 64, 8);

        // realloc of a null pointer behaves like alloc.
        let p3 = bet_realloc(ptr::null_mut(), 0, 8, 8);
        assert!(!p3.is_null());
        bet_free(p3, 8, 8);
    }
}

#[test]
fn allocator_context_stack() {
    unsafe {
        assert_eq!(bet_ctx_current().0, ptr::null_mut());
        let ctx = AllocCtx(0x1234 as *mut _);
        bet_ctx_push(ctx);
        assert_eq!(bet_ctx_current(), ctx);
        bet_ctx_pop();
        assert_eq!(bet_ctx_current().0, ptr::null_mut());
    }
}

#[test]
fn platform_layer_is_headless() {
    unsafe {
        let t1 = bet_gg_ticks();
        let t2 = bet_gg_ticks();
        assert!(t2 >= t1, "ticks is monotonic");

        let mut ev = Event {
            kind: 99,
            code: 0,
            x: 0,
            y: 0,
        };
        assert!(!bet_gg_poll(&mut ev), "headless: no events");
        assert_eq!(ev.kind, event_kind::NONE);

        // present/audio are no-ops in the headless runtime.
        bet_gg_present(ptr::null());
        bet_gg_audio(ptr::null(), 0);

        // gg compositor / mixer / mouse — headless: the id-returning entry points yield 0, and
        // the rest link and no-op.
        assert_eq!(bet_gg_tex(ptr::null(), 4, 4), 0);
        assert_eq!(bet_gg_sound(ptr::null(), 0, 1, 44_100), 0);
        assert_eq!(bet_gg_play(0, 0, 256), 0);
        assert_eq!(bet_gg_mouse(), 0);
        bet_gg_frame(320, 240, 0);
        bet_gg_sprite(0, 0, 0);
        bet_gg_sprite_sub(0, 0, 0, 4, 4, 0, 0);
        bet_gg_rect(0, 0, 10, 10, 0);
        bet_gg_flush();
        bet_gg_stop(0);

        // gg relative mouse / retune / show / streaming audio — headless: no deltas, retune and
        // show no-op, and the audio spec/backpressure report the documented fixed defaults.
        assert_eq!(bet_gg_mouse_delta(), 0);
        bet_gg_tune(0, 256, 128);
        bet_gg_show(ptr::null(), 320, 200);
        assert_eq!(bet_gg_audio_spec(), (48_000u64 << 32) | 2);
        assert_eq!(bet_gg_pending(), 0);
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)] // signature is fixed by the `slide` ABI
extern "C" fn increment(arg: *mut u8) {
    let counter = unsafe { &*(arg as *const AtomicU32) };
    counter.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn slide_spawns_and_joins() {
    let counter = AtomicU32::new(0);
    unsafe {
        let task = bet_slide(increment, &counter as *const AtomicU32 as *mut u8);
        bet_yield();
        bet_task_join(task);
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn lifecycle_and_print() {
    unsafe {
        bet_rt_init();
        let msg = b"runtime online\n";
        bet_print(msg.as_ptr(), msg.len());
        bet_recover_end(bet_recover_begin()); // balanced no-op boundary
        bet_rt_shutdown();
    }
}

/// Parity: the number-formatting primitives (`bet_print_{i64,u64,f64}`) must produce the exact
/// same bytes as `rt-stub`, with **no** trailing newline (the caller adds any newline). We
/// assert the exact bytes via the shared `fmt_*` helpers the extern entry points use verbatim,
/// then smoke-call the externs to prove they run. These assertions mirror `rt-stub`'s test.
#[test]
fn number_formatting_primitives() {
    // Signed decimal.
    assert_eq!(fmt_i64(-42), "-42");
    assert_eq!(fmt_i64(0), "0");
    assert_eq!(fmt_i64(i64::MIN), "-9223372036854775808");

    // Unsigned decimal.
    assert_eq!(fmt_u64(42), "42");
    assert_eq!(fmt_u64(0), "0");
    assert_eq!(fmt_u64(u64::MAX), "18446744073709551615");

    // Float: a finite integral value gets a single trailing `.0` (matching the interpreter's
    // `display_float`); everything else uses default `f64` formatting.
    assert_eq!(fmt_f64(3.0), "3.0");
    assert_eq!(fmt_f64(2.5), "2.5");
    assert_eq!(fmt_f64(100.0), "100.0");
    assert_eq!(fmt_f64(-7.0), "-7.0");
    assert_eq!(fmt_f64(0.1), "0.1");
    assert_eq!(fmt_f64(f64::INFINITY), "inf");
    assert_eq!(fmt_f64(f64::NEG_INFINITY), "-inf");
    assert_eq!(fmt_f64(f64::NAN), "NaN");

    // Smoke-call the externs: they must run and print (no newline) without panicking.
    unsafe {
        bet_print_i64(-42);
        bet_print_u64(42);
        bet_print_f64(3.0);
        bet_print_f64(2.5);
        println!(); // terminate the line we just wrote for tidy captured output
    }
}

// ===========================================================================
// Real-arena block — properties the naive stub does not provide.
// ===========================================================================

/// Every outstanding tag ghosts after a single O(1) `evict`, across many slots.
#[test]
fn evict_ghosts_all_outstanding_tags() {
    unsafe {
        let crib = bet_crib_new(8, 8, 3);
        let tags: Vec<Tag> = (0..3).map(|_| bet_cop(crib)).collect();
        for (i, &t) in tags.iter().enumerate() {
            assert_ne!(t, Tag::NULL);
            *(bet_holla_check(crib, t) as *mut i64) = 100 + i as i64;
        }
        for (i, &t) in tags.iter().enumerate() {
            assert_eq!(id_of(crib, t), 100 + i as i64);
        }

        bet_evict(crib);

        // All prior tags are now stale — the generational check rejects each one.
        for &t in &tags {
            assert_eq!(id_of(crib, t), -1, "stale tag must ghost after evict");
        }

        // Fresh cops reuse the physical slots at a newer generation and are live again.
        let fresh = bet_cop(crib);
        assert_ne!(fresh, Tag::NULL);
        assert!(!bet_holla_check(crib, fresh).is_null());
        bet_crib_free(crib);
    }
}

/// A stale tag that names a slot which has been *re-copped* (so the slot is occupied again)
/// still ghosts, because its generation is older than the slot's current generation. This is
/// the "attack an innocent bystander" bug the generation counter prevents.
#[test]
fn holla_rejects_stale_tag_for_reoccupied_slot() {
    unsafe {
        let crib = bet_crib_new(8, 8, 1); // single slot => guaranteed reuse
        let old = bet_cop(crib);
        *(bet_holla_check(crib, old) as *mut i64) = 1;

        bet_evict(crib);

        let new = bet_cop(crib);
        assert_eq!(new.slot, old.slot, "same physical slot");
        *(bet_holla_check(crib, new) as *mut i64) = 2;

        // The slot IS occupied, but by a newer generation: the old tag must not read it.
        assert!(
            bet_holla_check(crib, old).is_null(),
            "stale generation must not resolve even though the slot is occupied"
        );
        assert_eq!(id_of(crib, new), 2);
        bet_crib_free(crib);
    }
}

/// A bump crib grows past its initial reserve instead of aborting (the stub dies here), keeps
/// every pointer stable, and rewinds to the head chunk's base on `evict` while retaining the
/// grown capacity.
#[test]
fn bump_crib_grows_and_retains_capacity() {
    unsafe {
        let bc = bet_crib_new_bump(32); // tiny reserve, forces growth
        let mut ptrs = Vec::new();
        for i in 0..8u8 {
            let p = bet_bump_alloc(bc, 16, 16);
            assert!(!p.is_null(), "growth must not abort");
            // Prove the memory is usable and stable: write a marker, read it back later.
            *p = i;
            ptrs.push(p);
        }
        // Every allocation is a distinct, still-valid pointer.
        for (i, &p) in ptrs.iter().enumerate() {
            assert_eq!(*p, i as u8, "grown chunks stay live and stable");
        }
        let first = ptrs[0];

        bet_evict(bc); // O(1) rewind, keeps chunks
        let a2 = bet_bump_alloc(bc, 16, 16);
        assert_eq!(a2, first, "evict rewinds to the head chunk, reusing memory");

        // Re-growing after evict reuses the retained chunks (no fresh allocation needed to
        // reach the previous high-water mark) and stays valid.
        for _ in 0..8 {
            assert!(!bet_bump_alloc(bc, 16, 16).is_null());
        }
        bet_crib_free(bc);
    }
}

/// Bump allocation honors large alignments even after a misaligning small allocation.
#[test]
fn bump_alloc_respects_alignment() {
    unsafe {
        let bc = bet_crib_new_bump(256);
        let _misalign = bet_bump_alloc(bc, 1, 1); // leave the offset at 1
        let p = bet_bump_alloc(bc, 8, 64);
        assert!(!p.is_null());
        assert_eq!(p as usize % 64, 0, "64-aligned request must be 64-aligned");
        bet_crib_free(bc);
    }
}

/// The allocator-context stack nests arbitrarily deep and restores in LIFO order.
#[test]
fn allocator_context_nests_deeply() {
    unsafe {
        assert_eq!(bet_ctx_current().0, ptr::null_mut());
        let a = AllocCtx(0xa as *mut _);
        let b = AllocCtx(0xb as *mut _);
        let c = AllocCtx(0xc as *mut _);
        bet_ctx_push(a);
        bet_ctx_push(b);
        bet_ctx_push(c);
        assert_eq!(bet_ctx_current(), c);
        bet_ctx_pop();
        assert_eq!(bet_ctx_current(), b);
        bet_ctx_pop();
        assert_eq!(bet_ctx_current(), a);
        bet_ctx_pop();
        assert_eq!(bet_ctx_current().0, ptr::null_mut());
        // Popping past empty is harmless.
        bet_ctx_pop();
        assert_eq!(bet_ctx_current().0, ptr::null_mut());
    }
}

/// `sheesh` recovery boundaries are tracked in a real LIFO: nested `begin`s hand back
/// distinct non-null tokens and unwind cleanly.
#[test]
fn recover_boundaries_nest() {
    unsafe {
        let outer = bet_recover_begin();
        let inner = bet_recover_begin();
        assert!(!outer.is_null() && !inner.is_null());
        assert_ne!(outer, inner, "nested boundaries get distinct tokens");
        bet_recover_end(inner);
        bet_recover_end(outer);
    }
}

// ===========================================================================
// Single-thread handle contract (issue #44) — enforcement is fail-closed.
// ===========================================================================

/// Env flag that flips this test binary into "child" mode: perform the offending cross-thread
/// handle access and let the affinity guard abort the process. The parent re-execs this same
/// binary with the flag set and asserts the child died by SIGABRT.
const CROSS_THREAD_CHILD_ENV: &str = "BET_RT_TEST_CROSS_THREAD_ABORT";

/// A `bet_slide` task entry: `arg` is a `CribHandle`'s raw pointer created on the PARENT thread.
/// Touching the crib here — on the spawned task's own thread — must trip the single-thread
/// affinity guard in `crib()` and abort the whole process (issue #44). This is exactly the
/// escape the finding describes: a handle captured into a `bet_slide` task.
#[allow(clippy::not_unsafe_ptr_arg_deref)] // signature is fixed by the `slide` ABI
extern "C" fn touch_crib_from_task(arg: *mut u8) {
    let stolen = CribHandle(arg as *mut std::ffi::c_void);
    // Routes through `crib()` → `check_affinity` → `die` → `process::abort`.
    let _ = unsafe { bet_cop(stolen) };
}

/// Child-mode body: create a crib on THIS thread, then hand its handle to a `bet_slide` task on
/// a different thread. The guard must abort before the join can return.
fn cross_thread_child() {
    unsafe {
        let crib = bet_crib_new(8, 8, 1);
        let task = bet_slide(touch_crib_from_task, crib.0 as *mut u8);
        // The task aborts the whole process; this join never returns cleanly.
        bet_task_join(task);
        // Reached only if the guard FAILED to fire (a regression); free and fall through so the
        // parent observes a clean exit and its assertion fails loudly.
        bet_crib_free(crib);
    }
}

/// Using a handle from a thread that did not create it must **abort** (fail-closed), proving the
/// single-thread contract is enforced at runtime, not merely documented (issue #44). Because
/// `die`→`process::abort` kills the whole process, this is checked out-of-process: the parent
/// re-execs this test binary in child mode and asserts the child died by SIGABRT with the
/// contract diagnostic on stderr. Works under both `cargo test` and `nextest` (the child is just
/// the standard libtest CLI filtered to this one test).
#[test]
fn cross_thread_handle_use_aborts() {
    if std::env::var_os(CROSS_THREAD_CHILD_ENV).is_some() {
        // Child mode: do the offending thing and (expect to) abort before returning.
        cross_thread_child();
        // Only reached if the guard did not fire; exit 0 makes the parent's assertion fail.
        std::process::exit(0);
    }

    // Parent mode: re-exec ourselves in child mode and observe the abort.
    let exe = std::env::current_exe().expect("locate this test binary");
    let output = std::process::Command::new(exe)
        .args(["cross_thread_handle_use_aborts", "--exact"])
        .env(CROSS_THREAD_CHILD_ENV, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("re-exec this test binary in child mode");

    assert!(
        !output.status.success(),
        "cross-thread handle use must abort, but the child exited successfully"
    );

    // `die` → `process::abort` raises SIGABRT (signal 6) on unix.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            output.status.signal(),
            Some(6),
            "expected SIGABRT (6) from process::abort; got status {:?}",
            output.status
        );
    }

    // The diagnostic must explain the single-thread contract.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("single-threaded"),
        "abort message should explain the single-thread contract; stderr was:\n{stderr}"
    );
}
