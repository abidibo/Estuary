//! Meter plan (engine M4.4): `MeterTarget`s → metered nodes → slots, within the 64-slot budget.
//! Pure; the shell creates and destroys the actual streams.

use std::collections::{BTreeMap, BTreeSet};

use super::effects::PwOp;
use crate::appkey;
use crate::graph::{GraphMirror, NodeEntry, NodeKind, OwnedRole};
use crate::meter::{METER_SLOTS, MeterSlotId, MeterTarget, SlotAllocator};
use crate::model::DeviceKey;

#[derive(Debug, Clone, PartialEq)]
struct Active {
    slot: MeterSlotId,
    target: MeterTarget,
}

/// One node to meter, with how its meter stream connects (C5).
#[derive(Debug, Clone, PartialEq)]
struct Wanted {
    node_id: u32,
    serial: u64,
    target: MeterTarget,
    capture_sink: bool,
    autoconnect: bool,
}

#[derive(Debug)]
pub(crate) struct MeterPlan {
    /// Subscription from the UI; order is priority.
    targets: Vec<MeterTarget>,
    active: BTreeMap<u32, Active>,
    alloc: SlotAllocator,
    /// Node ids whose meter failed; retried only once the node is re-added.
    failed: BTreeSet<u32>,
    last_assignments: Vec<(MeterTarget, Vec<MeterSlotId>)>,
}

#[derive(Debug, Default)]
pub(crate) struct MeterDelta {
    pub ops: Vec<PwOp>,
    /// `Some` when the target → slots map changed.
    pub assignments: Option<Vec<(MeterTarget, Vec<MeterSlotId>)>>,
}

impl MeterPlan {
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            active: BTreeMap::new(),
            alloc: SlotAllocator::new(),
            failed: BTreeSet::new(),
            last_assignments: Vec::new(),
        }
    }

    pub fn set_targets(&mut self, targets: Vec<MeterTarget>) {
        self.targets = targets;
    }

    /// The connection is gone: every stream died with it. Targets are kept.
    pub fn reset(&mut self) {
        self.active.clear();
        self.alloc = SlotAllocator::new();
        self.failed.clear();
        self.last_assignments.clear();
    }

    /// A meter stream failed: free its slot and don't retry until its node comes back.
    pub fn on_failed(&mut self, slot: MeterSlotId) -> Option<MeterTarget> {
        let node = self
            .active
            .iter()
            .find(|(_, a)| a.slot == slot)
            .map(|(n, _)| *n)?;
        let a = self.active.remove(&node)?;
        self.alloc.free(slot);
        self.failed.insert(node);
        Some(a.target)
    }

    /// Recompute against the mirror (after a subscription or topology change).
    pub fn reconcile(&mut self, m: &GraphMirror) -> MeterDelta {
        self.failed.retain(|id| m.nodes.contains_key(id));
        let wanted = self.wanted(m);
        let mut delta = MeterDelta::default();

        let keep: BTreeSet<u32> = wanted.iter().map(|w| w.node_id).collect();
        let gone: Vec<u32> = self
            .active
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(a) = self.active.remove(&id) {
                tracing::debug!(
                    node = id,
                    slot = a.slot.0,
                    present = m.nodes.contains_key(&id),
                    kind = ?m.nodes.get(&id).map(|n| &n.kind),
                    name = ?m.nodes.get(&id).and_then(|n| n.props.get("node.name")),
                    "meter no longer wanted"
                );
                self.alloc.free(a.slot);
                delta.ops.push(PwOp::DestroyMeter { slot: a.slot });
            }
        }
        for w in wanted {
            if let Some(a) = self.active.get_mut(&w.node_id) {
                a.target = w.target; // the same node may now serve a different target
                continue;
            }
            let Some(slot) = self.alloc.alloc() else {
                break;
            };
            self.active.insert(
                w.node_id,
                Active {
                    slot,
                    target: w.target,
                },
            );
            delta.ops.push(PwOp::CreateMeter {
                slot,
                target_serial: w.serial,
                capture_sink: w.capture_sink,
                autoconnect: w.autoconnect,
            });
        }

        let assignments = self.assignments();
        if assignments != self.last_assignments {
            self.last_assignments.clone_from(&assignments);
            delta.assignments = Some(assignments);
        }
        delta
    }

    /// Target → slots, in subscription order (slots ordered by node id).
    fn assignments(&self) -> Vec<(MeterTarget, Vec<MeterSlotId>)> {
        let mut out: Vec<(MeterTarget, Vec<MeterSlotId>)> = Vec::new();
        for t in &self.targets {
            let slots: Vec<MeterSlotId> = self
                .active
                .values()
                .filter(|a| &a.target == t)
                .map(|a| a.slot)
                .collect();
            if !slots.is_empty() && !out.iter().any(|(ot, _)| ot == t) {
                out.push((t.clone(), slots));
            }
        }
        out
    }

    /// Nodes to meter, in priority order, capped at the slot budget.
    fn wanted(&self, m: &GraphMirror) -> Vec<Wanted> {
        let mut out: Vec<Wanted> = Vec::new();
        for t in &self.targets {
            for n in resolve(t, m) {
                if out.len() >= METER_SLOTS {
                    return out;
                }
                if self.failed.contains(&n.id) || out.iter().any(|w| w.node_id == n.id) {
                    continue;
                }
                let (capture_sink, autoconnect) = match (t, &n.kind) {
                    (MeterTarget::Device(_), NodeKind::Output) | (MeterTarget::Channel(_), _) => {
                        (true, true)
                    }
                    // Denoiser outputs are linked by the reconciler (C5/C8 amended, M10).
                    (MeterTarget::Denoised(_), _) => (false, false),
                    _ => (false, true),
                };
                out.push(Wanted {
                    node_id: n.id,
                    serial: n.serial,
                    target: t.clone(),
                    capture_sink,
                    autoconnect,
                });
            }
        }
        out
    }
}

