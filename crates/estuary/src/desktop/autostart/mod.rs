//! Start at login (desktop M11.D11, C10): the Background portal when sandboxed, an XDG
//! autostart file otherwise. Idempotent; run at startup and on every Preferences toggle.

pub mod file;
pub mod portal;

use std::path::PathBuf;

use self::file::FileOwnership;
use self::portal::PortalResult;
use super::close::BackgroundPermission;
use super::env::DesktopEnv;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutostartBackend {
    Portal,
    DesktopFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    NoBackgroundPortal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutostartState {
    Enabled {
        backend: AutostartBackend,
    },
    Disabled {
        backend: AutostartBackend,
    },
    /// The user refused in the portal dialog.
    Denied,
    /// Native: a foreign autostart file exists; never touched.
    ManagedElsewhere(PathBuf),
    Unavailable {
        reason: UnavailableReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutostartOutcome {
    pub state: AutostartState,
    pub background: BackgroundPermission,
}

#[derive(thiserror::Error, Debug)]
pub enum AutostartError {
    #[error("portal error: {0}")]
    Portal(#[from] ashpd::Error),
    #[error("autostart file: {0}")]
    File(#[from] std::io::Error),
}

/// `reason` is the localized portal text (gettext runs on the UI thread).
pub async fn apply_autostart(
    desired: bool,
    env: DesktopEnv,
    reason: String,
) -> Result<AutostartOutcome, AutostartError> {
    if !env.sandboxed {
        let Some(dir) = env.autostart_dir else {
            return Err(AutostartError::File(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config directory",
            )));
        };
        return Ok(AutostartOutcome {
            state: apply_file(desired, &dir.join(file::FILE_NAME))?,
            background: BackgroundPermission::NotApplicable,
        });
    }
    Ok(match portal::request(desired, reason).await? {
        PortalResult::Granted {
            run_in_background,
            auto_start,
        } => AutostartOutcome {
            state: if auto_start {
                AutostartState::Enabled {
                    backend: AutostartBackend::Portal,
                }
            } else {
                AutostartState::Disabled {
                    backend: AutostartBackend::Portal,
                }
            },
            background: if run_in_background {
                BackgroundPermission::Allowed
            } else {
                BackgroundPermission::Denied
            },
        },
        PortalResult::Cancelled => AutostartOutcome {
            state: AutostartState::Denied,
            background: BackgroundPermission::Denied,
        },
        PortalResult::Unavailable => AutostartOutcome {
            state: AutostartState::Unavailable {
                reason: UnavailableReason::NoBackgroundPortal,
            },
            background: BackgroundPermission::Unavailable,
        },
    })
}

/// The precedence table of M11.D9.
fn apply_file(desired: bool, path: &std::path::Path) -> std::io::Result<AutostartState> {
    let backend = AutostartBackend::DesktopFile(path.to_owned());
    Ok(match (desired, file::inspect(path)?) {
        (_, FileOwnership::Foreign) => AutostartState::ManagedElsewhere(path.to_owned()),
        (true, FileOwnership::Absent | FileOwnership::Ours) => {
            file::write(path)?;
            AutostartState::Enabled { backend }
        }
        (false, FileOwnership::Ours) => {
            file::remove_if_ours(path)?;
            AutostartState::Disabled { backend }
        }
        (false, FileOwnership::Absent) => AutostartState::Disabled { backend },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_precedence_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file::FILE_NAME);
        let enabled = AutostartState::Enabled {
            backend: AutostartBackend::DesktopFile(path.clone()),
        };
        let disabled = AutostartState::Disabled {
            backend: AutostartBackend::DesktopFile(path.clone()),
        };
        assert_eq!(apply_file(false, &path).unwrap(), disabled, "off + absent");
        assert_eq!(
            apply_file(true, &path).unwrap(),
            enabled,
            "on + absent writes"
        );
        assert_eq!(
            apply_file(true, &path).unwrap(),
            enabled,
            "on + ours rewrites"
        );
        assert_eq!(
            apply_file(false, &path).unwrap(),
            disabled,
            "off + ours removes"
        );
        assert!(!path.exists());
        std::fs::write(&path, "[Desktop Entry]\nExec=estuary\n").unwrap();
        for desired in [true, false] {
            assert_eq!(
                apply_file(desired, &path).unwrap(),
                AutostartState::ManagedElsewhere(path.clone())
            );
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[Desktop Entry]\nExec=estuary\n",
            "a foreign file is never touched"
        );
    }
}
