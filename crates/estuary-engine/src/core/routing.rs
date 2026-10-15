//! Per-app routing (engine M5.3–M5.6, master C5/C7): app rules are applied by writing
//! `target.object` in the `default` metadata; moves made elsewhere are reported as
//! `ExternalRetarget` instead of being undone. Pure: works on the mirror and returns ops/events.

use std::collections::{BTreeMap, BTreeSet};

use super::effects::PwOp;
use crate::appkey;
use crate::graph::{GraphMirror, NodeEntry, NodeKind, OwnedRole};
use crate::model::{AppKey, DeviceKey, RouteTarget};
use crate::setup::Setup;

pub(crate) const TARGET_KEY: &str = "target.object";

#[derive(Debug, Clone, PartialEq)]
struct StreamTrack {
    key: AppKey,
    /// C7: first link observed or 500 ms elapsed. Before that, metadata writes are WirePlumber's
    /// own placement, never "external".
    settled: bool,
    /// The value we last wrote (`Some(None)` = we deleted the key), until its echo arrives.
    awaiting_ack: Option<Option<String>>,
    /// Moved elsewhere: don't re-apply the old rule until the UI sends an updated setup.
    external_pending: bool,
    /// The node it was last seen linked to. Only a change of it can be an external move: where
    /// a stream already plays when first seen (e.g. before our channels exist) is not a move.
    last_linked: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct RoutingState {
    tracks: BTreeMap<u32, StreamTrack>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct RoutingOutput {
    pub ops: Vec<PwOp>,
    pub external: Vec<(AppKey, RouteTarget)>,
    /// New streams that need a settle timer.
    pub settle_timers: Vec<u32>,
}

impl RoutingOutput {
    pub fn merge_from(&mut self, o: RoutingOutput) {
        self.merge(o);
    }

    fn merge(&mut self, o: RoutingOutput) {
        self.ops.extend(o.ops);
        self.external.extend(o.external);
        self.settle_timers.extend(o.settle_timers);
    }
}

impl RoutingState {
    pub fn reset(&mut self) {
        self.tracks.clear();
    }

    /// Start tracking new playback streams, forget removed ones, settle linked ones.
    pub fn sync(&mut self, m: &GraphMirror, setup: Option<&Setup>) -> RoutingOutput {
        let mut out = RoutingOutput::default();
        let live: BTreeSet<u32> = playback_streams(m).map(|n| n.id).collect();
        self.tracks.retain(|id, _| live.contains(id));
        for n in playback_streams(m) {
            if let std::collections::btree_map::Entry::Vacant(e) = self.tracks.entry(n.id) {
                e.insert(StreamTrack {
                    key: appkey::derive(&n.props),
                    settled: false,
                    awaiting_ack: None,
                    external_pending: false,
                    last_linked: None,
                });
                out.settle_timers.push(n.id);
            }
        }
        // A first output link also settles a stream (C7).
        let linked: Vec<u32> = self
            .tracks
            .iter()
            .filter(|(id, t)| !t.settled && m.links.values().any(|l| l.out_node == **id))
            .map(|(id, _)| *id)
            .collect();
        for id in linked {
            out.merge(self.settle(id, m, setup));
        }
        out
    }

    pub fn settle(&mut self, id: u32, m: &GraphMirror, setup: Option<&Setup>) -> RoutingOutput {
        match self.tracks.get_mut(&id) {
            Some(t) if !t.settled => t.settled = true,
            _ => return RoutingOutput::default(),
        }
        self.evaluate(id, m, setup)
    }

    /// Re-apply rules to every settled stream (setup change, node appeared, rebuild).
    pub fn evaluate_all(&mut self, m: &GraphMirror, setup: Option<&Setup>) -> RoutingOutput {
        let ids: Vec<u32> = self.tracks.keys().copied().collect();
        let mut out = RoutingOutput::default();
        for id in ids {
            out.merge(self.evaluate(id, m, setup));
        }
        out
    }

    /// A new setup from the UI answers any pending external moves.
    pub fn on_setup(&mut self) {
        for t in self.tracks.values_mut() {
            t.external_pending = false;
        }
    }

    fn evaluate(&mut self, id: u32, m: &GraphMirror, setup: Option<&Setup>) -> RoutingOutput {
        let mut out = RoutingOutput::default();
        let (Some(track), Some(node), Some(setup)) =
            (self.tracks.get_mut(&id), m.nodes.get(&id), setup)
        else {
            return out;
        };
        if !track.settled || track.external_pending || dont_move(node) {
            return out;
        }
        let Some(rule) = setup.app_rules.iter().find(|r| r.app == track.key) else {
            return out;
        };
        let value = match &rule.target {
            RouteTarget::Default => None,
            target => match target_node(m, target) {
                Some(n) => Some(n.serial.to_string()),
                None => return out, // target absent (offline device, channel not built): retry later
            },
        };
        if current_target(m, id) == rule.target {
            return out;
        }
        if track.awaiting_ack.as_ref() == Some(&value) {
            return out; // already written, echo pending
        }
        tracing::debug!(stream = id, app = %track.key.0, ?value, "applying app rule");
        track.awaiting_ack = Some(value.clone());
        out.ops.push(PwOp::MetaWrite {
            subject: id,
            key: TARGET_KEY,
            type_: value.is_some().then_some("Spa:Id"),
            value,
        });
        out
    }

    /// A `target.object` change in the `default` metadata (C7 external-change rule, amended).
    pub fn on_metadata(
        &mut self,
        subject: u32,
        value: Option<&str>,
        m: &GraphMirror,
    ) -> RoutingOutput {
        let mut out = RoutingOutput::default();
        let Some(track) = self.tracks.get_mut(&subject) else {
            return out;
        };
        let value = value.map(str::to_owned);
        tracing::debug!(stream = subject, ?value, awaiting = ?track.awaiting_ack, settled = track.settled, "target.object changed");
        if track.awaiting_ack.as_ref() == Some(&value) {
            track.awaiting_ack = None; // our own echo
            return out;
        }
        if !track.settled {
            return out; // WirePlumber's restore or initial placement: mirror only
        }
        track.awaiting_ack = None;
        track.external_pending = true;
        out.external
            .push((track.key.clone(), resolve_value(m, value.as_deref())));
        out
    }

    /// Link-based external detection (C7 amended): a settled stream without `target.object`
    /// whose rule isn't Default moves to a node that is neither its rule target nor the default
    /// sink. Following the default sink never counts.
    pub fn on_links(&mut self, m: &GraphMirror, setup: Option<&Setup>) -> RoutingOutput {
        let mut out = RoutingOutput::default();
        let Some(setup) = setup else { return out };
        let default_sink = m.default_node_name("default.audio.sink");
        for (id, track) in &mut self.tracks {
            let Some(linked) = linked_node(m, *id) else {
                continue;
            };
            // First sighting or same node: no move happened.
            let previous = track.last_linked.replace(linked.id);
            if previous.is_none_or(|p| p == linked.id) {
                continue;
            }
            if !track.settled
                || track.external_pending
                || track.awaiting_ack.is_some()
                || m.default_meta.contains_key(&(*id, TARGET_KEY.to_owned()))
            {
                continue;
            }
            let Some(rule) = setup.app_rules.iter().find(|r| r.app == track.key) else {
                continue;
            };
            if rule.target == RouteTarget::Default {
                continue;
            }
            let is_rule_target = target_node(m, &rule.target).is_some_and(|n| n.id == linked.id);
            let is_default = linked.props.get("node.name") == default_sink.as_ref();
            if !is_rule_target && !is_default {
                track.external_pending = true;
                out.external
                    .push((track.key.clone(), node_to_target(m, linked)));
            }
        }
        out
    }
}

fn playback_streams(m: &GraphMirror) -> impl Iterator<Item = &NodeEntry> {
    m.nodes
        .values()
        .filter(|n| n.kind == NodeKind::AppPlayback && n.info_received)
}

fn dont_move(n: &NodeEntry) -> bool {
    n.props.get("node.dont-move").map(String::as_str) == Some("true")
}

/// The node a rule target refers to, if present.
pub(crate) fn target_node<'m>(m: &'m GraphMirror, t: &RouteTarget) -> Option<&'m NodeEntry> {
    match t {
        RouteTarget::Default => None,
        RouteTarget::Device { key } => m
            .nodes
            .values()
            .find(|n| n.kind == NodeKind::Output && n.props.get("node.name") == Some(&key.0)),
        RouteTarget::Channel { id } => m
            .nodes
            .values()
            .find(|n| matches!(&n.kind, NodeKind::Owned(OwnedRole::Channel(c)) if c == id)),
    }
}

