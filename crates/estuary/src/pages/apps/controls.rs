//! Volume control shared by app and output rows (UI M5.4): a slider plus a mute toggle.
//!
//! - Programmatic updates (from snapshots) never echo back as user edits (`updating` guard).
//! - Snapshots don't move the knob for 600 ms after the user touched it, so it doesn't jump.
//! - User changes are throttled to ~30 Hz.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use estuary_engine::Volume;
use relm4::gtk;
use relm4::gtk::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::state::volume::{percent, slider_to_linear, volume_level};

const THROTTLE: Duration = Duration::from_millis(33);
const HOLD_AFTER_USER: Duration = Duration::from_millis(600);

#[derive(Debug, Clone)]
pub struct VolumeControl {
    pub root: gtk::Box,
    scale: gtk::Scale,
    mute: gtk::ToggleButton,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
}

impl VolumeControl {
    /// `on_change(level, muted)` receives user edits only.
    pub fn new(name: &str, on_change: impl Fn(f64, bool) + 'static) -> Self {
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        scale.set_draw_value(false);
        scale.set_width_request(140);
        scale.set_valign(gtk::Align::Center);
        // Translators: accessible name of a volume slider; {} is an app or device name
        scale.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Volume of {}",
            &[name],
        ))]);
        let mute = gtk::ToggleButton::new();
        mute.set_icon_name("audio-volume-high-symbolic");
        mute.add_css_class("flat");
        mute.set_valign(gtk::Align::Center);
        mute.set_tooltip_text(Some(&i18n("Mute")));
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        root.append(&mute);
        root.append(&scale);

        let updating = Rc::new(Cell::new(false));
        let last_user = Rc::new(Cell::new(None::<Instant>));
        let on_change = Rc::new(on_change);
        let pending = Rc::new(Cell::new(false));

        scale.connect_value_changed({
            let (updating, last_user, mute, pending, on_change) = (
                updating.clone(),
                last_user.clone(),
                mute.clone(),
                pending.clone(),
                on_change.clone(),
            );
            move |scale| {
                if updating.get() {
                    return;
                }
                last_user.set(Some(Instant::now()));
                scale.set_tooltip_text(Some(&format!(
                    "{} %",
                    percent(slider_to_linear(scale.value()))
                )));
                if pending.replace(true) {
                    return; // a send is already scheduled; it will read the latest value
                }
                let (scale, mute, pending, on_change) = (
                    scale.clone(),
                    mute.clone(),
                    pending.clone(),
                    on_change.clone(),
                );
                gtk::glib::timeout_add_local_once(THROTTLE, move || {
                    pending.set(false);
                    on_change(scale.value(), mute.is_active());
                });
            }
        });
        mute.connect_toggled({
            let (updating, last_user, scale, on_change) = (
                updating.clone(),
                last_user.clone(),
                scale.clone(),
                on_change,
            );
            move |mute| {
                set_mute_icon(mute);
                if updating.get() {
                    return;
                }
                last_user.set(Some(Instant::now()));
                on_change(scale.value(), mute.is_active());
            }
        });
        Self {
            root,
            scale,
            mute,
            updating,
            last_user,
        }
    }

    /// Reflect the engine's state, unless the user is adjusting it right now.
    pub fn set_volume(&self, v: Option<&Volume>) {
        self.root.set_sensitive(v.is_some());
        let Some(v) = v else { return };
        if self
            .last_user
            .get()
            .is_some_and(|t| t.elapsed() < HOLD_AFTER_USER)
        {
            return;
        }
        self.updating.set(true);
        self.scale.set_value(volume_level(v));
        self.mute.set_active(v.muted);
        self.updating.set(false);
        self.scale.set_tooltip_text(Some(&format!(
            "{} %",
            percent(slider_to_linear(volume_level(v)))
        )));
        set_mute_icon(&self.mute);
    }
}

fn set_mute_icon(b: &gtk::ToggleButton) {
    b.set_icon_name(if b.is_active() {
        "audio-volume-muted-symbolic"
    } else {
        "audio-volume-high-symbolic"
    });
    b.set_tooltip_text(Some(&if b.is_active() {
        i18n("Unmute")
    } else {
        i18n("Mute")
    }));
}
