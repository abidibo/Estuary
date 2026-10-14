//! Rows of the Apps page (UI M3.6, M4, M5): app rows with meter, volume and output dropdown;
//! output rows with meter and volume; remembered rules for apps not playing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use estuary_engine::{
    AppGroup, AppKey, DeviceInfo, DeviceKey, GraphSnapshot, MeterTarget, RouteTarget,
};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::{adw, gtk};

use super::controls::VolumeControl;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::pages::reconcile::KeyedRow;
use crate::state::edit::Edit;
use crate::widgets::meter_hub::MeterHub;
use crate::widgets::peak_meter::PeakMeter;

/// Human-readable name of where an app plays.
pub fn target_label(target: &RouteTarget, snap: &GraphSnapshot) -> String {
    match target {
        RouteTarget::Default => {
            let device = snap
                .default_output
                .as_ref()
                .and_then(|k| snap.outputs.iter().find(|d| &d.key == k))
                .map(|d| d.description.as_str());
            match device {
                // Translators: {} is the name of the default output device
                Some(name) => i18n_f("Default output ({})", &[name]),
                None => i18n("Default output"),
            }
        }
        RouteTarget::Device { key } => snap
            .outputs
            .iter()
            .find(|d| &d.key == key)
            .map_or_else(|| key.0.clone(), |d| d.description.clone()),
        RouteTarget::Channel { id } => snap
            .channels
            .iter()
            .find(|c| &c.id == id)
            .map_or_else(|| i18n("Unknown channel"), |c| c.name.clone()),
    }
}

/// One entry of the output dropdown (UI M5.3).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetChoice {
    pub target: RouteTarget,
    pub label: String,
}

/// Default output, present channels, present outputs, then the row's own targets if they are
/// unavailable right now (marked "disconnected").
pub fn target_choices(snap: &GraphSnapshot, extra: &[&RouteTarget]) -> Vec<TargetChoice> {
    let mut v = vec![TargetChoice {
        target: RouteTarget::Default,
        label: target_label(&RouteTarget::Default, snap),
    }];
    for c in snap.channels.iter().filter(|c| c.present) {
        v.push(TargetChoice {
            target: RouteTarget::Channel { id: c.id },
            // Translators: an entry in the output list; {} is a channel name
            label: i18n_f("Channel: {}", &[&c.name]),
        });
    }
    for d in snap.outputs.iter().filter(|d| d.present) {
        v.push(TargetChoice {
            target: RouteTarget::Device { key: d.key.clone() },
            label: d.description.clone(),
        });
    }
    for t in extra {
        if !v.iter().any(|c| &c.target == *t) {
            v.push(TargetChoice {
                target: (*t).clone(),
                // Translators: {} is a device or channel name that is currently unavailable
                label: i18n_f("{} (disconnected)", &[&target_label(t, snap)]),
            });
        }
    }
    v
}

#[derive(Debug, Clone)]
pub struct AppRowData {
    pub group: AppGroup,
    pub choices: Vec<TargetChoice>,
    pub hub: MeterHub,
}

#[derive(Debug)]
pub struct AppRow {
    data: AppRowData,
}

#[derive(Debug)]
pub struct AppRowWidgets {
    row: adw::ActionRow,
    icon: gtk::Image,
    _meter: PeakMeter,
    volume: VolumeControl,
    dropdown: gtk::DropDown,
    model: gtk::StringList,
    labels: Vec<String>,
    updating: Rc<Cell<bool>>,
    /// The choices behind the dropdown entries, read at click time.
    shown: Rc<RefCell<Vec<TargetChoice>>>,
}

#[derive(Debug)]
pub enum AppRowInput {
    /// The user picked an entry; resolved to its target when clicked, so a snapshot that
    /// reorders the list in between can't change what was chosen.
    TargetSelected(RouteTarget),
}

#[derive(Debug)]
pub enum RowOutput {
    Edit(Edit),
}

