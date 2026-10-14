//! Rows of the Apps page (UI M3.6). Read-only in M3: meters arrive in M4, controls in M5.

use adw::prelude::*;
use estuary_engine::{AppGroup, AppKey, DeviceInfo, DeviceKey, GraphSnapshot, RouteTarget};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::{adw, gtk};

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::pages::reconcile::KeyedRow;
use crate::state::volume;

/// Human-readable name of where an app currently plays.
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

#[derive(Debug, Clone)]
pub struct AppRowData {
    pub group: AppGroup,
    pub target_label: String,
}

#[derive(Debug)]
pub struct AppRow {
    data: AppRowData,
}

#[derive(Debug)]
pub struct AppRowWidgets {
    row: adw::ActionRow,
    icon: gtk::Image,
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
        // Translators: shown under an app's name; {} is where it plays, e.g. "Headphones"
        parts.push(i18n_f("Playing on {}", &[&self.data.target_label]));
        parts.join(" · ")
    }
}

impl FactoryComponent for AppRow {
    type Init = AppRowData;
    type Input = ();
    type Output = ();
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
        let icon = gtk::Image::new();
        icon.set_pixel_size(32);
        root.add_prefix(&icon);
        let mut widgets = AppRowWidgets { row: root, icon };
        self.update_view(&mut widgets, sender);
        widgets
    }

    fn update_view(&self, widgets: &mut AppRowWidgets, _sender: FactorySender<Self>) {
        let g = &self.data.group;
        widgets.row.set_title(&g.display_name);
        widgets.row.set_subtitle(&self.subtitle());
        widgets.icon.set_icon_name(Some(
            g.icon_name.as_deref().unwrap_or("audio-x-generic-symbolic"),
        ));
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
}

#[derive(Debug)]
pub struct OutputRow {
    data: OutputRowData,
}

#[derive(Debug)]
pub struct OutputRowWidgets {
    row: adw::ActionRow,
    volume: gtk::Label,
}

impl FactoryComponent for OutputRow {
    type Init = OutputRowData;
    type Input = ();
    type Output = ();
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
        let icon = gtk::Image::from_icon_name("audio-speakers-symbolic");
        root.add_prefix(&icon);
        let volume = gtk::Label::new(None);
        volume.add_css_class("dim-label");
        volume.add_css_class("numeric");
        root.add_suffix(&volume);
        let mut widgets = OutputRowWidgets { row: root, volume };
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
        widgets.volume.set_label(&match &d.volume {
            Some(v) if v.muted => i18n("Muted"),
            // Translators: a volume percentage, e.g. "73 %"
            Some(v) => i18n_f("{} %", &[&volume::percent(v.average()).to_string()]),
            None => String::new(),
        });
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

    #[test]
    fn target_labels() {
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
        let no_default = GraphSnapshot::default();
        assert_eq!(
            target_label(&RouteTarget::Default, &no_default),
            "Default output"
        );
    }
}
