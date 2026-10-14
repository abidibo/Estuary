//! Reconciler scenarios (engine M8.13): the real `EngineCore` against `FakeShell`.

use std::collections::VecDeque;

use super::EngineCore;
use super::effects::{Effects, PwOp, SyncPurpose, TimerKind};
use crate::error::{Backoff, SetupIssue};
use crate::graph::{GraphChange, LinkEntry, NodeKind, OwnedRole};
use crate::model::{
    CellKey, CellStatus, ChannelId, DeviceKey, MatrixDest, MatrixSource, VirtualMicId, Volume,
};
use crate::setup::{AppRule, CellDef, ChannelDef, Setup, VirtualMicDef};
use crate::testing::{FakeGraph, FakeShell};
use crate::{Command, Event, RouteTarget};

struct Harness {
    core: EngineCore,
    shell: FakeShell,
    events: Vec<Event>,
    ops: Vec<PwOp>,
}

impl Harness {
    fn connected(g: &FakeGraph) -> Self {
        let mut core = EngineCore::new("inst".into(), Backoff::default());
        core.start();
        core.on_connect_result(true);
        core.on_sync_issued(SyncPurpose::InitialRegistry, 1);
        for ch in g.changes() {
            core.on_graph(&ch);
        }
        core.on_graph(&GraphChange::SyncDone { seq: 1 });
        core.on_sync_issued(SyncPurpose::WriteProbe, 2);
        let fx = core.on_graph(&GraphChange::SyncDone { seq: 2 });
        let mut h = Self {
            core,
            shell: FakeShell::new(),
            events: Vec::new(),
            ops: Vec::new(),
        };
        h.run(fx);
        h
    }

    /// Runs effects to quiescence: ops go through the fake shell, their changes come back,
    /// reconcile timers fire immediately (at most a few times per call, so fades stay pending).
    fn run(&mut self, fx: Effects) {
        let mut q = VecDeque::from([fx]);
        let mut reconciles = 0;
        while let Some(fx) = q.pop_front() {
            self.events.extend(fx.events);
            for op in fx.ops {
                for ch in self.shell.execute(&op) {
                    q.push_back(self.core.on_graph(&ch));
                }
                self.ops.push(op);
            }
            if fx.timers.iter().any(|(k, _)| *k == TimerKind::Reconcile) && reconciles < 8 {
                reconciles += 1;
                q.push_back(self.core.on_timer(TimerKind::Reconcile));
            }
        }
    }

    fn command(&mut self, cmd: Command) {
        let fx = self.core.on_command(cmd);
        self.run(fx);
    }

    fn graph(&mut self, ch: &GraphChange) {
        let fx = self.core.on_graph(ch);
        self.run(fx);
    }

    fn reconcile(&mut self) -> Vec<PwOp> {
        let before = self.ops.len();
        let fx = self.core.on_timer(TimerKind::Reconcile);
        self.run(fx);
        self.ops[before..].to_vec()
    }

    fn owned(&self, pred: impl Fn(&OwnedRole) -> bool) -> usize {
        self.core
            .mirror
            .nodes
            .values()
            .filter(|n| matches!(&n.kind, NodeKind::Owned(r) if pred(r)))
            .count()
    }

    fn status(&self, key: &CellKey) -> Option<CellStatus> {
        self.core.reconciler.status(key).cloned()
    }
}

struct Ids {
    music: ChannelId,
    chat: ChannelId,
    vmic: VirtualMicId,
}

fn output() -> DeviceKey {
    DeviceKey(FakeGraph::OUTPUT.into())
}

fn input() -> DeviceKey {
    DeviceKey(FakeGraph::INPUT.into())
}

fn cell(source: MatrixSource, dest: MatrixDest, gain_db: f32) -> CellDef {
    CellDef {
        source,
        dest,
        gain_db,
        enabled: true,
    }
}

