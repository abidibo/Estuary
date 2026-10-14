//! DSP errors (dsp M7.1). Each captures `errno` right after the failing libpipewire call.

use crate::consts::MAX_PORTS;

#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("too many {dir} ports: {n} (max {MAX_PORTS})")]
    TooManyPorts { dir: &'static str, n: usize },
    #[error("invalid node name {0:?} (must be non-empty, no NUL)")]
    InvalidName(String),
    #[error("pw_filter_new failed: {0}")]
    FilterNew(std::io::Error),
    #[error("pw_filter_add_port({port}) failed: {source}")]
    AddPort {
        port: &'static str,
        source: std::io::Error,
    },
    #[error("pw_filter_connect failed: {0}")]
    Connect(std::io::Error),
}
