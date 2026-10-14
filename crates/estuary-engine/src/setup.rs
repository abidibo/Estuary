//! Persisted desired state (master C6). Types only at M2; validation and sanitizing arrive in M6.

use serde::{Deserialize, Serialize};

use crate::model::{
    AppKey, CellKey, ChannelId, DeviceKey, MatrixDest, MatrixSource, RouteTarget, VirtualMicId,
    Volume,
};

pub const SCHEMA_VERSION: u32 = 1;

/// The whole config file (C6). `setup` is flattened so the TOML matches the documented layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFile {
    pub schema_version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(flatten)]
    pub setup: Setup,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            settings: Settings::default(),
            setup: Setup::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub close_action: CloseAction,
    pub start_at_login: bool,
    pub background_notice_shown: bool,
    pub first_run_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_action: CloseAction::Hide,
            start_at_login: true,
            background_notice_shown: false,
            first_run_done: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    #[default]
    Hide,
    Quit,
}

/// Desired state consumed by the engine (`Command::SetSetup`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    pub channels: Vec<ChannelDef>,
    pub virtual_mics: Vec<VirtualMicDef>,
    pub denoisers: Vec<DenoiserDef>,
    pub cells: Vec<CellDef>,
    pub app_rules: Vec<AppRule>,
    pub known_devices: Vec<KnownDevice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelDef {
    pub id: ChannelId,
    pub name: String,
    #[serde(default = "stereo_unity")]
    pub volume: Volume,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualMicDef {
    pub id: VirtualMicId,
    pub name: String,
    #[serde(default = "stereo_unity")]
    pub volume: Volume,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DenoiserDef {
    pub input: DeviceKey,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub vad_gate: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellDef {
    pub source: MatrixSource,
    pub dest: MatrixDest,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl CellDef {
    pub fn key(&self) -> CellKey {
        CellKey {
            source: self.source.clone(),
            dest: self.dest.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRule {
    pub app: AppKey,
    pub target: RouteTarget,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Output,
    Input,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownDevice {
    pub key: DeviceKey,
    pub kind: DeviceKind,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

fn stereo_unity() -> Volume {
    Volume::unity(2)
}

fn yes() -> bool {
    true
}

/// Canonical CellKey string (C6): `"<source-kind>:<id-or-key>-><dest-kind>:<id-or-key>"`.
pub fn canonical_cell_key(k: &CellKey) -> String {
    let src = match &k.source {
        MatrixSource::Channel { id } => format!("channel:{}", id.0.hyphenated()),
        MatrixSource::Input { key } => format!("input:{}", key.0),
        MatrixSource::Denoised { key } => format!("denoised:{}", key.0),
    };
    let dst = match &k.dest {
        MatrixDest::Output { key } => format!("output:{}", key.0),
        MatrixDest::VirtualMic { id } => format!("virtual_mic:{}", id.0.hyphenated()),
    };
    format!("{src}->{dst}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema_version = 1

[settings]
close_action = "hide"
start_at_login = true
background_notice_shown = false
first_run_done = true

[[channels]]
id = "5f0c7c2e-1a2b-4c3d-9e8f-001122334455"
name = "Music"
volume = { channels = [1.0, 1.0], muted = false }
position = { x = 120.0, y = 80.0 }

[[virtual_mics]]
id = "8a1d7c2e-1a2b-4c3d-9e8f-001122334455"
name = "Virtual mic"

[[denoisers]]
input = "alsa_input.pci-0000_00_1f.3.analog-stereo"
enabled = true
vad_gate = 0.0

[[cells]]
source = { kind = "channel", id = "5f0c7c2e-1a2b-4c3d-9e8f-001122334455" }
dest   = { kind = "output", key = "bluez_output.88_0E_85_5C_98_1B.1" }
gain_db = -18.0
enabled = true

[[app_rules]]
app = "bin:spotify"
target = { kind = "channel", id = "5f0c7c2e-1a2b-4c3d-9e8f-001122334455" }
display_name = "Spotify"

[[known_devices]]
key = "bluez_output.88_0E_85_5C_98_1B.1"
kind = "output"
description = "WH-1000XM4"
"#;

    #[test]
    fn parses_documented_c6_layout() {
        let cfg: ConfigFile = toml::from_str(SAMPLE).expect("C6 sample parses");
        assert_eq!(cfg.schema_version, 1);
        assert!(cfg.settings.first_run_done);
        assert_eq!(cfg.setup.channels[0].name, "Music");
        assert_eq!(cfg.setup.virtual_mics[0].volume, Volume::unity(2));
        assert_eq!(cfg.setup.cells[0].gain_db, -18.0);
        assert!(matches!(
            cfg.setup.app_rules[0].target,
            RouteTarget::Channel { .. }
        ));
        assert_eq!(cfg.setup.known_devices[0].kind, DeviceKind::Output);
    }

    #[test]
    fn round_trips() {
        let cfg: ConfigFile = toml::from_str(SAMPLE).expect("parse");
        let text = toml::to_string(&cfg).expect("serialize");
        let back: ConfigFile = toml::from_str(&text).expect("reparse");
        assert_eq!(cfg, back);
    }

    #[test]
    fn default_route_target_shape() {
        let rule: AppRule =
            toml::from_str("app = \"name:Firefox\"\ntarget = { kind = \"default\" }")
                .expect("parse");
        assert_eq!(rule.target, RouteTarget::Default);
    }

    #[test]
    fn canonical_key_format() {
        let cfg: ConfigFile = toml::from_str(SAMPLE).expect("parse");
        assert_eq!(
            canonical_cell_key(&cfg.setup.cells[0].key()),
            "channel:5f0c7c2e-1a2b-4c3d-9e8f-001122334455->output:bluez_output.88_0E_85_5C_98_1B.1"
        );
    }
}
