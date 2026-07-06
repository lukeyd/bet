//! ABI conformance tests for `rt-stub`. These pin the *semantics* every implementation of
//! `rt-abi` must honor — when the real `runtime` crate replaces the stub, it must pass the
//! same tests. The headline is the generational-access invariant (spec §7.2–§7.4).

// These tests drive the raw ABI directly, so they are wall-to-wall `unsafe`. The
// `unsafe_code` lint is `warn` workspace-wide; opt in for this test crate.
#![allow(unsafe_code)]

use rt_stub::*;
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
/// stale/null/double evicts are safe no-ops.
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

        // present/audio are no-ops in the headless stub.
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
        let msg = b"stub online\n";
        bet_print(msg.as_ptr(), msg.len());
        bet_recover_end(bet_recover_begin()); // no-op boundary
        bet_rt_shutdown();
    }
}

/// The number-formatting primitives (`bet_print_{i64,u64,f64}`) format a value and write it to
/// stdout with **no** trailing newline — the caller adds any newline. Capturing process stdout
/// in-process is awkward, so we assert the exact bytes via the shared `fmt_*` helpers the
/// extern entry points use verbatim, then smoke-call the externs to prove they run.
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
