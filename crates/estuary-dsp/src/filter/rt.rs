//! Realtime cycle (dsp M7.3), factored so tests can drive it with plain buffers.
//!
//! Partition rule for `CallbackData`: `rt` is touched only by the process callback (RT thread,
//! never concurrent with itself for one filter); `main` only on the engine thread; `shared` is
//! atomics. `CallbackData` lives pinned until after `pw_filter_destroy` returns.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::consts::{MAX_PORTS, MAX_QUANTUM};
use crate::processor::Processor;
use crate::stats::NodeStats;

use super::FilterState;

pub(crate) type StateListener = Box<dyn FnMut(&FilterState, Option<u32>)>;

pub(crate) struct CallbackData {
    pub rt: UnsafeCell<RtState>,
    pub main: MainState,
    pub shared: Shared,
}

pub(crate) struct Shared {
    /// Set before disconnect: late cycles output silence and skip the processor.
    pub shutdown: AtomicBool,
    pub stats: Arc<NodeStats>,
}

pub(crate) struct MainState {
    /// Copy of the filter pointer for engine-thread calls (never read `rt` from there).
    pub filter: Cell<*mut pipewire_sys::pw_filter>,
    pub state: RefCell<FilterState>,
    pub node_id: Cell<Option<u32>>,
    pub on_state: RefCell<Option<StateListener>>,
}

pub(crate) struct RtState {
    pub processor: Box<dyn Processor>,
    pub filter: *mut pipewire_sys::pw_filter,
    pub in_ports: [*mut c_void; MAX_PORTS],
    pub n_in: usize,
    pub out_ports: [*mut c_void; MAX_PORTS],
    pub n_out: usize,
    /// `MAX_QUANTUM` zeros, never written: what unlinked inputs read.
    pub silence: Box<[f32]>,
    /// `MAX_QUANTUM × MAX_PORTS`: discard target for missing output buffers.
    pub scratch: Box<[f32]>,
}

/// Port buffer access (real: `pw_filter_get_dsp_buffer`; tests: plain vectors).
pub(crate) trait PortIo {
    /// A pointer valid for `n` floats, or null. Called at most once per port per cycle.
    ///
    /// # Safety
    /// `port` must be a port handle of the filter being processed.
    unsafe fn dsp_buffer(&mut self, port: *mut c_void, n: u32) -> *mut f32;
    fn now_ns(&self) -> u64;
}

impl RtState {
    pub fn new(processor: Box<dyn Processor>, filter: *mut pipewire_sys::pw_filter) -> Self {
        Self {
            processor,
            filter,
            in_ports: [std::ptr::null_mut(); MAX_PORTS],
            n_in: 0,
            out_ports: [std::ptr::null_mut(); MAX_PORTS],
            n_out: 0,
            silence: vec![0.0; MAX_QUANTUM].into_boxed_slice(),
            scratch: vec![0.0; MAX_QUANTUM * MAX_PORTS].into_boxed_slice(),
        }
    }

