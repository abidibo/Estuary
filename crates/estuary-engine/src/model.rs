//! Model types (master C4): stable identities and the graph snapshot sent to the UI.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable application identity (C7), serialized as a plain string: `"bin:spotify"`, `"name:Firefox"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppKey(pub String);

/// Stable device identity: the device node's `node.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceKey(pub String);

/// Channel identity, serialized as a hyphenated UUID string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(pub Uuid);

/// Virtual microphone identity, serialized as a hyphenated UUID string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VirtualMicId(pub Uuid);

impl ChannelId {
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl VirtualMicId {
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Where an application's audio goes (decision R1). Serialized per C6: `{ kind = "channel", id = ".." }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteTarget {
    Default,
    Device { key: DeviceKey },
    Channel { id: ChannelId },
}

/// A matrix row (C4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatrixSource {
    Channel { id: ChannelId },
    Input { key: DeviceKey },
    Denoised { key: DeviceKey },
}

/// A matrix column (C4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatrixDest {
    Output { key: DeviceKey },
    VirtualMic { id: VirtualMicId },
}

/// A matrix crosspoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellKey {
    pub source: MatrixSource,
    pub dest: MatrixDest,
}

/// Linear per-channel volume as used by PipeWire `channelVolumes`, plus mute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub channels: Vec<f32>,
    pub muted: bool,
}

impl Volume {
    pub fn unity(channels: usize) -> Self {
        Self {
            channels: vec![1.0; channels.max(1)],
            muted: false,
        }
    }

    /// Mean of the channel volumes, for single-slider display.
    pub fn average(&self) -> f32 {
        if self.channels.is_empty() {
            return 0.0;
        }
        self.channels.iter().sum::<f32>() / self.channels.len() as f32
    }

    /// Same value on every channel, preserving the channel count.
    pub fn with_all(&self, v: f32) -> Self {
        Self {
            channels: vec![v; self.channels.len().max(1)],
            muted: self.muted,
        }
    }
}

/// `10^(db/20)`; only `NEG_INFINITY` maps to 0.0 (same mapping as C11 `GainHandle::set_db`).
pub fn db_to_linear(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else if !db.is_finite() {
        1.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// `20·log10(lin)`; `lin <= 0` maps to `NEG_INFINITY`.
pub fn linear_to_db(lin: f32) -> f32 {
    if lin <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * lin.log10()
    }
}

/// The full graph state the UI renders (C4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GraphSnapshot {
    pub apps: Vec<AppGroup>,
    pub outputs: Vec<DeviceInfo>,
    pub inputs: Vec<DeviceInfo>,
    pub channels: Vec<ChannelState>,
    pub virtual_mics: Vec<VirtualMicState>,
    pub denoisers: Vec<DenoiserState>,
    pub cells: Vec<CellState>,
    pub default_output: Option<DeviceKey>,
    pub default_input: Option<DeviceKey>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppGroup {
    pub key: AppKey,
    pub display_name: String,
    pub icon_name: Option<String>,
    pub streams: Vec<StreamInfo>,
    pub current_target: RouteTarget,
    pub movable: bool,
    pub volume: Volume,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub node_id: u32,
    pub serial: u64,
    pub media_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub key: DeviceKey,
    pub description: String,
    pub present: bool,
    pub channels: u32,
    pub volume: Option<Volume>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelState {
    pub id: ChannelId,
    pub name: String,
    pub present: bool,
    pub volume: Volume,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VirtualMicState {
    pub id: VirtualMicId,
    pub name: String,
    pub present: bool,
    pub volume: Volume,
    pub recording_apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DenoiserState {
    pub input: DeviceKey,
    pub enabled: bool,
    pub active: bool,
    pub xruns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CellState {
    pub key: CellKey,
    pub gain_db: f32,
    pub enabled: bool,
    pub status: CellStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CellStatus {
    Active,
    Pending,
    SourceMissing,
    DestinationMissing,
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_linear_round_trip() {
        let mut x = 0.001f32;
        while x <= 2.0 {
            let back = db_to_linear(linear_to_db(x));
            assert!((back - x).abs() < 1e-5 * x.max(1.0), "x={x} back={back}");
            x *= 1.37;
        }
    }

    #[test]
    fn db_edge_cases() {
        assert!((db_to_linear(-60.0) - 0.001).abs() < 1e-7);
        assert_eq!(db_to_linear(f32::NEG_INFINITY), 0.0);
        assert_eq!(linear_to_db(0.0), f32::NEG_INFINITY);
        assert_eq!(db_to_linear(0.0), 1.0);
    }

    #[test]
    fn volume_helpers() {
        let v = Volume {
            channels: vec![0.5, 1.0],
            muted: true,
        };
        assert!((v.average() - 0.75).abs() < 1e-6);
        let w = v.with_all(0.2);
        assert_eq!(w.channels, vec![0.2, 0.2]);
        assert!(w.muted);
        assert_eq!(Volume::unity(0).channels, vec![1.0]);
    }
}
