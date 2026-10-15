//! The engine's functional core: graph changes, commands and timers in, `Effects` out.
//! No PipeWire objects live here, so all of it is unit-testable (engine "functional core").

pub(crate) mod conn;
pub(crate) mod desired;
pub(crate) mod effects;
pub(crate) mod meters;
pub(crate) mod ports;
pub(crate) mod quit;
pub(crate) mod reconcile;
pub(crate) mod routing;
#[cfg(test)]
mod scenarios;
pub(crate) mod snapshot;

use std::sync::Arc;
use std::time::{Duration, Instant};

use conn::{CapabilityResult, ConnState, evaluate_capabilities};
use effects::{Effects, OwnedKey, PwOp, SyncPurpose, TimerKind};
use meters::MeterPlan;
use reconcile::Reconciler;
use routing::{RoutingOutput, RoutingState, TARGET_KEY};

use crate::error::{Backoff, UnsupportedReason};
use crate::error::{EngineWarning, SetupIssue, WarnedNode};
use crate::graph::{GraphChange, GraphMirror};
use crate::setup::Setup;
use crate::{Command, ConnectionState, Event};

const SNAPSHOT_COALESCE: Duration = Duration::from_millis(50);
const WP_WAIT: Duration = Duration::from_secs(5);
/// C7: a stream counts as settled at most this long after appearing.
const STREAM_SETTLE: Duration = Duration::from_millis(500);
/// C8: graph changes are reconciled after this debounce.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(30);
const STATS_POLL: Duration = Duration::from_secs(1);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// After a reconnect, `Restored` is sent once the rebuild settles, or after this long.
const RESTORE_MAX_WAIT: Duration = Duration::from_secs(5);
/// F12: more xruns than this within a minute on one cell → warning (at most once a minute).
const XRUNS_PER_MINUTE_WARN: u64 = 10;

/// What the engine remembers of a denoiser's counters between stats polls.
#[derive(Debug, Default, Clone)]
struct DenoiserTrack {
    xruns: u64,
    underruns: u64,
    underrun_log: Option<Instant>,
    unsupported_rate: u32,
}

#[derive(Debug)]
pub(crate) struct EngineCore {
    instance: String,
    backoff: Backoff,
    state: ConnState,
    mirror: GraphMirror,
    setup: Option<Setup>,
    meters: MeterPlan,
    routing: RoutingState,
    reconciler: Reconciler,
    /// When the armed Reconcile timer fires; an earlier request re-arms it.
    reconcile_due: Option<Instant>,
    stats_armed: bool,
    /// After a reconnect: when Ready happened, until `Restored` is sent.
    restore_pending: Option<Instant>,
    torn_down: bool,
    /// Per-cell xrun history for F12: (first total seen this minute, when, last warning).
    /// Per DSP node: (count at the start of the minute, minute start, last warning).
    xruns: std::collections::HashMap<WarnedNode, (u64, Instant, Option<Instant>)>,
    /// Per denoiser: latest counters from the stats poll (M10.2).
    denoisers: std::collections::HashMap<crate::model::DeviceKey, DenoiserTrack>,
    had_ready_before: bool,
    /// Sync requests in flight: purpose → sequence number returned by `core.sync()`.
    pending_syncs: Vec<(SyncPurpose, i32)>,
    probe_failed: bool,
    snapshot_dirty: bool,
    flush_armed: bool,
}

