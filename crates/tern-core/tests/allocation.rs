//! Snapshots in the steady state allocate nothing. Alone in its own test
//! binary because it replaces the global allocator.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::sh;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Counts the allocations made by each thread.
struct Counting;

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn snapshots_do_not_allocate_once_warm() {
    // Colors, wide characters, combining marks and links, changing all the
    // time.
    let mut h = sh(
        "i=0; while :; do printf '\\033[3%dm%06d \\033]8;;x\\033\\\\link\\033]8;;\\033\\\\ 日本 e\\314\\201\\n' $((i%8)) $i; i=$((i+1)); done",
    );
    h.wait_for("link");
    for _ in 0..50 {
        h.snapshot();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let mut changed = 0;
    let before = allocations();
    for _ in 0..200 {
        let seq = h.frame.seq;
        h.snapshot();
        if h.frame.seq != seq {
            changed += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let made = allocations() - before;
    // The counter itself works.
    let probe = allocations();
    drop(std::hint::black_box(Vec::<u8>::with_capacity(16)));
    assert_eq!(allocations(), probe + 1);
    assert!(changed > 100, "only {changed} snapshots saw new output");
    assert_eq!(made, 0, "{made} allocations in 200 snapshots");
}
