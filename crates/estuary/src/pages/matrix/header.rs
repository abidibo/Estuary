//! Matrix row and column headers (UI M9.3): name, level meter, volume and a menu.
//!
//! Row headers are exactly one cell tall and column headers exactly one cell wide, so the
//! three grids line up without size groups.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use estuary_engine::Volume;
use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use super::cell::{CELL_HEIGHT, CELL_WIDTH};
use crate::i18n::{i18n, i18n_f};
use crate::state::matrix_layout::{ColKey, ColSpec, RowKey, RowSpec};
use crate::state::volume::{percent, slider_to_linear, volume_level};
use crate::widgets::meter_hub::MeterHub;
use crate::widgets::peak_meter::PeakMeter;

pub const ROW_HEADER_WIDTH: i32 = 220;
pub const COL_HEADER_HEIGHT: i32 = 108;
const THROTTLE: Duration = Duration::from_millis(33);
const COMMIT_AFTER: Duration = Duration::from_millis(400);
const HOLD_AFTER_USER: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderAction {
    Rename,
    Delete,
    Forget,
    /// The input menu's "Noise suppression" check (UI M10.1).
    Denoise(bool),
}

/// From a noise-suppressed row's own controls (UI M10.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DenoiseEvent {
    Enabled(bool),
    Gate { gate: f32, commit: bool },
}

/// `(level, muted, commit)`: throttled while moving, committed 400 ms after the last change.
pub type VolumeFn = Rc<dyn Fn(f64, bool, bool)>;
pub type ActionFn = Rc<dyn Fn(HeaderAction)>;
pub type DenoiseFn = Rc<dyn Fn(DenoiseEvent)>;

/// User changes of `scale` reach `on_value(value, commit)`: throttled to 30 Hz with
/// `commit = false`, then once with `commit = true` 400 ms after the last change.
fn connect_throttled(
    scale: &gtk::Scale,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
    on_value: Rc<dyn Fn(f64, bool)>,
) {
    let throttle_pending = Rc::new(Cell::new(false));
    let commit_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    scale.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        last_user.set(Some(Instant::now()));
        if !throttle_pending.replace(true) {
            let (scale, pending, f) = (scale.clone(), throttle_pending.clone(), on_value.clone());
            glib::timeout_add_local_once(THROTTLE, move || {
                pending.set(false);
                f(scale.value(), false);
            });
        }
        if let Some(t) = commit_timer.borrow_mut().take() {
            t.remove();
        }
        let (scale, timer, f) = (scale.clone(), commit_timer.clone(), on_value.clone());
        let id = glib::timeout_add_local_once(COMMIT_AFTER, move || {
            timer.borrow_mut().take();
            f(scale.value(), true);
        });
        *commit_timer.borrow_mut() = Some(id);
    });
}

/// A bare volume slider for headers (no mute button: there's no room; mute stays as it is).
#[derive(Debug, Clone)]
struct CompactVolume {
    scale: gtk::Scale,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
    muted: Rc<Cell<bool>>,
}

impl CompactVolume {
    fn new(orientation: gtk::Orientation, length: i32, name: &str, on_change: VolumeFn) -> Self {
        let scale = gtk::Scale::with_range(orientation, 0.0, 1.0, 0.01);
        scale.set_draw_value(false);
        if orientation == gtk::Orientation::Vertical {
            scale.set_inverted(true);
            scale.set_height_request(length);
        } else {
            scale.set_width_request(length);
        }
        scale.set_valign(gtk::Align::Center);
        // Translators: accessible name of a volume slider; {} is a channel or device name
        scale.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Volume of {}",
            &[name],
        ))]);
        let this = Self {
            scale: scale.clone(),
            updating: Rc::default(),
            last_user: Rc::default(),
            muted: Rc::default(),
        };
        scale.connect_value_changed({
            let updating = this.updating.clone();
            move |scale| {
                if !updating.get() {
                    scale.set_tooltip_text(Some(&format!(
                        "{} %",
                        percent(slider_to_linear(scale.value()))
                    )));
                }
            }
        });
        let muted = this.muted.clone();
        connect_throttled(
            &scale,
            this.updating.clone(),
            this.last_user.clone(),
            Rc::new(move |level, commit| on_change(level, muted.get(), commit)),
        );
        this
    }

    fn set_volume(&self, v: Option<&Volume>) {
        self.scale.set_sensitive(v.is_some());
        let Some(v) = v else { return };
        self.muted.set(v.muted);
        if self
            .last_user
            .get()
            .is_some_and(|t| t.elapsed() < HOLD_AFTER_USER)
        {
            return;
        }
        self.updating.set(true);
        self.scale.set_value(volume_level(v));
        self.updating.set(false);
        self.scale.set_tooltip_text(Some(&format!(
            "{} %",
            percent(slider_to_linear(volume_level(v)))
        )));
    }
}