impl EngineCore {
    pub fn new(instance: String, backoff: Backoff) -> Self {
        Self {
            instance,
            backoff,
            state: ConnState::Connecting { attempt: 0 },
            mirror: GraphMirror::default(),
            setup: None,
            meters: MeterPlan::new(),
            routing: RoutingState::default(),
            reconciler: Reconciler::default(),
            reconcile_due: None,
            stats_armed: false,
            restore_pending: None,
            torn_down: false,
            xruns: std::collections::HashMap::new(),
            denoisers: std::collections::HashMap::new(),
            had_ready_before: false,
            pending_syncs: Vec::new(),
            probe_failed: false,
            snapshot_dirty: false,
            flush_armed: false,
        }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &ConnState {
        &self.state
    }

    /// First call after the thread starts.
    pub fn start(&mut self) -> Effects {
        Effects {
            events: vec![Event::Connection(ConnectionState::Connecting {
                attempt: 0,
            })],
            connect_now: true,
            ..Default::default()
        }
    }

    /// Result of a `connect_rc` attempt by the shell.
    pub fn on_connect_result(&mut self, ok: bool) -> Effects {
        let mut fx = Effects::default();
        if ok {
            self.state = ConnState::Syncing;
            fx.ops.push(PwOp::Sync {
                purpose: SyncPurpose::InitialRegistry,
            });
            return fx;
        }
        let next = match &self.state {
            ConnState::Connecting { attempt } => {
                let attempt = attempt + 1;
                fx.events
                    .push(Event::Connection(ConnectionState::Connecting { attempt }));
                self.state = ConnState::Connecting { attempt };
                attempt
            }
            ConnState::Reconnecting { since, attempt } => {
                let (since, attempt) = (*since, attempt + 1);
                fx.events
                    .push(Event::Connection(ConnectionState::Reconnecting {
                        since,
                        attempt,
                    }));
                self.state = ConnState::Reconnecting { since, attempt };
                attempt
            }
            _ => 0,
        };
        fx.timers.push((
            TimerKind::Reconnect,
            self.backoff.delay(next.saturating_sub(1)),
        ));
        fx
    }

    /// The shell issued `core.sync()` for `purpose` and got `seq` back.
    pub fn on_sync_issued(&mut self, purpose: SyncPurpose, seq: i32) {
        self.pending_syncs.retain(|(p, _)| *p != purpose);
        self.pending_syncs.push((purpose, seq));
    }

    pub fn on_graph(&mut self, change: &GraphChange) -> Effects {
        let mut fx = Effects::default();
        match change {
            GraphChange::Disconnected => return self.disconnected(),
            GraphChange::SyncDone { seq } => {
                if let Some(pos) = self.pending_syncs.iter().position(|(_, s)| s == seq) {
                    let (purpose, _) = self.pending_syncs.remove(pos);
                    fx.merge(self.sync_done(purpose));
                }
                return fx;
            }
            GraphChange::ProxyError {
                key: OwnedKey::ProbeMeta,
                res,
                message,
            } => {
                tracing::warn!(res, %message, "write probe on the default metadata failed");
                if self.state == ConnState::Probing {
                    self.probe_failed = true;
                }
                return fx;
            }
            GraphChange::VolumeWriteFailed { node_id } => {
                if let Some(n) = self.mirror.nodes.get(node_id) {
                    tracing::warn!(node = node_id, "volume write failed");
                    if matches!(
                        n.kind,
                        crate::graph::NodeKind::Output | crate::graph::NodeKind::Input
                    ) && let Some(key) = n.props.get("node.name")
                    {
                        fx.events
                            .push(Event::Warning(EngineWarning::VolumeWriteFailed {
                                node: WarnedNode::Device(crate::model::DeviceKey(key.clone())),
                            }));
                    }
                }
                return fx;
            }
            GraphChange::ProxyError { key, res, message } => {
                tracing::warn!(?key, res, %message, "creating an owned object failed");
                self.reconciler.on_failure(key);
                fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
                return fx;
            }
            GraphChange::CreateFailed { key, message } => {
                tracing::warn!(?key, %message, "creating an owned object failed");
                self.reconciler.on_failure(key);
                if let Some(node) = warned_node(key) {
                    fx.events
                        .push(Event::Warning(EngineWarning::NodeCreateFailed {
                            node,
                            message: message.clone(),
                        }));
                }
                fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
                return fx;
            }
            GraphChange::FilterError { key, message } => {
                fx.merge(self.cell_failed(key, message));
                return fx;
            }
            GraphChange::FilterPoisoned { key } => {
                fx.merge(self.cell_failed(key, "processing panicked; send recreated"));
                return fx;
            }
            GraphChange::FilterXruns { key, total } => {
                fx.merge(self.on_xruns(WarnedNode::Cell(key.clone()), *total));
                return fx;
            }
            GraphChange::DenoiserError { key, message } => {
                fx.merge(self.denoiser_failed(key, message));
                return fx;
            }
            GraphChange::DenoiserPoisoned { key } => {
                fx.merge(self.denoiser_failed(key, "processing panicked; recreated"));
                return fx;
            }
            GraphChange::DenoiserStats {
                key,
                xruns,
                underruns,
                unsupported_rate,
            } => {
                fx.merge(self.on_denoiser_stats(key, *xruns, *underruns, *unsupported_rate));
                return fx;
            }
            GraphChange::MeterStreamFailed { slot, message } => {
                tracing::warn!(slot = slot.0, %message, "meter stream failed");
                if let Some(target) = self.meters.on_failed(*slot) {
                    fx.events
                        .push(Event::Warning(EngineWarning::MeterFailed(target)));
                }
                fx.merge(self.reconcile_meters());
                return fx;
            }
            _ => {}
        }

        let impact = self.mirror.apply(change, &self.instance);
        if impact.capabilities
            && matches!(
                self.state,
                ConnState::WaitingForWp | ConnState::Unsupported(_)
            )
        {
            fx.merge(self.check_capabilities());
        }
        if impact.snapshot {
            fx.merge(self.mark_dirty());
        }
        if impact.topology {
            fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
            fx.merge(self.reconcile_meters());
            if self.state == ConnState::Ready {
                let setup = self.setup.as_ref();
                let mut out = self.routing.sync(&self.mirror, setup);
                out.merge_from(self.routing.on_links(&self.mirror, setup));
                out.merge_from(self.routing.evaluate_all(&self.mirror, setup));
                fx.merge(routing_effects(out));
            }
        }
        if let GraphChange::DefaultMetaProperty {
            subject,
            key,
            value,
            ..
        } = change
            && key == TARGET_KEY
            && self.state == ConnState::Ready
        {
            let out = self
                .routing
                .on_metadata(*subject, value.as_deref(), &self.mirror);
            fx.merge(routing_effects(out));
        }
        fx
    }

    pub fn on_command(&mut self, cmd: Command) -> Effects {
        let mut fx = Effects::default();
        match cmd {
            Command::SetSetup(setup) => {
                // One bad entry never blocks the rest: apply the sanitized setup (M6.2).
                let (setup, issues) = crate::setup::sanitize(setup);
                if !issues.is_empty() {
                    fx.events.push(Event::SetupIssues(issues));
                }
                self.setup = Some(setup);
                fx.merge(self.mark_dirty());
                self.reconciler.forget_failures();
                fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
                self.routing.on_setup();
                if self.state == ConnState::Ready {
                    let out = self.routing.evaluate_all(&self.mirror, self.setup.as_ref());
                    fx.merge(routing_effects(out));
                }
            }
            Command::SetAppVolume { app, volume } => {
                for n in self.mirror.nodes.values().filter(|n| {
                    n.kind == crate::graph::NodeKind::AppPlayback
                        && crate::appkey::derive(&n.props) == app
                }) {
                    let channels = n.volume.as_ref().map_or(2, |v| v.channels.len());
                    let v = if volume.channels.len() == channels {
                        volume.clone()
                    } else {
                        crate::model::Volume {
                            channels: vec![volume.average(); channels.max(1)],
                            muted: volume.muted,
                        }
                    };
                    fx.ops.push(PwOp::SetNodeVolume {
                        node_id: n.id,
                        volume: v,
                    });
                }
            }
            Command::SetDeviceVolume { key, volume } => {
                let node = self.mirror.nodes.values().find(|n| {
                    matches!(
                        n.kind,
                        crate::graph::NodeKind::Output | crate::graph::NodeKind::Input
                    ) && n.props.get("node.name") == Some(&key.0)
                });
                match node {
                    // M5.1 outcome B: hardware volume lives on the card's route.
                    Some(n) => match self.mirror.route_of(n) {
                        Some((card_id, route_index, route_device)) => {
                            fx.ops.push(PwOp::SetRouteVolume {
                                card_id,
                                route_index,
                                route_device,
                                volume,
                            });
                        }
                        None => fx.ops.push(PwOp::SetNodeVolume {
                            node_id: n.id,
                            volume,
                        }),
                    },
                    None => {
                        tracing::debug!(key = %key.0, "volume for a device that is not present");
                    }
                }
            }
            Command::SetMeterTargets(targets) => {
                self.meters.set_targets(targets);
                fx.merge(self.reconcile_meters());
            }
            Command::SetCellGain { cell, gain_db } => {
                let db = gain_db.clamp(crate::setup::GAIN_MIN_DB, crate::setup::GAIN_MAX_DB);
                if let Some(c) = self
                    .setup
                    .as_mut()
                    .and_then(|s| s.cells.iter_mut().find(|c| c.key() == cell))
                {
                    c.gain_db = db;
                }
                if self.owned_cell_exists(&cell) {
                    self.reconciler.on_gain_applied(&cell, db);
                    fx.ops.push(PwOp::SetGain { key: cell, db });
                }
            }
            Command::SetChannelVolume { id, volume } => {
                if let Some(c) = self
                    .setup
                    .as_mut()
                    .and_then(|s| s.channels.iter_mut().find(|c| c.id == id))
                {
                    c.volume = volume.clone();
                }
                if let Some(n) = self
                    .owned_node(|r| matches!(r, crate::graph::OwnedRole::Channel(c) if *c == id))
                {
                    fx.ops.push(PwOp::SetNodeVolume { node_id: n, volume });
                }
            }
            Command::SetVirtualMicVolume { id, volume } => {
                if let Some(v) = self
                    .setup
                    .as_mut()
                    .and_then(|s| s.virtual_mics.iter_mut().find(|v| v.id == id))
                {
                    v.volume = volume.clone();
                }
                if let Some(n) =
                    self.owned_node(|r| matches!(r, crate::graph::OwnedRole::Vmic(v) if *v == id))
                {
                    fx.ops.push(PwOp::SetNodeVolume { node_id: n, volume });
                }
            }
            Command::PrepareQuit => {
                let usages = self
                    .setup
                    .as_ref()
                    .map(|s| quit::usages(&self.mirror, s))
                    .unwrap_or_default();
                fx.events.push(Event::QuitCheck(usages));
            }
            Command::Teardown => fx.merge(self.teardown()),
            Command::SetVadGate { input, gate } => {
                let gate = if gate.is_nan() {
                    0.0
                } else {
                    gate.clamp(0.0, 0.95)
                };
                let def = self
                    .setup
                    .as_mut()
                    .and_then(|s| s.denoisers.iter_mut().find(|d| d.input == input));
                if let Some(d) = def {
                    d.vad_gate = gate;
                    let enabled = d.enabled;
                    if self.reconciler.denoiser_active(&input) {
                        self.reconciler.on_denoise_applied(&input, enabled, gate);
                        fx.ops.push(PwOp::SetDenoise {
                            key: input,
                            enabled,
                            vad_gate: gate,
                        });
                    }
                }
            }
        }
        fx
    }

    pub fn on_timer(&mut self, kind: TimerKind) -> Effects {
        let mut fx = Effects::default();
        match kind {
            TimerKind::Reconnect => {
                if matches!(
                    self.state,
                    ConnState::Connecting { .. } | ConnState::Reconnecting { .. }
                ) {
                    fx.connect_now = true;
                }
            }
            TimerKind::SnapshotFlush => {
                self.flush_armed = false;
                if self.snapshot_dirty && self.state.observing() {
                    fx.events.push(self.snapshot_event());
                }
            }
            TimerKind::WpWait => {
                if self.state == ConnState::WaitingForWp {
                    fx.merge(self.enter_unsupported(UnsupportedReason::NoWirePlumber));
                }
            }
            TimerKind::StreamSettle(id) => {
                if self.state == ConnState::Ready {
                    let out = self.routing.settle(id, &self.mirror, self.setup.as_ref());
                    fx.merge(routing_effects(out));
                }
            }
            TimerKind::Reconcile => {
                self.reconcile_due = None;
                fx.merge(self.reconcile());
            }
            TimerKind::StatsPoll => {
                self.stats_armed = false;
                if self.state == ConnState::Ready && self.any_owned_dsp() {
                    fx.ops.push(PwOp::PollStats);
                    self.stats_armed = true;
                    fx.timers.push((TimerKind::StatsPoll, STATS_POLL));
                }
            }
            TimerKind::TeardownTimeout => {
                if self.torn_down
                    && self
                        .pending_syncs
                        .iter()
                        .any(|(p, _)| *p == SyncPurpose::Teardown)
                {
                    tracing::warn!("teardown timed out");
                    self.pending_syncs
                        .retain(|(p, _)| *p != SyncPurpose::Teardown);
                    fx.events.push(Event::TeardownComplete);
                }
            }
        }
        fx
    }

    fn sync_done(&mut self, purpose: SyncPurpose) -> Effects {
        match (purpose, &self.state) {
            (SyncPurpose::InitialRegistry, ConnState::Syncing) => self.check_capabilities(),
            (SyncPurpose::Teardown, _) => Effects {
                events: vec![Event::TeardownComplete],
                cancel_timers: vec![TimerKind::TeardownTimeout],
                ..Default::default()
            },
            (SyncPurpose::WriteProbe, ConnState::Probing) => {
                if self.probe_failed {
                    self.state = ConnState::PermissionDenied;
                    let mut fx = Effects {
                        events: vec![Event::Connection(ConnectionState::PermissionDenied)],
                        ..Default::default()
                    };
                    fx.events.push(self.snapshot_event());
                    fx
                } else {
                    self.become_ready()
                }
            }
            _ => Effects::default(),
        }
    }

    fn check_capabilities(&mut self) -> Effects {
        let mut fx = Effects::default();
        match evaluate_capabilities(&self.mirror) {
            CapabilityResult::Ok { .. } => {
                fx.cancel_timers.push(TimerKind::WpWait);
                self.state = ConnState::Probing;
                self.probe_failed = false;
                fx.ops.extend([
                    PwOp::MetaWrite {
                        subject: 0,
                        key: "estuary.probe",
                        type_: Some("Spa:String"),
                        value: Some("1".to_owned()),
                    },
                    PwOp::MetaWrite {
                        subject: 0,
                        key: "estuary.probe",
                        type_: None,
                        value: None,
                    },
                    PwOp::Sync {
                        purpose: SyncPurpose::WriteProbe,
                    },
                ]);
            }
            CapabilityResult::WaitForWp => {
                if !matches!(
                    self.state,
                    ConnState::WaitingForWp | ConnState::Unsupported(_)
                ) {
                    self.state = ConnState::WaitingForWp;
                    fx.timers.push((TimerKind::WpWait, WP_WAIT));
                }
            }
            CapabilityResult::Unsupported(reason) => fx.merge(self.enter_unsupported(reason)),
        }
        fx
    }

    fn enter_unsupported(&mut self, reason: UnsupportedReason) -> Effects {
        if self.state == ConnState::Unsupported(reason.clone()) {
            return Effects::default();
        }
        self.state = ConnState::Unsupported(reason.clone());
        let mut fx = Effects {
            events: vec![Event::Connection(ConnectionState::Unsupported { reason })],
            ..Default::default()
        };
        fx.events.push(self.snapshot_event());
        fx
    }

    fn become_ready(&mut self) -> Effects {
        self.state = ConnState::Ready;
        let (pw, wp) = match evaluate_capabilities(&self.mirror) {
            CapabilityResult::Ok { pw, wp } => (pw, wp),
            _ => ("unknown".to_owned(), None),
        };
        let mut fx = Effects {
            events: vec![Event::Connection(ConnectionState::Connected {
                pipewire_version: pw,
                wireplumber_version: wp,
            })],
            ..Default::default()
        };
        // The first snapshot after Connected bypasses the coalescer (C3).
        fx.events.push(self.snapshot_event());
        if self.had_ready_before {
            // F4: sent once the rebuild settles (or after RESTORE_MAX_WAIT), see reconcile().
            self.restore_pending = Some(Instant::now());
        }
        self.had_ready_before = true;
        fx.merge(self.schedule_reconcile(Duration::ZERO));
        fx.merge(self.reconcile_meters());
        let setup = self.setup.as_ref();
        let mut out = self.routing.sync(&self.mirror, setup);
        out.merge_from(self.routing.evaluate_all(&self.mirror, setup));
        fx.merge(routing_effects(out));
        fx
    }

    fn schedule_reconcile(&mut self, after: Duration) -> Effects {
        if self.state != ConnState::Ready || self.torn_down {
            return Effects::default();
        }
        self.arm_reconcile(after)
    }

    /// Arms the Reconcile timer unless it already fires no later than `after` from now.
    fn arm_reconcile(&mut self, after: Duration) -> Effects {
        let at = Instant::now() + after;
        if self.reconcile_due.is_some_and(|due| due <= at) {
            return Effects::default();
        }
        self.reconcile_due = Some(at);
        Effects {
            timers: vec![(TimerKind::Reconcile, after)],
            ..Default::default()
        }
    }

    /// One reconcile pass (C8): ops, statuses, re-run scheduling, F4 `Restored`.
    fn reconcile(&mut self) -> Effects {
        let mut fx = Effects::default();
        if self.state != ConnState::Ready || self.torn_down {
            return fx;
        }
        let empty = Setup::default();
        let setup = self.setup.as_ref().unwrap_or(&empty);
        let desired = desired::build_desired(setup, &self.mirror, &self.meters.denoised_slots());
        let now = Instant::now();
        let out = self
            .reconciler
            .pass(&desired, &self.mirror, setup, &self.instance, now);
        tracing::debug!(
            ops = out.ops.len(),
            rerun_in = ?out.rerun_in,
            statuses = ?self.reconciler.statuses_debug(),
            "reconcile pass"
        );
        let creating = out.ops.iter().any(|op| {
            matches!(
                op,
                PwOp::CreateAdapter { .. } | PwOp::CreateCell { .. } | PwOp::CreateLink { .. }
            )
        });
        if !out.newly_failed.is_empty() {
            fx.events.push(Event::SetupIssues(
                out.newly_failed
                    .into_iter()
                    .map(|(cell, message)| SetupIssue::CellFailed { cell, message })
                    .collect(),
            ));
        }
        fx.ops = out.ops;
        if let Some(d) = out.rerun_in {
            fx.merge(self.arm_reconcile(d));
        }
        if let Some(since) = self.restore_pending
            && ((!creating && out.rerun_in.is_none()) || since.elapsed() >= RESTORE_MAX_WAIT)
        {
            self.restore_pending = None;
            fx.events.push(Event::Restored);
        }
        if !self.stats_armed
            && fx
                .ops
                .iter()
                .any(|op| matches!(op, PwOp::CreateCell { .. } | PwOp::CreateDenoiser { .. }))
        {
            self.stats_armed = true;
            fx.timers.push((TimerKind::StatsPoll, STATS_POLL));
        }
        fx.merge(self.mark_dirty());
        fx
    }

    /// A cell's filter failed or panicked: drop it (the next pass recreates it) and count it.
    fn cell_failed(&mut self, key: &crate::model::CellKey, message: &str) -> Effects {
        tracing::warn!(cell = %crate::setup::canonical_cell_key(key), %message, "send failed");
        let owned = OwnedKey::Cell(key.clone());
        self.reconciler.on_failure(&owned);
        let mut fx = Effects {
            ops: vec![PwOp::Destroy {
                key: owned,
                global_id: self.owned_cell_id(key),
            }],
            events: vec![Event::Warning(EngineWarning::NodeCreateFailed {
                node: WarnedNode::Cell(key.clone()),
                message: message.to_owned(),
            })],
            ..Default::default()
        };
        fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
        fx
    }

    /// A denoiser's filter failed or panicked: drop it (the next pass recreates it) and count it.
    fn denoiser_failed(&mut self, key: &crate::model::DeviceKey, message: &str) -> Effects {
        tracing::warn!(input = %key.0, %message, "noise suppression failed");
        let owned = OwnedKey::Denoiser(key.clone());
        self.reconciler.on_failure(&owned);
        let hash = crate::setup::input_hash(key);
        let global_id =
            self.owned_node(|r| matches!(r, crate::graph::OwnedRole::Denoise(h) if *h == hash));
        let mut fx = Effects {
            ops: vec![PwOp::Destroy {
                key: owned,
                global_id,
            }],
            events: vec![Event::Warning(EngineWarning::NodeCreateFailed {
                node: WarnedNode::Denoiser(key.clone()),
                message: message.to_owned(),
            })],
            ..Default::default()
        };
        fx.merge(self.schedule_reconcile(RECONCILE_DEBOUNCE));
        fx
    }

    /// M10.2: xruns like any DSP node; underruns logged (a bug indicator, at most once a
    /// minute); an unsupported graph rate is reported once per rate.
    fn on_denoiser_stats(
        &mut self,
        key: &crate::model::DeviceKey,
        xruns: u64,
        underruns: u64,
        unsupported_rate: u32,
    ) -> Effects {
        let mut fx = self.on_xruns(WarnedNode::Denoiser(key.clone()), xruns);
        let now = Instant::now();
        let t = self.denoisers.entry(key.clone()).or_default();
        if xruns != t.xruns {
            t.xruns = xruns;
            fx.merge(self.mark_dirty());
        }
        let t = self.denoisers.entry(key.clone()).or_default();
        if underruns > t.underruns
            && t.underrun_log
                .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(60))
        {
            tracing::warn!(input = %key.0, new = underruns - t.underruns, "denoiser underruns");
            t.underruns = underruns;
            t.underrun_log = Some(now);
        }
        if unsupported_rate != 0 && unsupported_rate != t.unsupported_rate {
            t.unsupported_rate = unsupported_rate;
            fx.events
                .push(Event::Warning(EngineWarning::DenoiseRateUnsupported {
                    input: key.clone(),
                    rate: unsupported_rate,
                }));
        }
        fx
    }

