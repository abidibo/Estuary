//! Realtime DSP nodes for Estuary. `unsafe` is permitted in this crate only (master C1),
//! confined to the pw_filter FFI and always behind a safe API.
#![deny(unsafe_code)] // individual FFI modules opt in with #[allow(unsafe_code)] + SAFETY comments
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod consts;
mod error;
mod filter;
mod fpu;
mod gain;
mod processor;
mod stats;

pub use consts::{GAIN_RAMP_SECS, MAX_PORTS, MAX_QUANTUM};
pub use error::DspError;
pub use filter::{FilterNode, FilterState};
pub use gain::{GainHandle, GainProcessor, db_to_linear, gain_processor, new_gain_node};
pub use processor::{PortSpec, Processor, STEREO_IN, STEREO_OUT};
pub use stats::NodeStats;