/// `suppression`: an input's "Noise suppression" check and its state (UI M10.1).
fn menu_button(
    name: &str,
    actions: &[(HeaderAction, String)],
    suppression: Option<bool>,
    on_action: &ActionFn,
) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_icon_name("view-more-symbolic");
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    // Translators: tooltip of a header's menu button; {} is a channel or device name
    button.set_tooltip_text(Some(&i18n_f("Options for {}", &[name])));
    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);
    if let Some(on) = suppression {
        let check = gtk::CheckButton::with_label(&i18n("Noise Suppression"));
        check.set_active(on);
        let f = on_action.clone();
        check.connect_toggled(move |c| f(HeaderAction::Denoise(c.is_active())));
        content.append(&check);
    }
    for (action, label) in actions {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        if *action != HeaderAction::Rename {
            b.add_css_class("destructive-action");
        }
        let (action, popover, f) = (*action, popover.clone(), on_action.clone());
        b.connect_clicked(move |_| {
            popover.popdown();
            f(action);
        });
        content.append(&b);
    }
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));
    button.set_visible(!actions.is_empty() || suppression.is_some());
    button
}

fn row_actions(key: &RowKey, present: bool) -> Vec<(HeaderAction, String)> {
    match key {
        RowKey::Channel(_) => vec![
            (HeaderAction::Rename, i18n("Rename…")),
            (HeaderAction::Delete, i18n("Delete…")),
        ],
        RowKey::Input(_) if !present => vec![(HeaderAction::Forget, i18n("Forget Device…"))],
        RowKey::Input(_) | RowKey::Denoised(_) => Vec::new(),
    }
}

fn col_actions(key: &ColKey, present: bool) -> Vec<(HeaderAction, String)> {
    match key {
        ColKey::VirtualMic(_) => vec![
            (HeaderAction::Rename, i18n("Rename…")),
            (HeaderAction::Delete, i18n("Delete…")),
        ],
        ColKey::Output(_) if !present => vec![(HeaderAction::Forget, i18n("Forget Device…"))],
        ColKey::Output(_) => Vec::new(),
    }
}

pub struct RowHeader {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    /// Owned here: the hub only holds meters weakly.
    _meter: PeakMeter,
    volume: Option<CompactVolume>,
    denoise: Option<DenoiseControls>,
    menu_slot: gtk::Box,
    spec: RefCell<RowSpec>,
    on_action: ActionFn,
}

/// A noise-suppressed row's switch, voice gate and badge (UI M10.2).
#[derive(Debug)]
struct DenoiseControls {
    switch: gtk::Switch,
    gate: gtk::Scale,
    badge: gtk::Image,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
}

