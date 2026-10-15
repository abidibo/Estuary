//! Destructive confirmations (UI M9.7, M9.8).

use adw::prelude::*;
use relm4::{adw, gtk};

use crate::i18n::{i18n, i18n_f, ni18n_f};

/// "Delete {name}?" listing what goes with it. `apps_moving`: rules that fall back to the
/// default output (channels only).
pub fn delete(
    parent: &impl IsA<gtk::Widget>,
    name: &str,
    apps_moving: u32,
    on_confirm: impl Fn() + 'static,
) {
    let mut body = i18n("Its sends will be removed.");
    if apps_moving > 0 {
        body.push('\n');
        body.push_str(&ni18n_f(
            "{} app will move to the default output.",
            "{} apps will move to the default output.",
            apps_moving,
            &[&apps_moving.to_string()],
        ));
    }
    // Translators: {} is a channel or virtual mic name
    let heading = i18n_f("Delete {}?", &[name]);
    present(parent, &heading, &body, &i18n("Delete"), on_confirm);
}

/// "Forget {name}?" for an unplugged device (F10).
pub fn forget(parent: &impl IsA<gtk::Widget>, name: &str, on_confirm: impl Fn() + 'static) {
    // Translators: {} is a device name
    let heading = i18n_f("Forget {}?", &[name]);
    let body = i18n("Its sends and noise suppression settings will be removed.");
    present(parent, &heading, &body, &i18n("Forget"), on_confirm);
}

fn present(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    action: &str,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", action);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "ok" {
            on_confirm();
        }
    });
    dialog.present(Some(parent));
}
