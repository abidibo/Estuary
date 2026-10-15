//! First-run presets (UI M11.1, C10). Pure.

use estuary_engine::setup::{
    CellDef, ChannelDef, DenoiserDef, DeviceKind, KnownDevice, Setup, VirtualMicDef,
};
use estuary_engine::{
    ChannelId, DeviceKey, GraphSnapshot, MatrixDest, MatrixSource, VirtualMicId, Volume,
};

use crate::i18n::i18n;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetKind {
    /// Music, Games, Chat to the default output; a noise-suppressed default mic to a virtual mic.
    Recommended,
    Empty,
}

pub fn build_preset(snap: &GraphSnapshot, kind: PresetKind) -> Setup {
    if kind == PresetKind::Empty {
        return Setup::default();
    }
    let mut s = Setup::default();
    let describe = |key: &DeviceKey| {
        snap.outputs
            .iter()
            .chain(&snap.inputs)
            .find(|d| &d.key == key)
            .map(|d| d.description.clone())
            .unwrap_or_default()
    };
    // Localized when created (C10): the names are the user's from then on.
    for name in [i18n("Music"), i18n("Games"), i18n("Chat")] {
        let id = ChannelId::new_random();
        s.channels.push(ChannelDef {
            id,
            name,
            volume: Volume::unity(2),
            position: None,
        });
        if let Some(out) = &snap.default_output {
            s.cells.push(CellDef {
                source: MatrixSource::Channel { id },
                dest: MatrixDest::Output { key: out.clone() },
                gain_db: 0.0,
                enabled: true,
            });
        }
    }
    let vmic = VirtualMicId::new_random();
    s.virtual_mics.push(VirtualMicDef {
        id: vmic,
        name: i18n("Virtual mic"),
        volume: Volume::unity(2),
        position: None,
    });
    if let Some(out) = &snap.default_output {
        s.known_devices.push(KnownDevice {
            key: out.clone(),
            kind: DeviceKind::Output,
            description: describe(out),
            position: None,
        });
    }
    if let Some(input) = &snap.default_input {
        s.denoisers.push(DenoiserDef {
            input: input.clone(),
            enabled: true,
            vad_gate: 0.0,
        });
        s.cells.push(CellDef {
            source: MatrixSource::Denoised { key: input.clone() },
            dest: MatrixDest::VirtualMic { id: vmic },
            gain_db: 0.0,
            enabled: true,
        });
        s.known_devices.push(KnownDevice {
            key: input.clone(),
            kind: DeviceKind::Input,
            description: describe(input),
            position: None,
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::DeviceInfo;

    fn device(key: &str, description: &str) -> DeviceInfo {
        DeviceInfo {
            key: DeviceKey(key.into()),
            description: description.into(),
            present: true,
            channels: 2,
            volume: None,
        }
    }

    #[test]
    fn recommended_with_default_devices() {
        let snap = GraphSnapshot {
            outputs: vec![device("hp", "Headphones")],
            inputs: vec![device("mic", "Laptop mic")],
            default_output: Some(DeviceKey("hp".into())),
            default_input: Some(DeviceKey("mic".into())),
            ..Default::default()
        };
        let s = build_preset(&snap, PresetKind::Recommended);
        let names: Vec<&str> = s.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Music", "Games", "Chat"]);
        assert_eq!(s.virtual_mics.len(), 1);
        assert_eq!(
            s.cells.len(),
            4,
            "3 channels to the output + the clean mic to the vmic"
        );
        assert!(s.cells.iter().all(|c| c.enabled && c.gain_db == 0.0));
        assert_eq!(s.denoisers.len(), 1);
        assert!(s.denoisers[0].enabled && s.denoisers[0].vad_gate == 0.0);
        assert_eq!(s.known_devices.len(), 2);
        assert_eq!(s.known_devices[1].description, "Laptop mic");
        assert!(s.app_rules.is_empty());
        assert!(
            estuary_engine::setup::validate(&s).is_empty(),
            "a valid C6 setup"
        );
    }

    #[test]
    fn recommended_without_devices_still_makes_channels() {
        let s = build_preset(&GraphSnapshot::default(), PresetKind::Recommended);
        assert_eq!(s.channels.len(), 3);
        assert_eq!(s.virtual_mics.len(), 1);
        assert!(s.cells.is_empty() && s.denoisers.is_empty() && s.known_devices.is_empty());
        assert!(estuary_engine::setup::validate(&s).is_empty());
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(
            build_preset(&GraphSnapshot::default(), PresetKind::Empty),
            Setup::default()
        );
    }
}