    /// F12: warn when a DSP node exceeds the xrun rate, at most once a minute.
    fn on_xruns(&mut self, node: WarnedNode, total: u64) -> Effects {
        let now = Instant::now();
        let entry = self.xruns.entry(node.clone()).or_insert((total, now, None));
        if now.duration_since(entry.1) >= Duration::from_secs(60) {
            *entry = (total, now, entry.2);
        }
        let per_minute = total.saturating_sub(entry.0);
        let recently_warned = entry
            .2
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(60));
        if per_minute > XRUNS_PER_MINUTE_WARN && !recently_warned {
            entry.2 = Some(now);
            return Effects {
                events: vec![Event::Warning(EngineWarning::Xruns {
                    node,
                    per_minute: u32::try_from(per_minute).unwrap_or(u32::MAX),
                })],
                ..Default::default()
            };
        }
        Effects::default()
    }

    /// C10 step 3: clear our retargets (into owned nodes, and every stream an app rule placed),
    /// destroy everything we own (sends, mics, channels), then confirm with a sync.
    fn teardown(&mut self) -> Effects {
        let mut fx = Effects::default();
        if self.state != ConnState::Ready {
            fx.events.push(Event::TeardownComplete);
            return fx;
        }
        self.torn_down = true;
        let owned_ids: std::collections::BTreeSet<u64> = self
            .mirror
            .nodes
            .values()
            .filter(|n| matches!(n.kind, crate::graph::NodeKind::Owned(_)))
            .map(|n| n.serial)
            .collect();
        let ruled = self.routing.ruled_streams(self.setup.as_ref());
        for ((subject, key), entry) in &self.mirror.default_meta {
            if key == TARGET_KEY
                && (ruled.contains(subject)
                    || entry
                        .value
                        .parse::<u64>()
                        .is_ok_and(|s| owned_ids.contains(&s)))
            {
                fx.ops.push(PwOp::MetaWrite {
                    subject: *subject,
                    key: TARGET_KEY,
                    type_: None,
                    value: None,
                });
            }
        }
        for rank in 0..3 {
            for n in self.mirror.nodes.values() {
                let (r, key) = match &n.kind {
                    crate::graph::NodeKind::Owned(
                        crate::graph::OwnedRole::Cell(_) | crate::graph::OwnedRole::Denoise(_),
                    ) => (0, OwnedKey::StaleNode(n.id)),
                    crate::graph::NodeKind::Owned(crate::graph::OwnedRole::Vmic(id)) => {
                        (1, OwnedKey::Vmic(*id))
                    }
                    crate::graph::NodeKind::Owned(crate::graph::OwnedRole::Channel(id)) => {
                        (2, OwnedKey::Channel(*id))
                    }
                    _ => continue,
                };
                if r == rank {
                    fx.ops.push(PwOp::Destroy {
                        key,
                        global_id: Some(n.id),
                    });
                }
            }
        }
        fx.ops.push(PwOp::Sync {
            purpose: SyncPurpose::Teardown,
        });
        fx.timers
            .push((TimerKind::TeardownTimeout, TEARDOWN_TIMEOUT));
        fx.cancel_timers
            .extend([TimerKind::Reconcile, TimerKind::StatsPoll]);
        fx
    }

    fn owned_node(&self, pred: impl Fn(&crate::graph::OwnedRole) -> bool) -> Option<u32> {
        self.mirror
            .nodes
            .values()
            .find(|n| matches!(&n.kind, crate::graph::NodeKind::Owned(r) if pred(r)))
            .map(|n| n.id)
    }

    fn owned_cell_id(&self, key: &crate::model::CellKey) -> Option<u32> {
        let hash = crate::setup::cell_hash(key);
        self.owned_node(|r| matches!(r, crate::graph::OwnedRole::Cell(h) if *h == hash))
    }

    fn owned_cell_exists(&self, key: &crate::model::CellKey) -> bool {
        self.owned_cell_id(key).is_some()
    }

    fn any_owned_dsp(&self) -> bool {
        self.owned_node(|r| {
            matches!(
                r,
                crate::graph::OwnedRole::Cell(_) | crate::graph::OwnedRole::Denoise(_)
            )
        })
        .is_some()
    }

    /// Meter streams exist only while Ready (C9).
    fn reconcile_meters(&mut self) -> Effects {
        if self.state != ConnState::Ready {
            return Effects::default();
        }
        let delta = self.meters.reconcile(&self.mirror);
        let mut fx = Effects {
            ops: delta.ops,
            ..Default::default()
        };
        if let Some(a) = delta.assignments {
            fx.events.push(Event::MeterAssignments(a));
        }
        fx
    }

    fn disconnected(&mut self) -> Effects {
        if matches!(
            self.state,
            ConnState::Connecting { .. } | ConnState::Reconnecting { .. }
        ) {
            return Effects::default();
        }
        self.mirror.clear();
        self.meters.reset();
        self.routing.reset();
        self.reconciler.reset();
        self.reconcile_due = None;
        self.stats_armed = false;
        self.xruns.clear();
        self.pending_syncs.clear();
        self.snapshot_dirty = false;
        let since = Instant::now();
        self.state = ConnState::Reconnecting { since, attempt: 0 };
        Effects {
            events: vec![Event::Connection(ConnectionState::Reconnecting {
                since,
                attempt: 0,
            })],
            timers: vec![(TimerKind::Reconnect, self.backoff.delay(0))],
            cancel_timers: vec![
                TimerKind::WpWait,
                TimerKind::SnapshotFlush,
                TimerKind::Reconcile,
                TimerKind::StatsPoll,
            ],
            reset_connection: true,
            ..Default::default()
        }
    }

    fn mark_dirty(&mut self) -> Effects {
        self.snapshot_dirty = true;
        if self.flush_armed || !self.state.observing() {
            return Effects::default();
        }
        self.flush_armed = true;
        Effects {
            timers: vec![(TimerKind::SnapshotFlush, SNAPSHOT_COALESCE)],
            ..Default::default()
        }
    }

    fn snapshot_event(&mut self) -> Event {
        self.snapshot_dirty = false;
        let xruns: std::collections::HashMap<_, _> = self
            .denoisers
            .iter()
            .map(|(k, t)| (k.clone(), t.xruns))
            .collect();
        Event::Snapshot(Arc::new(snapshot::build_snapshot(
            &self.mirror,
            self.setup.as_ref(),
            &self.reconciler,
            &xruns,
        )))
    }
}

