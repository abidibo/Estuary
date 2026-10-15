//! Pure rules for recovery surfaces (UI M11.2, M11.5): the start-at-login row, the banner and
//! setup-issue texts.

use std::time::Duration;

use estuary_engine::SetupIssue;

use crate::desktop::autostart::{AutostartBackend, AutostartOutcome, AutostartState};
use crate::i18n::{i18n, i18n_f};

/// How long reconnecting may take before the banner appears (F4).
pub const RECONNECT_BANNER_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartRowSpec {
    pub sensitive: bool,
    pub subtitle: Option<String>,
}

/// The switch shows the user's intent; the subtitle explains the effective state. The setting
/// is never rewritten (desktop rule).
pub fn autostart_row(
    intent: bool,
    last: Option<&Result<AutostartOutcome, String>>,
) -> AutostartRowSpec {
    let row = |sensitive, subtitle: Option<String>| AutostartRowSpec {
        sensitive,
        subtitle,
    };
    match last {
        None => row(true, None),
        Some(Err(e)) => row(
            true,
            Some(i18n_f("Couldn't update start at login: {}", &[e])),
        ),
        Some(Ok(o)) => match &o.state {
            AutostartState::Disabled { .. } if intent => row(
                true,
                Some(i18n("Your desktop didn't allow starting at login.")),
            ),
            AutostartState::Enabled { .. } | AutostartState::Disabled { .. } => row(true, None),
            AutostartState::Denied => row(
                true,
                Some(i18n(
                    "Background activity isn't allowed for estuary. You can allow it in Settings → Apps → estuary.",
                )),
            ),
            AutostartState::ManagedElsewhere(path) => row(
                true,
                Some(i18n_f(
                    "Another autostart entry exists at {}. estuary won't change it.",
                    &[&path.display().to_string()],
                )),
            ),
            AutostartState::Unavailable { .. } => row(
                false,
                Some(i18n(
                    "Not available on this desktop: it has no background portal. Estuary still keeps running while its window is closed.",
                )),
            ),
        },
    }
}

