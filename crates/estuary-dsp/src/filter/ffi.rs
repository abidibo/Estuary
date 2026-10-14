//! `extern "C"` callbacks for `pw_filter` (dsp M7.5). Every body contains panics: unwinding
//! across the FFI boundary would abort the process.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;

use pipewire_sys as pw_sys;

use super::FilterState;
use super::rt::{CallbackData, PortIo};

/// Real port access for the RT thread.
pub(crate) struct FfiIo {
    pub filter: *mut pw_sys::pw_filter,
}

impl PortIo for FfiIo {
    unsafe fn dsp_buffer(&mut self, port: *mut c_void, n: u32) -> *mut f32 {
        // SAFETY: `port` is a handle returned by pw_filter_add_port for this filter, and we are
        // inside its process callback (the only place buffers may be dequeued).
        unsafe { pw_sys::pw_filter_get_dsp_buffer(port, n).cast() }
    }

    fn now_ns(&self) -> u64 {
        // SAFETY: pw_filter_get_nsec is documented RT-safe; `filter` is alive during process.
        unsafe { pw_sys::pw_filter_get_nsec(self.filter) }
    }
}

/// RT thread: one graph cycle.
pub(crate) unsafe extern "C" fn on_process(
    data: *mut c_void,
    position: *mut libspa_sys::spa_io_position,
) {
    if data.is_null() || position.is_null() {
        return;
    }
    // SAFETY: `data` is the pinned CallbackData registered with the listener; it outlives the
    // filter (freed only after pw_filter_destroy returns). Only the RT thread touches `rt`.
    let cb = unsafe { &*data.cast::<CallbackData>() };
    // SAFETY: PipeWire passes a valid position for the duration of the callback.
    let (n, rate) = unsafe {
        let clock = &(*position).clock;
        (
            usize::try_from(clock.duration).unwrap_or(0),
            clock.rate.denom,
        )
    };
    // SAFETY: exclusive RT access per the partition rule (process is never re-entered).
    let rt = unsafe { &mut *cb.rt.get() };
    let mut io = FfiIo { filter: rt.filter };
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: called from the process callback with this filter's own ports.
        unsafe { rt.run_cycle(&mut io, &cb.shared, n, rate) };
    }));
    if outcome.is_err() {
        // Backstop only: run_cycle already contains processor panics.
        cb.shared.stats.poisoned.store(true, Ordering::Release);
    }
}

/// Engine thread: state transitions.
pub(crate) unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: pw_sys::pw_filter_state,
    new: pw_sys::pw_filter_state,
    error: *const c_char,
) {
    if data.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in on_process; only the `main` and `shared` fields are used here.
        let cb = unsafe { &*data.cast::<CallbackData>() };
        let state = match new {
            pw_sys::pw_filter_state_PW_FILTER_STATE_UNCONNECTED => FilterState::Unconnected,
            pw_sys::pw_filter_state_PW_FILTER_STATE_CONNECTING => FilterState::Connecting,
            pw_sys::pw_filter_state_PW_FILTER_STATE_PAUSED => FilterState::Paused,
            pw_sys::pw_filter_state_PW_FILTER_STATE_STREAMING => FilterState::Streaming,
            _ => FilterState::Error(if error.is_null() {
                String::new()
            } else {
                // SAFETY: PipeWire passes a NUL-terminated error string or null.
                unsafe { CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            }),
        };
        if matches!(state, FilterState::Paused | FilterState::Streaming) {
            // SAFETY: the main-state copy of the filter pointer is valid until destroy.
            let id = unsafe { pw_sys::pw_filter_get_node_id(cb.main.filter.get()) };
            if id != libspa_sys::SPA_ID_INVALID {
                cb.main.node_id.set(Some(id));
            }
        }
        if let FilterState::Error(e) = &state {
            tracing::warn!(error = %e, "pw_filter entered the error state");
        }
        *cb.main.state.borrow_mut() = state.clone();
        if let Some(listener) = cb.main.on_state.borrow_mut().as_mut() {
            listener(&state, cb.main.node_id.get());
        }
    }));
}

/// Engine thread: the filter is being destroyed.
pub(crate) unsafe extern "C" fn on_destroy(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in on_state_changed.
        let cb = unsafe { &*data.cast::<CallbackData>() };
        *cb.main.state.borrow_mut() = FilterState::Unconnected;
        cb.main.node_id.set(None);
    }));
}
