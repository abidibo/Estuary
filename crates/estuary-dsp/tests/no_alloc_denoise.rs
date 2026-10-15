//! The denoiser must never allocate after warm-up (dsp M10.7). A counting global allocator, armed only around
//! the measured section; this binary holds a single test so nothing else allocates meanwhile.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use estuary_dsp::{MAX_QUANTUM, Processor, denoise_processor};

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
fn denoise_processor_never_allocates_after_warm_up() {
    let (mut p, handle) = denoise_processor();
    let mut rng = Rng(0xd3_0153);
    let input: Vec<f32> = (0..MAX_QUANTUM)
        .map(|_| (rng.next() >> 40) as f32 / (1u64 << 24) as f32 * 0.2 - 0.1)
        .collect();
    let mut l = vec![0.0f32; MAX_QUANTUM];
    let mut r = vec![0.0f32; MAX_QUANTUM];
    let mut cycle = |p: &mut dyn Processor, n: usize, rate: u32| {
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        p.process(&[&input[..n], &input[..n]], &mut outs, n, rate);
    };
    // Warm-up on this thread: RNNoise's FFT planner cache (thread-local) and every rate used.
    for rate in [48_000, 44_100, 96_000, 48_000] {
        for _ in 0..40 {
            cycle(&mut p, 480, rate);
        }
    }

    ARMED.store(true, Ordering::SeqCst);
    let rates = [48_000, 44_100, 96_000];
    let mut rate = 48_000;
    for i in 0..10_000 {
        let n = (rng.next() % 2048) as usize + 1;
        if rng.next().is_multiple_of(16) {
            handle.set_enabled(!handle.enabled.load(Ordering::Relaxed));
        }
        if rng.next().is_multiple_of(8) {
            handle.set_vad_gate((rng.next() % 100) as f32 / 100.0);
        }
        if i % 2_000 == 1_999 {
            rate = rates[(rng.next() % rates.len() as u64) as usize];
        }
        cycle(&mut p, n, rate);
    }
    ARMED.store(false, Ordering::SeqCst);

    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "allocations in the RT path"
    );
}
