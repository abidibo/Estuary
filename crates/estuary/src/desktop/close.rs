//! What the window's close button does (desktop M11.D3). Pure.

use estuary_engine::setup::{CloseAction, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundPermission {
    /// Native build: nothing kills hidden apps.
    NotApplicable,
    /// Sandboxed, the portal hasn't answered yet.
    Unknown,
    Allowed,
    /// The portal refused (or the request was cancelled): a hidden app would be killed.
    Denied,
    /// Sandboxed without a Background portal (COSMIC): hidden apps aren't killed.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseDecision {
    Hide,
    /// C10: the one-time notice, shown while the window is still visible, then hide.
    ShowBackgroundNoticeThenHide,
    Quit,
    /// Hiding would get the app killed; `show_notice` once per session.
    QuitBecauseBackgroundDenied {
        show_notice: bool,
    },
}

pub fn decide_close(
    settings: &Settings,
    bg: BackgroundPermission,
    denied_notice_shown_this_session: bool,
) -> CloseDecision {
    match (settings.close_action, bg) {
        (CloseAction::Quit, _) => CloseDecision::Quit,
        (CloseAction::Hide, BackgroundPermission::Denied) => {
            CloseDecision::QuitBecauseBackgroundDenied {
                show_notice: !denied_notice_shown_this_session,
            }
        }
        (CloseAction::Hide, _) if !settings.background_notice_shown => {
            CloseDecision::ShowBackgroundNoticeThenHide
        }
        (CloseAction::Hide, _) => CloseDecision::Hide,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERMISSIONS: [BackgroundPermission; 5] = [
        BackgroundPermission::NotApplicable,
        BackgroundPermission::Unknown,
        BackgroundPermission::Allowed,
        BackgroundPermission::Denied,
        BackgroundPermission::Unavailable,
    ];

    #[test]
    fn exhaustive_table() {
        for action in [CloseAction::Hide, CloseAction::Quit] {
            for bg in PERMISSIONS {
                for notice_shown in [false, true] {
                    for denied_shown in [false, true] {
                        let s = Settings {
                            close_action: action,
                            background_notice_shown: notice_shown,
                            ..Default::default()
                        };
                        let d = decide_close(&s, bg, denied_shown);
                        let expected = match (action, bg) {
                            (CloseAction::Quit, _) => CloseDecision::Quit,
                            (_, BackgroundPermission::Denied) => {
                                CloseDecision::QuitBecauseBackgroundDenied {
                                    show_notice: !denied_shown,
                                }
                            }
                            _ if !notice_shown => CloseDecision::ShowBackgroundNoticeThenHide,
                            _ => CloseDecision::Hide,
                        };
                        assert_eq!(
                            d, expected,
                            "{action:?} {bg:?} {notice_shown} {denied_shown}"
                        );
                        if d == CloseDecision::ShowBackgroundNoticeThenHide {
                            assert!(!notice_shown && bg != BackgroundPermission::Denied);
                        }
                    }
                }
            }
        }
    }
}
