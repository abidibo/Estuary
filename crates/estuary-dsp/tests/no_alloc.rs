//! RT processors must never allocate (dsp M7.7). A counting global allocator, armed only around
//! the measured section; this binary holds a single test so nothing else allocates meanwhile.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use estuary_dsp::{MAX_QUANTUM, Processor, gain_processor};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: delegates every call to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Deterministic pseudo-random numbers without a dependency (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn gain_processor_never_allocates() {
    let (mut p, handle) = gain_processor(0.0);
    let input = vec![0.25f32; MAX_QUANTUM];
    let mut l = vec![0.0f32; MAX_QUANTUM];
    let mut r = vec![0.0f32; MAX_QUANTUM];
    let mut rng = Rng(0x5eed);
    let mut cycle = |p: &mut dyn Processor, n: usize| {
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        p.process(&[&input[..n], &input[..n]], &mut outs, n, 48_000);
    };
    for _ in 0..10 {
        cycle(&mut p, 256);
    }

    ARMED.store(true, Ordering::SeqCst);
    for _ in 0..10_000 {
        let n = (rng.next() % MAX_QUANTUM as u64) as usize + 1;
        if rng.next().is_multiple_of(4) {
            handle.set_db(-((rng.next() % 600) as f32) / 10.0);
        }
        cycle(&mut p, n);
    }
    ARMED.store(false, Ordering::SeqCst);

    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "allocations in the RT path"
    );
}
