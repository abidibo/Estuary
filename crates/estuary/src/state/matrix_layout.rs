//! Matrix layout (UI M9.1): rows, columns and crosspoints from the desired state and the live
//! snapshot. Pure: unit-tested without GTK.

use estuary_engine::{
    CellKey, CellStatus, ChannelId, DeviceKey, GraphSnapshot, MatrixDest, MatrixSource,
    MeterTarget, Setup, VirtualMicId, Volume,
};

use super::warnings::{Badge, Warnings};
use crate::i18n::i18n_f;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Channel(ChannelId),
    Input(DeviceKey),
    Denoised(DeviceKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColKey {
    Output(DeviceKey),
    VirtualMic(VirtualMicId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub key: RowKey,
    pub source: MatrixSource,
    pub label: String,
    pub present: bool,
    pub volume: Option<Volume>,
    pub meter: MeterTarget,
    pub denoise: Option<DenoiseView>,
    /// Inputs only: whether noise suppression is on (the menu check, UI M10.1).
    pub suppression: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColSpec {
    pub key: ColKey,
    pub dest: MatrixDest,
    pub label: String,
    pub present: bool,
    pub volume: Option<Volume>,
    pub meter: MeterTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CellView {
    pub key: CellKey,
    pub exists: bool,
    pub enabled: bool,
    pub gain_db: f32,
    /// `None` when there is no send.
    pub status: Option<CellStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DenoiseView {
    pub enabled: bool,
    /// The node is in the graph (off still passes the mic through: constant latency, C11).
    pub active: bool,
    pub vad_gate: f32,
    /// Overload, creation failure or unsupported rate (M11.5 mapping).
    pub badge: Option<Badge>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatrixLayout {
    pub rows: Vec<RowSpec>,
    pub cols: Vec<ColSpec>,
    /// `cells[row][col]`.
    pub cells: Vec<Vec<CellView>>,
}

impl MatrixLayout {
    /// Rows and columns by key: a change means the grid must be rebuilt.
    pub fn shape_key(&self) -> (Vec<RowKey>, Vec<ColKey>) {
        (
            self.rows.iter().map(|r| r.key.clone()).collect(),
            self.cols.iter().map(|c| c.key.clone()).collect(),
        )
    }

    pub fn has_owned_nodes(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.key, RowKey::Channel(_)))
            || self
                .cols
                .iter()
                .any(|c| matches!(c.key, ColKey::VirtualMic(_)))
    }
}

pub fn compute(setup: &Setup, snap: &GraphSnapshot, warnings: &Warnings) -> MatrixLayout {
    let mut rows = Vec::new();
    for c in &setup.channels {
        let live = snap.channels.iter().find(|s| s.id == c.id);
        rows.push(RowSpec {
            key: RowKey::Channel(c.id),
            source: MatrixSource::Channel { id: c.id },
            label: c.name.clone(),
            present: live.is_some_and(|s| s.present),
            volume: Some(live.map_or_else(|| c.volume.clone(), |s| s.volume.clone())),
            meter: MeterTarget::Channel(c.id),
            denoise: None,
            suppression: None,
        });
    }
    // The snapshot lists present and known-offline devices (C4).
    let mut inputs: Vec<_> = snap.inputs.iter().collect();
    inputs.sort_by_key(|d| d.description.to_lowercase());
    for d in inputs {
        rows.push(RowSpec {
            key: RowKey::Input(d.key.clone()),
            source: MatrixSource::Input { key: d.key.clone() },
            label: d.description.clone(),
            present: d.present,
            volume: d.volume.clone(),
            meter: MeterTarget::Device(d.key.clone()),
            denoise: None,
            suppression: Some(
                setup
                    .denoisers
                    .iter()
                    .any(|n| n.input == d.key && n.enabled),
            ),
        });
        if let Some(def) = setup.denoisers.iter().find(|n| n.input == d.key) {
            let live = snap.denoisers.iter().find(|n| n.input == d.key);
            rows.push(RowSpec {
                key: RowKey::Denoised(d.key.clone()),
                source: MatrixSource::Denoised { key: d.key.clone() },
                // Translators: {} is a microphone name
                label: i18n_f("{} (noise-suppressed)", &[&d.description]),
                present: d.present,
                volume: None,
                meter: MeterTarget::Denoised(d.key.clone()),
                denoise: Some(DenoiseView {
                    enabled: def.enabled,
                    active: live.is_some_and(|n| n.active),
                    vad_gate: def.vad_gate,
                    badge: warnings
                        .get(&estuary_engine::WarnedNode::Denoiser(d.key.clone()))
                        .cloned(),
                }),
                suppression: None,
            });
        }
    }

    let mut cols = Vec::new();
    let mut outputs: Vec<_> = snap.outputs.iter().collect();
    outputs.sort_by_key(|d| d.description.to_lowercase());
    for d in outputs {
        cols.push(ColSpec {
            key: ColKey::Output(d.key.clone()),
            dest: MatrixDest::Output { key: d.key.clone() },
            label: d.description.clone(),
            present: d.present,
            volume: d.volume.clone(),
            meter: MeterTarget::Device(d.key.clone()),
        });
    }
    for m in &setup.virtual_mics {
        let live = snap.virtual_mics.iter().find(|s| s.id == m.id);
        cols.push(ColSpec {
            key: ColKey::VirtualMic(m.id),
            dest: MatrixDest::VirtualMic { id: m.id },
            label: m.name.clone(),
            present: live.is_some_and(|s| s.present),
            volume: Some(live.map_or_else(|| m.volume.clone(), |s| s.volume.clone())),
            meter: MeterTarget::VirtualMic(m.id),
        });
    }

    let cells = rows
        .iter()
        .map(|r| {
            cols.iter()
                .map(|c| {
                    let key = CellKey {
                        source: r.source.clone(),
                        dest: c.dest.clone(),
                    };
                    let def = setup.cells.iter().find(|d| d.key() == key);
                    let status = def.map(|_| {
                        snap.cells
                            .iter()
                            .find(|s| s.key == key)
                            .map_or(CellStatus::Pending, |s| s.status.clone())
                    });
                    CellView {
                        exists: def.is_some(),
                        enabled: def.is_some_and(|d| d.enabled),
                        gain_db: def.map_or(0.0, |d| d.gain_db),
                        status,
                        key,
                    }
                })
                .collect()
        })
        .collect();
    MatrixLayout { rows, cols, cells }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::setup::{CellDef, ChannelDef, DenoiserDef, VirtualMicDef};
    use estuary_engine::{CellState, ChannelState, DeviceInfo};

    fn device(key: &str, description: &str, present: bool) -> DeviceInfo {
        DeviceInfo {
            key: DeviceKey(key.into()),
            description: description.into(),
            present,
            channels: 2,
            volume: Some(Volume::unity(2)),
        }
    }

    struct Fixture {
        setup: Setup,
        snap: GraphSnapshot,
        music: ChannelId,
        mic: VirtualMicId,
    }

    fn fixture() -> Fixture {
        let music = ChannelId::new_random();
        let chat = ChannelId::new_random();
        let mic = VirtualMicId::new_random();
        let ch = |id, name: &str| ChannelDef {
            id,
            name: name.into(),
            volume: Volume::unity(2),
            position: None,
        };
        let setup = Setup {
            channels: vec![ch(music, "Music"), ch(chat, "Chat")],
            virtual_mics: vec![VirtualMicDef {
                id: mic,
                name: "Virtual mic".into(),
                volume: Volume::unity(2),
                position: None,
            }],
            denoisers: vec![DenoiserDef {
                input: DeviceKey("usb".into()),
                enabled: true,
                vad_gate: 0.5,
            }],
            cells: vec![
                CellDef {
                    source: MatrixSource::Channel { id: music },
                    dest: MatrixDest::Output {
                        key: DeviceKey("hp".into()),
                    },
                    gain_db: -6.0,
                    enabled: true,
                },
                CellDef {
                    source: MatrixSource::Channel { id: music },
                    dest: MatrixDest::VirtualMic { id: mic },
                    gain_db: -18.0,
                    enabled: false,
                },
            ],
            ..Default::default()
        };
        let snap = GraphSnapshot {
            outputs: vec![
                device("speakers", "Speakers", true),
                device("hp", "Headphones", true),
            ],
            inputs: vec![
                device("usb", "USB mic", false),
                device("laptop", "Laptop mic", true),
            ],
            channels: vec![ChannelState {
                id: music,
                name: "Music".into(),
                present: true,
                volume: Volume {
                    channels: vec![0.5, 0.5],
                    muted: false,
                },
            }],
            cells: vec![CellState {
                key: setup.cells[0].key(),
                gain_db: -6.0,
                enabled: true,
                status: CellStatus::Active,
            }],
            ..Default::default()
        };
        Fixture {
            setup,
            snap,
            music,
            mic,
        }
    }

    #[test]
    fn rows_channels_then_sorted_inputs_with_denoised_below() {
        let f = fixture();
        let l = compute(&f.setup, &f.snap, &Warnings::default());
        let labels: Vec<&str> = l.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Music",
                "Chat",
                "Laptop mic",
                "USB mic",
                "USB mic (noise-suppressed)"
            ]
        );
        assert_eq!(l.rows[4].key, RowKey::Denoised(DeviceKey("usb".into())));
        assert!(
            l.rows[4]
                .denoise
                .as_ref()
                .is_some_and(|d| d.enabled && !d.active)
        );
    }

    #[test]
    fn columns_outputs_by_description_then_mics() {
        let f = fixture();
        let l = compute(&f.setup, &f.snap, &Warnings::default());
        let labels: Vec<&str> = l.cols.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Headphones", "Speakers", "Virtual mic"]);
        assert_eq!(l.cols[2].key, ColKey::VirtualMic(f.mic));
    }

    #[test]
    fn offline_devices_stay_listed() {
        let f = fixture();
        let l = compute(&f.setup, &f.snap, &Warnings::default());
        let usb = l.rows.iter().find(|r| r.label == "USB mic").unwrap();
        assert!(
            !usb.present,
            "known but unplugged: shown dimmed, can be forgotten"
        );
    }

    #[test]
    fn cells_carry_setup_and_status() {
        let f = fixture();
        let l = compute(&f.setup, &f.snap, &Warnings::default());
        assert_eq!(l.cells.len(), l.rows.len());
        assert!(l.cells.iter().all(|r| r.len() == l.cols.len()));
        let to_hp = &l.cells[0][0];
        assert!(to_hp.exists && to_hp.enabled && to_hp.gain_db == -6.0);
        assert_eq!(to_hp.status, Some(CellStatus::Active));
        let to_mic = &l.cells[0][2];
        assert!(to_mic.exists && !to_mic.enabled);
        assert_eq!(
            to_mic.status,
            Some(CellStatus::Pending),
            "not in the snapshot yet"
        );
        let none = &l.cells[1][0];
        assert!(!none.exists && none.status.is_none());
    }

    #[test]
    fn live_channel_volume_and_presence() {
        let f = fixture();
        let l = compute(&f.setup, &f.snap, &Warnings::default());
        assert!(l.rows[0].present);
        assert_eq!(l.rows[0].volume.as_ref().unwrap().channels, vec![0.5, 0.5]);
        assert!(!l.rows[1].present, "Chat isn't in the graph yet");
        assert_eq!(l.rows[0].meter, MeterTarget::Channel(f.music));
    }

    #[test]
    fn shape_changes_only_with_rows_or_columns() {
        let f = fixture();
        let a = compute(&f.setup, &f.snap, &Warnings::default());
        let mut setup = f.setup.clone();
        setup.cells[0].gain_db = 0.0;
        assert_eq!(
            compute(&setup, &f.snap, &Warnings::default()).shape_key(),
            a.shape_key()
        );
        setup.channels.pop();
        assert_ne!(
            compute(&setup, &f.snap, &Warnings::default()).shape_key(),
            a.shape_key()
        );
        assert!(a.has_owned_nodes());
        assert!(!compute(&Setup::default(), &f.snap, &Warnings::default()).has_owned_nodes());
    }
}
