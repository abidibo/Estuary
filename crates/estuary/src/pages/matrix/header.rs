//! Matrix row and column headers (UI M9.3, redesigned in the M12 UX pass): a name and a level
//! meter. Clicking the name opens a popover with everything else: volume, rename, delete, noise
//! suppression, the voice gate, forget. Only the noise-suppressed row keeps its on/off switch
//! in view.
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
use crate::state::matrix_layout::{ColKey, ColSpec, DenoiseView, RowKey, RowSpec};
use crate::state::volume::{percent, slider_to_linear, volume_level};
use crate::widgets::meter_hub::MeterHub;
use crate::widgets::peak_meter::PeakMeter;

pub const ROW_HEADER_WIDTH: i32 = 240;
pub const COL_HEADER_HEIGHT: i32 = 62;
const THROTTLE: Duration = Duration::from_millis(33);
const COMMIT_AFTER: Duration = Duration::from_millis(400);
const HOLD_AFTER_USER: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderAction {
    Rename,
    Delete,
    Forget,
    /// An input's "Noise suppression" check (UI M10.1).
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

/// A volume slider with its percentage (mute stays as it is).
#[derive(Debug, Clone)]
struct VolumeSlider {
    root: gtk::Box,
    scale: gtk::Scale,
    value: gtk::Label,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
    muted: Rc<Cell<bool>>,
}

impl VolumeSlider {
    fn new(name: &str, on_change: VolumeFn) -> Self {
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        scale.set_draw_value(false);
        scale.set_hexpand(true);
        // Translators: accessible name of a volume slider; {} is a channel or device name
        scale.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Volume of {}",
            &[name],
        ))]);
        let value = gtk::Label::new(None);
        value.set_width_chars(5);
        value.set_xalign(1.0);
        value.add_css_class("numeric");
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.append(&scale);
        root.append(&value);
        let this = Self {
            root,
            scale: scale.clone(),
            value,
            updating: Rc::default(),
            last_user: Rc::default(),
            muted: Rc::default(),
        };
        scale.connect_value_changed({
            let label = this.value.clone();
            move |scale| label.set_label(&format!("{} %", percent(slider_to_linear(scale.value()))))
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
        self.root.set_sensitive(v.is_some());
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
        self.value
            .set_label(&format!("{} %", percent(slider_to_linear(volume_level(v)))));
    }
}

/// The voice gate: mutes what RNNoise doesn't think is speech (UI M10.2).
#[derive(Debug)]
struct GateSlider {
    scale: gtk::Scale,
    updating: Rc<Cell<bool>>,
    last_user: Rc<Cell<Option<Instant>>>,
}

impl GateSlider {
    fn new(on_denoise: &DenoiseFn) -> Self {
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 0.95, 0.05);
        scale.set_draw_value(false);
        scale.set_hexpand(true);
        scale.update_property(&[gtk::accessible::Property::Label(&i18n("Voice gate"))]);
        let this = Self {
            scale: scale.clone(),
            updating: Rc::default(),
            last_user: Rc::default(),
        };
        let f = on_denoise.clone();
        connect_throttled(
            &scale,
            this.updating.clone(),
            this.last_user.clone(),
            Rc::new(move |v, commit| {
                #[allow(clippy::cast_possible_truncation)] // 0..0.95
                f(DenoiseEvent::Gate {
                    gate: v as f32,
                    commit,
                });
            }),
        );
        this
    }

    fn apply(&self, v: &DenoiseView) {
        if self
            .last_user
            .get()
            .is_none_or(|t| t.elapsed() >= HOLD_AFTER_USER)
        {
            self.updating.set(true);
            self.scale.set_value(f64::from(v.vad_gate));
            self.updating.set(false);
        }
        self.scale.set_sensitive(v.enabled);
    }
}

/// A header's popover: the full name, then the controls that apply to it.
#[derive(Debug)]
struct Popover {
    title: gtk::Label,
    volume: Option<VolumeSlider>,
    suppression: Option<(gtk::CheckButton, Rc<Cell<bool>>)>,
    gate: Option<GateSlider>,
    forget: gtk::Button,
}

