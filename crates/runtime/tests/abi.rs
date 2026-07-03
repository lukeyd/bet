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

        // present/audio are no-ops in the headless runtime.
        bet_gg_present(ptr::null());
        bet_gg_audio(ptr::null(), 0);
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
