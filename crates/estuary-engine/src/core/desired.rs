//! Desired graph (engine M8.3): what should exist, derived from the setup and present devices.

use std::collections::BTreeMap;

use crate::graph::{GraphMirror, NodeKind};
use crate::model::{CellKey, ChannelId, DeviceKey, MatrixDest, MatrixSource, VirtualMicId, Volume};
use crate::setup::{Setup, cell_hash};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DesiredNodeKey {
    Channel(ChannelId),
    Vmic(VirtualMicId),
    Cell(CellKeyOrd),
}

/// `CellKey` with a total order (by its canonical string) for map keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CellKeyOrd(pub CellKey);

impl Ord for CellKeyOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        crate::setup::canonical_cell_key(&self.0).cmp(&crate::setup::canonical_cell_key(&other.0))
    }
}

impl PartialOrd for CellKeyOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DesiredNode {
    Channel { name: String, volume: Volume },
    Vmic { name: String, volume: Volume },
    Cell { gain_db: f32 },
}

/// A link endpoint before resolution to node/port ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Owned(DesiredNodeKey),
    Device(DeviceKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesiredLink {
    pub from: Endpoint,
    pub to: Endpoint,
    pub passive: bool,
}

#[derive(Debug, Default)]
pub(crate) struct DesiredGraph {
    pub nodes: BTreeMap<DesiredNodeKey, DesiredNode>,
    pub links: Vec<DesiredLink>,
    /// `estuary.cell.<hash>` → key, to recognise our own cell nodes in the registry.
    pub hash_to_key: BTreeMap<String, DesiredNodeKey>,
}

/// Passivity of cell nodes and their links (C5; confirmed by the M8.1 experiment).
pub(crate) const CELL_LINKS_PASSIVE: bool = true;

pub(crate) fn build_desired(setup: &Setup, m: &GraphMirror) -> DesiredGraph {
    let mut g = DesiredGraph::default();
    for c in &setup.channels {
        g.nodes.insert(
            DesiredNodeKey::Channel(c.id),
            DesiredNode::Channel {
                name: c.name.clone(),
                volume: c.volume.clone(),
            },
        );
    }
    for v in &setup.virtual_mics {
        g.nodes.insert(
            DesiredNodeKey::Vmic(v.id),
            DesiredNode::Vmic {
                name: v.name.clone(),
                volume: v.volume.clone(),
            },
        );
    }
    // Every setup cell maps back from its node name, so disabled or unavailable cells that
    // still have a node can be faded out and destroyed rather than treated as strangers.
    for cell in &setup.cells {
        g.hash_to_key.insert(
            cell_hash(&cell.key()),
            DesiredNodeKey::Cell(CellKeyOrd(cell.key())),
        );
    }
    let device_present = |key: &DeviceKey, kind: &NodeKind| {
        m.nodes
            .values()
            .any(|n| &n.kind == kind && n.props.get("node.name") == Some(&key.0))
    };
    for cell in setup.cells.iter().filter(|c| c.enabled) {
        let from = match &cell.source {
            MatrixSource::Channel { id } if g.nodes.contains_key(&DesiredNodeKey::Channel(*id)) => {
                Endpoint::Owned(DesiredNodeKey::Channel(*id))
            }
            MatrixSource::Input { key } if device_present(key, &NodeKind::Input) => {
                Endpoint::Device(key.clone())
            }
            // Denoisers arrive in M10.
            _ => continue,
        };
        let to = match &cell.dest {
            MatrixDest::Output { key } if device_present(key, &NodeKind::Output) => {
                Endpoint::Device(key.clone())
            }
            MatrixDest::VirtualMic { id } if g.nodes.contains_key(&DesiredNodeKey::Vmic(*id)) => {
                Endpoint::Owned(DesiredNodeKey::Vmic(*id))
            }
            _ => continue,
        };
        let key = DesiredNodeKey::Cell(CellKeyOrd(cell.key()));
        g.nodes.insert(
            key.clone(),
            DesiredNode::Cell {
                gain_db: cell.gain_db,
            },
        );
        g.links.push(DesiredLink {
            from,
            to: Endpoint::Owned(key.clone()),
            passive: CELL_LINKS_PASSIVE,
        });
        g.links.push(DesiredLink {
            from: Endpoint::Owned(key),
            to,
            passive: CELL_LINKS_PASSIVE,
        });
    }
    g
}