/// What a header's popover offers.
struct PopoverSpec<'a> {
    name: &'a str,
    volume: Option<VolumeFn>,
    /// Inputs: the "Noise suppression" check.
    suppression: bool,
    /// Noise-suppressed rows: the voice gate.
    gate: Option<&'a DenoiseFn>,
    /// Channels and virtual mics.
    rename_delete: bool,
    /// What this row or column is for, when that isn't obvious (also the name's tooltip).
    hint: Option<&'a str>,
}

/// Explains the noise-suppressed row: where its sound has to go to be useful.
fn denoised_hint() -> String {
    i18n(
        "Your microphone without background noise. Send it to a virtual mic, then choose that mic in Teams, Discord or OBS.",
    )
}

/// Explains a virtual mic column: how other apps get to hear it.
fn vmic_hint() -> String {
    i18n(
        "Choose this microphone in Teams, Discord or OBS, or make it the default input in Settings → Sound.",
    )
}

impl Popover {
    fn new(spec: &PopoverSpec<'_>, on_action: &ActionFn) -> (gtk::Popover, Self) {
        let popover = gtk::Popover::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_width_request(240);
        let title = gtk::Label::new(Some(spec.name));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_max_width_chars(28);
        content.append(&title);
        if let Some(text) = spec.hint {
            let hint = gtk::Label::new(Some(text));
            hint.add_css_class("caption");
            hint.add_css_class("dim-label");
            hint.set_wrap(true);
            hint.set_max_width_chars(30);
            hint.set_xalign(0.0);
            content.append(&hint);
        }

        let caption = |text: &str| {
            let l = gtk::Label::new(Some(text));
            l.add_css_class("caption-heading");
            l.add_css_class("dim-label");
            l.set_xalign(0.0);
            l
        };
        let volume = spec.volume.clone().map(|f| {
            content.append(&caption(&i18n("Volume")));
            let v = VolumeSlider::new(spec.name, f);
            content.append(&v.root);
            v
        });
        let suppression = spec.suppression.then(|| {
            let check = gtk::CheckButton::with_label(&i18n("Noise Suppression"));
            let updating = Rc::new(Cell::new(false));
            let (f, u) = (on_action.clone(), updating.clone());
            check.connect_toggled(move |c| {
                if !u.get() {
                    f(HeaderAction::Denoise(c.is_active()));
                }
            });
            content.append(&check);
            (check, updating)
        });
        let gate = spec.gate.map(|f| {
            content.append(&caption(&i18n("Voice gate")));
            let g = GateSlider::new(f);
            content.append(&g.scale);
            let hint = gtk::Label::new(Some(&i18n(
                "Mutes sound that is unlikely to be speech. Off by default.",
            )));
            hint.add_css_class("caption");
            hint.add_css_class("dim-label");
            hint.set_wrap(true);
            hint.set_max_width_chars(30);
            hint.set_xalign(0.0);
            content.append(&hint);
            g
        });

        let action_button = |label: &str, action: HeaderAction, destructive: bool| {
            let b = gtk::Button::with_label(label);
            b.add_css_class("flat");
            if destructive {
                b.add_css_class("destructive-action");
            }
            let (p, f) = (popover.clone(), on_action.clone());
            b.connect_clicked(move |_| {
                p.popdown();
                f(action);
            });
            b
        };
        if spec.rename_delete {
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            content.append(&action_button(
                &i18n("Rename…"),
                HeaderAction::Rename,
                false,
            ));
            content.append(&action_button(&i18n("Delete…"), HeaderAction::Delete, true));
        }
        let forget = action_button(&i18n("Forget Device…"), HeaderAction::Forget, true);
        forget.set_visible(false);
        content.append(&forget);
        popover.set_child(Some(&content));
        (
            popover,
            Self {
                title,
                volume,
                suppression,
                gate,
                forget,
            },
        )
    }

    fn apply(&self, name: &str, present: bool, volume: Option<&Volume>) {
        self.title.set_label(name);
        if let Some(v) = &self.volume {
            v.set_volume(volume);
        }
        // Forgetting is only for unplugged devices (F10).
        self.forget.set_visible(!present);
    }
}

