//! Registry props → `NodeKind` (engine "Registry → model classification" table, C4/C5/C7 amended).

use uuid::Uuid;

use super::{NodeKind, OwnedRole, Props};
use crate::model::{ChannelId, VirtualMicId};

pub(crate) fn classify(
    props: &Props,
    client_id: Option<u32>,
    own_client_id: Option<u32>,
    own_instance: &str,
) -> NodeKind {
    let get = |k: &str| props.get(k).map(String::as_str);
    let name = get("node.name").unwrap_or("");

    if name.starts_with("estuary.") {
        // Ownership (C5 amended): our client id, or our per-process instance id.
        let owned = (client_id.is_some() && client_id == own_client_id)
            || get("estuary.instance") == Some(own_instance);
        if !owned {
            return NodeKind::ForeignEstuary;
        }
        return parse_role(name, get("estuary.role"))
            .map_or(NodeKind::ForeignEstuary, NodeKind::Owned);
    }

    match get("media.class") {
        Some("Audio/Sink") => NodeKind::Output,
        Some("Audio/Source" | "Audio/Source/Virtual") => NodeKind::Input,
        Some("Stream/Output/Audio") if get("stream.monitor") != Some("true") => {
            NodeKind::AppPlayback
        }
        Some("Stream/Input/Audio") if get("stream.monitor") != Some("true") => NodeKind::AppCapture,
        _ => NodeKind::Other,
    }
}

fn parse_role(name: &str, role: Option<&str>) -> Option<OwnedRole> {
    let suffix = |prefix: &str| name.strip_prefix(prefix).filter(|s| !s.is_empty());
    match role? {
        "channel" => Uuid::parse_str(suffix("estuary.channel.")?)
            .ok()
            .map(|u| OwnedRole::Channel(ChannelId(u))),
        "vmic" => Uuid::parse_str(suffix("estuary.vmic.")?)
            .ok()
            .map(|u| OwnedRole::Vmic(VirtualMicId(u))),
        "cell" => suffix("estuary.cell.").map(|h| OwnedRole::Cell(h.to_owned())),
        "denoise" => suffix("estuary.denoise.").map(|h| OwnedRole::Denoise(h.to_owned())),
        "meter" => suffix("estuary.meter.")?.parse().ok().map(OwnedRole::Meter),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INST: &str = "inst";

    fn props(pairs: &[(&str, &str)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn kind(pairs: &[(&str, &str)], client: Option<u32>, own: Option<u32>) -> NodeKind {
        classify(&props(pairs), client, own, INST)
    }

    #[test]
    fn owned_roles() {
        let uuid = "5f0c7c2e1a2b4c3d9e8f001122334455";
        let ch = kind(
            &[
                ("node.name", &format!("estuary.channel.{uuid}")),
                ("estuary.role", "channel"),
            ],
            Some(7),
            Some(7),
        );
        assert_eq!(
            ch,
            NodeKind::Owned(OwnedRole::Channel(ChannelId(
                Uuid::parse_str(uuid).unwrap()
            )))
        );
        let vm = kind(
            &[
                ("node.name", &format!("estuary.vmic.{uuid}")),
                ("estuary.role", "vmic"),
                ("estuary.instance", INST),
            ],
            None,
            Some(7),
        );
        assert!(matches!(vm, NodeKind::Owned(OwnedRole::Vmic(_))));
        assert_eq!(
            kind(
                &[
                    ("node.name", "estuary.cell.abc123"),
                    ("estuary.role", "cell")
                ],
                Some(7),
                Some(7)
            ),
            NodeKind::Owned(OwnedRole::Cell("abc123".into()))
        );
        assert_eq!(
            kind(
                &[
                    ("node.name", "estuary.denoise.ff00"),
                    ("estuary.role", "denoise")
                ],
                Some(7),
                Some(7)
            ),
            NodeKind::Owned(OwnedRole::Denoise("ff00".into()))
        );
        assert_eq!(
            kind(
                &[("node.name", "estuary.meter.12"), ("estuary.role", "meter")],
                Some(7),
                Some(7)
            ),
            NodeKind::Owned(OwnedRole::Meter(12))
        );
    }

    #[test]
    fn foreign_estuary_is_ignored() {
        // Another instance's node: different client, different instance id.
        assert_eq!(
            kind(
                &[
                    ("node.name", "estuary.meter.1"),
                    ("estuary.role", "meter"),
                    ("estuary.instance", "other")
                ],
                Some(9),
                Some(7),
            ),
            NodeKind::ForeignEstuary
        );
        // Owned but malformed role → not trusted.
        assert_eq!(
            kind(
                &[
                    ("node.name", "estuary.channel.not-a-uuid"),
                    ("estuary.role", "channel")
                ],
                Some(7),
                Some(7)
            ),
            NodeKind::ForeignEstuary
        );
    }

    #[test]
    fn media_classes() {
        assert_eq!(
            kind(&[("media.class", "Audio/Sink")], None, None),
            NodeKind::Output
        );
        assert_eq!(
            kind(&[("media.class", "Audio/Source")], None, None),
            NodeKind::Input
        );
        assert_eq!(
            kind(&[("media.class", "Audio/Source/Virtual")], None, None),
            NodeKind::Input
        );
        assert_eq!(
            kind(&[("media.class", "Stream/Output/Audio")], None, None),
            NodeKind::AppPlayback
        );
        assert_eq!(
            kind(&[("media.class", "Stream/Input/Audio")], None, None),
            NodeKind::AppCapture
        );
        assert_eq!(
            kind(&[("media.class", "Video/Source")], None, None),
            NodeKind::Other
        );
        assert_eq!(kind(&[], None, None), NodeKind::Other);
    }

    #[test]
    fn monitor_streams_are_excluded() {
        assert_eq!(
            kind(
                &[
                    ("media.class", "Stream/Output/Audio"),
                    ("stream.monitor", "true")
                ],
                None,
                None
            ),
            NodeKind::Other
        );
        assert_eq!(
            kind(
                &[
                    ("media.class", "Stream/Input/Audio"),
                    ("stream.monitor", "true")
                ],
                None,
                None
            ),
            NodeKind::Other
        );
    }
}
