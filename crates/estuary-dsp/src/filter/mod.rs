//! Safe wrapper over `pw_filter` (dsp M7.5, master C11): a PipeWire node whose processing runs a
//! Rust `Processor` on the RT thread. All `unsafe` stays in this module tree.
#![allow(unsafe_code)]

mod ffi;
mod props;
pub(crate) mod rt;

pub(crate) use props::process_latency_param;

use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::CString;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pipewire::properties::{PropertiesBox, properties};
use pipewire::spa::utils::Direction;
use pipewire_sys as pw_sys;

use self::rt::{CallbackData, MainState, RtState, Shared};
use crate::consts::MAX_PORTS;
use crate::error::DspError;
use crate::processor::{PortSpec, Processor};
use crate::stats::NodeStats;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterState {
    Unconnected,
    Connecting,
    Paused,
    Streaming,
    Error(String),
}

/// A connected `pw_filter` node. Lives and dies on the engine thread (C2): `!Send`, `!Sync`.
pub struct FilterNode {
    filter: NonNull<pw_sys::pw_filter>,
    // Dropped after `Drop::drop` destroyed the filter, in this order.
    _listener: Pin<Box<libspa_sys::spa_hook>>,
    _events: Pin<Box<pw_sys::pw_filter_events>>,
    data: Pin<Box<CallbackData>>,
    _not_send: PhantomData<*mut ()>,
}

impl std::fmt::Debug for FilterNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterNode")
            .field("state", &self.state())
            .field("node_id", &self.node_id())
            .finish_non_exhaustive()
    }
}

impl FilterNode {
    /// Creates and connects the filter. Must be called on the engine thread that owns `core`.
    /// The port order defines the processor's `inputs` / `outputs` indices.
    pub fn new<P: Processor>(
        core: &pipewire::core::Core,
        name: &str,
        props: PropertiesBox,
        inputs: &[PortSpec],
        outputs: &[PortSpec],
        processor: P,
    ) -> Result<FilterNode, DspError> {
        Self::new_with_params(core, name, props, inputs, outputs, processor, &[])
    }

