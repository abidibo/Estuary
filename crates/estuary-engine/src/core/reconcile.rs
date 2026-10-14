//! The reconciler (engine M8.5–M8.9, master C8): compares the desired graph with the mirror and
//! emits idempotent create/destroy/link ops. Level-triggered: every pass recomputes from scratch.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::desired::{CellKeyOrd, DesiredGraph, DesiredNode, DesiredNodeKey, Endpoint};
use super::effects::{LinkKey, OwnedKey, PwOp};
use super::ports::{PortRole, match_ports, ports};
use crate::graph::{GraphMirror, NodeEntry, NodeKind, OwnedRole, Props};
use crate::model::{CellKey, CellStatus, DeviceKey, MatrixDest, MatrixSource};
use crate::setup::{Setup, cell_hash};

/// A creation not confirmed by the registry within this long counts as a failure (C8).
pub(crate) const PENDING_TIMEOUT: Duration = Duration::from_secs(2);
/// After this many consecutive failures an item is not retried (C8).
pub(crate) const MAX_FAILURES: u8 = 3;
/// Cells are created silent and faded in; disabled cells fade out before being destroyed.
pub(crate) const FADE_DB: f32 = -60.0;
pub(crate) const FADE_OUT: Duration = Duration::from_millis(50);

#[derive(Debug, Default)]
pub(crate) struct Reconciler {
    pending_nodes: BTreeMap<DesiredNodeKey, Instant>,
    pending_links: BTreeMap<LinkKey, Instant>,
    failures: BTreeMap<DesiredNodeKey, u8>,
    applied_gain: BTreeMap<CellKeyOrd, f32>,
    volume_applied: BTreeSet<DesiredNodeKey>,
    /// Disabled cells fading out: destroy after the deadline.
    fading: BTreeMap<CellKeyOrd, Instant>,
    statuses: BTreeMap<CellKeyOrd, CellStatus>,
    /// Items whose permanent failure was already reported.
    reported_failed: BTreeSet<DesiredNodeKey>,
}

#[derive(Debug, Default)]
pub(crate) struct PassOutput {
    pub ops: Vec<PwOp>,
    /// When to run again (fades ending, pending items expiring).
    pub rerun_in: Option<Duration>,
    /// Cells that just failed for good (→ `SetupIssue::CellFailed`).
    pub newly_failed: Vec<(CellKey, String)>,
}

impl Reconciler {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn status(&self, key: &CellKey) -> Option<&CellStatus> {
        self.statuses.get(&CellKeyOrd(key.clone()))
    }

    /// A setup change touching an item gives it fresh attempts (C8).
    pub fn statuses_debug(&self) -> Vec<String> {
        self.statuses.values().map(|s| format!("{s:?}")).collect()
    }

    pub fn forget_failures(&mut self) {
        self.failures.clear();
        self.reported_failed.clear();
    }

    /// A creation failed (proxy error or synchronous error).
    pub fn on_failure(&mut self, key: &OwnedKey) {
        match key {
            OwnedKey::Link(l) => {
                self.pending_links.remove(l);
            }
            other => {
                if let Some(k) = desired_key_of(other) {
                    self.pending_nodes.remove(&k);
                    *self.failures.entry(k).or_default() += 1;
                }
            }
        }
    }

    pub fn on_gain_applied(&mut self, key: &CellKey, db: f32) {
        self.applied_gain.insert(CellKeyOrd(key.clone()), db);
    }

