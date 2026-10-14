//! Realtime limits (dsp M7.1).

/// PipeWire's default `clock.max-quantum`; larger cycles are processed in chunks.
pub const MAX_QUANTUM: usize = 8192;
/// Ports per direction.
pub const MAX_PORTS: usize = 8;
/// Gain changes ramp linearly over this long, at any rate (C11).
pub const GAIN_RAMP_SECS: f32 = 0.010;
