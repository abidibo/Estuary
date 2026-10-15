//! Matrix crosspoint (UI M9.2): one send from a row to a column, shown as a dB pill.
//!
//! Click toggles, vertical drag and scroll change the gain, double click resets it, a popover
//! offers exact entry. Every change leaves as an `Edit` through `connect_edit`; the view only
//! follows the next snapshot. Arrow keys belong to grid navigation (M9.5), so gain keys are
//! `+`/`−`, Page Up/Down, Home and End.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use estuary_engine::{CellKey, CellStatus};
use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;
use relm4::gtk::subclass::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::state::edit::Edit;
use crate::state::matrix_layout::CellView;
use crate::state::volume::{CELL_DB_MAX, CELL_DB_MIN, clamp_db, format_db};

pub const CELL_WIDTH: i32 = 72;
pub const CELL_HEIGHT: i32 = 44;
const DRAG_THRESHOLD: f64 = 4.0;
const DB_PER_PX: f32 = 0.25;
const DB_PER_PX_FINE: f32 = 0.05;
const THROTTLE: Duration = Duration::from_millis(33);
const COMMIT_AFTER: Duration = Duration::from_millis(400);
/// Snapshots don't overwrite a gain the user just changed.
const HOLD_AFTER_USER: Duration = Duration::from_millis(600);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct State {
        pub key: Option<CellKey>,
        pub view: Option<CellView>,
        pub name: String,
        /// The gain on screen; ahead of the view while the user adjusts it.
        pub gain: f32,
        pub last_user: Option<Instant>,
        pub drag_start: f32,
        pub moved: bool,
        pub throttle_pending: bool,
        pub commit_timer: Option<glib::SourceId>,
        pub toggle_timer: Option<glib::SourceId>,
        pub on_edit: Option<Rc<dyn Fn(Edit)>>,
        pub popover: Option<gtk::Popover>,
        /// The menu's spin button is being set from the view, not by the user.
        pub menu_updating: bool,
    }

    impl std::fmt::Debug for State {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("State")
                .field("key", &self.key)
                .field("gain", &self.gain)
                .finish_non_exhaustive()
        }
    }

    #[derive(Debug, Default)]
    pub struct CellButton {
        pub label: gtk::Label,
        pub badge: gtk::Image,
        pub spinner: gtk::Spinner,
        pub state: RefCell<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CellButton {
        const NAME: &'static str = "EstuaryCellButton";
        type Type = super::CellButton;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("cellbutton");
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_accessible_role(gtk::AccessibleRole::Slider);
        }
    }

    impl ObjectImpl for CellButton {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_size_request(CELL_WIDTH, CELL_HEIGHT);
            obj.set_focusable(true);
            obj.set_hexpand(false);
            obj.set_vexpand(false);
            let overlay = gtk::Overlay::new();
            self.label.add_css_class("pill");
            self.label.set_halign(gtk::Align::Center);
            self.label.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&self.label));
            for w in [
                self.badge.upcast_ref::<gtk::Widget>(),
                self.spinner.upcast_ref(),
            ] {
                w.set_halign(gtk::Align::End);
                w.set_valign(gtk::Align::Start);
                w.set_can_target(false);
                w.set_visible(false);
                overlay.add_overlay(w);
            }
            self.badge.set_pixel_size(12);
            self.spinner.set_size_request(12, 12);
            overlay.set_parent(&*obj);
        }

        fn dispose(&self) {
            let mut st = self.state.borrow_mut();
            for t in [st.commit_timer.take(), st.toggle_timer.take()]
                .into_iter()
                .flatten()
            {
                t.remove();
            }
            st.on_edit = None;
            st.popover = None;
            drop(st);
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CellButton {}
}

