//! The realtime processing contract (master C11).

use std::sync::Arc;

use crate::stats::NodeStats;

/// Realtime processor, called on the PipeWire RT thread.
pub trait Processor: Send + 'static {
    /// MUST NOT allocate, lock, block, log or make syscalls.
    ///
    /// `inputs.len()` / `outputs.len()` equal the port counts given to `FilterNode::new`, in the
    /// same order. Unlinked inputs are silence. Every output sample in `0..n_samples` must be
    /// written. Guarantee (C11 amended): `1 <= n_samples <= MAX_QUANTUM` and `rate > 0`.
    fn process(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        n_samples: usize,
        rate: u32,
    );

    /// Called once by `FilterNode::new` before connecting (C11 amended).
    fn attach_stats(&mut self, _stats: Arc<NodeStats>) {}
}

/// A DSP port: `port.name` and `audio.channel` (C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSpec {
    pub name: &'static str,
    pub channel: &'static str,
}

pub const STEREO_IN: [PortSpec; 2] = [
    PortSpec {
        name: "input_FL",
        channel: "FL",
    },
    PortSpec {
        name: "input_FR",
        channel: "FR",
    },
];
pub const STEREO_OUT: [PortSpec; 2] = [
    PortSpec {
        name: "output_FL",
        channel: "FL",
    },
    PortSpec {
        name: "output_FR",
        channel: "FR",
    },
];