impl DenoiseControls {
    fn new(root: &gtk::Box, name: &str, on_denoise: &DenoiseFn) -> Self {
        let updating: Rc<Cell<bool>> = Rc::default();
        let last_user: Rc<Cell<Option<Instant>>> = Rc::default();
        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        // Translators: accessible name; {} is a microphone name
        switch.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Noise suppression for {}",
            &[name],
        ))]);
        switch.connect_active_notify({
            let (updating, f) = (updating.clone(), on_denoise.clone());
            move |s| {
                if !updating.get() {
                    f(DenoiseEvent::Enabled(s.is_active()));
                }
            }
        });
        let gate = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 0.95, 0.05);
        gate.set_draw_value(false);
        gate.set_width_request(56);
        gate.set_valign(gtk::Align::Center);
        gate.set_tooltip_text(Some(&i18n(
            "Mutes sound that is unlikely to be speech. Off by default.",
        )));
        gate.update_property(&[gtk::accessible::Property::Label(&i18n("Voice gate"))]);
        let f = on_denoise.clone();
        connect_throttled(
            &gate,
            updating.clone(),
            last_user.clone(),
            Rc::new(move |v, commit| {
                #[allow(clippy::cast_possible_truncation)] // 0..0.95
                f(DenoiseEvent::Gate {
                    gate: v as f32,
                    commit,
                });
            }),
        );
        let badge = gtk::Image::new();
        badge.set_pixel_size(16);
        badge.set_visible(false);
        root.append(&switch);
        root.append(&gate);
        root.append(&badge);
        Self {
            switch,
            gate,
            badge,
            updating,
            last_user,
        }
    }

    fn apply(&self, v: &crate::state::matrix_layout::DenoiseView) {
        self.updating.set(true);
        self.switch.set_active(v.enabled);
        if self
            .last_user
            .get()
            .is_none_or(|t| t.elapsed() >= HOLD_AFTER_USER)
        {
            self.gate.set_value(f64::from(v.vad_gate));
        }
        self.updating.set(false);
        self.gate.set_sensitive(v.enabled);
        match &v.badge {
            Some(b) => {
                self.badge.set_icon_name(Some(if b.error {
                    "dialog-error-symbolic"
                } else {
                    "dialog-warning-symbolic"
                }));
                self.badge.set_tooltip_text(Some(&b.text));
                self.badge.set_visible(true);
            }
            None => self.badge.set_visible(false),
        }
    }
}

impl std::fmt::Debug for RowHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowHeader")
            .field("key", &self.spec.borrow().key)
            .finish_non_exhaustive()
    }
}

impl RowHeader {
    pub fn new(
        spec: &RowSpec,
        hub: &MeterHub,
        on_volume: VolumeFn,
        on_action: ActionFn,
        on_denoise: &DenoiseFn,
    ) -> Self {
        let root: gtk::Box = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 6)
            .property("accessible-role", gtk::AccessibleRole::RowHeader)
            .build();
        root.set_size_request(ROW_HEADER_WIDTH, CELL_HEIGHT);
        root.add_css_class("matrix-header");
        if matches!(spec.key, RowKey::Denoised(_)) {
            root.set_margin_start(16);
        }
        let meter = PeakMeter::vertical(&spec.label, 6, 36);
        hub.register(&meter, spec.meter.clone());
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(18);
        let subtitle = gtk::Label::new(Some(&i18n("Disconnected")));
        subtitle.set_xalign(0.0);
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
        labels.set_valign(gtk::Align::Center);
        labels.set_hexpand(true);
        labels.append(&title);
        labels.append(&subtitle);
        root.append(meter.widget());
        root.append(&labels);
        let volume = spec.volume.as_ref().map(|_| {
            let v = CompactVolume::new(gtk::Orientation::Horizontal, 80, &spec.label, on_volume);
            root.append(&v.scale);
            v
        });
        let denoise = spec
            .denoise
            .as_ref()
            .map(|_| DenoiseControls::new(&root, &spec.label, on_denoise));
        let menu_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        if denoise.is_none() {
            menu_slot.set_size_request(34, -1);
        }
        root.append(&menu_slot);
        let this = Self {
            root,
            title,
            subtitle,
            _meter: meter,
            volume,
            denoise,
            menu_slot,
            spec: RefCell::new(spec.clone()),
            on_action,
        };
        this.apply(spec, true);
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn update(&self, spec: &RowSpec) {
        let menu_changed = {
            let old = self.spec.borrow();
            old.present != spec.present
                || old.label != spec.label
                || old.suppression != spec.suppression
        };
        self.apply(spec, menu_changed);
        *self.spec.borrow_mut() = spec.clone();
    }

