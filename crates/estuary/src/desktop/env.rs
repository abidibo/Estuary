//! Where we run (desktop M11.D8).

use std::path::PathBuf;

use relm4::gtk::glib;

#[derive(Clone, Debug)]
pub struct DesktopEnv {
    /// Flatpak or Snap: the Background portal is the only start-at-login path.
    pub sandboxed: bool,
    /// Diagnostics only.
    pub flatpak_id: Option<String>,
    /// Native: `$XDG_CONFIG_HOME/autostart`. Sandboxed: never touched (C14).
    pub autostart_dir: Option<PathBuf>,
}

impl DesktopEnv {
    pub fn detect() -> Self {
        let sandboxed = ashpd::is_sandboxed();
        Self {
            sandboxed,
            flatpak_id: std::env::var("FLATPAK_ID").ok(),
            autostart_dir: (!sandboxed).then(|| glib::user_config_dir().join("autostart")),
        }
    }
}