    /// As `new`, plus global params (serialized SPA pods) passed to `pw_filter_connect`.
    pub(crate) fn new_with_params<P: Processor>(
        core: &pipewire::core::Core,
        name: &str,
        props: PropertiesBox,
        inputs: &[PortSpec],
        outputs: &[PortSpec],
        processor: P,
        params: &[Vec<u8>],
    ) -> Result<FilterNode, DspError> {
        if name.is_empty() || name.contains('\0') {
            return Err(DspError::InvalidName(name.to_owned()));
        }
        for (dir, ports) in [("input", inputs), ("output", outputs)] {
            if ports.len() > MAX_PORTS {
                return Err(DspError::TooManyPorts {
                    dir,
                    n: ports.len(),
                });
            }
        }
        let mut props = props;
        if props.get("node.name").is_none() {
            props.insert("node.name", name);
        }
        let c_name = CString::new(name).map_err(|_| DspError::InvalidName(name.to_owned()))?;

        // SAFETY: core is a live pw_core on this thread; props ownership passes to C.
        let raw =
            unsafe { pw_sys::pw_filter_new(core.as_raw_ptr(), c_name.as_ptr(), props.into_raw()) };
        let Some(filter) = NonNull::new(raw) else {
            return Err(DspError::FilterNew(std::io::Error::last_os_error()));
        };

        let stats = Arc::new(NodeStats::default());
        let mut processor = processor;
        processor.attach_stats(Arc::clone(&stats));
        let data = Box::pin(CallbackData {
            rt: UnsafeCell::new(RtState::new(Box::new(processor), raw)),
            main: MainState {
                filter: Cell::new(raw),
                state: RefCell::new(FilterState::Unconnected),
                node_id: Cell::new(None),
                on_state: RefCell::new(None),
            },
            shared: Shared {
                shutdown: AtomicBool::new(false),
                stats,
            },
        });
        let events = Box::pin(pw_sys::pw_filter_events {
            version: pw_sys::PW_VERSION_FILTER_EVENTS,
            destroy: Some(ffi::on_destroy),
            state_changed: Some(ffi::on_state_changed),
            io_changed: None,
            param_changed: None,
            add_buffer: None,
            remove_buffer: None,
            process: Some(ffi::on_process),
            drained: None,
            command: None,
        });
        // SAFETY: an all-zero spa_hook is its documented initial state.
        let listener: Pin<Box<libspa_sys::spa_hook>> = Box::pin(unsafe { std::mem::zeroed() });
        let mut node = FilterNode {
            filter,
            _listener: listener,
            _events: events,
            data,
            _not_send: PhantomData,
        };
        // SAFETY: the hook, events table and callback data are pinned and owned by `node`, which
        // destroys the filter before freeing them (Drop). The hook is never moved after this.
        unsafe {
            pw_sys::pw_filter_add_listener(
                raw,
                node._listener.as_mut().get_unchecked_mut(),
                &raw const *node._events,
                (&raw const *node.data).cast_mut().cast(),
            );
        }

        for (dir, ports) in [(Direction::Input, inputs), (Direction::Output, outputs)] {
            for spec in ports {
                let pprops = properties! {
                    "format.dsp" => "32 bit float mono audio",
                    "port.name" => spec.name,
                    "audio.channel" => spec.channel,
                };
                // SAFETY: filter is live and not connected yet; props ownership passes to C.
                let port = unsafe {
                    pw_sys::pw_filter_add_port(
                        raw,
                        dir.as_raw(),
                        pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                        0,
                        pprops.into_raw(),
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if port.is_null() {
                    let source = std::io::Error::last_os_error();
                    drop(node); // destroys the filter
                    return Err(DspError::AddPort {
                        port: spec.name,
                        source,
                    });
                }
                // SAFETY: not connected yet, so no process callback can observe `rt`.
                let rt = unsafe { &mut *node.data.rt.get() };
                if dir == Direction::Input {
                    rt.in_ports[rt.n_in] = port;
                    rt.n_in += 1;
                } else {
                    rt.out_ports[rt.n_out] = port;
                    rt.n_out += 1;
                }
            }
        }

        // Followers scheduled by the graph: never DRIVER, TRIGGER or ASYNC.
        let mut pods: Vec<*const libspa_sys::spa_pod> = params
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.as_ptr().cast())
            .collect();
        let n_pods = u32::try_from(pods.len()).unwrap_or(0);
        // SAFETY: filter is live; each pod pointer refers to a complete serialized pod in
        // `params`, which outlives the call (PipeWire copies params during connect).
        let res = unsafe {
            pw_sys::pw_filter_connect(
                raw,
                pw_sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                if pods.is_empty() {
                    std::ptr::null_mut()
                } else {
                    pods.as_mut_ptr()
                },
                n_pods,
            )
        };
        if res < 0 {
            let source = std::io::Error::from_raw_os_error(-res);
            drop(node);
            return Err(DspError::Connect(source));
        }
        Ok(node)
    }

    /// Known once the filter reached `Paused`/`Streaming`.
    pub fn node_id(&self) -> Option<u32> {
        self.data.main.node_id.get()
    }

    pub fn state(&self) -> FilterState {
        self.data.main.state.borrow().clone()
    }

    pub fn stats(&self) -> Arc<NodeStats> {
        Arc::clone(&self.data.shared.stats)
    }

    /// Called on the engine thread on every state change, with the node id once known.
    pub fn set_state_listener(&self, f: impl FnMut(&FilterState, Option<u32>) + 'static) {
        *self.data.main.on_state.borrow_mut() = Some(Box::new(f));
    }
}

impl Drop for FilterNode {
    fn drop(&mut self) {
        // Late RT cycles output silence and skip the processor.
        self.data.shared.shutdown.store(true, Ordering::Release);
        // SAFETY: the filter is live; destroy also cleans our hook. The pinned listener, events
        // and data are freed only after this returns (fields drop after Drop::drop).
        unsafe {
            pw_sys::pw_filter_disconnect(self.filter.as_ptr());
            pw_sys::pw_filter_destroy(self.filter.as_ptr());
        }
    }
}
