//! Apps page (UI M3.6): what is playing now, and the outputs it can play on.

mod row;

use std::sync::Arc;

use adw::prelude::*;
use estuary_engine::GraphSnapshot;
use relm4::factory::FactoryVecDeque;
use relm4::{ComponentParts, ComponentSender, SimpleComponent, adw, gtk};

use self::row::{AppRow, AppRowData, OutputRow, OutputRowData, target_label};
use crate::i18n::i18n;
use crate::pages::reconcile::reconcile;
use crate::widgets::meter_hub::MeterHub;

#[derive(Debug)]
pub struct AppsPage {
    hub: MeterHub,
    apps: FactoryVecDeque<AppRow>,
    outputs: FactoryVecDeque<OutputRow>,
}

#[derive(Debug)]
pub enum AppsMsg {
    Update(Arc<GraphSnapshot>),
}

impl SimpleComponent for AppsPage {
    type Init = MeterHub;
    type Input = AppsMsg;
    type Output = ();
    type Root = gtk::ScrolledWindow;
    type Widgets = ();

    fn init_root() -> gtk::ScrolledWindow {
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller
    }

    fn init(
        hub: MeterHub,
        root: gtk::ScrolledWindow,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let apps_list = boxed_list();
        apps_list.set_placeholder(Some(&placeholder(
            &i18n("Nothing is playing"),
            &i18n("Start playing audio in any app and it will show up here."),
        )));
        let outputs_list = boxed_list();
        outputs_list.set_placeholder(Some(&placeholder(&i18n("No outputs found"), "")));

        let apps_group = adw::PreferencesGroup::new();
        apps_group.set_title(&i18n("Playing Now"));
        apps_group.add(&apps_list);
        let outputs_group = adw::PreferencesGroup::new();
        outputs_group.set_title(&i18n("Outputs"));
        outputs_group.add(&outputs_list);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&apps_group);
        content.append(&outputs_group);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(720);
        clamp.set_child(Some(&content));
        root.set_child(Some(&clamp));

        let model = AppsPage {
            hub,
            apps: FactoryVecDeque::builder().launch(apps_list).detach(),
            outputs: FactoryVecDeque::builder().launch(outputs_list).detach(),
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: AppsMsg, _sender: ComponentSender<Self>) {
        match msg {
            AppsMsg::Update(snap) => {
                let apps = snap
                    .apps
                    .iter()
                    .map(|g| AppRowData {
                        group: g.clone(),
                        target_label: target_label(&g.current_target, &snap),
                        hub: self.hub.clone(),
                    })
                    .collect();
                reconcile(&mut self.apps, apps);
                let outputs = snap
                    .outputs
                    .iter()
                    .filter(|d| d.present)
                    .map(|d| OutputRowData {
                        device: d.clone(),
                        is_default: snap.default_output.as_ref() == Some(&d.key),
                        hub: self.hub.clone(),
                    })
                    .collect();
                reconcile(&mut self.outputs, outputs);
            }
        }
    }
}

fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list
}

fn placeholder(title: &str, description: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.set_margin_top(18);
    b.set_margin_bottom(18);
    b.set_margin_start(12);
    b.set_margin_end(12);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("heading");
    b.append(&t);
    if !description.is_empty() {
        let d = gtk::Label::new(Some(description));
        d.add_css_class("dim-label");
        d.set_wrap(true);
        d.set_justify(gtk::Justification::Center);
        b.append(&d);
    }
    b
}
