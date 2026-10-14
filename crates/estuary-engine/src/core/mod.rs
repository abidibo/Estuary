//! The engine's functional core: graph changes, commands and timers in, `Effects` out.
//! No PipeWire objects live here, so all of it is unit-testable (engine "functional core").

pub(crate) mod conn;
pub(crate) mod effects;
pub(crate) mod meters;
pub(crate) mod snapshot;

use std::sync::Arc;
use std::time::{Duration, Instant};

use conn::{CapabilityResult, ConnState, evaluate_capabilities};
use effects::{Effects, OwnedKey, PwOp, SyncPurpose, TimerKind};
use meters::MeterPlan;

use crate::error::EngineWarning;
use crate::error::{Backoff, UnsupportedReason};
use crate::graph::{GraphChange, GraphMirror};
use crate::setup::Setup;
use crate::{Command, ConnectionState, Event};

const SNAPSHOT_COALESCE: Duration = Duration::from_millis(50);
const WP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct EngineCore {
    instance: String,
    backoff: Backoff,
    state: ConnState,
    mirror: GraphMirror,
    setup: Option<Setup>,
    meters: MeterPlan,
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
            fx.merge(self.reconcile_meters());
        }
        fx
    }

    pub fn on_command(&mut self, cmd: Command) -> Effects {
        let mut fx = Effects::default();
        match cmd {
            Command::SetSetup(setup) => {
                // Read-only at M2: the setup is stored and reflected in snapshots; nothing is built.
                self.setup = Some(setup);
                fx.merge(self.mark_dirty());
            }
            Command::SetMeterTargets(targets) => {
                self.meters.set_targets(targets);
                fx.merge(self.reconcile_meters());
            }
            Command::PrepareQuit => {
                // No owned objects exist yet, so nothing can be in use.
                fx.events.push(Event::QuitCheck(Vec::new()));
            }
            Command::Teardown => fx.events.push(Event::TeardownComplete),
            other => tracing::debug!(?other, "command not implemented before M5/M8; ignored"),
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
        }
        fx
    }

    fn sync_done(&mut self, purpose: SyncPurpose) -> Effects {
        match (purpose, &self.state) {
            (SyncPurpose::InitialRegistry, ConnState::Syncing) => self.check_capabilities(),
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
            // Nothing is owned before M8, so the setup is "rebuilt" as soon as we're ready.
            fx.events.push(Event::Restored);
        }
        self.had_ready_before = true;
        fx.merge(self.reconcile_meters());
        fx
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
            cancel_timers: vec![TimerKind::WpWait, TimerKind::SnapshotFlush],
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
        Event::Snapshot(Arc::new(snapshot::build_snapshot(
            &self.mirror,
            self.setup.as_ref(),
        )))
    }
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
        assert_eq!(ev.last().map(String::as_str), Some("Restored"));
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
