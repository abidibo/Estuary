//! What the pure core asks the PipeWire shell to do.

use std::time::Duration;

use crate::Event;
use crate::meter::MeterSlotId;
use crate::model::Volume;

/// Objects the engine owns or tracks by proxy. Grows with the reconciler (M8).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum OwnedKey {
    /// The `default` metadata proxy used for the C13 write probe.
    ProbeMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PwOp {
    MetaWrite {
        subject: u32,
        key: &'static str,
        type_: Option<&'static str>,
        value: Option<String>,
    },
    Sync {
        purpose: SyncPurpose,
    },
    CreateMeter {
        slot: MeterSlotId,
        target_serial: u64,
        capture_sink: bool,
        autoconnect: bool,
    },
    DestroyMeter {
        slot: MeterSlotId,
    },
    SetNodeVolume {
        node_id: u32,
        volume: Volume,
    },
    SetRouteVolume {
        card_id: u32,
        route_index: i32,
        route_device: i32,
        volume: Volume,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncPurpose {
    InitialRegistry,
    WriteProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerKind {
    Reconnect,
    SnapshotFlush,
    WpWait,
    /// C7: a new stream settles 500 ms after appearing if no link settled it earlier.
    StreamSettle(u32),
}

#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub ops: Vec<PwOp>,
    pub events: Vec<Event>,
    /// Arm or re-arm.
    pub timers: Vec<(TimerKind, Duration)>,
    pub cancel_timers: Vec<TimerKind>,
    /// Drop core/registry/proxies; a reconnect follows on the `Reconnect` timer.
    pub reset_connection: bool,
    /// Attempt `connect_rc` now (initial start and `Reconnect` timer).
    pub connect_now: bool,
}

impl Effects {
    pub fn merge(&mut self, mut other: Effects) {
        self.ops.append(&mut other.ops);
        self.events.append(&mut other.events);
        self.timers.append(&mut other.timers);
        self.cancel_timers.append(&mut other.cancel_timers);
        self.reset_connection |= other.reset_connection;
        self.connect_now |= other.connect_now;
    }
}
