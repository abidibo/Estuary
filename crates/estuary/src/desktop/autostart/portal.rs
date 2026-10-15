//! Background portal (desktop M11.D10). Sandboxed builds only; futures run on relm4's tokio
//! runtime (`relm4::spawn`).

use ashpd::desktop::ResponseError;
use ashpd::desktop::background::{Background, BackgroundProxy, SetStatusOptions};

use crate::i18n::i18n;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalResult {
    Granted {
        run_in_background: bool,
        auto_start: bool,
    },
    /// The user dismissed the dialog: treated as denied.
    Cancelled,
    /// No Background portal (COSMIC today, F19).
    Unavailable,
}

pub async fn request(auto_start: bool, reason: String) -> Result<PortalResult, ashpd::Error> {
    let req = Background::request()
        .reason(reason.as_str())
        .auto_start(auto_start)
        .command(["estuary", "--background"])
        .dbus_activatable(false);
    match req.send().await {
        Err(ashpd::Error::PortalNotFound(_)) => Ok(PortalResult::Unavailable),
        Err(e) => Err(e),
        Ok(request) => match request.response() {
            Ok(bg) => Ok(PortalResult::Granted {
                run_in_background: bg.run_in_background(),
                auto_start: bg.auto_start(),
            }),
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(PortalResult::Cancelled),
            Err(e) => Err(e),
        },
    }
}

/// Localized on the UI thread (gettext) before the request is spawned.
pub fn reason() -> String {
    i18n("Keep audio routing active when the window is closed")
}

/// Best effort: the status line in GNOME's Background Apps list (portal v2+).
pub async fn set_background_status(message: Option<String>) {
    let msg: String = message.unwrap_or_default().chars().take(96).collect();
    let result = async {
        BackgroundProxy::new()
            .await?
            .set_status(SetStatusOptions::default().set_message(&msg))
            .await
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("background status: {e}");
    }
}
