//! The application object: single instance and `--background` (desktop M11.D1).

use relm4::adw;
use relm4::gtk::prelude::*;
use relm4::gtk::{gio, glib};

use crate::config_build::APP_ID;
use crate::i18n::i18n;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    Foreground,
    Background,
}

/// Builds the application; must run before `RelmApp::from_app`.
pub fn build_application() -> adw::Application {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "background",
        glib::Char::from(b'b'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        &i18n("Start without showing the window"),
        None,
    );
    // Runs in the primary instance, for the first launch and for every later one.
    app.connect_command_line(|app, cmdline| {
        let mode = if cmdline.options_dict().contains("background") {
            LaunchMode::Background
        } else {
            LaunchMode::Foreground
        };
        tracing::debug!(?mode, "command line");
        if mode == LaunchMode::Foreground {
            app.activate(); // relm4's activate handler presents the window
        }
        glib::ExitCode::SUCCESS
    });
    app
}
