#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod app;
mod config;
mod config_build;
mod dialogs;
mod i18n;
mod pages;
mod state;
mod widgets;

use adw::prelude::*;
use config_build::APP_ID;
use relm4::{RelmApp, adw, gtk};

fn init_tracing() {
    // C14: ESTUARY_LOG, EnvFilter syntax, default "warn,estuary=info".
    let filter = tracing_subscriber::EnvFilter::try_from_env("ESTUARY_LOG").unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("warn,estuary=info,estuary_engine=info")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() {
    init_tracing();
    i18n::init();
    let app = adw::Application::builder().application_id(APP_ID).build();

    // Single instance. relm4 0.11's `RelmApp::run` ends with a blocking
    // `MainContext::iteration(true)` that hangs a remote (second) instance, which has nothing
    // left to process. So a second launch must never reach relm4: it forwards `activate` to the
    // primary instance (which presents its window) and exits. The check must not register the
    // app on the primary path, because registering emits `startup` before relm4 connects the
    // handler that builds the window.
    if primary_instance_running() {
        // Plain GApplication run (no relm4): as a remote instance it forwards `activate`,
        // flushes and unregisters cleanly, then returns.
        tracing::debug!("another instance is running; activating it");
        app.run();
        return;
    }

    RelmApp::from_app(app).run::<app::AppModel>(());
}

/// True if another process already owns our application ID on the session bus.
fn primary_instance_running() -> bool {
    use gtk::{gio, glib};
    let owned = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
        bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(APP_ID,).to_variant()),
            Some(glib::VariantTy::new("(b)").expect("valid variant type")),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
    });
    match owned {
        Ok(reply) => reply.get::<(bool,)>().is_some_and(|(owned,)| owned),
        Err(e) => {
            // No session bus: GApplication can't enforce single instance either; run normally.
            tracing::warn!("cannot query the session bus: {e}");
            false
        }
    }
}