/// Two channels and a virtual mic; Music and Chat to the speakers, Music (−18 dB) and the
/// hardware mic to the virtual mic.
fn preset() -> (Setup, Ids) {
    let ids = Ids {
        music: ChannelId::new_random(),
        chat: ChannelId::new_random(),
        vmic: VirtualMicId::new_random(),
    };
    let ch = |id, name: &str| ChannelDef {
        id,
        name: name.into(),
        volume: Volume::unity(2),
        position: None,
    };
    let setup = Setup {
        channels: vec![ch(ids.music, "Music"), ch(ids.chat, "Chat")],
        virtual_mics: vec![VirtualMicDef {
            id: ids.vmic,
            name: "Virtual mic".into(),
            volume: Volume::unity(2),
            position: None,
        }],
        cells: vec![
            cell(
                MatrixSource::Channel { id: ids.music },
                MatrixDest::Output { key: output() },
                0.0,
            ),
            cell(
                MatrixSource::Channel { id: ids.chat },
                MatrixDest::Output { key: output() },
                0.0,
            ),
            cell(
                MatrixSource::Channel { id: ids.music },
                MatrixDest::VirtualMic { id: ids.vmic },
                -18.0,
            ),
            cell(
                MatrixSource::Input { key: input() },
                MatrixDest::VirtualMic { id: ids.vmic },
                0.0,
            ),
        ],
        ..Default::default()
    };
    (setup, ids)
}

#[test]
fn preset_builds_everything_and_is_idempotent() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, _) = preset();
    h.command(Command::SetSetup(setup.clone()));
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Channel(_))), 2);
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Vmic(_))), 1);
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Cell(_))), 4);
    assert_eq!(
        h.core.mirror.links.len(),
        2 + 4 * 4,
        "spotify's 2 + 4 cells × (2 in + 2 out)"
    );
    for c in &setup.cells {
        assert_eq!(h.status(&c.key()), Some(CellStatus::Active), "{c:?}");
    }
    // Cells fade in to their gain.
    assert_eq!(h.shell.gains.get(&setup.cells[2].key()), Some(&-18.0));
    // A second pass with nothing new does nothing.
    let ops = h.reconcile();
    assert!(ops.is_empty(), "idempotent, got {ops:?}");
}

#[test]
fn foreign_links_on_cells_are_removed_but_app_links_are_not() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, _) = preset();
    // Music → speakers: fed by a channel, never by the mic.
    let hash = crate::setup::cell_hash(&setup.cells[0].key());
    h.command(Command::SetSetup(setup));
    let cell = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Cell(c)) if *c == hash))
        .unwrap()
        .id;
    let cell_in = h
        .core
        .mirror
        .ports
        .values()
        .find(|p| p.node_id == cell && p.name == "input_FL")
        .unwrap()
        .id;
    let mic = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| n.kind == NodeKind::Input)
        .unwrap()
        .id;
    let mic_out = h
        .core
        .mirror
        .ports
        .values()
        .find(|p| p.node_id == mic && p.name == "capture_FR")
        .unwrap()
        .id;
    // Someone links the mic into a cell that isn't fed by the mic.
    let stray = LinkEntry {
        id: 9999,
        out_node: mic,
        out_port: mic_out,
        in_node: cell,
        in_port: cell_in,
        client_id: Some(33),
    };
    h.graph(&GraphChange::LinkAdded(stray));
    h.reconcile();
    assert!(
        !h.core.mirror.links.contains_key(&9999),
        "stray link on a cell removed"
    );
    // Spotify → default output (an app link) is never touched.
    let spotify = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| n.kind == NodeKind::AppPlayback)
        .unwrap()
        .id;
    assert!(h.core.mirror.links.values().any(|l| l.out_node == spotify));
}

#[test]
fn device_unplugged_and_back_restores_routes() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, _) = preset();
    h.command(Command::SetSetup(setup.clone()));
    let out_id = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| n.kind == NodeKind::Output)
        .unwrap()
        .id;
    h.graph(&GraphChange::NodeRemoved(out_id));
    h.reconcile();
    assert_eq!(
        h.owned(|r| matches!(r, OwnedRole::Cell(_))),
        2,
        "the two sends to the speakers are gone"
    );
    assert_eq!(
        h.status(&setup.cells[0].key()),
        Some(CellStatus::DestinationMissing)
    );
    // Plugged back in.
    for ch in FakeGraph::new()
        .device(
            FakeGraph::OUTPUT,
            "Built-in Audio Analog Stereo",
            "Audio/Sink",
        )
        .changes()
    {
        h.graph(&ch);
    }
    h.reconcile();
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Cell(_))), 4);
    assert_eq!(h.status(&setup.cells[0].key()), Some(CellStatus::Active));
}

