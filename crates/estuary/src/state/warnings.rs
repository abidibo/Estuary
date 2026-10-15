//! Engine warnings shown as badges (UI M10.2, the denoiser part of the M11.5 mapping).
//! Pure: badges clear after 60 s without a repeat; toasts show once per key.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use estuary_engine::{EngineWarning, WarnedNode};

use crate::i18n::{i18n_f, ni18n_f};

const BADGE_LIFETIME: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    /// Error (`dialog-error-symbolic`) rather than warning.
    pub error: bool,
    pub text: String,
    at: Instant,
}

#[derive(Debug, Default, Clone)]
pub struct Warnings {
    badges: HashMap<WarnedNode, Badge>,
    toasted: HashSet<String>,
}

impl Warnings {
    /// Records a warning; returns a toast to show, if any.
    pub fn on_warning(&mut self, w: &EngineWarning, now: Instant) -> Option<String> {
        match w {
            EngineWarning::Xruns {
                node: node @ WarnedNode::Denoiser(_),
                per_minute,
            } => {
                let text = ni18n_f(
                    "Audio processing is overloaded ({} dropout per minute)",
                    "Audio processing is overloaded ({} dropouts per minute)",
                    *per_minute,
                    &[&per_minute.to_string()],
                );
                self.badge(node.clone(), false, text, now);
                None
            }
            EngineWarning::NodeCreateFailed {
                node: node @ WarnedNode::Denoiser(_),
                message,
            } => {
                self.badge(node.clone(), true, message.clone(), now);
                None
            }
            EngineWarning::DenoiseRateUnsupported { input, rate } => {
                // Translators: {} is a sample rate in Hz, e.g. 50000
                let text = i18n_f(
                    "Noise suppression doesn't support {} Hz",
                    &[&rate.to_string()],
                );
                self.badge(WarnedNode::Denoiser(input.clone()), true, text.clone(), now);
                self.toasted
                    .insert(format!("warn:rate:{}:{rate}", input.0))
                    .then_some(text)
            }
            // The rest of the M11.5 mapping arrives with M11.
            _ => None,
        }
    }

    fn badge(&mut self, node: WarnedNode, error: bool, text: String, at: Instant) {
        self.badges.insert(node, Badge { error, text, at });
    }

    /// Drops badges older than 60 s; `true` if any went.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.badges.len();
        self.badges
            .retain(|_, b| now.duration_since(b.at) < BADGE_LIFETIME);
        self.badges.len() != before
    }

    pub fn get(&self, node: &WarnedNode) -> Option<&Badge> {
        self.badges.get(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::DeviceKey;

    fn mic() -> WarnedNode {
        WarnedNode::Denoiser(DeviceKey("mic".into()))
    }

    #[test]
    fn xruns_badge_clears_after_a_quiet_minute() {
        let mut w = Warnings::default();
        let t0 = Instant::now();
        let toast = w.on_warning(
            &EngineWarning::Xruns {
                node: mic(),
                per_minute: 12,
            },
            t0,
        );
        assert!(toast.is_none());
        let b = w.get(&mic()).unwrap();
        assert!(!b.error && b.text.contains("12 dropouts"));
        assert!(!w.expire(t0 + Duration::from_secs(59)));
        assert!(w.expire(t0 + Duration::from_secs(61)));
        assert!(w.get(&mic()).is_none());
    }

    #[test]
    fn unsupported_rate_badges_and_toasts_once() {
        let mut w = Warnings::default();
        let warn = EngineWarning::DenoiseRateUnsupported {
            input: DeviceKey("mic".into()),
            rate: 50_000,
        };
        let now = Instant::now();
        assert_eq!(
            w.on_warning(&warn, now).as_deref(),
            Some("Noise suppression doesn't support 50000 Hz")
        );
        assert!(w.on_warning(&warn, now).is_none(), "toasted once");
        assert!(w.get(&mic()).unwrap().error);
    }

    #[test]
    fn creation_failure_is_an_error_badge() {
        let mut w = Warnings::default();
        let warn = EngineWarning::NodeCreateFailed {
            node: mic(),
            message: "no memory".into(),
        };
        w.on_warning(&warn, Instant::now());
        let b = w.get(&mic()).unwrap();
        assert!(b.error && b.text == "no memory");
    }
}
