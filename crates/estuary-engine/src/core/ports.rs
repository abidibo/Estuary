//! Port selection and matching (engine M8.4, master C5 amended).

use crate::graph::{GraphMirror, PortDir, PortEntry};

/// Which ports of a node take part in a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortRole {
    /// A channel (null sink) as a source: its monitor ports.
    Monitor,
    /// A hardware input as a source: its capture ports.
    Capture,
    /// An output device as a destination: its playback ports.
    Playback,
    /// A DSP node's `input_*` ports.
    FilterIn,
    /// A DSP node's `output_*` ports.
    FilterOut,
    /// A virtual mic's `input_*` ports.
    VmicIn,
}

pub(crate) fn ports(m: &GraphMirror, node_id: u32, role: PortRole) -> Vec<&PortEntry> {
    if role == PortRole::Capture {
        // A source's outputs. Virtual sources (null sinks with a source class) flag every
        // output `port.monitor`; prefer real capture ports, else take what there is.
        let outs: Vec<&PortEntry> = m
            .ports
            .values()
            .filter(|p| p.node_id == node_id && p.dir == PortDir::Out)
            .collect();
        let real: Vec<&PortEntry> = outs.iter().copied().filter(|p| !p.monitor).collect();
        return if real.is_empty() { outs } else { real };
    }
    m.ports
        .values()
        .filter(|p| p.node_id == node_id)
        .filter(|p| match role {
            PortRole::Monitor => p.dir == PortDir::Out && p.monitor,
            PortRole::Capture => unreachable!("handled above"),
            PortRole::Playback => p.dir == PortDir::In,
            PortRole::FilterIn | PortRole::VmicIn => {
                p.dir == PortDir::In && p.name.starts_with("input_")
            }
            PortRole::FilterOut => p.dir == PortDir::Out && p.name.starts_with("output_"),
        })
        .collect()
}

/// Front-pair channel of a port: FL/FR, AUX0/AUX1 as FL/FR when no FL/FR exist, MONO.
fn normalized<'a>(side: &[&'a PortEntry]) -> Vec<(&'static str, &'a PortEntry)> {
    let has_front = side.iter().any(|p| p.channel == "FL" || p.channel == "FR");
    side.iter()
        .filter_map(|p| {
            let ch = match p.channel.as_str() {
                "FL" => "FL",
                "FR" => "FR",
                "MONO" => "MONO",
                "AUX0" if !has_front => "FL",
                "AUX1" if !has_front => "FR",
                _ => return None,
            };
            Some((ch, *p))
        })
        .collect()
}

/// Port pairs to link (C5): equal channels; a MONO side feeds or takes both FL and FR.
pub(crate) fn match_ports<'a>(
    outs: &[&'a PortEntry],
    ins: &[&'a PortEntry],
) -> Vec<(&'a PortEntry, &'a PortEntry)> {
    let outs = normalized(outs);
    let ins = normalized(ins);
    let mut pairs = Vec::new();
    for (oc, o) in &outs {
        for (ic, i) in &ins {
            let matches = oc == ic || *oc == "MONO" || *ic == "MONO";
            if matches
                && !pairs
                    .iter()
                    .any(|(po, pi): &(&PortEntry, &PortEntry)| po.id == o.id && pi.id == i.id)
            {
                pairs.push((*o, *i));
            }
        }
    }
    pairs
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    fn port(id: u32, dir: PortDir, name: &str, ch: &str, monitor: bool) -> PortEntry {
        PortEntry {
            id,
            node_id: 1,
            dir,
            name: name.into(),
            channel: ch.into(),
            monitor,
        }
    }

    fn ids(pairs: &[(&PortEntry, &PortEntry)]) -> Vec<(u32, u32)> {
        pairs.iter().map(|(o, i)| (o.id, i.id)).collect()
    }

    #[test]
    fn stereo_to_stereo() {
        let o = [
            port(1, PortDir::Out, "output_FL", "FL", false),
            port(2, PortDir::Out, "output_FR", "FR", false),
        ];
        let i = [
            port(3, PortDir::In, "playback_FL", "FL", false),
            port(4, PortDir::In, "playback_FR", "FR", false),
        ];
        assert_eq!(
            ids(&match_ports(&[&o[0], &o[1]], &[&i[0], &i[1]])),
            vec![(1, 3), (2, 4)]
        );
    }

    #[test]
    fn mono_mic_feeds_both_inputs() {
        let o = [port(1, PortDir::Out, "capture_MONO", "MONO", false)];
        let i = [
            port(3, PortDir::In, "input_FL", "FL", false),
            port(4, PortDir::In, "input_FR", "FR", false),
        ];
        assert_eq!(
            ids(&match_ports(&[&o[0]], &[&i[0], &i[1]])),
            vec![(1, 3), (1, 4)]
        );
    }

    #[test]
    fn surround_device_uses_front_pair_only() {
        let o = [
            port(1, PortDir::Out, "output_FL", "FL", false),
            port(2, PortDir::Out, "output_FR", "FR", false),
        ];
        let chans = ["FL", "FR", "FC", "LFE", "RL", "RR"];
        let i: Vec<PortEntry> = chans
            .iter()
            .enumerate()
            .map(|(k, c)| {
                port(
                    10 + k as u32,
                    PortDir::In,
                    &format!("playback_{c}"),
                    c,
                    false,
                )
            })
            .collect();
        let ins: Vec<&PortEntry> = i.iter().collect();
        assert_eq!(
            ids(&match_ports(&[&o[0], &o[1]], &ins)),
            vec![(1, 10), (2, 11)]
        );
    }

    #[test]
    fn aux_only_interface_maps_to_front_pair() {
        let o = [
            port(1, PortDir::Out, "output_FL", "FL", false),
            port(2, PortDir::Out, "output_FR", "FR", false),
        ];
        let i = [
            port(3, PortDir::In, "playback_AUX0", "AUX0", false),
            port(4, PortDir::In, "playback_AUX1", "AUX1", false),
            port(5, PortDir::In, "playback_AUX2", "AUX2", false),
        ];
        assert_eq!(
            ids(&match_ports(&[&o[0], &o[1]], &[&i[0], &i[1], &i[2]])),
            vec![(1, 3), (2, 4)]
        );
    }

    #[test]
    fn monitor_vs_playback_selection() {
        let mut m = GraphMirror::default();
        for p in [
            port(1, PortDir::In, "playback_FL", "FL", false),
            port(2, PortDir::Out, "monitor_FL", "FL", true),
            port(3, PortDir::Out, "capture_FL", "FL", false),
        ] {
            m.ports.insert(p.id, p);
        }
        assert_eq!(
            ports(&m, 1, PortRole::Monitor)
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            ports(&m, 1, PortRole::Playback)
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            ports(&m, 1, PortRole::Capture)
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn virtual_source_captures_from_its_monitor_flagged_outputs() {
        let mut m = GraphMirror::default();
        for p in [
            port(1, PortDir::In, "input_FL", "FL", false),
            port(2, PortDir::Out, "capture_FL", "FL", true),
            port(3, PortDir::Out, "capture_FR", "FR", true),
        ] {
            m.ports.insert(p.id, p);
        }
        let mut ids: Vec<u32> = ports(&m, 1, PortRole::Capture)
            .iter()
            .map(|p| p.id)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 3]);
    }
}