impl AppRow {
    fn subtitle(&self) -> String {
        let g = &self.data.group;
        let mut parts = Vec::new();
        if let Some(media) = g.streams.first().and_then(|s| s.media_name.as_deref()) {
            parts.push(media.to_owned());
        }
        if g.streams.len() > 1 {
            let n = u32::try_from(g.streams.len()).unwrap_or(u32::MAX);
            parts.push(ni18n_f("{} stream", "{} streams", n, &[&n.to_string()]));
        }
        parts.join(" · ")
    }
}

impl FactoryComponent for AppRow {
    type Init = AppRowData;
    type Input = AppRowInput;
    type Output = RowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;
    type Root = adw::ActionRow;
    type Widgets = AppRowWidgets;
    type Index = DynamicIndex;

    fn init_model(data: AppRowData, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { data }
    }

    fn init_root(&self) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        row
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: adw::ActionRow,
        _returned: &gtk::ListBoxRow,
        sender: FactorySender<Self>,
    ) -> AppRowWidgets {
        let name = self.data.group.display_name.clone();
        let icon = gtk::Image::new();
        icon.set_pixel_size(32);
        root.add_prefix(&icon);

        // Translators: accessible name of a level meter; {} is an app name
        let meter = PeakMeter::new(&i18n_f("Level of {}", &[&name]));
        root.add_suffix(meter.widget());
        self.data
            .hub
            .register(&meter, MeterTarget::App(self.data.group.key.clone()));

        let volume = VolumeControl::new(&name, {
            let (app, sender) = (self.data.group.key.clone(), sender.clone());
            move |level, muted| {
                let _ = sender.output(RowOutput::Edit(Edit::AppVolume {
                    app: app.clone(),
                    level,
                    muted,
                }));
            }
        });
        root.add_suffix(&volume.root);

        let model = gtk::StringList::new(&[]);
        let dropdown = gtk::DropDown::new(Some(model.clone()), None::<gtk::Expression>);
        dropdown.set_valign(gtk::Align::Center);
        dropdown.set_width_request(200);
        // Translators: accessible name of the output list; {} is an app name
        dropdown.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Output of {}",
            &[&name],
        ))]);
        let updating = Rc::new(Cell::new(false));
        let shown: Rc<RefCell<Vec<TargetChoice>>> = Rc::default();
        dropdown.connect_selected_notify({
            let (updating, shown, sender) = (updating.clone(), shown.clone(), sender.clone());
            move |d| {
                if updating.get() {
                    return;
                }
                let picked = usize::try_from(d.selected())
                    .ok()
                    .and_then(|i| shown.borrow().get(i).map(|c| c.target.clone()));
                if let Some(target) = picked {
                    sender.input(AppRowInput::TargetSelected(target));
                }
            }
        });
        root.add_suffix(&dropdown);

        let mut widgets = AppRowWidgets {
            row: root,
            icon,
            _meter: meter,
            volume,
            dropdown,
            model,
            labels: Vec::new(),
            updating,
            shown,
        };
        self.update_view(&mut widgets, sender);
        widgets
    }

    fn update(&mut self, msg: AppRowInput, sender: FactorySender<Self>) {
        match msg {
            AppRowInput::TargetSelected(target) => {
                if target == self.data.group.current_target {
                    return;
                }
                let _ = sender.output(RowOutput::Edit(Edit::AssignApp {
                    app: self.data.group.key.clone(),
                    display_name: self.data.group.display_name.clone(),
                    target,
                }));
            }
        }
    }

    fn update_view(&self, widgets: &mut AppRowWidgets, _sender: FactorySender<Self>) {
        let g = &self.data.group;
        widgets.row.set_title(&g.display_name);
        widgets.row.set_subtitle(&self.subtitle());
        widgets.icon.set_icon_name(Some(
            g.icon_name.as_deref().unwrap_or("audio-x-generic-symbolic"),
        ));
        widgets.volume.set_volume(Some(&g.volume));

        // Programmatic changes must not come back as TargetSelected (UI M5.3).
        widgets.updating.set(true);
        let labels: Vec<String> = self.data.choices.iter().map(|c| c.label.clone()).collect();
        if labels != widgets.labels {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let n = widgets.model.n_items();
            widgets.model.splice(0, n, &refs);
            widgets.labels = labels;
        }
        widgets.shown.borrow_mut().clone_from(&self.data.choices);
        if let Some(i) = self
            .data
            .choices
            .iter()
            .position(|c| c.target == g.current_target)
        {
            widgets.dropdown.set_selected(u32::try_from(i).unwrap_or(0));
        }
        widgets.updating.set(false);
        widgets.dropdown.set_sensitive(g.movable);
        let tip = (!g.movable).then(|| i18n("This app does not allow moving its audio"));
        widgets.dropdown.set_tooltip_text(tip.as_deref());
    }
}