fn node_to_target(m: &GraphMirror, n: &NodeEntry) -> RouteTarget {
    match &n.kind {
        NodeKind::Owned(OwnedRole::Channel(id)) => RouteTarget::Channel { id: *id },
        NodeKind::Output => {
            let name = n.props.get("node.name");
            if name.is_some() && name == m.default_node_name("default.audio.sink").as_ref() {
                RouteTarget::Default
            } else {
                name.map_or(RouteTarget::Default, |k| RouteTarget::Device {
                    key: DeviceKey(k.clone()),
                })
            }
        }
        _ => RouteTarget::Default,
    }
}

/// A `target.object` value: numeric = `object.serial`, otherwise `node.name` (C5). Unknown,
/// foreign or deleted → `Default`.
fn resolve_value(m: &GraphMirror, value: Option<&str>) -> RouteTarget {
    let Some(v) = value else {
        return RouteTarget::Default;
    };
    let node = match v.parse::<u64>() {
        Ok(serial) => m.nodes.values().find(|n| n.serial == serial),
        Err(_) => m
            .nodes
            .values()
            .find(|n| n.props.get("node.name").map(String::as_str) == Some(v)),
    };
    match node {
        Some(n) => match &n.kind {
            NodeKind::Owned(OwnedRole::Channel(id)) => RouteTarget::Channel { id: *id },
            NodeKind::Output => {
                n.props
                    .get("node.name")
                    .map_or(RouteTarget::Default, |k| RouteTarget::Device {
                        key: DeviceKey(k.clone()),
                    })
            }
            _ => RouteTarget::Default,
        },
        None => RouteTarget::Default,
    }
}