/// Native builds: the path of an enabled autostart file, for the row's tooltip.
pub fn autostart_tooltip(last: Option<&Result<AutostartOutcome, String>>) -> Option<String> {
    match last {
        Some(Ok(AutostartOutcome {
            state:
                AutostartState::Enabled {
                    backend: AutostartBackend::DesktopFile(p),
                },
            ..
        })) => Some(p.display().to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerSpec {
    pub title: String,
    /// "Retry" restarts the engine.
    pub retry: bool,
}

/// One banner at a time: read-only (F16) wins over a long reconnect (F4).
pub fn banner_state(read_only: bool, reconnecting_for: Option<Duration>) -> Option<BannerSpec> {
    if read_only {
        return Some(BannerSpec {
            title: i18n(
                "This configuration was saved by a newer version of Estuary. Changes won't be saved.",
            ),
            retry: false,
        });
    }
    match reconnecting_for {
        Some(d) if d > RECONNECT_BANNER_AFTER => Some(BannerSpec {
            title: i18n("Can't reach the audio system. Retrying…"),
            retry: true,
        }),
        _ => None,
    }
}

/// Localized text of a setup issue (`SetupIssue::message` is English, C3).
pub fn issue_text(i: &SetupIssue) -> String {
    match i {
        SetupIssue::DuplicateId(id) => i18n_f("Duplicate entry “{}” was ignored", &[id]),
        SetupIssue::DanglingReference { what, missing } => {
            i18n_f("{} refers to a missing item ({})", &[what, missing])
        }
        SetupIssue::DuplicateCell(c) => i18n_f("Duplicate send {} was ignored", &[c]),
        SetupIssue::InvalidGain { cell, value } => i18n_f(
            "Invalid gain {} for {} was corrected",
            &[&value.to_string(), cell],
        ),
        SetupIssue::MissingKnownDevice(k) => {
            i18n_f("Device {} was added to the known devices", &[&k.0])
        }
        SetupIssue::DenoisedWithoutDenoiser(k) => i18n_f(
            "Noise-suppressed source for {} has no noise suppressor",
            &[&k.0],
        ),
        SetupIssue::CellFailed { message, .. } => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::autostart::UnavailableReason;
    use crate::desktop::close::BackgroundPermission;
    use estuary_engine::DeviceKey;
    use std::path::PathBuf;

    fn ok(state: AutostartState) -> Result<AutostartOutcome, String> {
        Ok(AutostartOutcome {
            state,
            background: BackgroundPermission::NotApplicable,
        })
    }

    #[test]
    fn autostart_row_per_state() {
        assert_eq!(
            autostart_row(true, None),
            AutostartRowSpec {
                sensitive: true,
                subtitle: None
            }
        );
        let file =
            AutostartBackend::DesktopFile(PathBuf::from("/a/io.github.abidibo.Estuary.desktop"));
        let enabled = ok(AutostartState::Enabled {
            backend: file.clone(),
        });
        assert_eq!(autostart_row(true, Some(&enabled)).subtitle, None);
        assert_eq!(
            autostart_tooltip(Some(&enabled)).as_deref(),
            Some("/a/io.github.abidibo.Estuary.desktop")
        );
        let disabled = ok(AutostartState::Disabled {
            backend: AutostartBackend::Portal,
        });
        assert_eq!(
            autostart_row(false, Some(&disabled)).subtitle,
            None,
            "matches the intent"
        );
        assert!(
            autostart_row(true, Some(&disabled))
                .subtitle
                .unwrap()
                .contains("didn't allow")
        );
        assert!(
            autostart_row(true, Some(&ok(AutostartState::Denied)))
                .subtitle
                .unwrap()
                .contains("Settings → Apps")
        );
        let elsewhere = ok(AutostartState::ManagedElsewhere(PathBuf::from(
            "/x.desktop",
        )));
        assert!(
            autostart_row(true, Some(&elsewhere))
                .subtitle
                .unwrap()
                .contains("/x.desktop")
        );
        let unavailable = ok(AutostartState::Unavailable {
            reason: UnavailableReason::NoBackgroundPortal,
        });
        let r = autostart_row(true, Some(&unavailable));
        assert!(!r.sensitive && r.subtitle.unwrap().contains("no background portal"));
        let err: Result<AutostartOutcome, String> = Err("boom".into());
        let r = autostart_row(true, Some(&err));
        assert!(r.sensitive && r.subtitle.unwrap().ends_with("boom"));
    }

    #[test]
    fn banner_priority() {
        assert_eq!(banner_state(false, None), None);
        assert_eq!(banner_state(false, Some(Duration::from_secs(10))), None);
        let b = banner_state(false, Some(Duration::from_secs(31))).unwrap();
        assert!(b.retry);
        let b = banner_state(true, Some(Duration::from_secs(31))).unwrap();
        assert!(!b.retry, "read-only wins");
    }

    #[test]
    fn issue_texts() {
        let k = DeviceKey("mic".into());
        assert_eq!(
            issue_text(&SetupIssue::DuplicateId("x".into())),
            "Duplicate entry “x” was ignored"
        );
        assert!(issue_text(&SetupIssue::MissingKnownDevice(k.clone())).contains("mic"));
        assert!(
            issue_text(&SetupIssue::DenoisedWithoutDenoiser(k)).contains("no noise suppressor")
        );
        assert!(
            issue_text(&SetupIssue::InvalidGain {
                cell: "c".into(),
                value: 99.0
            })
            .contains("99")
        );
        assert!(
            issue_text(&SetupIssue::DanglingReference {
                what: "a".into(),
                missing: "b".into()
            })
            .contains("(b)")
        );
        assert!(issue_text(&SetupIssue::DuplicateCell("c".into())).contains("c"));
    }
}
