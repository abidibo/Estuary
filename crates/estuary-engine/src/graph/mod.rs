//! Mirror of the PipeWire registry (engine M2.4). Pure: fed with `GraphChange`s by the shell.

pub(crate) mod classify;

use std::collections::{BTreeMap, BTreeSet};

use crate::core::effects::OwnedKey;
use crate::meter::MeterSlotId;
use crate::model::{ChannelId, VirtualMicId, Volume};

pub(crate) type Props = BTreeMap<String, String>;

pub(crate) use classify::classify;

#[derive(Debug, Default)]
pub(crate) struct GraphMirror {
    pub nodes: BTreeMap<u32, NodeEntry>,
    pub ports: BTreeMap<u32, PortEntry>,
    pub links: BTreeMap<u32, LinkEntry>,
    pub clients: BTreeMap<u32, ClientEntry>,
    /// `(subject, key) → value`, only the `default` metadata.
    pub default_meta: BTreeMap<(u32, String), MetaEntry>,
    /// Names of metadata objects present, e.g. `default`, `sm-settings`.
    pub metadata_names: BTreeSet<String>,
    pub own_client_id: Option<u32>,
    pub core: Option<CoreInfo>,
    /// Active routes per card (`Audio/Device` id): `route.device` → `route.index` (M5.1).
    pub routes: BTreeMap<u32, BTreeMap<i32, i32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodeEntry {
    pub id: u32,
    pub serial: u64,
    pub client_id: Option<u32>,
    pub props: Props,
    pub kind: NodeKind,
    pub volume: Option<Volume>,
    pub info_received: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeKind {
    Owned(OwnedRole),
    ForeignEstuary,
    Output,
    Input,
    AppPlayback,
    AppCapture,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // payloads are consumed by the reconciler (M8)
pub(crate) enum OwnedRole {
    Channel(ChannelId),
    Vmic(VirtualMicId),
    Cell(String),
    Denoise(String),
    Meter(u16),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PortEntry {
    pub id: u32,
    pub node_id: u32,
    pub dir: PortDir,
    pub name: String,
    pub channel: String,
    pub monitor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortDir {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkEntry {
    pub id: u32,
    pub out_node: u32,
    pub out_port: u32,
    pub in_node: u32,
    pub in_port: u32,
    pub client_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClientEntry {
    pub id: u32,
    pub props: Props,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MetaEntry {
    pub type_: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CoreInfo {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GraphChange {
    CoreInfo(CoreInfo),
    ClientAdded(ClientEntry),
    ClientRemoved(u32),
    NodeAdded {
        id: u32,
        global_props: Props,
    },
    NodeInfo {
        id: u32,
        props: Props,
    },
    /// Partial Props update: a missing field keeps the previous value.
    NodeVolume {
        id: u32,
        channels: Option<Vec<f32>>,
        muted: Option<bool>,
    },
    NodeRemoved(u32),
    PortAdded(PortEntry),
    PortRemoved(u32),
    LinkAdded(LinkEntry),
    LinkRemoved(u32),
    MetadataAdded(String),
    MetadataRemoved(String),
    DefaultMetaProperty {
        subject: u32,
        key: String,
        type_: Option<String>,
        value: Option<String>,
    },
    /// Key-less metadata update: clear all keys of `subject`, or everything when `None`.
    DefaultMetaCleared {
        subject: Option<u32>,
    },
    ProxyError {
        key: OwnedKey,
        res: i32,
        message: String,
    },
    MeterStreamFailed {
        slot: MeterSlotId,
        message: String,
    },
    DeviceRoute {
        device_id: u32,
        index: i32,
        device: i32,
    },
    DeviceRemoved(u32),
    VolumeWriteFailed {
        node_id: u32,
    },
    SyncDone {
        seq: i32,
    },
    Disconnected,
}

/// What a change affected, so the core knows which follow-up work is needed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChangeImpact {
    pub topology: bool,
    pub snapshot: bool,
    pub capabilities: bool,
}

impl ChangeImpact {
    fn all() -> Self {
        Self {
            topology: true,
            snapshot: true,
            capabilities: false,
        }
    }
}

impl GraphMirror {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn apply(&mut self, change: &GraphChange, own_instance: &str) -> ChangeImpact {
        match change {
            GraphChange::CoreInfo(info) => {
                self.core = Some(info.clone());
                ChangeImpact {
                    capabilities: true,
                    ..Default::default()
                }
            }
            GraphChange::ClientAdded(c) => {
                if c.props.get("estuary.instance").map(String::as_str) == Some(own_instance) {
                    self.own_client_id = Some(c.id);
                }
                let is_wp = is_wireplumber(&c.props);
                self.clients.insert(c.id, c.clone());
                if self.own_client_id == Some(c.id) {
                    self.reclassify_all(own_instance);
                }
                ChangeImpact {
                    capabilities: is_wp,
                    snapshot: true,
                    ..Default::default()
                }
            }
            GraphChange::ClientRemoved(id) => {
                let was_wp = self
                    .clients
                    .remove(id)
                    .is_some_and(|c| is_wireplumber(&c.props));
                ChangeImpact {
                    capabilities: was_wp,
                    ..Default::default()
                }
            }
            GraphChange::NodeAdded { id, global_props } => {
                let client_id = parse_u32(global_props.get("client.id"));
                let serial = parse_u64(global_props.get("object.serial")).unwrap_or(u64::from(*id));
                let kind = classify(global_props, client_id, self.own_client_id, own_instance);
                self.nodes.insert(
                    *id,
                    NodeEntry {
                        id: *id,
                        serial,
                        client_id,
                        props: global_props.clone(),
                        kind,
                        volume: None,
                        info_received: false,
                    },
                );
                ChangeImpact::all()
            }
            GraphChange::NodeInfo { id, props } => {
                if props.is_empty() {
                    // Defensive: an update without props must never wipe a node's identity.
                    return ChangeImpact::default();
                }
                let own = self.own_client_id;
                let Some(n) = self.nodes.get_mut(id) else {
                    return ChangeImpact::default();
                };
                n.props = props.clone();
                if let Some(c) = parse_u32(props.get("client.id")) {
                    n.client_id = Some(c);
                }
                if let Some(s) = parse_u64(props.get("object.serial")) {
                    n.serial = s;
                }
                n.kind = classify(&n.props, n.client_id, own, own_instance);
                n.info_received = true;
                ChangeImpact::all()
            }
            GraphChange::NodeVolume {
                id,
                channels,
                muted,
            } => {
                let Some(n) = self.nodes.get_mut(id) else {
                    return ChangeImpact::default();
                };
                let mut v = n.volume.clone().unwrap_or_else(|| Volume::unity(2));
                if let Some(c) = channels {
                    v.channels.clone_from(c);
                }
                if let Some(m) = muted {
                    v.muted = *m;
                }
                if n.volume.as_ref() == Some(&v) {
                    return ChangeImpact::default();
                }
                n.volume = Some(v);
                ChangeImpact {
                    snapshot: true,
                    ..Default::default()
                }
            }
            GraphChange::NodeRemoved(id) => {
                if self.nodes.remove(id).is_none() {
                    return ChangeImpact::default();
                }
                // Keep the mirror consistent even if port/link removals arrive later or never.
                self.ports.retain(|_, p| p.node_id != *id);
                self.links
                    .retain(|_, l| l.out_node != *id && l.in_node != *id);
                self.default_meta.retain(|(subject, _), _| subject != id);
                ChangeImpact::all()
            }
            GraphChange::PortAdded(p) => {
                self.ports.insert(p.id, p.clone());
                ChangeImpact {
                    topology: true,
                    ..Default::default()
                }
            }
            GraphChange::PortRemoved(id) => {
                self.ports.remove(id);
                self.links
                    .retain(|_, l| l.out_port != *id && l.in_port != *id);
                ChangeImpact {
                    topology: true,
                    ..Default::default()
                }
            }
            GraphChange::LinkAdded(l) => {
                self.links.insert(l.id, l.clone());
                ChangeImpact::all()
            }
            GraphChange::LinkRemoved(id) => {
                if self.links.remove(id).is_some() {
                    ChangeImpact::all()
                } else {
                    ChangeImpact::default()
                }
            }
            GraphChange::MetadataAdded(name) => {
                self.metadata_names.insert(name.clone());
                ChangeImpact {
                    capabilities: true,
                    ..Default::default()
                }
            }
            GraphChange::MetadataRemoved(name) => {
                self.metadata_names.remove(name);
                if name == "default" {
                    self.default_meta.clear();
                }
                ChangeImpact {
                    capabilities: true,
                    snapshot: true,
                    ..Default::default()
                }
            }
            GraphChange::DefaultMetaProperty {
                subject,
                key,
                type_,
                value,
            } => {
                let k = (*subject, key.clone());
                match value {
                    Some(v) => {
                        self.default_meta.insert(
                            k,
                            MetaEntry {
                                type_: type_.clone(),
                                value: v.clone(),
                            },
                        );
                    }
                    None => {
                        self.default_meta.remove(&k);
                    }
                }
                ChangeImpact {
                    snapshot: true,
                    ..Default::default()
                }
            }
            GraphChange::DefaultMetaCleared { subject } => {
                match subject {
                    Some(s) => self.default_meta.retain(|(subj, _), _| subj != s),
                    None => self.default_meta.clear(),
                }
                ChangeImpact {
                    snapshot: true,
                    ..Default::default()
                }
            }
            GraphChange::DeviceRoute {
                device_id,
                index,
                device,
            } => {
                self.routes
                    .entry(*device_id)
                    .or_default()
                    .insert(*device, *index);
                ChangeImpact::default()
            }
            GraphChange::DeviceRemoved(id) => {
                self.routes.remove(id);
                ChangeImpact::default()
            }
            GraphChange::ProxyError { .. }
            | GraphChange::VolumeWriteFailed { .. }
            | GraphChange::MeterStreamFailed { .. }
            | GraphChange::SyncDone { .. } => ChangeImpact::default(),
            GraphChange::Disconnected => {
                self.clear();
                ChangeImpact::default()
            }
        }
    }

    fn reclassify_all(&mut self, own_instance: &str) {
        let own = self.own_client_id;
        for n in self.nodes.values_mut() {
            n.kind = classify(&n.props, n.client_id, own, own_instance);
        }
    }

    /// The card route feeding a device node: `(card id, route index, route device)`.
    pub fn route_of(&self, node: &NodeEntry) -> Option<(u32, i32, i32)> {
        let card = parse_u32(node.props.get("device.id"))?;
        let device: i32 = node.props.get("card.profile.device")?.parse().ok()?;
        let index = *self.routes.get(&card)?.get(&device)?;
        Some((card, index, device))
    }

    pub fn wireplumber_client(&self) -> Option<&ClientEntry> {
        self.clients.values().find(|c| is_wireplumber(&c.props))
    }

    /// The `node.name` stored in a `default.audio.*` metadata JSON value (`{"name": "<node.name>"}`).
    pub fn default_node_name(&self, key: &str) -> Option<String> {
        let entry = self.default_meta.get(&(0, key.to_owned()))?;
        parse_default_name(&entry.value)
    }
}

fn is_wireplumber(props: &Props) -> bool {
    props.get("application.name").map(String::as_str) == Some("WirePlumber")
}

pub(crate) fn parse_u32(v: Option<&String>) -> Option<u32> {
    v.and_then(|s| s.parse().ok())
}

pub(crate) fn parse_u64(v: Option<&String>) -> Option<u64> {
    v.and_then(|s| s.parse().ok())
}

/// Minimal extraction of `"name"` from the metadata JSON `{"name":"alsa_output..."}`.
fn parse_default_name(json: &str) -> Option<String> {
    let after = json.split_once("\"name\"")?.1;
    let after = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn default_name_json() {
        assert_eq!(
            parse_default_name(r#"{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}"#)
                .as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(
            parse_default_name(r#"{ "name" : "a\"b" }"#).as_deref(),
            Some("a\"b")
        );
        assert_eq!(parse_default_name("{}"), None);
    }

    #[test]
    fn node_removal_cascades() {
        let mut m = GraphMirror::default();
        m.apply(
            &GraphChange::NodeAdded {
                id: 10,
                global_props: props(&[]),
            },
            "x",
        );
        m.apply(
            &GraphChange::NodeAdded {
                id: 11,
                global_props: props(&[]),
            },
            "x",
        );
        m.apply(
            &GraphChange::PortAdded(PortEntry {
                id: 20,
                node_id: 10,
                dir: PortDir::Out,
                name: "out".into(),
                channel: "FL".into(),
                monitor: false,
            }),
            "x",
        );
        m.apply(
            &GraphChange::LinkAdded(LinkEntry {
                id: 30,
                out_node: 10,
                out_port: 20,
                in_node: 11,
                in_port: 21,
                client_id: None,
            }),
            "x",
        );
        m.apply(&GraphChange::NodeRemoved(10), "x");
        assert!(m.ports.is_empty());
        assert!(m.links.is_empty());
        assert!(m.nodes.contains_key(&11));
    }

    #[test]
    fn empty_info_props_keep_the_node() {
        let mut m = GraphMirror::default();
        let p = props(&[("node.name", "spk"), ("media.class", "Audio/Sink")]);
        m.apply(
            &GraphChange::NodeAdded {
                id: 1,
                global_props: p.clone(),
            },
            "x",
        );
        m.apply(&GraphChange::NodeInfo { id: 1, props: p }, "x");
        m.apply(
            &GraphChange::NodeInfo {
                id: 1,
                props: Props::new(),
            },
            "x",
        );
        assert_eq!(m.nodes[&1].kind, NodeKind::Output);
        assert_eq!(
            m.nodes[&1].props.get("node.name").map(String::as_str),
            Some("spk")
        );
    }

    #[test]
    fn own_client_reclassifies_nodes() {
        let mut m = GraphMirror::default();
        // Node arrives before our client's info: provisionally foreign.
        m.apply(
            &GraphChange::NodeAdded {
                id: 50,
                global_props: props(&[
                    ("node.name", "estuary.meter.3"),
                    ("estuary.role", "meter"),
                    ("client.id", "7"),
                ]),
            },
            "inst",
        );
        assert_eq!(m.nodes[&50].kind, NodeKind::ForeignEstuary);
        m.apply(
            &GraphChange::ClientAdded(ClientEntry {
                id: 7,
                props: props(&[("estuary.instance", "inst")]),
            }),
            "inst",
        );
        assert_eq!(m.own_client_id, Some(7));
        assert_eq!(m.nodes[&50].kind, NodeKind::Owned(OwnedRole::Meter(3)));
    }
}