glib::wrapper! {
    pub struct CellButton(ObjectSubclass<imp::CellButton>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CellButton {
    /// `name` is the accessible label, e.g. "Music to Headphones".
    pub fn new(key: CellKey, name: String) -> Self {
        let obj: Self = glib::Object::new();
        {
            let mut st = obj.imp().state.borrow_mut();
            st.key = Some(key);
            st.name = name;
        }
        obj.install_gestures();
        obj
    }

    pub fn key(&self) -> CellKey {
        self.imp().state.borrow().key.clone().expect("set in new()")
    }

    pub fn connect_edit<F: Fn(Edit) + 'static>(&self, f: F) {
        self.imp().state.borrow_mut().on_edit = Some(Rc::new(f));
    }

    /// Follow the engine's state; never emits an edit.
    pub fn set_view(&self, v: &CellView) {
        let mut st = self.imp().state.borrow_mut();
        let user_active = st.last_user.is_some_and(|t| t.elapsed() < HOLD_AFTER_USER);
        if !user_active || !v.exists {
            st.gain = v.gain_db;
        }
        st.view = Some(v.clone());
        drop(st);
        self.render();
    }

    fn view(&self) -> Option<CellView> {
        self.imp().state.borrow().view.clone()
    }

    fn exists(&self) -> bool {
        self.view().is_some_and(|v| v.exists)
    }

    fn render(&self) {
        let imp = self.imp();
        let st = imp.state.borrow();
        let Some(v) = &st.view else { return };
        for c in ["absent", "on", "off", "pending", "missing", "error"] {
            self.remove_css_class(c);
        }
        let (text, value_text) = if !v.exists {
            self.add_css_class("absent");
            ("+".to_owned(), i18n("no send"))
        } else if v.enabled {
            self.add_css_class("on");
            // Translators: accessible value of a send, e.g. "−18 dB, on"
            (format_db(st.gain), i18n_f("{}, on", &[&format_db(st.gain)]))
        } else {
            self.add_css_class("off");
            (format_db(st.gain), i18n("off"))
        };
        imp.label.set_label(&text);
        // A disabled send has no node, so its status means nothing (C8).
        let status = if v.exists && v.enabled {
            v.status.clone()
        } else {
            None
        };
        let mut tooltip = None;
        imp.spinner.set_visible(false);
        imp.spinner.set_spinning(false);
        imp.badge.set_visible(false);
        match status {
            Some(CellStatus::Pending) => {
                self.add_css_class("pending");
                imp.spinner.set_visible(true);
                imp.spinner.set_spinning(true);
            }
            Some(CellStatus::SourceMissing | CellStatus::DestinationMissing) => {
                self.add_css_class("missing");
                imp.badge.set_icon_name(Some("network-offline-symbolic"));
                imp.badge.set_visible(true);
                tooltip = Some(i18n("Disconnected, will reconnect automatically"));
            }
            Some(CellStatus::Error(msg)) => {
                self.add_css_class("error");
                imp.badge.set_icon_name(Some("dialog-error-symbolic"));
                imp.badge.set_visible(true);
                tooltip = Some(msg);
            }
            Some(CellStatus::Active) | None => {}
        }
        self.set_tooltip_text(tooltip.as_deref().or(Some(&st.name)));
        self.update_property(&[
            gtk::accessible::Property::Label(&st.name),
            gtk::accessible::Property::ValueMin(f64::from(CELL_DB_MIN)),
            gtk::accessible::Property::ValueMax(f64::from(CELL_DB_MAX)),
            gtk::accessible::Property::ValueNow(f64::from(st.gain)),
            gtk::accessible::Property::ValueText(&value_text),
        ]);
    }

    fn emit(&self, edit: Edit) {
        let f = self.imp().state.borrow().on_edit.clone();
        if let Some(f) = f {
            f(edit);
        }
    }

    fn toggle(&self) {
        self.cancel_toggle();
        self.emit(Edit::ToggleCell { key: self.key() });
    }

    fn cancel_toggle(&self) {
        if let Some(t) = self.imp().state.borrow_mut().toggle_timer.take() {
            t.remove();
        }
    }

    /// Changes the gain on screen and sends it: throttled while adjusting, then committed.
    fn adjust(&self, gain: f32, commit: Commit) {
        if !self.exists() {
            return; // gain gestures on an absent cell do nothing
        }
        {
            let mut st = self.imp().state.borrow_mut();
            st.gain = clamp_db(gain);
            st.last_user = Some(Instant::now());
        }
        self.render();
        match commit {
            Commit::Now => self.send_gain(true),
            Commit::Later => {
                self.send_throttled();
                self.schedule_commit();
            }
            Commit::Never => self.send_throttled(),
        }
    }

    fn gain(&self) -> f32 {
        self.imp().state.borrow().gain
    }

    fn send_gain(&self, commit: bool) {
        let key = self.key();
        let gain_db = self.gain();
        self.emit(Edit::SetCellGain {
            key,
            gain_db,
            commit,
        });
    }

    fn send_throttled(&self) {
        if std::mem::replace(&mut self.imp().state.borrow_mut().throttle_pending, true) {
            return; // the scheduled send reads the latest gain
        }
        let weak = self.downgrade();
        glib::timeout_add_local_once(THROTTLE, move || {
            if let Some(obj) = weak.upgrade() {
                obj.imp().state.borrow_mut().throttle_pending = false;
                obj.send_gain(false);
            }
        });
    }

    fn schedule_commit(&self) {
        if let Some(t) = self.imp().state.borrow_mut().commit_timer.take() {
            t.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(COMMIT_AFTER, move || {
            if let Some(obj) = weak.upgrade() {
                obj.imp().state.borrow_mut().commit_timer = None;
                obj.send_gain(true);
            }
        });
        self.imp().state.borrow_mut().commit_timer = Some(id);
    }

    fn install_gestures(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.connect_pressed({
            let weak = self.downgrade();
            move |_, n_press, _, _| {
                let Some(obj) = weak.upgrade() else { return };
                obj.grab_focus();
                if n_press == 2 && obj.exists() {
                    obj.cancel_toggle();
                    obj.adjust(0.0, Commit::Now);
                }
            }
        });
        click.connect_released({
            let weak = self.downgrade();
            move |_, n_press, _, _| {
                let Some(obj) = weak.upgrade() else { return };
                if n_press != 1 || obj.imp().state.borrow().moved {
                    return;
                }
                let active = obj.view().is_some_and(|v| v.exists && v.enabled);
                if !active {
                    obj.toggle(); // turning a send on is instant
                    return;
                }
                // Turning an active send off waits out a possible double click (reset to 0 dB),
                // so a double click never interrupts the audio.
                let delay = gtk::Settings::default().map_or(400, |s| s.gtk_double_click_time());
                let delay = Duration::from_millis(u64::try_from(delay).unwrap_or(400));
                let weak = obj.downgrade();
                let id = glib::timeout_add_local_once(delay, move || {
                    if let Some(obj) = weak.upgrade() {
                        obj.imp().state.borrow_mut().toggle_timer = None;
                        obj.emit(Edit::ToggleCell { key: obj.key() });
                    }
                });
                obj.cancel_toggle();
                obj.imp().state.borrow_mut().toggle_timer = Some(id);
            }
        });
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(gtk::gdk::BUTTON_PRIMARY);
        drag.connect_drag_begin({
            let weak = self.downgrade();
            move |_, _, _| {
                if let Some(obj) = weak.upgrade() {
                    let mut st = obj.imp().state.borrow_mut();
                    st.moved = false;
                    st.drag_start = st.gain;
                }
            }
        });
        drag.connect_drag_update({
            let weak = self.downgrade();
            move |g, dx, dy| {
                let Some(obj) = weak.upgrade() else { return };
                let (moved, start) = {
                    let mut st = obj.imp().state.borrow_mut();
                    if !st.moved && dx.hypot(dy) > DRAG_THRESHOLD {
                        st.moved = true;
                    }
                    (st.moved, st.drag_start)
                };
                if !moved {
                    return;
                }
                obj.cancel_toggle();
                let fine = g
                    .current_event_state()
                    .contains(gtk::gdk::ModifierType::SHIFT_MASK);
                let per_px = if fine { DB_PER_PX_FINE } else { DB_PER_PX };
                #[allow(clippy::cast_possible_truncation)] // pixels
                let delta = -(dy as f32) * per_px;
                obj.adjust(start + delta, Commit::Never);
            }
        });
        drag.connect_drag_end({
            let weak = self.downgrade();
            move |_, _, _| {
                if let Some(obj) = weak.upgrade()
                    && obj.imp().state.borrow().moved
                {
                    obj.adjust(obj.gain(), Commit::Now);
                }
            }
        });
        self.add_controller(drag);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let weak = self.downgrade();
            move |c, _, dy| {
                let Some(obj) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !obj.exists() {
                    return glib::Propagation::Proceed; // let the grid scroll over empty cells
                }
                let fine = c
                    .current_event_state()
                    .contains(gtk::gdk::ModifierType::SHIFT_MASK);
                let step = if fine { 0.1 } else { 1.0 };
                #[allow(clippy::cast_possible_truncation)]
                let delta = -(dy as f32) * step;
                obj.adjust(obj.gain() + delta, Commit::Later);
                glib::Propagation::Stop
            }
        });
        self.add_controller(scroll);

        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        secondary.connect_pressed({
            let weak = self.downgrade();
            move |_, _, _, _| {
                if let Some(obj) = weak.upgrade() {
                    obj.show_menu();
                }
            }
        });
        self.add_controller(secondary);
        let long = gtk::GestureLongPress::new();
        long.set_touch_only(true);
        long.connect_pressed({
            let weak = self.downgrade();
            move |_, _, _| {
                if let Some(obj) = weak.upgrade() {
                    obj.show_menu();
                }
            }
        });
        self.add_controller(long);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let weak = self.downgrade();
            move |_, key, _, state| {
                let Some(obj) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if obj.on_key(key, state) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        self.add_controller(keys);
    }

    fn on_key(&self, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
        use gtk::gdk::Key;
        let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
        let gain = self.gain();
        match key {
            Key::space | Key::Return | Key::KP_Enter => self.toggle(),
            Key::plus | Key::equal | Key::KP_Add => self.adjust(gain + 1.0, Commit::Later),
            Key::minus | Key::KP_Subtract => self.adjust(gain - 1.0, Commit::Later),
            Key::Page_Up => self.adjust(gain + 6.0, Commit::Later),
            Key::Page_Down => self.adjust(gain - 6.0, Commit::Later),
            Key::Home => self.adjust(0.0, Commit::Now),
            Key::End => self.adjust(CELL_DB_MIN, Commit::Now),
            Key::Delete | Key::KP_Delete if self.exists() => {
                self.emit(Edit::RemoveCell { key: self.key() });
            }
            Key::Menu => self.show_menu(),
            Key::F10 if shift => self.show_menu(),
            _ => return false,
        }
        true
    }

    fn show_menu(&self) {
        let popover = self.imp().state.borrow().popover.clone();
        let popover = popover.unwrap_or_else(|| {
            let p = self.build_menu();
            self.imp().state.borrow_mut().popover = Some(p.clone());
            p
        });
        self.refresh_menu(&popover);
        popover.popup();
    }

    fn build_menu(&self) -> gtk::Popover {
        let popover = gtk::Popover::new();
        popover.set_parent(self);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        let toggle = gtk::Button::new();
        toggle.add_css_class("flat");
        toggle.set_widget_name("toggle");
        let reset = gtk::Button::with_label(&i18n("Set to 0 dB"));
        reset.add_css_class("flat");
        reset.set_widget_name("reset");
        let remove = gtk::Button::with_label(&i18n("Remove send"));
        remove.add_css_class("flat");
        remove.add_css_class("destructive-action");
        remove.set_widget_name("remove");
        let spin = gtk::SpinButton::with_range(f64::from(CELL_DB_MIN), f64::from(CELL_DB_MAX), 0.5);
        spin.set_digits(1);
        spin.set_widget_name("spin");
        spin.update_property(&[gtk::accessible::Property::Label(&i18n("Gain in dB"))]);
        for w in [
            toggle.upcast_ref::<gtk::Widget>(),
            reset.upcast_ref(),
            remove.upcast_ref(),
            spin.upcast_ref(),
        ] {
            content.append(w);
        }
        popover.set_child(Some(&content));

        let weak = self.downgrade();
        toggle.connect_clicked({
            let (weak, popover) = (weak.clone(), popover.clone());
            move |_| {
                popover.popdown();
                if let Some(obj) = weak.upgrade() {
                    obj.toggle();
                }
            }
        });
        reset.connect_clicked({
            let (weak, popover) = (weak.clone(), popover.clone());
            move |_| {
                popover.popdown();
                if let Some(obj) = weak.upgrade() {
                    obj.adjust(0.0, Commit::Now);
                }
            }
        });
        remove.connect_clicked({
            let (weak, popover) = (weak.clone(), popover.clone());
            move |_| {
                popover.popdown();
                if let Some(obj) = weak.upgrade() {
                    obj.emit(Edit::RemoveCell { key: obj.key() });
                }
            }
        });
        spin.connect_value_changed(move |spin| {
            if let Some(obj) = weak.upgrade()
                && !obj.imp().state.borrow().menu_updating
            {
                #[allow(clippy::cast_possible_truncation)] // −60..+6
                obj.adjust(spin.value() as f32, Commit::Later);
            }
        });
        popover
    }

    fn refresh_menu(&self, popover: &gtk::Popover) {
        let v = self.view();
        let exists = v.as_ref().is_some_and(|v| v.exists);
        let enabled = v.as_ref().is_some_and(|v| v.enabled);
        let find = |name: &str| -> Option<gtk::Widget> {
            let content = popover.child()?;
            let mut child = content.first_child();
            while let Some(c) = child {
                if c.widget_name() == name {
                    return Some(c);
                }
                child = c.next_sibling();
            }
            None
        };
        if let Some(b) = find("toggle").and_downcast::<gtk::Button>() {
            b.set_label(&if !exists {
                i18n("Add send")
            } else if enabled {
                i18n("Turn off")
            } else {
                i18n("Turn on")
            });
        }
        for name in ["reset", "remove", "spin"] {
            if let Some(w) = find(name) {
                w.set_sensitive(exists);
            }
        }
        if let Some(spin) = find("spin").and_downcast::<gtk::SpinButton>() {
            self.imp().state.borrow_mut().menu_updating = true;
            spin.set_value(f64::from(self.gain()));
            self.imp().state.borrow_mut().menu_updating = false;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Commit {
    /// Saved right away (release, double click, menu, Home/End).
    Now,
    /// Saved 400 ms after the last step (scroll, keys, spin button).
    Later,
    /// Not saved (drag in progress).
    Never,
}

/// Matrix styling: pills, states and focus ring. Installed once per display.
pub fn install_css() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

const CSS: &str = "
cellbutton { border-radius: 8px; }
cellbutton:hover { background-color: alpha(currentColor, 0.05); }
cellbutton:focus-visible { outline: 2px solid alpha(@accent_color, 0.6); outline-offset: -2px; }
cellbutton label.pill { border-radius: 999px; padding: 3px 8px; font-feature-settings: 'tnum'; }
cellbutton.absent label.pill { opacity: 0; }
cellbutton.absent:hover label.pill, cellbutton.absent:focus-visible label.pill { opacity: 0.45; }
cellbutton.on label.pill { background-color: @accent_bg_color; color: @accent_fg_color; }
cellbutton.off label.pill { box-shadow: inset 0 0 0 1px alpha(currentColor, 0.35); color: alpha(currentColor, 0.55); }
cellbutton.missing label.pill, cellbutton.error label.pill { opacity: 0.6; }
cellbutton.error image { color: @error_color; }
.matrix-header.offline { opacity: 0.55; }
";
