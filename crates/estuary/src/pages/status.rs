//! Full-page status for blocking connection states (UI M3.5, master C13).

use adw::prelude::*;
use estuary_engine::{ConnectionState, EngineError, UnsupportedReason};
use relm4::{adw, gtk};

use crate::i18n::{i18n, i18n_f};

const README_URL: &str = "https://github.com/abidibo/Estuary#requirements";

/// What the root stack should show for a connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootView {
    Status,
    Main,
}

pub fn root_view(state: Option<&ConnectionState>, fatal: bool) -> RootView {
    match (fatal, state) {
        (true, _) => RootView::Status,
        (_, Some(ConnectionState::Connected { .. } | ConnectionState::Reconnecting { .. })) => {
            RootView::Main
        }
        _ => RootView::Status,
    }
}

/// Builds a fresh status page; it is replaced, not mutated, on every state change.
pub fn status_page(
    state: Option<&ConnectionState>,
    fatal: Option<&EngineError>,
    on_restart: impl Fn() + 'static,
) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    if let Some(e) = fatal {
        page.set_icon_name(Some("computer-fail-symbolic"));
        page.set_title(&i18n("The audio engine stopped"));
        page.set_description(Some(&e.to_string()));
        let button = pill_button(&i18n("Restart Engine"));
        button.connect_clicked(move |_| on_restart());
        page.set_child(Some(&button));
        return page;
    }
    match state {
        None | Some(ConnectionState::Connecting { .. }) => {
            page.set_title(&i18n("Waiting for PipeWire…"));
            page.set_description(Some(&i18n(
                "Estuary will connect as soon as the audio system is available.",
            )));
            let spinner = gtk::Spinner::new();
            spinner.set_spinning(true);
            spinner.set_size_request(32, 32);
            page.set_child(Some(&spinner));
        }
        Some(ConnectionState::Unsupported { reason }) => {
            page.set_icon_name(Some("dialog-warning-symbolic"));
            page.set_title(&i18n("Unsupported audio setup"));
            page.set_description(Some(&unsupported_text(reason)));
            page.set_child(Some(&learn_more()));
        }
        Some(ConnectionState::PermissionDenied) => {
            page.set_icon_name(Some("action-unavailable-symbolic"));
            page.set_title(&i18n("Estuary can't change audio routing"));
            page.set_description(Some(&i18n(
                "The audio system only granted read access. If you run Estuary as a Flatpak, make sure it is up to date.",
            )));
            page.set_child(Some(&learn_more()));
        }
        // Not blocking states: the main view is shown instead.
        Some(ConnectionState::Connected { .. } | ConnectionState::Reconnecting { .. }) => {}
    }
    page
}

fn unsupported_text(reason: &UnsupportedReason) -> String {
    let unknown = i18n("unknown version");
    match reason {
        UnsupportedReason::PipeWireTooOld { found } => {
            // Translators: {} is a version number, e.g. 0.3.65
            i18n_f("PipeWire 1.0 or newer is required (found {}).", &[found])
        }
        UnsupportedReason::WirePlumberTooOld { found } => {
            // Translators: {} is a version number, e.g. 0.4.17
            i18n_f(
                "WirePlumber 0.5 or newer is required (found {}).",
                &[found.as_deref().unwrap_or(&unknown)],
            )
        }
        UnsupportedReason::NoWirePlumber => i18n("WirePlumber is not running."),
    }
}

fn pill_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("pill");
    button.add_css_class("suggested-action");
    button.set_halign(gtk::Align::Center);
    button
}

fn learn_more() -> gtk::Button {
    let button = pill_button(&i18n("Learn More"));
    button.remove_css_class("suggested-action");
    button.connect_clicked(|b| {
        let window = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        gtk::UriLauncher::new(README_URL).launch(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            |res| {
                if let Err(e) = res {
                    tracing::warn!("cannot open {README_URL}: {e}");
                }
            },
        );
    });
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn root_view_mapping() {
        let connected = ConnectionState::Connected {
            pipewire_version: "1.6.8".into(),
            wireplumber_version: None,
        };
        let reconnecting = ConnectionState::Reconnecting {
            since: Instant::now(),
            attempt: 0,
        };
        assert_eq!(root_view(None, false), RootView::Status);
        assert_eq!(
            root_view(Some(&ConnectionState::Connecting { attempt: 0 }), false),
            RootView::Status
        );
        assert_eq!(root_view(Some(&connected), false), RootView::Main);
        assert_eq!(root_view(Some(&reconnecting), false), RootView::Main);
        assert_eq!(
            root_view(Some(&ConnectionState::PermissionDenied), false),
            RootView::Status
        );
        assert_eq!(
            root_view(Some(&connected), true),
            RootView::Status,
            "fatal wins"
        );
    }
}