/// Engine-internal mapping used by warning emitters (WarnedNode per C3).
pub(crate) fn warned_node(key: &OwnedKey) -> Option<WarnedNode> {
    match key {
        OwnedKey::Channel(id) => Some(WarnedNode::Channel(*id)),
        OwnedKey::Vmic(id) => Some(WarnedNode::VirtualMic(*id)),
        OwnedKey::Cell(c) => Some(WarnedNode::Cell(c.clone())),
        OwnedKey::Denoiser(k) => Some(WarnedNode::Denoiser(k.clone())),
        _ => None,
    }
}

fn routing_effects(out: RoutingOutput) -> Effects {
    let mut fx = Effects {
        ops: out.ops,
        ..Default::default()
    };
    fx.events.extend(
        out.external
            .into_iter()
            .map(|(app, target)| Event::ExternalRetarget { app, target }),
    );
    fx.timers.extend(
        out.settle_timers
            .into_iter()
            .map(|id| (TimerKind::StreamSettle(id), STREAM_SETTLE)),
    );
    fx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeGraph;

    fn events_of(fx: &Effects) -> Vec<String> {
        fx.events
            .iter()
            .map(|e| match e {
                Event::Connection(c) => format!("conn:{c:?}"),
                Event::Snapshot(_) => "snapshot".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Drive a core through connect + initial sync + probe like the real shell does.
    fn connected_core(g: &FakeGraph) -> (EngineCore, Vec<Effects>) {
        let mut core = EngineCore::new("inst".into(), Backoff::default());
        let mut log = vec![core.start()];
        log.push(core.on_connect_result(true));
        core.on_sync_issued(SyncPurpose::InitialRegistry, 1);
        for ch in g.changes() {
            log.push(core.on_graph(&ch));
        }
        log.push(core.on_graph(&GraphChange::SyncDone { seq: 1 }));
        core.on_sync_issued(SyncPurpose::WriteProbe, 2);
        log.push(core.on_graph(&GraphChange::SyncDone { seq: 2 }));
        (core, log)
    }

    #[test]
    fn connects_and_emits_connected_then_snapshot() {
        let g = FakeGraph::standard();
        let (core, log) = connected_core(&g);
        assert_eq!(core.state(), &ConnState::Ready);
        let last = log.last().expect("effects");
        let ev = events_of(last);
        assert_eq!(
            ev[0],
            "conn:Connected { pipewire_version: \"1.5.85\", wireplumber_version: Some(\"0.5.12\") }"
        );
        assert_eq!(ev[1], "snapshot");
        let Event::Snapshot(snap) = &last.events[1] else {
            unreachable!()
        };
        assert_eq!(snap.outputs.len(), 1);
        assert_eq!(snap.inputs.len(), 1);
        assert_eq!(snap.apps.len(), 1);
        assert_eq!(snap.apps[0].display_name, "Spotify");
        assert_eq!(
            snap.default_output.as_ref().map(|k| k.0.as_str()),
            Some(FakeGraph::OUTPUT)
        );
        // No graph changes produce snapshots before Ready (only the immediate one).
        assert!(
            log[..log.len() - 1]
                .iter()
                .all(|fx| !events_of(fx).contains(&"snapshot".into()))
        );
    }

    #[test]
    fn probe_failure_means_permission_denied() {
        let g = FakeGraph::standard();
        let mut core = EngineCore::new("inst".into(), Backoff::default());
        core.start();
        core.on_connect_result(true);
        core.on_sync_issued(SyncPurpose::InitialRegistry, 1);
        for ch in g.changes() {
            core.on_graph(&ch);
        }
        let fx = core.on_graph(&GraphChange::SyncDone { seq: 1 });
        assert!(
            fx.ops
                .iter()
                .any(|op| matches!(op, PwOp::MetaWrite { value: Some(_), .. }))
        );
        core.on_sync_issued(SyncPurpose::WriteProbe, 2);
        core.on_graph(&GraphChange::ProxyError {
            key: OwnedKey::ProbeMeta,
            res: -1,
            message: "denied".into(),
        });
        let fx = core.on_graph(&GraphChange::SyncDone { seq: 2 });
        assert_eq!(core.state(), &ConnState::PermissionDenied);
        assert_eq!(events_of(&fx)[0], "conn:PermissionDenied");
    }

    #[test]
    fn waits_for_wireplumber_then_unsupported() {
        let g = FakeGraph::without_wireplumber();
        let mut core = EngineCore::new("inst".into(), Backoff::default());
        core.start();
        core.on_connect_result(true);
        core.on_sync_issued(SyncPurpose::InitialRegistry, 1);
        for ch in g.changes() {
            core.on_graph(&ch);
        }
        let fx = core.on_graph(&GraphChange::SyncDone { seq: 1 });
        assert_eq!(core.state(), &ConnState::WaitingForWp);
        assert!(fx.timers.contains(&(TimerKind::WpWait, WP_WAIT)));
        let fx = core.on_timer(TimerKind::WpWait);
        assert_eq!(
            events_of(&fx)[0],
            "conn:Unsupported { reason: NoWirePlumber }"
        );
        // WirePlumber starting later recovers (login race).
        for ch in FakeGraph::wireplumber_changes() {
            core.on_graph(&ch);
        }
        assert_eq!(core.state(), &ConnState::Probing);
    }

    #[test]
    fn coalesces_snapshots_to_one_per_window() {
        let g = FakeGraph::standard();
        let (mut core, _) = connected_core(&g);
        let mut armed = 0;
        for i in 0..100u32 {
            let fx = core.on_graph(&GraphChange::DefaultMetaProperty {
                subject: 0,
                key: format!("k{i}"),
                type_: None,
                value: Some("v".into()),
            });
            armed += fx
                .timers
                .iter()
                .filter(|(k, _)| *k == TimerKind::SnapshotFlush)
                .count();
            assert!(fx.events.is_empty());
        }
        assert_eq!(armed, 1, "the flush timer is armed once per window");
        let fx = core.on_timer(TimerKind::SnapshotFlush);
        assert_eq!(events_of(&fx), vec!["snapshot"]);
        // Next change arms a new window.
        let fx = core.on_graph(&GraphChange::DefaultMetaProperty {
            subject: 0,
            key: "later".into(),
            type_: None,
            value: Some("v".into()),
        });
        assert_eq!(
            fx.timers,
            vec![(TimerKind::SnapshotFlush, SNAPSHOT_COALESCE)]
        );
        // A flush with nothing dirty emits nothing.
        core.on_timer(TimerKind::SnapshotFlush);
        assert!(core.on_timer(TimerKind::SnapshotFlush).events.is_empty());
    }

    #[test]
    fn reconnect_cycle_emits_restored_once() {
        let g = FakeGraph::standard();
        let (mut core, _) = connected_core(&g);
        let fx = core.on_graph(&GraphChange::Disconnected);
        assert!(fx.reset_connection);
        assert!(matches!(
            core.state(),
            ConnState::Reconnecting { attempt: 0, .. }
        ));
        assert!(
            fx.timers
                .contains(&(TimerKind::Reconnect, Duration::from_millis(500)))
        );
        // First retry fails → backoff grows.
        assert!(core.on_timer(TimerKind::Reconnect).connect_now);
        let fx = core.on_connect_result(false);
        assert!(
            fx.timers
                .contains(&(TimerKind::Reconnect, Duration::from_millis(500)))
        );
        let fx = core.on_connect_result(false);
        assert!(
            fx.timers
                .contains(&(TimerKind::Reconnect, Duration::from_millis(1000)))
        );
        // Then succeeds.
        core.on_connect_result(true);
        core.on_sync_issued(SyncPurpose::InitialRegistry, 10);
        for ch in g.changes() {
            core.on_graph(&ch);
        }
        core.on_graph(&GraphChange::SyncDone { seq: 10 });
        core.on_sync_issued(SyncPurpose::WriteProbe, 11);
        let fx = core.on_graph(&GraphChange::SyncDone { seq: 11 });
        let ev = events_of(&fx);
        assert!(ev[0].starts_with("conn:Connected"));
        assert!(
            !ev.contains(&"Restored".to_owned()),
            "Restored waits for the rebuild pass"
        );
        assert!(fx.timers.iter().any(|(k, _)| *k == TimerKind::Reconcile));
        // Nothing to build (empty setup): the first pass settles and reports Restored once.
        let fx = core.on_timer(TimerKind::Reconcile);
        assert!(events_of(&fx).contains(&"Restored".to_owned()));
        let fx = core.on_timer(TimerKind::Reconcile);
        assert!(
            !events_of(&fx).contains(&"Restored".to_owned()),
            "exactly once"
        );
    }

    #[test]
    fn snapshot_ordering() {
        let g = FakeGraph::standard()
            .device("alsa_output.usb-b", "beta USB", "Audio/Sink")
            .device("alsa_output.usb-a", "Alpha USB", "Audio/Sink")
            .app_playing("zed", "zed player", FakeGraph::OUTPUT)
            .app_playing("amp", "Amp", FakeGraph::OUTPUT);
        let (_, log) = connected_core(&g);
        let Some(Event::Snapshot(snap)) = log.last().and_then(|fx| fx.events.get(1)) else {
            panic!("expected the first snapshot")
        };
        let outs: Vec<&str> = snap
            .outputs
            .iter()
            .map(|d| d.description.as_str())
            .collect();
        assert_eq!(
            outs,
            vec!["Alpha USB", "beta USB", "Built-in Audio Analog Stereo"]
        );
        let apps: Vec<&str> = snap.apps.iter().map(|a| a.display_name.as_str()).collect();
        assert_eq!(
            apps,
            vec!["Amp", "Spotify", "zed player"],
            "case-insensitive"
        );
    }

    #[test]
    fn setup_is_stored_and_reflected() {
        let g = FakeGraph::standard();
        let (mut core, _) = connected_core(&g);
        let id = crate::model::ChannelId::new_random();
        let setup = Setup {
            channels: vec![crate::setup::ChannelDef {
                id,
                name: "Music".into(),
                volume: crate::model::Volume::unity(2),
                position: None,
            }],
            ..Default::default()
        };
        let fx = core.on_command(Command::SetSetup(setup));
        assert!(fx.ops.is_empty(), "read-only engine builds nothing");
        let fx = core.on_timer(TimerKind::SnapshotFlush);
        let Event::Snapshot(snap) = &fx.events[0] else {
            panic!("expected snapshot")
        };
        assert_eq!(snap.channels[0].name, "Music");
        assert!(!snap.channels[0].present);
        assert_eq!(
            events_of(&core.on_command(Command::PrepareQuit)),
            vec!["QuitCheck([])"]
        );
    }
}
