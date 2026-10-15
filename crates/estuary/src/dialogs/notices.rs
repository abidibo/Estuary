//! Alert dialogs of the lifecycle (UI M11.3–M11.5).

use adw::prelude::*;
use estuary_engine::{SetupIssue, UsageKind, VirtualDeviceUsage};
use relm4::{adw, gtk};

use crate::i18n::{i18n, i18n_f};
use crate::state::surfaces::issue_text;

fn ok_dialog(heading: &str, body: &str, on_dismissed: impl Fn() + 'static) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("ok", &i18n("OK"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.connect_response(None, move |_, _| on_dismissed());
    dialog
}

/// C10: shown once, while the window is still visible, before hiding it.
pub fn background_notice(parent: &impl IsA<gtk::Widget>, on_dismissed: impl Fn() + 'static) {
    ok_dialog(
        &i18n("estuary keeps running"),
        &i18n("Your audio routing keeps working in the background. Use Quit in the menu to stop estuary."),
        on_dismissed,
    )
    .present(Some(parent));
}

/// Sandboxed with background activity denied: closing quits (once per session).
pub fn background_denied_notice(parent: &impl IsA<gtk::Widget>, on_dismissed: impl Fn() + 'static) {
    ok_dialog(
        &i18n("estuary will quit"),
        &i18n("Background activity is not allowed for estuary, so closing the window quits it. You can allow it in Settings → Apps → estuary."),
        on_dismissed,
    )
    .present(Some(parent));
}

/// C10 step 2: apps depending on our devices. Returns the dialog so a signal can force-close
/// it without a response.
pub fn quit_confirm(
    parent: &impl IsA<gtk::Widget>,
    usages: &[VirtualDeviceUsage],
    on_answer: impl Fn(bool) + 'static,
) -> adw::AlertDialog {
    let mut body: Vec<String> = usages
        .iter()
        .map(|u| match u.kind {
            // Translators: {} are an app name and a channel name
            UsageKind::PlayingToChannel => i18n_f(
                "{} is playing into “{}”.",
                &[&u.app_display_name, &u.device_name],
            ),
            // Translators: {} are an app name and a virtual microphone name
            UsageKind::RecordingFromVirtualMic => i18n_f(
                "{} is recording from “{}”.",
                &[&u.app_display_name, &u.device_name],
            ),
        })
        .collect();
    body.push(i18n(
        "Quitting will switch these apps back to your default devices.",
    ));
    let dialog = adw::AlertDialog::new(Some(&i18n("Quit estuary?")), Some(&body.join("\n")));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("quit", &i18n("Quit"));
    dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, r| on_answer(r == "quit"));
    dialog.present(Some(parent));
    dialog
}

/// "Details" of the setup-issues toast.
pub fn issue_details(parent: &impl IsA<gtk::Widget>, issues: &[SetupIssue]) {
    let body = issues
        .iter()
        .map(|i| format!("• {}", issue_text(i)))
        .collect::<Vec<_>>()
        .join("\n");
    ok_dialog(&i18n("Some routes couldn't be created"), &body, || {}).present(Some(parent));
}
