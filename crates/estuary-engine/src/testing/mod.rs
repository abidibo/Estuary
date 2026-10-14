//! Test fixtures (engine M2.10). `FakeGraph` produces deterministic `GraphChange` sequences
//! that mirror what the PipeWire shell emits for a real session. Extended in M8 with a
//! `FakeShell` that simulates object creation.

use crate::graph::{ClientEntry, CoreInfo, GraphChange, LinkEntry, PortDir, PortEntry, Props};

fn props(pairs: &[(&str, &str)]) -> Props {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[derive(Debug, Default)]
pub(crate) struct FakeGraph {
    changes: Vec<GraphChange>,
    next_id: u32,
}

impl FakeGraph {
    pub const OUTPUT: &'static str = "alsa_output.pci-0000_00_1f.3.analog-stereo";
    pub const INPUT: &'static str = "alsa_input.pci-0000_00_1f.3.analog-stereo";

    pub fn new() -> Self {
        Self {
            changes: Vec::new(),
            next_id: 100,
        }
    }

    /// PipeWire 1.5.85 + WirePlumber 0.5.12, one output, one input, Spotify playing to the output.
    pub fn standard() -> Self {
        let mut g = Self::new().core("1.5.85");
        g.changes.extend(Self::wireplumber_changes());
        g.metadata("default")
            .default_device("default.audio.sink", Self::OUTPUT)
            .default_device("default.audio.source", Self::INPUT)
            .device(Self::OUTPUT, "Built-in Audio Analog Stereo", "Audio/Sink")
            .device(Self::INPUT, "Built-in Audio Analog Stereo", "Audio/Source")
            .app_playing("spotify", "Spotify", Self::OUTPUT)
    }

    /// A session where WirePlumber has not started (yet).
    pub fn without_wireplumber() -> Self {
        Self::new().core("1.5.85").metadata("default").device(
            Self::OUTPUT,
            "Built-in Audio Analog Stereo",
            "Audio/Sink",
        )
    }

    /// The WirePlumber client plus the metadata objects it creates.
    pub fn wireplumber_changes() -> Vec<GraphChange> {
        vec![
            GraphChange::ClientAdded(ClientEntry {
                id: 33,
                props: props(&[
                    ("application.name", "WirePlumber"),
                    ("application.version", "0.5.12"),
                    ("core.version", "1.5.85"),
                ]),
            }),
            GraphChange::MetadataAdded("sm-settings".into()),
        ]
    }

    pub fn changes(&self) -> Vec<GraphChange> {
        self.changes.clone()
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn core(mut self, version: &str) -> Self {
        self.changes.push(GraphChange::CoreInfo(CoreInfo {
            version: version.into(),
        }));
        self
    }

    pub fn metadata(mut self, name: &str) -> Self {
        self.changes.push(GraphChange::MetadataAdded(name.into()));
        self
    }

    pub fn default_device(mut self, key: &str, node_name: &str) -> Self {
        self.changes.push(GraphChange::DefaultMetaProperty {
            subject: 0,
            key: key.into(),
            type_: Some("Spa:String:JSON".into()),
            value: Some(format!("{{\"name\":\"{node_name}\"}}")),
        });
        self
    }

    /// A hardware device with FL/FR ports (and monitor ports for sinks).
    pub fn device(mut self, node_name: &str, description: &str, media_class: &str) -> Self {
        let id = self.id();
        let p = props(&[
            ("node.name", node_name),
            ("node.description", description),
            ("media.class", media_class),
            ("audio.channels", "2"),
            ("object.serial", &(u64::from(id) + 1000).to_string()),
        ]);
        self.changes.push(GraphChange::NodeAdded {
            id,
            global_props: p.clone(),
        });
        self.changes.push(GraphChange::NodeInfo { id, props: p });
        let sink = media_class == "Audio/Sink";
        for ch in ["FL", "FR"] {
            let (dir, name) = if sink {
                (PortDir::In, "playback")
            } else {
                (PortDir::Out, "capture")
            };
            let pid = self.id();
            self.changes.push(GraphChange::PortAdded(PortEntry {
                id: pid,
                node_id: id,
                dir,
                name: format!("{name}_{ch}"),
                channel: ch.into(),
                monitor: false,
            }));
            if sink {
                let mid = self.id();
                self.changes.push(GraphChange::PortAdded(PortEntry {
                    id: mid,
                    node_id: id,
                    dir: PortDir::Out,
                    name: format!("monitor_{ch}"),
                    channel: ch.into(),
                    monitor: true,
                }));
            }
        }
        self
    }

    /// An app playback stream linked (by WirePlumber) to the device `to`.
    pub fn app_playing(self, binary: &str, name: &str, to: &str) -> Self {
        self.app(binary, name, Some(to), &[])
    }

    /// A playback stream not linked yet (not settled, per C7).
    pub fn app_unlinked(self, binary: &str, name: &str) -> Self {
        self.app(binary, name, None, &[])
    }

    /// A stream that refuses to be moved (`node.dont-move=true`).
    pub fn app_dont_move(self, binary: &str, name: &str, to: &str) -> Self {
        self.app(binary, name, Some(to), &[("node.dont-move", "true")])
    }

    fn app(mut self, binary: &str, name: &str, to: Option<&str>, extra: &[(&str, &str)]) -> Self {
        let id = self.id();
        let serial = (u64::from(id) + 1000).to_string();
        let mut pairs = vec![
            ("media.class", "Stream/Output/Audio"),
            ("application.process.binary", binary),
            ("application.name", name),
            ("node.name", name),
            ("object.serial", serial.as_str()),
        ];
        pairs.extend_from_slice(extra);
        let p = props(&pairs);
        self.changes.push(GraphChange::NodeAdded {
            id,
            global_props: p.clone(),
        });
        self.changes.push(GraphChange::NodeInfo { id, props: p });
        let target = to.and_then(|t| self.node_id_of(t));
        for ch in ["FL", "FR"] {
            let pid = self.id();
            self.changes.push(GraphChange::PortAdded(PortEntry {
                id: pid,
                node_id: id,
                dir: PortDir::Out,
                name: format!("output_{ch}"),
                channel: ch.into(),
                monitor: false,
            }));
            if let Some((tnode, tport)) =
                target.and_then(|t| self.input_port(t, ch).map(|p| (t, p)))
            {
                let lid = self.id();
                self.changes.push(GraphChange::LinkAdded(LinkEntry {
                    id: lid,
                    out_node: id,
                    out_port: pid,
                    in_node: tnode,
                    in_port: tport,
                    client_id: Some(33),
                }));
            }
        }
        self
    }

    fn node_id_of(&self, node_name: &str) -> Option<u32> {
        self.changes.iter().find_map(|c| match c {
            GraphChange::NodeAdded { id, global_props }
                if global_props.get("node.name").map(String::as_str) == Some(node_name) =>
            {
                Some(*id)
            }
            _ => None,
        })
    }

    fn input_port(&self, node: u32, ch: &str) -> Option<u32> {
        self.changes.iter().find_map(|c| match c {
            GraphChange::PortAdded(p)
                if p.node_id == node && p.dir == PortDir::In && p.channel == ch =>
            {
                Some(p.id)
            }
            _ => None,
        })
    }
}
