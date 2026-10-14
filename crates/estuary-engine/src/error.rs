//! Auxiliary public types (master C3, amended).

use std::time::Duration;

use crate::meter::MeterTarget;
use crate::model::{CellKey, ChannelId, DeviceKey, VirtualMicId};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("engine thread is not running")]
    ThreadGone,
    #[error("engine thread panicked: {0}")]
    Panicked(String),
    #[error("shutdown timed out")]
    ShutdownTimeout,
    #[error("failed to spawn engine thread: {0}")]
    Spawn(String),
    #[error("pipewire error: {0}")]
    PipeWire(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnsupportedReason {
    PipeWireTooOld { found: String },
    NoWirePlumber,
    WirePlumberTooOld { found: Option<String> },
}

/// Identity of the node a warning is about, so the UI can badge the right row/cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WarnedNode {
    Cell(CellKey),
    Denoiser(DeviceKey),
    Channel(ChannelId),
    VirtualMic(VirtualMicId),
    Device(DeviceKey),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineWarning {
    Xruns {
        node: WarnedNode,
        per_minute: u32,
    },
    LinkFailed {
        cell: Option<CellKey>,
        description: String,
        message: String,
    },
    NodeCreateFailed {
        node: WarnedNode,
        message: String,
    },
    MeterFailed(MeterTarget),
    VolumeWriteFailed {
        node: WarnedNode,
    },
    DenoiseRateUnsupported {
        input: DeviceKey,
        rate: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetupIssue {
    DuplicateId(String),
    DanglingReference {
        what: String,
        missing: String,
    },
    DuplicateCell(String),
    InvalidGain {
        cell: String,
        value: f32,
    },
    MissingKnownDevice(DeviceKey),
    DenoisedWithoutDenoiser(DeviceKey),
    /// Runtime: 3 consecutive creation failures (C8).
    CellFailed {
        cell: CellKey,
        message: String,
    },
}

impl SetupIssue {
    /// Stable de-duplication key (UI toasts).
    pub fn key(&self) -> String {
        match self {
            Self::DuplicateId(id) => format!("duplicate_id:{id}"),
            Self::DanglingReference { what, missing } => format!("dangling:{what}:{missing}"),
            Self::DuplicateCell(c) => format!("duplicate_cell:{c}"),
            Self::InvalidGain { cell, .. } => format!("invalid_gain:{cell}"),
            Self::MissingKnownDevice(k) => format!("missing_known_device:{}", k.0),
            Self::DenoisedWithoutDenoiser(k) => format!("denoised_without_denoiser:{}", k.0),
            Self::CellFailed { cell, .. } => {
                format!("cell_failed:{}", crate::setup::canonical_cell_key(cell))
            }
        }
    }

    /// `Some` only for `CellFailed`.
    pub fn cell(&self) -> Option<&CellKey> {
        match self {
            Self::CellFailed { cell, .. } => Some(cell),
            _ => None,
        }
    }

    /// English, single sentence, no trailing period. The UI wraps known variants in gettext.
    pub fn message(&self) -> String {
        match self {
            Self::DuplicateId(id) => format!("Duplicate id {id}"),
            Self::DanglingReference { what, missing } => {
                format!("{what} refers to missing {missing}")
            }
            Self::DuplicateCell(c) => format!("Duplicate send {c}"),
            Self::InvalidGain { cell, value } => format!("Invalid gain {value} on send {cell}"),
            Self::MissingKnownDevice(k) => format!("Device {} is not in known devices", k.0),
            Self::DenoisedWithoutDenoiser(k) => {
                format!(
                    "Noise-suppressed source {} has no noise suppression entry",
                    k.0
                )
            }
            Self::CellFailed { cell, message } => format!(
                "Send {} failed: {message}",
                crate::setup::canonical_cell_key(cell)
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VirtualDeviceUsage {
    pub app_display_name: String,
    pub device_name: String,
    pub kind: UsageKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKind {
    PlayingToChannel,
    RecordingFromVirtualMic,
}

/// Reconnect backoff (C3): `min(initial · factor^attempt, max)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    pub initial: Duration,
    pub factor: u32,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            factor: 2,
            max: Duration::from_secs(5),
        }
    }
}

impl Backoff {
    pub fn delay(&self, attempt: u32) -> Duration {
        let mult = self.factor.checked_pow(attempt).unwrap_or(u32::MAX);
        self.initial
            .checked_mul(mult)
            .unwrap_or(self.max)
            .min(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_issue_keys_are_stable_and_distinct() {
        use crate::model::{DeviceKey, MatrixDest, MatrixSource};
        let cell = CellKey {
            source: MatrixSource::Input {
                key: DeviceKey("mic".into()),
            },
            dest: MatrixDest::Output {
                key: DeviceKey("spk".into()),
            },
        };
        let issues = [
            SetupIssue::DuplicateId("a".into()),
            SetupIssue::DanglingReference {
                what: "cell".into(),
                missing: "channel:x".into(),
            },
            SetupIssue::DuplicateCell("c".into()),
            SetupIssue::InvalidGain {
                cell: "c".into(),
                value: f32::NAN,
            },
            SetupIssue::MissingKnownDevice(DeviceKey("k".into())),
            SetupIssue::DenoisedWithoutDenoiser(DeviceKey("k".into())),
            SetupIssue::CellFailed {
                cell: cell.clone(),
                message: "no ports".into(),
            },
        ];
        let keys: std::collections::HashSet<String> = issues.iter().map(SetupIssue::key).collect();
        assert_eq!(keys.len(), issues.len(), "one distinct key per variant");
        assert_eq!(issues[6].key(), "cell_failed:input:mic->output:spk");
        assert_eq!(issues[6].key(), issues[6].clone().key(), "stable");
        assert_eq!(issues[6].cell(), Some(&cell));
        assert!(issues[0].cell().is_none());
        assert!(!issues[6].message().ends_with('.'));
    }

    #[test]
    fn backoff_sequence() {
        let b = Backoff::default();
        let got: Vec<Duration> = (0..6).map(|a| b.delay(a)).collect();
        let ms = Duration::from_millis;
        assert_eq!(
            got,
            vec![ms(500), ms(1000), ms(2000), ms(4000), ms(5000), ms(5000)]
        );
        assert_eq!(b.delay(100), Duration::from_secs(5));
    }
}