#[test]
fn three_failures_stop_retrying_until_the_setup_changes() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let ids = Ids {
        music: ChannelId::new_random(),
        chat: ChannelId::new_random(),
        vmic: VirtualMicId::new_random(),
    };
    let setup = Setup {
        channels: vec![ChannelDef {
            id: ids.music,
            name: "Music".into(),
            volume: Volume::unity(2),
            position: None,
        }],
        cells: vec![cell(
            MatrixSource::Channel { id: ids.music },
            MatrixDest::Output { key: output() },
            0.0,
        )],
        ..Default::default()
    };
    h.shell.fail_cell_creates = 10;
    h.command(Command::SetSetup(setup.clone()));
    for _ in 0..5 {
        h.reconcile();
    }
    let attempts = 10 - h.shell.fail_cell_creates;
    assert_eq!(attempts, 3, "exactly three attempts (C8)");
    assert!(matches!(
        h.status(&setup.cells[0].key()),
        Some(CellStatus::Error(_))
    ));
    let reported = h
        .events
        .iter()
        .filter(|e| matches!(e, Event::SetupIssues(v) if v.iter().any(|i| matches!(i, SetupIssue::CellFailed { .. }))))
        .count();
    assert_eq!(reported, 1, "reported once");
    // A new setup resets the count.
    h.shell.fail_cell_creates = 0;
    h.command(Command::SetSetup(setup.clone()));
    assert_eq!(h.status(&setup.cells[0].key()), Some(CellStatus::Active));
}

#[test]
fn disabling_fades_out_then_destroys() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (mut setup, _) = preset();
    h.command(Command::SetSetup(setup.clone()));
    let key = setup.cells[0].key();
    setup.cells[0].enabled = false;
    h.command(Command::SetSetup(setup.clone()));
    assert_eq!(
        h.shell.gains.get(&key),
        Some(&super::reconcile::FADE_DB),
        "faded to -60 dB"
    );
    assert_eq!(
        h.owned(|r| matches!(r, OwnedRole::Cell(_))),
        4,
        "still there during the fade"
    );
    std::thread::sleep(super::reconcile::FADE_OUT + std::time::Duration::from_millis(20));
    h.reconcile();
    assert_eq!(
        h.owned(|r| matches!(r, OwnedRole::Cell(_))),
        3,
        "destroyed after the fade"
    );
    // Re-enabling creates it silent and fades in.
    setup.cells[0].enabled = true;
    h.command(Command::SetSetup(setup));
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Cell(_))), 4);
    assert_eq!(h.shell.gains.get(&key), Some(&0.0));
}

#[test]
fn renaming_a_channel_recreates_it_and_rules_follow() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (mut setup, ids) = preset();
    setup.app_rules.push(AppRule {
        app: crate::AppKey("bin:spotify".into()),
        target: RouteTarget::Channel { id: ids.music },
        display_name: "Spotify".into(),
    });
    h.command(Command::SetSetup(setup.clone()));
    let old = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Channel(c)) if *c == ids.music))
        .unwrap()
        .serial;
    setup.channels[0].name = "Tunes".into();
    h.command(Command::SetSetup(setup));
    h.reconcile();
    let renamed = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Channel(c)) if *c == ids.music))
        .unwrap();
    assert_eq!(
        renamed.props.get("node.description").map(String::as_str),
        Some("Tunes")
    );
    assert_ne!(renamed.serial, old, "a new node");
    let serial = renamed.serial.to_string();
    assert!(
        h.ops
            .iter()
            .any(|op| matches!(op, PwOp::MetaWrite { value: Some(v), .. } if *v == serial)),
        "spotify retargeted to the new node"
    );
}

#[test]
fn prepare_quit_and_teardown() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, ids) = preset();
    h.command(Command::SetSetup(setup));
    // Spotify plays into Music.
    let spotify = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| n.kind == NodeKind::AppPlayback)
        .unwrap()
        .id;
    let music = h
        .core
        .mirror
        .nodes
        .values()
        .find(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Channel(c)) if *c == ids.music))
        .unwrap()
        .id;
    let sp_out = h
        .core
        .mirror
        .ports
        .values()
        .find(|p| p.node_id == spotify)
        .unwrap()
        .id;
    let music_in = h
        .core
        .mirror
        .ports
        .values()
        .find(|p| p.node_id == music && !p.monitor)
        .unwrap()
        .id;
    h.graph(&GraphChange::LinkAdded(LinkEntry {
        id: 8888,
        out_node: spotify,
        out_port: sp_out,
        in_node: music,
        in_port: music_in,
        client_id: Some(33),
    }));
    h.command(Command::PrepareQuit);
    let usages = h
        .events
        .iter()
        .find_map(|e| match e {
            Event::QuitCheck(u) => Some(u.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].device_name, "Music");

    let before = h.ops.len();
    let fx = h.core.on_command(Command::Teardown);
    let ops = fx.ops.clone();
    h.run(fx);
    let destroys: Vec<&PwOp> = ops
        .iter()
        .filter(|op| matches!(op, PwOp::Destroy { .. }))
        .collect();
    assert_eq!(destroys.len(), 4 + 1 + 2, "sends, virtual mic, channels");
    let first_channel = ops
        .iter()
        .position(|op| {
            matches!(
                op,
                PwOp::Destroy {
                    key: super::effects::OwnedKey::Channel(_),
                    ..
                }
            )
        })
        .unwrap();
    let last_cell = ops
        .iter()
        .rposition(|op| {
            matches!(
                op,
                PwOp::Destroy {
                    key: super::effects::OwnedKey::StaleNode(_),
                    ..
                }
            )
        })
        .unwrap();
    assert!(last_cell < first_channel, "sends go before channels");
    assert!(matches!(
        ops.last(),
        Some(PwOp::Sync {
            purpose: SyncPurpose::Teardown
        })
    ));
    assert!(h.ops.len() > before);
    assert!(
        h.core.reconcile().ops.is_empty(),
        "nothing is rebuilt after teardown"
    );
}

