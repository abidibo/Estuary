//! SIGTERM / SIGINT / SIGHUP quit without a dialog; a second one forces it (desktop M11.D7).

use relm4::gtk::glib;

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGTERM: i32 = 15;

pub fn install_signal_handlers(on_signal: impl Fn(i32) + Clone + 'static) -> Vec<glib::SourceId> {
    [SIGTERM, SIGINT, SIGHUP]
        .into_iter()
        .map(|sig| {
            let f = on_signal.clone();
            glib_unix::unix_signal_add_local(sig, move || {
                f(sig);
                glib::ControlFlow::Continue
            })
        })
        .collect()
}
