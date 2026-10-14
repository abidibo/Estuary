//! Connection state machine and capability evaluation (engine M2.7/M2.8, master C13).

use std::time::Instant;

use crate::error::UnsupportedReason;
use crate::graph::GraphMirror;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConnState {
    Connecting {
        attempt: u32,
    },
    Syncing,
    /// Capability checks done, write probe in flight.
    Probing,
    Ready,
    WaitingForWp,
    Unsupported(UnsupportedReason),
    PermissionDenied,
    Reconnecting {
        since: Instant,
        attempt: u32,
    },
}

impl ConnState {
    /// States in which the graph is mirrored and snapshots are produced.
    pub fn observing(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::WaitingForWp | Self::Unsupported(_) | Self::PermissionDenied
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CapabilityResult {
    Ok { pw: String, wp: Option<String> },
    WaitForWp,
    Unsupported(UnsupportedReason),
}

pub(crate) fn evaluate_capabilities(m: &GraphMirror) -> CapabilityResult {
    let pw = m
        .core
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |c| c.version.clone());
    if let Some((major, _)) = parse_major_minor(&pw)
        && major < 1
    {
        return CapabilityResult::Unsupported(UnsupportedReason::PipeWireTooOld { found: pw });
    }
    // M2.1: the WirePlumber version is its client's `application.version`.
    let wp_version = m
        .wireplumber_client()
        .and_then(|c| c.props.get("application.version").cloned());
    if m.metadata_names.contains("sm-settings") {
        // `sm-settings` was introduced in WirePlumber 0.5 (C13 gate).
        return CapabilityResult::Ok { pw, wp: wp_version };
    }
    if m.wireplumber_client().is_some() {
        return CapabilityResult::Unsupported(UnsupportedReason::WirePlumberTooOld {
            found: wp_version,
        });
    }
    CapabilityResult::WaitForWp
}

fn parse_major_minor(v: &str) -> Option<(u32, u32)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ClientEntry, CoreInfo, GraphChange};

    fn mirror(pw: &str, wp: Option<&str>, sm_settings: bool) -> GraphMirror {
        let mut m = GraphMirror::default();
        m.apply(&GraphChange::CoreInfo(CoreInfo { version: pw.into() }), "i");
        if let Some(v) = wp {
            let props = [
                ("application.name", "WirePlumber"),
                ("application.version", v),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
            m.apply(
                &GraphChange::ClientAdded(ClientEntry { id: 33, props }),
                "i",
            );
        }
        if sm_settings {
            m.apply(&GraphChange::MetadataAdded("sm-settings".into()), "i");
        }
        m
    }

    #[test]
    fn supported_setup() {
        assert_eq!(
            evaluate_capabilities(&mirror("1.5.85", Some("0.5.12"), true)),
            CapabilityResult::Ok {
                pw: "1.5.85".into(),
                wp: Some("0.5.12".into())
            }
        );
    }

    #[test]
    fn old_pipewire() {
        assert!(matches!(
            evaluate_capabilities(&mirror("0.3.65", Some("0.5.0"), true)),
            CapabilityResult::Unsupported(UnsupportedReason::PipeWireTooOld { .. })
        ));
    }

    #[test]
    fn old_wireplumber_has_no_sm_settings() {
        assert_eq!(
            evaluate_capabilities(&mirror("1.0.5", Some("0.4.17"), false)),
            CapabilityResult::Unsupported(UnsupportedReason::WirePlumberTooOld {
                found: Some("0.4.17".into())
            })
        );
    }

    #[test]
    fn no_wireplumber_waits() {
        assert_eq!(
            evaluate_capabilities(&mirror("1.5.85", None, false)),
            CapabilityResult::WaitForWp
        );
    }
}