    /// One graph cycle of `n` frames at `rate`.
    ///
    /// # Safety
    /// Must be called from the process callback only (or a test), with `io` returning
    /// pointers valid for `n` floats.
    pub unsafe fn run_cycle<I: PortIo>(
        &mut self,
        io: &mut I,
        shared: &Shared,
        n: usize,
        rate: u32,
    ) {
        let t0 = io.now_ns();
        let n_u32 = u32::try_from(n).unwrap_or(u32::MAX);
        // Phase 1: fetch every port pointer once, before anything can panic.
        let mut in_ptr = [std::ptr::null::<f32>(); MAX_PORTS];
        let mut out_ptr = [std::ptr::null_mut::<f32>(); MAX_PORTS];
        for (dst, port) in in_ptr.iter_mut().zip(&self.in_ports[..self.n_in]) {
            // SAFETY: `port` is one of this filter's port handles (caller contract).
            *dst = unsafe { io.dsp_buffer(*port, n_u32) };
        }
        for (dst, port) in out_ptr.iter_mut().zip(&self.out_ports[..self.n_out]) {
            // SAFETY: as above.
            *dst = unsafe { io.dsp_buffer(*port, n_u32) };
        }
        let outs = &out_ptr[..self.n_out];

        if shared.shutdown.load(Ordering::Acquire)
            || shared.stats.poisoned.load(Ordering::Relaxed)
            || rate == 0
            || n == 0
        {
            // SAFETY: each non-null pointer is valid for n floats (caller contract).
            unsafe { zero_outputs(outs, n) };
            return;
        }
        crate::fpu::flush_denormals();

        // Phase 2: process in chunks of at most MAX_QUANTUM, containing any panic.
        let Self {
            processor,
            silence,
            scratch,
            n_in,
            n_out,
            ..
        } = self;
        let (n_in, n_out) = (*n_in, *n_out);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut off = 0;
            while off < n {
                let len = (n - off).min(MAX_QUANTUM);
                let mut ins: [&[f32]; MAX_PORTS] = [&[]; MAX_PORTS];
                for (dst, p) in ins.iter_mut().zip(&in_ptr[..n_in]) {
                    *dst = if p.is_null() {
                        &silence[..len]
                    } else {
                        // SAFETY: valid for n floats; off + len <= n.
                        unsafe { std::slice::from_raw_parts(p.add(off), len) }
                    };
                }
                let mut outs_s: [&mut [f32]; MAX_PORTS] = Default::default();
                for ((dst, p), sc) in outs_s
                    .iter_mut()
                    .zip(&out_ptr[..n_out])
                    .zip(scratch.chunks_exact_mut(MAX_QUANTUM))
                {
                    *dst = if p.is_null() {
                        &mut sc[..len]
                    } else {
                        // SAFETY: valid for n floats, distinct per port; off + len <= n.
                        unsafe { std::slice::from_raw_parts_mut(p.add(off), len) }
                    };
                }
                processor.process(&ins[..n_in], &mut outs_s[..n_out], len, rate);
                off += len;
            }
        }));
        match result {
            Ok(()) => {
                shared
                    .stats
                    .processed_blocks
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(_payload) => {
                // Exceptional path: the payload's allocation was made by the panic machinery.
                shared.stats.poisoned.store(true, Ordering::Release);
                // SAFETY: as in phase 1.
                unsafe { zero_outputs(outs, n) };
            }
        }
        let dt = io.now_ns().saturating_sub(t0);
        shared.stats.max_process_ns.fetch_max(dt, Ordering::Relaxed);
        if dt.saturating_mul(u64::from(rate)) > (n as u64).saturating_mul(1_000_000_000) {
            shared.stats.xruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// # Safety
/// Each non-null pointer must be valid for `n` floats.
unsafe fn zero_outputs(outs: &[*mut f32], n: usize) {
    for p in outs.iter().filter(|p| !p.is_null()) {
        // SAFETY: caller contract.
        unsafe { std::ptr::write_bytes(*p, 0, n) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports are indices into `bufs`; a `None` buffer makes `dsp_buffer` return null.
    struct FakeIo {
        bufs: Vec<Option<Vec<f32>>>,
        clock: Cell<u64>,
        step_ns: u64,
    }

    impl PortIo for FakeIo {
        unsafe fn dsp_buffer(&mut self, port: *mut c_void, n: u32) -> *mut f32 {
            match self.bufs.get_mut(port as usize).and_then(Option::as_mut) {
                Some(b) => {
                    b.resize(n as usize, 0.0);
                    b.as_mut_ptr()
                }
                None => std::ptr::null_mut(),
            }
        }
        fn now_ns(&self) -> u64 {
            let t = self.clock.get();
            self.clock.set(t + self.step_ns);
            t
        }
    }

    /// Records chunk sizes and copies each input to the matching output, plus a constant.
    struct Recorder {
        calls: Arc<std::sync::Mutex<Vec<usize>>>,
        add: f32,
        panic: bool,
    }

    impl Processor for Recorder {
        fn process(&mut self, ins: &[&[f32]], outs: &mut [&mut [f32]], n: usize, _rate: u32) {
            assert!(!self.panic, "boom");
            self.calls.lock().unwrap().push(n);
            for (i, o) in outs.iter_mut().enumerate() {
                for k in 0..n {
                    o[k] = ins.get(i).map_or(0.0, |s| s[k]) + self.add;
                }
            }
        }
    }

    fn rt_with(p: Recorder, n_in: usize, n_out: usize) -> RtState {
        let mut rt = RtState::new(Box::new(p), std::ptr::null_mut());
        for i in 0..n_in {
            rt.in_ports[i] = i as *mut c_void;
        }
        for o in 0..n_out {
            rt.out_ports[o] = (n_in + o) as *mut c_void;
        }
        rt.n_in = n_in;
        rt.n_out = n_out;
        rt
    }

    fn shared() -> Shared {
        Shared {
            shutdown: AtomicBool::new(false),
            stats: Arc::default(),
        }
    }

    fn recorder() -> (Recorder, Arc<std::sync::Mutex<Vec<usize>>>) {
        let calls = Arc::default();
        (
            Recorder {
                calls: Arc::clone(&calls),
                add: 0.5,
                panic: false,
            },
            calls,
        )
    }

    #[test]
    fn null_input_is_silence_and_null_output_goes_to_scratch() {
        let (p, _) = recorder();
        let mut rt = rt_with(p, 2, 2);
        let mut io = FakeIo {
            bufs: vec![Some(vec![1.0; 4]), None, None, Some(vec![9.0; 4])],
            clock: Cell::new(0),
            step_ns: 1,
        };
        let sh = shared();
        // SAFETY: test buffers sized by dsp_buffer.
        unsafe { rt.run_cycle(&mut io, &sh, 4, 48_000) };
        assert_eq!(
            io.bufs[3].as_ref().unwrap(),
            &vec![0.5; 4],
            "input 1 was silence (0 + 0.5)"
        );
        assert_eq!(sh.stats.processed_blocks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn large_quanta_are_chunked() {
        let (p, calls) = recorder();
        let mut rt = rt_with(p, 1, 1);
        let mut io = FakeIo {
            bufs: vec![Some(vec![]), Some(vec![])],
            clock: Cell::new(0),
            step_ns: 1,
        };
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &shared(), 20_000, 48_000) };
        assert_eq!(*calls.lock().unwrap(), vec![8192, 8192, 3616]);
    }

    #[test]
    fn panic_poisons_and_silences() {
        let (mut p, calls) = recorder();
        p.panic = true;
        let mut rt = rt_with(p, 1, 1);
        let mut io = FakeIo {
            bufs: vec![Some(vec![1.0; 8]), Some(vec![7.0; 8])],
            clock: Cell::new(0),
            step_ns: 1,
        };
        let sh = shared();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the test output clean
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &sh, 8, 48_000) };
        std::panic::set_hook(hook);
        assert!(sh.stats.poisoned.load(Ordering::Relaxed));
        assert_eq!(
            io.bufs[1].as_ref().unwrap(),
            &vec![0.0; 8],
            "outputs zeroed"
        );
        io.bufs[1] = Some(vec![7.0; 8]);
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &sh, 8, 48_000) };
        assert_eq!(
            io.bufs[1].as_ref().unwrap(),
            &vec![0.0; 8],
            "poisoned node stays silent"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "processor never ran successfully"
        );
    }

    #[test]
    fn shutdown_flag_silences_without_processing() {
        let (p, calls) = recorder();
        let mut rt = rt_with(p, 1, 1);
        let mut io = FakeIo {
            bufs: vec![Some(vec![1.0; 4]), Some(vec![7.0; 4])],
            clock: Cell::new(0),
            step_ns: 1,
        };
        let sh = shared();
        sh.shutdown.store(true, Ordering::Release);
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &sh, 4, 48_000) };
        assert_eq!(io.bufs[1].as_ref().unwrap(), &vec![0.0; 4]);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn xrun_accounting() {
        let (p, _) = recorder();
        let mut rt = rt_with(p, 1, 1);
        // 480 frames at 48 kHz = 10 ms budget; the fake clock advances 20 ms per reading.
        let mut io = FakeIo {
            bufs: vec![Some(vec![]), Some(vec![])],
            clock: Cell::new(0),
            step_ns: 20_000_000,
        };
        let sh = shared();
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &sh, 480, 48_000) };
        assert_eq!(sh.stats.xruns.load(Ordering::Relaxed), 1);
        assert_eq!(sh.stats.max_process_ns.load(Ordering::Relaxed), 20_000_000);
        io.step_ns = 1_000; // 1 µs: within budget
        // SAFETY: as above.
        unsafe { rt.run_cycle(&mut io, &sh, 480, 48_000) };
        assert_eq!(sh.stats.xruns.load(Ordering::Relaxed), 1);
    }
}