    #[allow(clippy::too_many_lines)] // one pass, in the C8 order
    pub fn pass(
        &mut self,
        desired: &DesiredGraph,
        m: &GraphMirror,
        setup: &Setup,
        instance: &str,
        now: Instant,
    ) -> PassOutput {
        let mut out = PassOutput::default();
        let owned = owned_nodes(m, desired);

        // Expire pending creations.
        let expired: Vec<DesiredNodeKey> = self
            .pending_nodes
            .iter()
            .filter(|(k, since)| {
                !owned.contains_key(*k) && now.duration_since(**since) > PENDING_TIMEOUT
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            self.pending_nodes.remove(&k);
            *self.failures.entry(k).or_default() += 1;
        }
        self.pending_links
            .retain(|_, since| now.duration_since(*since) <= PENDING_TIMEOUT);
        for k in owned.keys() {
            self.pending_nodes.remove(k);
            self.failures.remove(k);
        }

        // Owned nodes that shouldn't exist (or must be recreated after a rename).
        for (key, node) in &owned {
            let rename = match (desired.nodes.get(key), key) {
                (Some(DesiredNode::Channel { name, .. } | DesiredNode::Vmic { name, .. }), _) => {
                    node.props.get("node.description") != Some(name)
                }
                _ => false,
            };
            if desired.nodes.contains_key(key) && !rename {
                self.fading
                    .retain(|c, _| DesiredNodeKey::Cell(c.clone()) != *key);
                continue;
            }
            if let DesiredNodeKey::Cell(c) = key
                && is_disabled_cell(setup, &c.0)
            {
                match self.fading.get(c) {
                    None => {
                        self.fading.insert(c.clone(), now + FADE_OUT);
                        out.ops.push(PwOp::SetGain {
                            key: c.0.clone(),
                            db: FADE_DB,
                        });
                        self.applied_gain.insert(c.clone(), FADE_DB);
                        out.rerun_in = min_dur(out.rerun_in, FADE_OUT + Duration::from_millis(5));
                        continue;
                    }
                    Some(deadline) if now < *deadline => {
                        out.rerun_in = min_dur(
                            out.rerun_in,
                            deadline.duration_since(now) + Duration::from_millis(5),
                        );
                        continue;
                    }
                    Some(_) => {
                        self.fading.remove(c);
                    }
                }
            }
            out.ops.push(PwOp::Destroy {
                key: owned_key_of(key),
                global_id: Some(node.id),
            });
            self.volume_applied.remove(key);
            if let DesiredNodeKey::Cell(c) = key {
                self.applied_gain.remove(c);
            }
        }
        // Owned nodes we can't map to anything desired (stale cells from an older setup).
        for n in m.nodes.values() {
            if let NodeKind::Owned(OwnedRole::Cell(hash)) = &n.kind
                && !desired.hash_to_key.contains_key(hash)
            {
                out.ops.push(PwOp::Destroy {
                    key: OwnedKey::StaleNode(n.id),
                    global_id: Some(n.id),
                });
            }
        }

        // Nodes to create.
        for (key, node) in &desired.nodes {
            let renamed = owned.get(key).is_some_and(|n| match node {
                DesiredNode::Channel { name, .. } | DesiredNode::Vmic { name, .. } => {
                    n.props.get("node.description") != Some(name)
                }
                DesiredNode::Cell { .. } => false,
            });
            if (owned.contains_key(key) && !renamed) || self.pending_nodes.contains_key(key) {
                continue;
            }
            if renamed {
                continue; // destroyed above; recreated on the next pass once it's gone
            }
            if self.failures.get(key).copied().unwrap_or(0) >= MAX_FAILURES {
                continue;
            }
            self.pending_nodes.insert(key.clone(), now);
            out.rerun_in = min_dur(out.rerun_in, PENDING_TIMEOUT + Duration::from_millis(50));
            match (key, node) {
                (DesiredNodeKey::Channel(id), DesiredNode::Channel { name, .. }) => {
                    out.ops.push(PwOp::CreateAdapter {
                        key: OwnedKey::Channel(*id),
                        props: adapter_props(
                            &format!("estuary.channel.{}", id.0.simple()),
                            name,
                            "Audio/Sink",
                            "channel",
                            instance,
                        ),
                    });
                }
                (DesiredNodeKey::Vmic(id), DesiredNode::Vmic { name, .. }) => {
                    out.ops.push(PwOp::CreateAdapter {
                        key: OwnedKey::Vmic(*id),
                        props: adapter_props(
                            &format!("estuary.vmic.{}", id.0.simple()),
                            name,
                            "Audio/Source/Virtual",
                            "vmic",
                            instance,
                        ),
                    });
                }
                (DesiredNodeKey::Cell(c), DesiredNode::Cell { gain_db }) => {
                    let name = format!("estuary.cell.{}", cell_hash(&c.0));
                    out.ops.push(PwOp::CreateCell {
                        key: c.0.clone(),
                        name: name.clone(),
                        props: cell_props(&name, &c.0, instance),
                        initial_db: FADE_DB,
                    });
                    // Fade in from silence (C11 ramp).
                    out.ops.push(PwOp::SetGain {
                        key: c.0.clone(),
                        db: *gain_db,
                    });
                    self.applied_gain.insert(c.clone(), *gain_db);
                }
                _ => {}
            }
        }

        // Links: create the desired ones, remember which port pairs are desired.
        let mut desired_pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut cell_waiting: BTreeSet<CellKeyOrd> = BTreeSet::new();
        for link in &desired.links {
            let cell = [&link.from, &link.to].into_iter().find_map(|e| match e {
                Endpoint::Owned(DesiredNodeKey::Cell(c)) => Some(c.clone()),
                _ => None,
            });
            let (Some((from_node, from_role)), Some((to_node, to_role))) = (
                resolve(m, &owned, &link.from, true),
                resolve(m, &owned, &link.to, false),
            ) else {
                if let Some(c) = cell {
                    cell_waiting.insert(c);
                }
                continue;
            };
            let pairs = match_ports(&ports(m, from_node, from_role), &ports(m, to_node, to_role));
            if pairs.is_empty() {
                if let Some(c) = cell {
                    cell_waiting.insert(c);
                }
                continue;
            }
            for (o, i) in pairs {
                desired_pairs.insert((o.id, i.id));
                let key = LinkKey {
                    out_port: o.id,
                    in_port: i.id,
                };
                if m.links
                    .values()
                    .any(|l| l.out_port == o.id && l.in_port == i.id)
                {
                    self.pending_links.remove(&key);
                    continue;
                }
                if let Some(c) = &cell {
                    cell_waiting.insert(c.clone());
                }
                if self.pending_links.contains_key(&key) {
                    continue;
                }
                self.pending_links.insert(key, now);
                out.rerun_in = min_dur(out.rerun_in, PENDING_TIMEOUT + Duration::from_millis(50));
                out.ops.push(PwOp::CreateLink {
                    key,
                    out_node: from_node,
                    out_port: o.id,
                    in_node: to_node,
                    in_port: i.id,
                    passive: link.passive,
                });
            }
        }

        // Links to destroy (C8): touching an owned cell, both ends ours or referenced devices,
        // and not desired. App→channel and vmic→recorder links are never touched.
        let referenced = referenced_devices(m, setup);
        let ours = |id: u32| {
            m.nodes
                .get(&id)
                .is_some_and(|n| matches!(n.kind, NodeKind::Owned(_)) || referenced.contains(&id))
        };
        let is_cell = |id: u32| {
            m.nodes
                .get(&id)
                .is_some_and(|n| matches!(n.kind, NodeKind::Owned(OwnedRole::Cell(_))))
        };
        for l in m.links.values() {
            if (is_cell(l.out_node) || is_cell(l.in_node))
                && ours(l.out_node)
                && ours(l.in_node)
                && !desired_pairs.contains(&(l.out_port, l.in_port))
            {
                out.ops.push(PwOp::Destroy {
                    key: OwnedKey::Link(LinkKey {
                        out_port: l.out_port,
                        in_port: l.in_port,
                    }),
                    global_id: Some(l.id),
                });
            }
        }

        // Gains and initial volumes.
        for (key, node) in &desired.nodes {
            match (key, node) {
                (DesiredNodeKey::Cell(c), DesiredNode::Cell { gain_db }) => {
                    if owned.contains_key(key)
                        && self
                            .applied_gain
                            .get(c)
                            .is_none_or(|g| (g - gain_db).abs() > f32::EPSILON)
                    {
                        out.ops.push(PwOp::SetGain {
                            key: c.0.clone(),
                            db: *gain_db,
                        });
                        self.applied_gain.insert(c.clone(), *gain_db);
                    }
                }
                (_, DesiredNode::Channel { volume, .. } | DesiredNode::Vmic { volume, .. }) => {
                    if let Some(n) = owned.get(key)
                        && n.info_received
                        && self.volume_applied.insert(key.clone())
                    {
                        out.ops.push(PwOp::SetNodeVolume {
                            node_id: n.id,
                            volume: volume.clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        // Statuses (M8.9).
        self.statuses.clear();
        for cell in &setup.cells {
            let c = CellKeyOrd(cell.key());
            let dkey = DesiredNodeKey::Cell(c.clone());
            let failed = self.failures.get(&dkey).copied().unwrap_or(0) >= MAX_FAILURES;
            if cell.enabled && failed && self.reported_failed.insert(dkey.clone()) {
                out.newly_failed
                    .push((cell.key(), "could not create the send".into()));
            }
            let status = if !cell.enabled {
                CellStatus::Active
            } else if failed {
                CellStatus::Error("could not create the send".into())
            } else if source_missing(m, &cell.source) {
                CellStatus::SourceMissing
            } else if dest_missing(m, &cell.dest) {
                CellStatus::DestinationMissing
            } else if !owned.contains_key(&dkey) || cell_waiting.contains(&c) {
                CellStatus::Pending
            } else {
                CellStatus::Active
            };
            self.statuses.insert(c, status);
        }
        out
    }
}

fn min_dur(a: Option<Duration>, b: Duration) -> Option<Duration> {
    Some(a.map_or(b, |a| a.min(b)))
}

fn is_disabled_cell(setup: &Setup, key: &CellKey) -> bool {
    setup.cells.iter().any(|c| &c.key() == key && !c.enabled)
}

/// Mirror nodes we own, keyed by what they implement.
fn owned_nodes<'m>(
    m: &'m GraphMirror,
    desired: &DesiredGraph,
) -> BTreeMap<DesiredNodeKey, &'m NodeEntry> {
    let mut out = BTreeMap::new();
    for n in m.nodes.values().filter(|n| n.info_received) {
        let key = match &n.kind {
            NodeKind::Owned(OwnedRole::Channel(id)) => Some(DesiredNodeKey::Channel(*id)),
            NodeKind::Owned(OwnedRole::Vmic(id)) => Some(DesiredNodeKey::Vmic(*id)),
            NodeKind::Owned(OwnedRole::Cell(hash)) => desired.hash_to_key.get(hash).cloned(),
            _ => None,
        };
        if let Some(k) = key {
            out.insert(k, n);
        }
    }
    out
}

fn device_node<'m>(m: &'m GraphMirror, key: &DeviceKey) -> Option<&'m NodeEntry> {
    m.nodes.values().find(|n| {
        matches!(n.kind, NodeKind::Output | NodeKind::Input)
            && n.props.get("node.name") == Some(&key.0)
    })
}

/// Node id and port role of a link endpoint (`as_source`: the link's output side).
fn resolve(
    m: &GraphMirror,
    owned: &BTreeMap<DesiredNodeKey, &NodeEntry>,
    e: &Endpoint,
    as_source: bool,
) -> Option<(u32, PortRole)> {
    match e {
        Endpoint::Owned(k) => {
            let n = owned.get(k)?;
            let role = match (k, as_source) {
                (DesiredNodeKey::Channel(_), true) => PortRole::Monitor,
                (DesiredNodeKey::Vmic(_), false) => PortRole::VmicIn,
                (DesiredNodeKey::Cell(_), true) => PortRole::FilterOut,
                (DesiredNodeKey::Cell(_), false) => PortRole::FilterIn,
                _ => return None,
            };
            Some((n.id, role))
        }
        Endpoint::Device(key) => {
            let n = device_node(m, key)?;
            let role = if as_source {
                PortRole::Capture
            } else {
                PortRole::Playback
            };
            Some((n.id, role))
        }
    }
}

fn referenced_devices(m: &GraphMirror, setup: &Setup) -> BTreeSet<u32> {
    let mut keys: Vec<&DeviceKey> = Vec::new();
    for c in &setup.cells {
        match &c.source {
            MatrixSource::Input { key } | MatrixSource::Denoised { key } => keys.push(key),
            MatrixSource::Channel { .. } => {}
        }
        if let MatrixDest::Output { key } = &c.dest {
            keys.push(key);
        }
    }
    keys.into_iter()
        .filter_map(|k| device_node(m, k))
        .map(|n| n.id)
        .collect()
}

fn source_missing(m: &GraphMirror, s: &MatrixSource) -> bool {
    match s {
        MatrixSource::Input { key } | MatrixSource::Denoised { key } => {
            device_node(m, key).is_none()
        }
        MatrixSource::Channel { .. } => false,
    }
}

fn dest_missing(m: &GraphMirror, d: &MatrixDest) -> bool {
    match d {
        MatrixDest::Output { key } => device_node(m, key).is_none(),
        MatrixDest::VirtualMic { .. } => false,
    }
}

fn owned_key_of(k: &DesiredNodeKey) -> OwnedKey {
    match k {
        DesiredNodeKey::Channel(id) => OwnedKey::Channel(*id),
        DesiredNodeKey::Vmic(id) => OwnedKey::Vmic(*id),
        DesiredNodeKey::Cell(c) => OwnedKey::Cell(c.0.clone()),
    }
}

fn desired_key_of(k: &OwnedKey) -> Option<DesiredNodeKey> {
    match k {
        OwnedKey::Channel(id) => Some(DesiredNodeKey::Channel(*id)),
        OwnedKey::Vmic(id) => Some(DesiredNodeKey::Vmic(*id)),
        OwnedKey::Cell(c) => Some(DesiredNodeKey::Cell(CellKeyOrd(c.clone()))),
        _ => None,
    }
}

/// C5 props for a channel (null sink) or virtual mic.
fn adapter_props(
    node_name: &str,
    description: &str,
    media_class: &str,
    role: &str,
    instance: &str,
) -> Props {
    [
        ("factory.name", "support.null-audio-sink"),
        ("node.name", node_name),
        ("node.description", description),
        ("media.class", media_class),
        ("audio.position", "[ FL FR ]"),
        ("monitor.channel-volumes", "true"),
        ("monitor.passthrough", "true"),
        ("node.virtual", "true"),
        ("object.linger", "false"),
        ("estuary.role", role),
        ("estuary.instance", instance),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

/// C5 props for a cell (gain `FilterNode`).
fn cell_props(node_name: &str, key: &CellKey, instance: &str) -> Props {
    [
        ("node.name", node_name.to_owned()),
        (
            "node.description",
            format!("Estuary send: {}", crate::setup::canonical_cell_key(key)),
        ),
        ("media.type", "Audio".to_owned()),
        ("media.category", "Filter".to_owned()),
        ("media.role", "DSP".to_owned()),
        ("node.autoconnect", "false".to_owned()),
        (
            "node.passive",
            super::desired::CELL_LINKS_PASSIVE.to_string(),
        ),
        ("estuary.role", "cell".to_owned()),
        ("estuary.instance", instance.to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}
