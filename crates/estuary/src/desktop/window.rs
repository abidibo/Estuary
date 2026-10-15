//! Showing, hiding and keeping the app alive while hidden (desktop M11.D2).

use relm4::adw;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

/// Keeps the process alive while the window is hidden; dropped only by the quit `Finish`.
pub fn acquire_hold(app: &adw::Application) -> gio::ApplicationHoldGuard {
    app.hold()
}

pub fn show_window(window: &adw::ApplicationWindow) {
    window.set_visible(true);
    window.present();
}

/// Hides without destroying anything.
pub fn hide_window(window: &adw::ApplicationWindow) {
    window.set_visible(false);
}
