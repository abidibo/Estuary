//! What the pure core asks the PipeWire shell to do.

use std::time::Duration;

use crate::Event;
use crate::graph::Props;
use crate::meter::MeterSlotId;
use crate::model::{CellKey, ChannelId, VirtualMicId, Volume};

/// Objects the engine owns or tracks by proxy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum OwnedKey {
    /// The `default` metadata proxy used for the C13 write probe.
    ProbeMeta,
    Channel(ChannelId),
    Vmic(VirtualMicId),
    Cell(CellKey),
    Link(LinkKey),
    /// An owned node no setup item explains (e.g. a cell from an older setup).
    StaleNode(u32),
}

/// A link is identified by its port pair (unique within a connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LinkKey {
    pub out_port: u32,
    pub in_port: u32,
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
    /// Channel or virtual mic (C5 adapter props).
    CreateAdapter {
        key: OwnedKey,
        props: Props,
    },
    CreateCell {
        key: CellKey,
        name: String,
        props: Props,
        initial_db: f32,
    },
    CreateLink {
        key: LinkKey,
        out_node: u32,
        out_port: u32,
        in_node: u32,
        in_port: u32,
        passive: bool,
    },
    Destroy {
        key: OwnedKey,
        global_id: Option<u32>,
    },
    SetGain {
        key: CellKey,
        db: f32,
    },
    /// Read node stats (M8.12); the shell reports findings as graph changes.
    PollStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncPurpose {
    InitialRegistry,
    WriteProbe,
    Teardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerKind {
    Reconnect,
    SnapshotFlush,
    WpWait,
    /// C7: a new stream settles 500 ms after appearing if no link settled it earlier.
    StreamSettle(u32),
    /// C8: debounced reconcile pass (also re-runs for fades and pending timeouts).
    Reconcile,
    StatsPoll,
    TeardownTimeout,
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
