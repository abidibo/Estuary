//! Per-node counters shared with the engine (master C11 amended). Atomics only.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

#[derive(Debug, Default)]
pub struct NodeStats {
    /// Cycles whose own processing time exceeded the quantum period (n / rate).
    pub xruns: AtomicU64,
    /// Successful process cycles.
    pub processed_blocks: AtomicU64,
    /// Longest observed processing time; the engine may reset it with `store(0)`.
    pub max_process_ns: AtomicU64,
    /// A panic occurred; the node outputs silence until the engine recreates it.
    pub poisoned: AtomicBool,
    /// Denoiser output FIFO underruns (should stay 0).
    pub underruns: AtomicU64,
    /// Last graph rate the denoiser could not handle (0 = none).
    pub unsupported_rate: AtomicU32,
    /// Graph rate and quantum of the latest cycle (diagnostics).
    pub last_rate: AtomicU32,
    pub last_quantum: AtomicU32,
}
