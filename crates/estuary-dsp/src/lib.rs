//! Realtime DSP nodes for Estuary. `unsafe` is permitted in this crate only (master C1),
//! confined to the pw_filter FFI and always behind a safe API.
#![deny(unsafe_code)] // individual FFI modules opt in with #[allow(unsafe_code)] + SAFETY comments
