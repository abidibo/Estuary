//! Persisted desired state (master C6): types, validation, sanitizing and stable hashes.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::SetupIssue;

use crate::model::{
    AppKey, CellKey, ChannelId, DeviceKey, MatrixDest, MatrixSource, RouteTarget, VirtualMicId,
    Volume,
};

pub const SCHEMA_VERSION: u32 = 1;
pub const GAIN_MIN_DB: f32 = -60.0;
pub const GAIN_MAX_DB: f32 = 6.0;

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

/// First 12 hex chars of SHA-256 (C5 node names for cells and denoisers).
fn short_hash(data: &str) -> String {
    Sha256::digest(data.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `estuary.cell.<hash>` suffix (C5).
pub fn cell_hash(k: &CellKey) -> String {
    short_hash(&canonical_cell_key(k))
}

/// `estuary.denoise.<hash>` suffix (C5).
pub fn input_hash(k: &DeviceKey) -> String {
    short_hash(&k.0)
}

/// All C6 invariant violations; never panics. Equals the issues `sanitize` reports.
pub fn validate(s: &Setup) -> Vec<SetupIssue> {
    sanitize(s.clone()).1
}

/// Repairs a setup (C12 partial recovery): drops dangling references, duplicates and non-finite
/// gains, clamps finite gains into range, and adds referenced devices missing from
/// `known_devices`. Every change is reported.
pub fn sanitize(mut s: Setup) -> (Setup, Vec<SetupIssue>) {
    let mut issues = Vec::new();

    let mut ids = HashSet::new();
    s.channels
        .retain(|c| keep_unique(&mut ids, c.id.0.to_string(), &mut issues));
    s.virtual_mics
        .retain(|m| keep_unique(&mut ids, m.id.0.to_string(), &mut issues));
    let mut inputs = HashSet::new();
    s.denoisers
        .retain(|d| keep_unique(&mut inputs, format!("denoiser:{}", d.input.0), &mut issues));
    for d in &mut s.denoisers {
        d.vad_gate = if d.vad_gate.is_finite() {
            d.vad_gate.clamp(0.0, 0.95)
        } else {
            0.0
        };
    }

    let channels: HashSet<ChannelId> = s.channels.iter().map(|c| c.id).collect();
    let mics: HashSet<VirtualMicId> = s.virtual_mics.iter().map(|m| m.id).collect();
    let denoised: HashSet<DeviceKey> = s.denoisers.iter().map(|d| d.input.clone()).collect();

    let mut seen_cells = HashSet::new();
    s.cells.retain(|c| {
        let key = c.key();
        let name = canonical_cell_key(&key);
        let missing = match (&c.source, &c.dest) {
            (MatrixSource::Channel { id }, _) if !channels.contains(id) => {
                Some(format!("channel:{}", id.0))
            }
            (MatrixSource::Denoised { key }, _) if !denoised.contains(key) => {
                issues.push(SetupIssue::DenoisedWithoutDenoiser(key.clone()));
                return false;
            }
            (_, MatrixDest::VirtualMic { id }) if !mics.contains(id) => {
                Some(format!("virtual_mic:{}", id.0))
            }
            _ => None,
        };
        if let Some(missing) = missing {
            issues.push(SetupIssue::DanglingReference {
                what: format!("cell {name}"),
                missing,
            });
            return false;
        }
        if !c.gain_db.is_finite() {
            issues.push(SetupIssue::InvalidGain {
                cell: name,
                value: c.gain_db,
            });
            return false;
        }
        if !seen_cells.insert(key) {
            issues.push(SetupIssue::DuplicateCell(name));
            return false;
        }
        true
    });
    for c in &mut s.cells {
        c.gain_db = c.gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }

    s.app_rules.retain(|r| match &r.target {
        RouteTarget::Channel { id } if !channels.contains(id) => {
            issues.push(SetupIssue::DanglingReference {
                what: format!("rule {}", r.app.0),
                missing: format!("channel:{}", id.0),
            });
            false
        }
        _ => true,
    });
    let mut apps = HashSet::new();
    s.app_rules
        .retain(|r| keep_unique(&mut apps, format!("rule:{}", r.app.0), &mut issues));

    // C6: every device referenced anywhere appears in known_devices.
    let mut referenced: Vec<(DeviceKey, DeviceKind)> = Vec::new();
    for c in &s.cells {
        match &c.source {
            MatrixSource::Input { key } | MatrixSource::Denoised { key } => {
                referenced.push((key.clone(), DeviceKind::Input));
            }
            MatrixSource::Channel { .. } => {}
        }
        if let MatrixDest::Output { key } = &c.dest {
            referenced.push((key.clone(), DeviceKind::Output));
        }
    }
    referenced.extend(
        s.denoisers
            .iter()
            .map(|d| (d.input.clone(), DeviceKind::Input)),
    );
    referenced.extend(s.app_rules.iter().filter_map(|r| match &r.target {
        RouteTarget::Device { key } => Some((key.clone(), DeviceKind::Output)),
        _ => None,
    }));
    let mut known_keys = HashSet::new();
    s.known_devices.retain(|d| known_keys.insert(d.key.clone()));
    for (key, kind) in referenced {
        if known_keys.insert(key.clone()) {
            issues.push(SetupIssue::MissingKnownDevice(key.clone()));
            s.known_devices.push(KnownDevice {
                key,
                kind,
                description: String::new(),
                position: None,
            });
        }
    }
    (s, issues)
}

fn keep_unique(seen: &mut HashSet<String>, id: String, issues: &mut Vec<SetupIssue>) -> bool {
    if seen.insert(id.clone()) {
        true
    } else {
        issues.push(SetupIssue::DuplicateId(id));
        false
    }
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

[[known_devices]]
key = "alsa_input.pci-0000_00_1f.3.analog-stereo"
kind = "input"
description = "Built-in Microphone"
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
        assert_eq!(cfg.setup.known_devices[1].kind, DeviceKind::Input);
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
    fn known_answer_hashes() {
        let cfg: ConfigFile = toml::from_str(SAMPLE).expect("parse");
        let key = cfg.setup.cells[0].key();
        let h = cell_hash(&key);
        assert_eq!(h.len(), 12);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, cell_hash(&key.clone()), "stable");
        // Known answers: SHA-256 prefixes of the canonical strings.
        assert_eq!(input_hash(&DeviceKey("abc".into())), "ba7816bf8f01");
        assert_eq!(short_hash(""), "e3b0c44298fc");
    }

    #[test]
    fn valid_sample_has_no_issues() {
        let cfg: ConfigFile = toml::from_str(SAMPLE).expect("parse");
        assert_eq!(validate(&cfg.setup), vec![]);
        let (s, issues) = sanitize(cfg.setup.clone());
        assert!(issues.is_empty());
        assert_eq!(s, cfg.setup, "a valid setup is untouched");
    }

    #[test]
    fn sanitize_repairs_and_reports() {
        let mut cfg: ConfigFile = toml::from_str(SAMPLE).expect("parse");
        let ch = cfg.setup.channels[0].clone();
        let cell = cfg.setup.cells[0].clone();
        cfg.setup.channels.push(ch.clone()); // duplicate id
        cfg.setup.cells.push(cell.clone()); // duplicate cell
        let mut loud = cell.clone();
        loud.dest = MatrixDest::Output {
            key: DeviceKey("other".into()),
        };
        loud.gain_db = 40.0; // clamped
        cfg.setup.cells.push(loud);
        let mut nan = cell.clone();
        nan.dest = MatrixDest::Output {
            key: DeviceKey("nan".into()),
        };
        nan.gain_db = f32::NAN; // dropped
        cfg.setup.cells.push(nan);
        let mut dangling = cell.clone();
        dangling.source = MatrixSource::Channel {
            id: ChannelId(uuid::Uuid::nil()),
        };
        cfg.setup.cells.push(dangling); // dropped
        cfg.setup.cells.push(CellDef {
            source: MatrixSource::Denoised {
                key: DeviceKey("mic2".into()),
            },
            dest: cell.dest.clone(),
            gain_db: 0.0,
            enabled: true,
        }); // no denoiser for mic2 → dropped
        cfg.setup.app_rules.push(AppRule {
            app: AppKey("bin:x".into()),
            target: RouteTarget::Channel {
                id: ChannelId(uuid::Uuid::nil()),
            },
            display_name: String::new(),
        }); // dangling rule → dropped

        let (s, issues) = sanitize(cfg.setup);
        assert_eq!(s.channels.len(), 1);
        assert_eq!(s.cells.len(), 2, "original + clamped");
        assert_eq!(s.cells[1].gain_db, GAIN_MAX_DB);
        assert_eq!(s.app_rules.len(), 1);
        assert!(
            s.known_devices.iter().any(|d| d.key.0 == "other"),
            "referenced device remembered"
        );
        let kinds: Vec<String> = issues
            .iter()
            .map(|i| i.key().split(':').next().unwrap_or("").to_owned())
            .collect();
        for k in [
            "duplicate_id",
            "duplicate_cell",
            "invalid_gain",
            "dangling",
            "denoised_without_denoiser",
            "missing_known_device",
        ] {
            assert!(kinds.iter().any(|x| x == k), "missing issue {k}: {kinds:?}");
        }
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
