//! Mirror + setup → `GraphSnapshot` (engine M2.9, master C4 ordering).

use std::collections::BTreeMap;

use crate::appkey;
use crate::graph::{GraphMirror, NodeEntry, NodeKind, OwnedRole, parse_u32};
use crate::model::{
    AppGroup, AppKey, CellState, CellStatus, ChannelState, DenoiserState, DeviceInfo, DeviceKey,
    GraphSnapshot, StreamInfo, VirtualMicState, Volume,
};
use crate::setup::{DeviceKind, Setup};

pub(crate) fn build_snapshot(m: &GraphMirror, setup: Option<&Setup>) -> GraphSnapshot {
    let empty = Setup::default();
    let setup = setup.unwrap_or(&empty);

    let mut outputs = devices(m, &NodeKind::Output);
    let mut inputs = devices(m, &NodeKind::Input);
    // Keep-and-restore (F10): known devices that are not present appear as offline.
    for kd in &setup.known_devices {
        let list = match kd.kind {
            DeviceKind::Output => &mut outputs,
            DeviceKind::Input => &mut inputs,
        };
        if !list.iter().any(|d| d.key == kd.key) {
            list.push(DeviceInfo {
                key: kd.key.clone(),
                description: if kd.description.is_empty() {
                    kd.key.0.clone()
                } else {
                    kd.description.clone()
                },
                present: false,
                channels: 2,
                volume: None,
            });
        }
    }
    outputs.sort_by_key(|d| d.description.to_lowercase());
    inputs.sort_by_key(|d| d.description.to_lowercase());

    GraphSnapshot {
        apps: apps(m),
        outputs,
        inputs,
        channels: setup
            .channels
            .iter()
            .map(|c| {
                let node = owned_node(m, |r| matches!(r, OwnedRole::Channel(id) if *id == c.id));
                ChannelState {
                    id: c.id,
                    name: c.name.clone(),
                    present: node.is_some(),
                    volume: node
                        .and_then(|n| n.volume.clone())
                        .unwrap_or_else(|| c.volume.clone()),
                }
            })
            .collect(),
        virtual_mics: setup
            .virtual_mics
            .iter()
            .map(|v| {
                let node = owned_node(m, |r| matches!(r, OwnedRole::Vmic(id) if *id == v.id));
                VirtualMicState {
                    id: v.id,
                    name: v.name.clone(),
                    present: node.is_some(),
                    volume: node
                        .and_then(|n| n.volume.clone())
                        .unwrap_or_else(|| v.volume.clone()),
                    recording_apps: node.map(|n| recording_apps(m, n.id)).unwrap_or_default(),
                }
            })
            .collect(),
        denoisers: setup
            .denoisers
            .iter()
            .map(|d| DenoiserState {
                input: d.input.clone(),
                enabled: d.enabled,
                active: false,
                xruns: 0,
            })
            .collect(),
        cells: setup
            .cells
            .iter()
            .map(|c| CellState {
                key: c.key(),
                gain_db: c.gain_db,
                enabled: c.enabled,
                // Nothing is built before the reconciler exists (M8).
                status: CellStatus::Pending,
            })
            .collect(),
        default_output: m.default_node_name("default.audio.sink").map(DeviceKey),
        default_input: m.default_node_name("default.audio.source").map(DeviceKey),
    }
}

fn devices(m: &GraphMirror, kind: &NodeKind) -> Vec<DeviceInfo> {
    m.nodes
        .values()
        .filter(|n| &n.kind == kind && n.info_received)
        .filter_map(|n| {
            let key = n.props.get("node.name")?.clone();
            let description = ["node.description", "node.nick", "node.name"]
                .iter()
                .find_map(|k| n.props.get(*k).filter(|v| !v.is_empty()).cloned())
                .unwrap_or_else(|| key.clone());
            let channels = parse_u32(n.props.get("audio.channels"))
                .unwrap_or_else(|| port_count(m, n.id).max(1));
            Some(DeviceInfo {
                key: DeviceKey(key),
                description,
                present: true,
                channels,
                volume: n.volume.clone(),
            })
        })
        .collect()
}

fn port_count(m: &GraphMirror, node_id: u32) -> u32 {
    let n = m
        .ports
        .values()
        .filter(|p| p.node_id == node_id && !p.monitor)
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn apps(m: &GraphMirror) -> Vec<AppGroup> {
    let mut groups: BTreeMap<AppKey, Vec<&NodeEntry>> = BTreeMap::new();
    for n in m
        .nodes
        .values()
        .filter(|n| n.kind == NodeKind::AppPlayback && n.info_received)
    {
        groups.entry(appkey::derive(&n.props)).or_default().push(n);
    }
    let mut apps: Vec<AppGroup> = groups
        .into_iter()
        .map(|(key, streams)| {
            let first = streams[0];
            AppGroup {
                key,
                display_name: appkey::display_name(&first.props),
                icon_name: first.props.get("application.icon-name").cloned(),
                streams: streams
                    .iter()
                    .map(|s| StreamInfo {
                        node_id: s.id,
                        serial: s.serial,
                        media_name: s.props.get("media.name").cloned(),
                    })
                    .collect(),
                current_target: super::routing::current_target(m, first.id),
                movable: !streams
                    .iter()
                    .any(|s| s.props.get("node.dont-move").map(String::as_str) == Some("true")),
                volume: first.volume.clone().unwrap_or_else(|| Volume::unity(2)),
            }
        })
        .collect();
    apps.sort_by_key(|a| a.display_name.to_lowercase());
    apps
}

fn owned_node(m: &GraphMirror, pred: impl Fn(&OwnedRole) -> bool) -> Option<&NodeEntry> {
    m.nodes
        .values()
        .find(|n| matches!(&n.kind, NodeKind::Owned(r) if pred(r)))
}

fn recording_apps(m: &GraphMirror, vmic_node: u32) -> Vec<String> {
    let mut names: Vec<String> = m
        .links
        .values()
        .filter(|l| l.out_node == vmic_node)
        .filter_map(|l| m.nodes.get(&l.in_node))
        .filter(|n| n.kind == NodeKind::AppCapture)
        .map(|n| appkey::display_name(&n.props))
        .collect();
    names.sort();
    names.dedup();
    names
}