    fn apply(&self, spec: &RowSpec, rebuild_menu: bool) {
        // The noise-suppressed row sits right under its mic: a short label, the full name in
        // the tooltip.
        let title = if spec.denoise.is_some() {
            i18n("↳ Noise-suppressed")
        } else {
            spec.label.clone()
        };
        self.title.set_label(&title);
        self.root.set_tooltip_text(Some(&spec.label));
        self.root
            .update_property(&[gtk::accessible::Property::Label(&spec.label)]);
        let off = spec.denoise.as_ref().is_some_and(|d| !d.enabled);
        self.subtitle.set_label(&if !spec.present {
            i18n("Disconnected")
        } else if off {
            // Translators: a noise-suppressed row while suppression is off (the mic passes through)
            i18n("Off")
        } else {
            String::new()
        });
        self.subtitle.set_visible(!spec.present || off);
        if let (Some(c), Some(v)) = (&self.denoise, &spec.denoise) {
            c.apply(v);
        }
        if spec.present {
            self.root.remove_css_class("offline");
        } else {
            self.root.add_css_class("offline");
        }
        if let Some(v) = &self.volume {
            v.set_volume(spec.volume.as_ref());
        }
        if rebuild_menu {
            while let Some(c) = self.menu_slot.first_child() {
                self.menu_slot.remove(&c);
            }
            let actions = row_actions(&spec.key, spec.present);
            self.menu_slot.append(&menu_button(
                &spec.label,
                &actions,
                spec.suppression,
                &self.on_action,
            ));
        }
    }
}

pub struct ColHeader {
    root: gtk::Box,
    title: gtk::Label,
    /// Owned here: the hub only holds meters weakly.
    _meter: PeakMeter,
    volume: Option<CompactVolume>,
    menu_slot: gtk::Box,
    spec: RefCell<ColSpec>,
    on_action: ActionFn,
}

impl std::fmt::Debug for ColHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColHeader")
            .field("key", &self.spec.borrow().key)
            .finish_non_exhaustive()
    }
}

impl ColHeader {
    pub fn new(spec: &ColSpec, hub: &MeterHub, on_volume: VolumeFn, on_action: ActionFn) -> Self {
        let root: gtk::Box = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 4)
            .property("accessible-role", gtk::AccessibleRole::ColumnHeader)
            .build();
        root.set_size_request(CELL_WIDTH, COL_HEADER_HEIGHT);
        root.add_css_class("matrix-header");
        let title = gtk::Label::new(None);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(2);
        title.set_max_width_chars(8);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_justify(gtk::Justification::Center);
        title.set_height_request(34);
        title.set_valign(gtk::Align::End);
        let meter = PeakMeter::horizontal(&spec.label, 56, 6);
        meter.widget().set_halign(gtk::Align::Center);
        hub.register(&meter, spec.meter.clone());
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls.set_halign(gtk::Align::Center);
        let volume = spec.volume.as_ref().map(|_| {
            let v = CompactVolume::new(gtk::Orientation::Vertical, 56, &spec.label, on_volume);
            controls.append(&v.scale);
            v
        });
        let menu_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        menu_slot.set_valign(gtk::Align::End);
        controls.append(&menu_slot);
        root.append(&title);
        root.append(meter.widget());
        root.append(&controls);
        let this = Self {
            root,
            title,
            _meter: meter,
            volume,
            menu_slot,
            spec: RefCell::new(spec.clone()),
            on_action,
        };
        this.apply(spec, true);
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn update(&self, spec: &ColSpec) {
        let menu_changed = {
            let old = self.spec.borrow();
            old.present != spec.present || old.label != spec.label
        };
        self.apply(spec, menu_changed);
        *self.spec.borrow_mut() = spec.clone();
    }

    fn apply(&self, spec: &ColSpec, rebuild_menu: bool) {
        self.title.set_label(&spec.label);
        let tooltip = if spec.present {
            spec.label.clone()
        } else {
            // Translators: tooltip of an unplugged device; {} is its name
            i18n_f("{} (disconnected)", &[&spec.label])
        };
        self.root.set_tooltip_text(Some(&tooltip));
        self.root
            .update_property(&[gtk::accessible::Property::Label(&spec.label)]);
        if spec.present {
            self.root.remove_css_class("offline");
        } else {
            self.root.add_css_class("offline");
        }
        if let Some(v) = &self.volume {
            v.set_volume(spec.volume.as_ref());
        }
        if rebuild_menu {
            while let Some(c) = self.menu_slot.first_child() {
                self.menu_slot.remove(&c);
            }
            let actions = col_actions(&spec.key, spec.present);
            self.menu_slot
                .append(&menu_button(&spec.label, &actions, None, &self.on_action));
        }
    }
}