fn linked_node(m: &GraphMirror, stream_id: u32) -> Option<&NodeEntry> {
    m.links
        .values()
        .find(|l| l.out_node == stream_id)
        .and_then(|l| m.nodes.get(&l.in_node))
}

/// Where a stream plays now (engine M5.6): explicit metadata target first, then its links;
/// a stream on the default sink, or unlinked, is `Default`.
pub(crate) fn current_target(m: &GraphMirror, stream_id: u32) -> RouteTarget {
    if let Some(entry) = m.default_meta.get(&(stream_id, TARGET_KEY.to_owned())) {
        return resolve_value(m, Some(&entry.value));
    }
    linked_node(m, stream_id).map_or(RouteTarget::Default, |n| node_to_target(m, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphChange;
    use crate::setup::AppRule;
    use crate::testing::FakeGraph;

    const SPK: &str = "alsa_output.usb-speakers";

    fn mirror(g: &FakeGraph) -> GraphMirror {
        let mut m = GraphMirror::default();
        for ch in g.changes() {
            m.apply(&ch, "inst");
        }
        m
    }

    fn rule(app: &str, target: RouteTarget) -> Setup {
        Setup {
            app_rules: vec![AppRule {
                app: AppKey(app.into()),
                target,
                display_name: String::new(),
            }],
            ..Default::default()
        }
    }

    fn graph() -> FakeGraph {
        FakeGraph::standard().device(SPK, "USB Speakers", "Audio/Sink")
    }

    fn spotify(m: &GraphMirror) -> u32 {
        m.nodes
            .values()
            .find(|n| n.kind == NodeKind::AppPlayback)
            .map(|n| n.id)
            .expect("spotify")
    }

    fn serial_of(m: &GraphMirror, name: &str) -> String {
        m.nodes
            .values()
            .find(|n| n.props.get("node.name").map(String::as_str) == Some(name))
            .map(|n| n.serial.to_string())
            .expect("node")
    }

    fn device(name: &str) -> RouteTarget {
        RouteTarget::Device {
            key: DeviceKey(name.into()),
        }
    }

    #[test]
    fn rule_applied_after_settle_exactly_once() {
        let m = mirror(&graph());
        let setup = rule("bin:spotify", device(SPK));
        let mut r = RoutingState::default();
        let out = r.sync(&m, Some(&setup));
        // The fake stream is already linked to the default output → settles immediately.
        assert_eq!(out.settle_timers, vec![spotify(&m)]);
        assert_eq!(
            out.ops,
            vec![PwOp::MetaWrite {
                subject: spotify(&m),
                key: TARGET_KEY,
                type_: Some("Spa:Id"),
                value: Some(serial_of(&m, SPK)),
            }]
        );
        // Re-evaluating before the echo writes nothing more.
        assert!(r.evaluate_all(&m, Some(&setup)).ops.is_empty());
    }

    #[test]
    fn absent_target_waits_then_applies() {
        let target_name = "bluez_output.headphones";
        let m = mirror(&FakeGraph::standard());
        let setup = rule("bin:spotify", device(target_name));
        let mut r = RoutingState::default();
        assert!(
            r.sync(&m, Some(&setup)).ops.is_empty(),
            "headphones offline: no write"
        );
        let m = mirror(&FakeGraph::standard().device(target_name, "Headphones", "Audio/Sink"));
        let out = r.evaluate_all(&m, Some(&setup));
        assert_eq!(
            out.ops.len(),
            1,
            "headphones appeared: rule applied (F10 auto-restore)"
        );
    }

    #[test]
    fn echo_is_ours_external_change_is_reported_once() {
        let mut m = mirror(&graph());
        let setup = rule("bin:spotify", device(SPK));
        let mut r = RoutingState::default();
        r.sync(&m, Some(&setup));
        let id = spotify(&m);
        let spk = serial_of(&m, SPK);
        // Our echo.
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: id,
                key: TARGET_KEY.into(),
                type_: None,
                value: Some(spk.clone()),
            },
            "inst",
        );
        assert_eq!(r.on_metadata(id, Some(&spk), &m), RoutingOutput::default());
        // Someone moves it back to the built-in output.
        let builtin = serial_of(&m, FakeGraph::OUTPUT);
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: id,
                key: TARGET_KEY.into(),
                type_: None,
                value: Some(builtin.clone()),
            },
            "inst",
        );
        let out = r.on_metadata(id, Some(&builtin), &m);
        assert_eq!(
            out.external,
            vec![(AppKey("bin:spotify".into()), device(FakeGraph::OUTPUT))]
        );
        // No ping-pong: the old rule is not re-applied before the UI answers.
        assert!(r.evaluate_all(&m, Some(&setup)).ops.is_empty());
        // The UI adopts the move: nothing to write.
        let adopted = rule("bin:spotify", device(FakeGraph::OUTPUT));
        r.on_setup();
        assert!(r.evaluate_all(&m, Some(&adopted)).ops.is_empty());
    }

    #[test]
    fn writes_before_settle_are_not_external() {
        let g = FakeGraph::standard().app_unlinked("mpv", "mpv");
        let m = mirror(&g);
        let mut r = RoutingState::default();
        let out = r.sync(&m, None);
        let mpv = m
            .nodes
            .values()
            .find(|n| {
                n.props
                    .get("application.process.binary")
                    .map(String::as_str)
                    == Some("mpv")
            })
            .map(|n| n.id)
            .expect("mpv");
        assert!(out.settle_timers.contains(&mpv));
        assert!(
            r.on_metadata(mpv, Some("123"), &m).external.is_empty(),
            "WP restore before settle"
        );
    }

    #[test]
    fn dont_move_streams_are_never_written() {
        let g = graph().app_dont_move("pulse-app", "Pulse App", FakeGraph::OUTPUT);
        let m = mirror(&g);
        let setup = rule("bin:pulse-app", device(SPK));
        let mut r = RoutingState::default();
        assert!(r.sync(&m, Some(&setup)).ops.is_empty());
    }

    #[test]
    fn rule_back_to_default_deletes_the_key() {
        let mut m = mirror(&graph());
        let id = spotify(&m);
        let spk = serial_of(&m, SPK);
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: id,
                key: TARGET_KEY.into(),
                type_: None,
                value: Some(spk),
            },
            "inst",
        );
        let mut r = RoutingState::default();
        let out = r.sync(&m, Some(&rule("bin:spotify", RouteTarget::Default)));
        assert_eq!(
            out.ops,
            vec![PwOp::MetaWrite {
                subject: id,
                key: TARGET_KEY,
                type_: None,
                value: None
            }]
        );
    }

    #[test]
    fn following_the_default_sink_is_not_external() {
        let mut m = mirror(&graph());
        let mut r = RoutingState::default();
        r.sync(&m, Some(&rule("bin:spotify", RouteTarget::Default)));
        // The default sink changes to the USB speakers; the stream follows (no metadata).
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: 0,
                key: "default.audio.sink".into(),
                type_: None,
                value: Some(format!("{{\"name\":\"{SPK}\"}}")),
            },
            "inst",
        );
        assert!(
            r.on_links(&m, Some(&rule("bin:spotify", RouteTarget::Default)))
                .external
                .is_empty()
        );
        assert_eq!(
            current_target(&m, spotify(&m)),
            device(FakeGraph::OUTPUT),
            "still linked to built-in, which is no longer default"
        );
    }

    #[test]
    fn current_target_resolution() {
        let mut m = mirror(&graph());
        let id = spotify(&m);
        assert_eq!(
            current_target(&m, id),
            RouteTarget::Default,
            "linked to the default sink"
        );
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: id,
                key: TARGET_KEY.into(),
                type_: None,
                value: Some(SPK.into()),
            },
            "inst",
        );
        assert_eq!(
            current_target(&m, id),
            device(SPK),
            "node.name values resolve too"
        );
        m.apply(
            &GraphChange::DefaultMetaProperty {
                subject: id,
                key: TARGET_KEY.into(),
                type_: None,
                value: Some("999999".into()),
            },
            "inst",
        );
        assert_eq!(
            current_target(&m, id),
            RouteTarget::Default,
            "unknown serial → Default"
        );
    }

    #[test]
    fn where_a_stream_already_plays_is_not_an_external_move() {
        // Live M12 finding: an app already on the speakers when the engine starts, its rule
        // pointing at headphones that aren't there yet. That must not rewrite the rule.
        let hp = "bluez_output.headphones";
        let g = graph().app_playing("mpv", "mpv", SPK);
        let m = mirror(&g);
        let setup = rule("bin:mpv", device(hp));
        let mut r = RoutingState::default();
        assert!(
            r.sync(&m, Some(&setup)).ops.is_empty(),
            "target absent: wait"
        );
        assert!(
            r.on_links(&m, Some(&setup)).external.is_empty(),
            "initial placement"
        );
        // The headphones appear: the rule is applied.
        let m = mirror(&graph().app_playing("mpv", "mpv", SPK).device(
            hp,
            "Headphones",
            "Audio/Sink",
        ));
        let out = r.evaluate_all(&m, Some(&setup));
        assert_eq!(out.ops.len(), 1);
        assert!(r.on_links(&m, Some(&setup)).external.is_empty());
    }

    #[test]
    fn a_later_move_by_links_is_external() {
        let other = "alsa_output.hdmi";
        let g = graph()
            .device(other, "HDMI", "Audio/Sink")
            .app_playing("mpv", "mpv", SPK);
        let mut m = mirror(&g);
        let setup = rule("bin:mpv", device("bluez_output.headphones"));
        let mut r = RoutingState::default();
        r.sync(&m, Some(&setup));
        assert!(
            r.on_links(&m, Some(&setup)).external.is_empty(),
            "first sighting"
        );
        // Something else relinks it to the HDMI output (not the default, not its rule target).
        let hdmi = m
            .nodes
            .values()
            .find(|n| n.props.get("node.name").map(String::as_str) == Some(other))
            .unwrap()
            .id;
        let mpv = m
            .nodes
            .values()
            .find(|n| n.kind == NodeKind::AppPlayback && appkey::derive(&n.props).0 == "bin:mpv")
            .unwrap()
            .id;
        for l in m.links.values_mut().filter(|l| l.out_node == mpv) {
            l.in_node = hdmi;
        }
        let out = r.on_links(&m, Some(&setup));
        assert_eq!(
            out.external,
            vec![(AppKey("bin:mpv".into()), device(other))]
        );
    }
}
