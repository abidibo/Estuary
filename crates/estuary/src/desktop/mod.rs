//! Desktop integration (estuary-desktop sub-spec, M11): single instance, background mode,
//! quitting, start at login, tray and signals.

pub mod app;
pub mod autostart;
pub mod close;
pub mod env;
pub mod quit;
pub mod signals;
pub mod tray;
pub mod window;