impl KeyedRow for AppRow {
    type Key = AppKey;
    type Data = AppRowData;
    fn data_key(data: &AppRowData) -> AppKey {
        data.group.key.clone()
    }
    fn key(&self) -> AppKey {
        self.data.group.key.clone()
    }
    fn set_data(&mut self, data: AppRowData) {
        self.data = data;
    }
}

#[derive(Debug, Clone)]
pub struct OutputRowData {
    pub device: DeviceInfo,
    pub is_default: bool,
    pub hub: MeterHub,
}

#[derive(Debug)]
pub struct OutputRow {
    data: OutputRowData,
}

#[derive(Debug)]
pub struct OutputRowWidgets {
    row: adw::ActionRow,
    _meter: PeakMeter,
    volume: VolumeControl,
}

impl FactoryComponent for OutputRow {
    type Init = OutputRowData;
    type Input = ();
    type Output = RowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;
    type Root = adw::ActionRow;
    type Widgets = OutputRowWidgets;
    type Index = DynamicIndex;

    fn init_model(
        data: OutputRowData,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        Self { data }
    }

    fn init_root(&self) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        row
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: adw::ActionRow,
        _returned: &gtk::ListBoxRow,
        sender: FactorySender<Self>,
    ) -> OutputRowWidgets {
        let name = self.data.device.description.clone();
        root.add_prefix(&gtk::Image::from_icon_name("audio-speakers-symbolic"));
        // Translators: accessible name of a level meter; {} is a device name
        let meter = PeakMeter::new(&i18n_f("Level of {}", &[&name]));
        root.add_suffix(meter.widget());
        self.data
            .hub
            .register(&meter, MeterTarget::Device(self.data.device.key.clone()));
        let volume = VolumeControl::new(&name, {
            let (key, sender) = (self.data.device.key.clone(), sender.clone());
            move |level, muted| {
                let _ = sender.output(RowOutput::Edit(Edit::DeviceVolume {
                    key: key.clone(),
                    level,
                    muted,
                }));
            }
        });
        root.add_suffix(&volume.root);
        let mut widgets = OutputRowWidgets {
            row: root,
            _meter: meter,
            volume,
        };
        self.update_view(&mut widgets, sender);
        widgets
    }

    fn update_view(&self, widgets: &mut OutputRowWidgets, _sender: FactorySender<Self>) {
        let d = &self.data.device;
        widgets.row.set_title(&d.description);
        widgets.row.set_subtitle(&if self.data.is_default {
            i18n("Default output")
        } else {
            String::new()
        });
        widgets.volume.set_volume(d.volume.as_ref());
    }
}

impl KeyedRow for OutputRow {
    type Key = DeviceKey;
    type Data = OutputRowData;
    fn data_key(data: &OutputRowData) -> DeviceKey {
        data.device.key.clone()
    }
    fn key(&self) -> DeviceKey {
        self.data.device.key.clone()
    }
    fn set_data(&mut self, data: OutputRowData) {
        self.data = data;
    }
}

/// A rule for an app that isn't playing now (UI M5.6), so it can be seen and forgotten.
#[derive(Debug, Clone)]
pub struct RememberedRowData {
    pub app: AppKey,
    pub display_name: String,
    pub target_label: String,
}

#[derive(Debug)]
pub struct RememberedRow {
    data: RememberedRowData,
}

#[derive(Debug)]
pub struct RememberedRowWidgets {
    row: adw::ActionRow,
}