/// The clickable name: a flat menu button holding the labels.
fn name_button(
    child: &impl IsA<gtk::Widget>,
    popover: &gtk::Popover,
    name: &str,
    hint: Option<&str>,
) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.add_css_class("flat");
    button.add_css_class("matrix-name");
    button.set_child(Some(child));
    button.set_popover(Some(popover));
    button.set_hexpand(true);
    // Translators: tooltip of a matrix header; {} is a channel or device name
    let tip = i18n_f("{}: click for options", &[name]);
    button.set_tooltip_text(Some(&match hint {
        Some(h) => format!("{h}\n\n{tip}"),
        None => tip,
    }));
    button
}

pub struct RowHeader {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    /// Owned here: the hub only holds meters weakly.
    _meter: PeakMeter,
    popover: Popover,
    switch: Option<(gtk::Switch, Rc<Cell<bool>>)>,
    badge: gtk::Image,
}

impl std::fmt::Debug for RowHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowHeader").finish_non_exhaustive()
    }
}

impl RowHeader {
    pub fn new(
        spec: &RowSpec,
        hub: &MeterHub,
        on_volume: VolumeFn,
        on_action: &ActionFn,
        on_denoise: &DenoiseFn,
    ) -> Self {
        let root: gtk::Box = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 4)
            .property("accessible-role", gtk::AccessibleRole::RowHeader)
            .build();
        root.set_size_request(ROW_HEADER_WIDTH, CELL_HEIGHT);
        root.add_css_class("matrix-header");
        let denoised = matches!(spec.key, RowKey::Denoised(_));
        let meter = PeakMeter::vertical(&spec.label, 6, 30);
        hub.register(&meter, spec.meter.clone());
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let subtitle = gtk::Label::new(None);
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
        labels.set_valign(gtk::Align::Center);
        labels.append(&title);
        labels.append(&subtitle);
        let hint = denoised.then(denoised_hint);
        let (popover, pop) = Popover::new(
            &PopoverSpec {
                name: &spec.label,
                volume: spec.volume.as_ref().map(|_| on_volume),
                suppression: matches!(spec.key, RowKey::Input(_)),
                gate: denoised.then_some(on_denoise),
                rename_delete: matches!(spec.key, RowKey::Channel(_)),
                hint: hint.as_deref(),
            },
            on_action,
        );
        let button = name_button(&labels, &popover, &spec.label, hint.as_deref());
        root.append(meter.widget());
        root.append(&button);
        let badge = gtk::Image::new();
        badge.set_pixel_size(16);
        badge.set_visible(false);
        root.append(&badge);
        let switch = denoised.then(|| {
            let s = gtk::Switch::new();
            s.set_valign(gtk::Align::Center);
            s.set_margin_end(8);
            // Translators: accessible name; {} is a microphone name
            s.update_property(&[gtk::accessible::Property::Label(&i18n_f(
                "Noise suppression for {}",
                &[&spec.label],
            ))]);
            let updating = Rc::new(Cell::new(false));
            let (u, f) = (updating.clone(), on_denoise.clone());
            s.connect_active_notify(move |s| {
                if !u.get() {
                    f(DenoiseEvent::Enabled(s.is_active()));
                }
            });
            root.append(&s);
            (s, updating)
        });
        let this = Self {
            root,
            title,
            subtitle,
            _meter: meter,
            popover: pop,
            switch,
            badge,
        };
        this.update(spec);
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn update(&self, spec: &RowSpec) {
        // The noise-suppressed row sits right under its mic: a short label, the full name in
        // the tooltip and the popover.
        let denoise = spec.denoise.as_ref();
        self.title.set_label(&if denoise.is_some() {
            i18n("↳ Noise-free")
        } else {
            spec.label.clone()
        });
        self.root
            .update_property(&[gtk::accessible::Property::Label(&spec.label)]);
        let off = denoise.is_some_and(|d| !d.enabled);
        let channel = matches!(spec.key, RowKey::Channel(_));
        let subtitle = if !spec.present && !channel {
            i18n("Disconnected")
        } else if off {
            // Translators: a noise-suppressed row while suppression is off (the mic passes through)
            i18n("Off")
        } else if channel && spec.apps.is_empty() {
            // Translators: under an empty channel in the matrix
            i18n("No apps yet")
        } else if channel {
            spec.apps.join(" · ")
        } else {
            String::new()
        };
        self.subtitle.set_label(&subtitle);
        let tip = if channel && spec.apps.is_empty() {
            Some(i18n("Choose this channel for an app on the Apps page."))
        } else {
            channel.then(|| subtitle.clone())
        };
        self.subtitle.set_tooltip_text(tip.as_deref());
        self.subtitle.set_visible(!subtitle.is_empty());
        if spec.present {
            self.root.remove_css_class("offline");
        } else {
            self.root.add_css_class("offline");
        }
        self.popover
            .apply(&spec.label, spec.present, spec.volume.as_ref());
        if let (Some((check, updating)), Some(on)) = (&self.popover.suppression, spec.suppression) {
            updating.set(true);
            check.set_active(on);
            updating.set(false);
        }
        if let Some(d) = denoise {
            if let Some(g) = &self.popover.gate {
                g.apply(d);
            }
            if let Some((s, updating)) = &self.switch {
                updating.set(true);
                s.set_active(d.enabled);
                updating.set(false);
            }
            match &d.badge {
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
}

pub struct ColHeader {
    root: gtk::Box,
    title: gtk::Label,
    offline: gtk::Label,
    meter_widget: gtk::DrawingArea,
    /// Owned here: the hub only holds meters weakly.
    _meter: PeakMeter,
    popover: Popover,
}

impl std::fmt::Debug for ColHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColHeader").finish_non_exhaustive()
    }
}

impl ColHeader {
    pub fn new(spec: &ColSpec, hub: &MeterHub, on_volume: VolumeFn, on_action: &ActionFn) -> Self {
        let root: gtk::Box = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 0)
            .property("accessible-role", gtk::AccessibleRole::ColumnHeader)
            .build();
        root.set_size_request(CELL_WIDTH, COL_HEADER_HEIGHT);
        root.set_hexpand(true);
        root.add_css_class("matrix-header");
        let title = gtk::Label::new(None);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(2);
        title.set_max_width_chars(10);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_justify(gtk::Justification::Center);
        // Always two lines tall, text at the bottom: every column's meter sits on one line.
        title.set_height_request(36);
        title.set_yalign(1.0);
        let meter = PeakMeter::horizontal(&spec.label, CELL_WIDTH - 28, 5);
        meter.widget().set_halign(gtk::Align::Center);
        meter.widget().set_margin_top(4);
        hub.register(&meter, spec.meter.clone());
        let offline = gtk::Label::new(Some(&i18n("Disconnected")));
        offline.add_css_class("caption");
        offline.add_css_class("dim-label");
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner.set_valign(gtk::Align::End);
        inner.append(&title);
        inner.append(meter.widget());
        inner.append(&offline);
        let vmic = matches!(spec.key, ColKey::VirtualMic(_));
        let hint = vmic.then(vmic_hint);
        let (popover, pop) = Popover::new(
            &PopoverSpec {
                name: &spec.label,
                volume: spec.volume.as_ref().map(|_| on_volume),
                suppression: false,
                gate: None,
                rename_delete: vmic,
                hint: hint.as_deref(),
            },
            on_action,
        );
        let button = name_button(&inner, &popover, &spec.label, hint.as_deref());
        root.append(&button);
        let this = Self {
            root,
            title,
            offline,
            meter_widget: meter.widget().clone(),
            _meter: meter,
            popover: pop,
        };
        this.update(spec);
        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn update(&self, spec: &ColSpec) {
        self.title.set_label(&spec.label);
        self.offline.set_visible(!spec.present);
        self.meter_widget.set_visible(spec.present);
        self.root
            .update_property(&[gtk::accessible::Property::Label(&spec.label)]);
        if spec.present {
            self.root.remove_css_class("offline");
        } else {
            self.root.add_css_class("offline");
        }
        self.popover
            .apply(&spec.label, spec.present, spec.volume.as_ref());
    }
}