#[test]
fn a_topology_change_pulls_an_armed_retry_earlier() {
    let reconcile_after = |fx: &Effects| {
        fx.timers
            .iter()
            .find(|(k, _)| *k == TimerKind::Reconcile)
            .map(|(_, d)| *d)
    };
    let link = |id| {
        GraphChange::LinkAdded(LinkEntry {
            id,
            out_node: 1,
            out_port: 2,
            in_node: 3,
            in_port: 4,
            client_id: None,
        })
    };
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, _) = preset();
    let _ = h.core.on_command(Command::SetSetup(setup));
    // The creating pass arms the pending-timeout retry.
    let fx = h.core.on_timer(TimerKind::Reconcile);
    assert!(reconcile_after(&fx).is_some_and(|d| d > std::time::Duration::from_secs(1)));
    // Our objects appearing must not wait for that retry (live M8 finding).
    let fx = h.core.on_graph(&link(7001));
    assert!(reconcile_after(&fx).is_some_and(|d| d <= super::RECONCILE_DEBOUNCE));
    // Already due soon: no re-arm.
    let fx = h.core.on_graph(&link(7002));
    assert_eq!(reconcile_after(&fx), None);
}

#[test]
fn late_ports_keep_cells_pending_until_they_appear() {
    let mut h = Harness::connected(&FakeGraph::standard());
    let (setup, _) = preset();
    h.shell.held_ports = Some(Vec::new());
    h.command(Command::SetSetup(setup.clone()));
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Cell(_))), 4);
    assert_eq!(h.status(&setup.cells[0].key()), Some(CellStatus::Pending));
    assert!(!h.ops.iter().any(|op| matches!(op, PwOp::CreateLink { .. })));
    for ch in h.shell.held_ports.take().unwrap() {
        h.graph(&ch);
    }
    for c in &setup.cells {
        assert_eq!(h.status(&c.key()), Some(CellStatus::Active));
    }
    assert_eq!(h.core.mirror.links.len(), 2 + 16);
}

#[test]
fn a_reconnect_rebuilds_everything_and_restores_once() {
    let g = FakeGraph::standard();
    let mut h = Harness::connected(&g);
    let (setup, _) = preset();
    h.command(Command::SetSetup(setup.clone()));
    h.graph(&GraphChange::Disconnected);
    assert_eq!(h.owned(|_| true), 0);
    // PipeWire came back without our objects.
    h.shell = FakeShell::new();
    let fx = h.core.on_timer(TimerKind::Reconnect);
    assert!(fx.connect_now);
    h.core.on_connect_result(true);
    h.core.on_sync_issued(SyncPurpose::InitialRegistry, 3);
    for ch in g.changes() {
        h.graph(&ch);
    }
    h.graph(&GraphChange::SyncDone { seq: 3 });
    h.core.on_sync_issued(SyncPurpose::WriteProbe, 4);
    h.graph(&GraphChange::SyncDone { seq: 4 });
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Cell(_))), 4);
    assert_eq!(h.owned(|r| matches!(r, OwnedRole::Channel(_))), 2);
    for c in &setup.cells {
        assert_eq!(h.status(&c.key()), Some(CellStatus::Active));
    }
    let restored = h
        .events
        .iter()
        .filter(|e| matches!(e, Event::Restored))
        .count();
    assert_eq!(restored, 1);
}