impl FactoryComponent for RememberedRow {
    type Init = RememberedRowData;
    type Input = ();
    type Output = RowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;
    type Root = adw::ActionRow;
    type Widgets = RememberedRowWidgets;
    type Index = DynamicIndex;

    fn init_model(
        data: RememberedRowData,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        Self { data }
    }

    fn init_root(&self) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        row
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: adw::ActionRow,
        _returned: &gtk::ListBoxRow,
        sender: FactorySender<Self>,
    ) -> RememberedRowWidgets {
        let forget = gtk::Button::from_icon_name("user-trash-symbolic");
        forget.add_css_class("flat");
        forget.set_valign(gtk::Align::Center);
        // Translators: button to delete a remembered app rule; {} is an app name
        let label = i18n_f("Forget {}", &[&self.data.display_name]);
        forget.set_tooltip_text(Some(&label));
        forget.update_property(&[gtk::accessible::Property::Label(&label)]);
        forget.connect_clicked({
            let (app, sender) = (self.data.app.clone(), sender.clone());
            move |_| {
                let _ = sender.output(RowOutput::Edit(Edit::ForgetAppRule { app: app.clone() }));
            }
        });
        root.add_suffix(&forget);
        let mut widgets = RememberedRowWidgets { row: root };
        self.update_view(&mut widgets, sender);
        widgets
    }

    fn update_view(&self, widgets: &mut RememberedRowWidgets, _sender: FactorySender<Self>) {
        widgets.row.set_title(&self.data.display_name);
        // Translators: {} is where a remembered app will play
        widgets
            .row
            .set_subtitle(&i18n_f("Plays on {}", &[&self.data.target_label]));
    }
}

impl KeyedRow for RememberedRow {
    type Key = AppKey;
    type Data = RememberedRowData;
    fn data_key(data: &RememberedRowData) -> AppKey {
        data.app.clone()
    }
    fn key(&self) -> AppKey {
        self.data.app.clone()
    }
    fn set_data(&mut self, data: RememberedRowData) {
        self.data = data;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::{ChannelId, ChannelState, Volume};

    fn device(key: &str, description: &str) -> DeviceInfo {
        DeviceInfo {
            key: DeviceKey(key.into()),
            description: description.into(),
            present: true,
            channels: 2,
            volume: None,
        }
    }

    fn snap() -> (GraphSnapshot, ChannelId) {
        let id = ChannelId::new_random();
        let snap = GraphSnapshot {
            outputs: vec![device("spk", "Speakers"), device("hp", "Headphones")],
            channels: vec![ChannelState {
                id,
                name: "Music".into(),
                present: true,
                volume: Volume::unity(2),
            }],
            default_output: Some(DeviceKey("spk".into())),
            ..Default::default()
        };
        (snap, id)
    }

    #[test]
    fn target_labels() {
        let (snap, id) = snap();
        assert_eq!(
            target_label(&RouteTarget::Default, &snap),
            "Default output (Speakers)"
        );
        assert_eq!(
            target_label(
                &RouteTarget::Device {
                    key: DeviceKey("hp".into())
                },
                &snap
            ),
            "Headphones"
        );
        assert_eq!(
            target_label(
                &RouteTarget::Device {
                    key: DeviceKey("gone".into())
                },
                &snap
            ),
            "gone",
            "unknown devices fall back to their key"
        );
        assert_eq!(target_label(&RouteTarget::Channel { id }, &snap), "Music");
        assert_eq!(
            target_label(&RouteTarget::Default, &GraphSnapshot::default()),
            "Default output"
        );
    }

    #[test]
    fn choices_order_and_disconnected_targets() {
        let (snap, id) = snap();
        let offline = RouteTarget::Device {
            key: DeviceKey("bt".into()),
        };
        let c = target_choices(&snap, &[&offline, &RouteTarget::Default]);
        let labels: Vec<&str> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Default output (Speakers)",
                "Channel: Music",
                "Speakers",
                "Headphones",
                "bt (disconnected)"
            ]
        );
        assert_eq!(c[1].target, RouteTarget::Channel { id });
    }
}