/// The nodes a target refers to (C9: an app target meters every stream of the app).
fn resolve<'m>(t: &MeterTarget, m: &'m GraphMirror) -> Vec<&'m NodeEntry> {
    let nodes = m.nodes.values().filter(|n| n.info_received);
    match t {
        MeterTarget::App(key) => nodes
            .filter(|n| n.kind == NodeKind::AppPlayback && &appkey::derive(&n.props) == key)
            .collect(),
        MeterTarget::Device(DeviceKey(name)) => nodes
            .filter(|n| matches!(n.kind, NodeKind::Output | NodeKind::Input))
            .filter(|n| n.props.get("node.name") == Some(name))
            .collect(),
        MeterTarget::Channel(id) => nodes
            .filter(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Channel(c)) if c == id))
            .collect(),
        MeterTarget::VirtualMic(id) => nodes
            .filter(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Vmic(v)) if v == id))
            .collect(),
        // Denoiser nodes are identified by hash once the reconciler exists (M10).
        MeterTarget::Denoised(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphChange;
    use crate::model::AppKey;
    use crate::testing::FakeGraph;

    fn mirror(g: &FakeGraph) -> GraphMirror {
        let mut m = GraphMirror::default();
        for ch in g.changes() {
            m.apply(&ch, "inst");
        }
        m
    }

    fn creates(d: &MeterDelta) -> Vec<(MeterSlotId, bool)> {
        d.ops
            .iter()
            .filter_map(|op| match op {
                PwOp::CreateMeter {
                    slot, capture_sink, ..
                } => Some((*slot, *capture_sink)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn device_and_app_targets() {
        let g = FakeGraph::standard();
        let m = mirror(&g);
        let mut plan = MeterPlan::new();
        plan.set_targets(vec![
            MeterTarget::Device(DeviceKey(FakeGraph::OUTPUT.into())),
            MeterTarget::Device(DeviceKey(FakeGraph::INPUT.into())),
            MeterTarget::App(AppKey("bin:spotify".into())),
        ]);
        let d = plan.reconcile(&m);
        let c = creates(&d);
        assert_eq!(c.len(), 3);
        assert!(c[0].1, "an output is metered through its monitor ports");
        assert!(!c[1].1, "an input is captured directly");
        assert!(!c[2].1, "an app stream is captured directly");
        let a = d.assignments.expect("assignments emitted");
        assert_eq!(a.len(), 3);
        // Nothing changed → nothing to do.
        let d = plan.reconcile(&m);
        assert!(d.ops.is_empty() && d.assignments.is_none());
    }

    #[test]
    fn app_with_three_streams_gets_three_slots() {
        let g = FakeGraph::standard()
            .app_playing("firefox", "Firefox", FakeGraph::OUTPUT)
            .app_playing("firefox", "Firefox", FakeGraph::OUTPUT)
            .app_playing("firefox", "Firefox", FakeGraph::OUTPUT);
        let m = mirror(&g);
        let mut plan = MeterPlan::new();
        let key = AppKey("bin:firefox".into());
        plan.set_targets(vec![MeterTarget::App(key.clone())]);
        let d = plan.reconcile(&m);
        let a = d.assignments.expect("assignments");
        assert_eq!(a, vec![(MeterTarget::App(key), a[0].1.clone())]);
        assert_eq!(a[0].1.len(), 3);
    }

    #[test]
    fn budget_is_64_in_priority_order() {
        let mut g = FakeGraph::standard();
        for i in 0..70 {
            g = g.app_playing(
                &format!("app{i:02}"),
                &format!("App {i:02}"),
                FakeGraph::OUTPUT,
            );
        }
        let m = mirror(&g);
        let mut plan = MeterPlan::new();
        let targets: Vec<MeterTarget> = (0..70)
            .map(|i| MeterTarget::App(AppKey(format!("bin:app{i:02}"))))
            .collect();
        plan.set_targets(targets.clone());
        let d = plan.reconcile(&m);
        assert_eq!(creates(&d).len(), METER_SLOTS);
        let a = d.assignments.expect("assignments");
        let assigned: Vec<&MeterTarget> = a.iter().map(|(t, _)| t).collect();
        assert_eq!(
            assigned,
            targets.iter().take(METER_SLOTS).collect::<Vec<_>>()
        );
    }

    #[test]
    fn removed_stream_frees_its_slot_and_failed_nodes_wait() {
        let g = FakeGraph::standard();
        let mut m = mirror(&g);
        let mut plan = MeterPlan::new();
        let key = AppKey("bin:spotify".into());
        plan.set_targets(vec![MeterTarget::App(key.clone())]);
        let d = plan.reconcile(&m);
        let (slot, _) = creates(&d)[0];
        let spotify = *m
            .nodes
            .iter()
            .find(|(_, n)| n.kind == NodeKind::AppPlayback)
            .map(|(id, _)| id)
            .expect("spotify node");

        // A failed meter is not retried while the node stays.
        assert_eq!(plan.on_failed(slot), Some(MeterTarget::App(key.clone())));
        let d = plan.reconcile(&m);
        assert!(creates(&d).is_empty());
        assert_eq!(d.assignments, Some(vec![]));

        // The node goes away and comes back (app restarted) → metered again.
        m.apply(&GraphChange::NodeRemoved(spotify), "inst");
        assert!(plan.reconcile(&m).ops.is_empty());
        for ch in g.changes() {
            m.apply(&ch, "inst");
        }
        assert_eq!(creates(&plan.reconcile(&m)).len(), 1);

        // Unsubscribing destroys the meter.
        plan.set_targets(vec![]);
        let d = plan.reconcile(&m);
        assert!(matches!(d.ops[..], [PwOp::DestroyMeter { .. }]));
    }
}
