//! Apps page (UI M3.6, M5): what is playing now and where, the outputs, and remembered rules.

mod controls;
pub(crate) mod identity;
mod row;

use std::sync::Arc;

use adw::prelude::*;
use estuary_engine::{GraphSnapshot, Setup};
use relm4::factory::FactoryVecDeque;
use relm4::{ComponentParts, ComponentSender, SimpleComponent, adw, gtk};

use self::row::{
    AppRow, AppRowData, OutputRow, OutputRowData, RememberedRow, RememberedRowData, RowOutput,
    target_choices, target_label,
};
use crate::i18n::i18n;
use crate::pages::reconcile::reconcile;
use crate::state::edit::Edit;
use crate::widgets::meter_hub::MeterHub;

#[derive(Debug)]
pub struct AppsPage {
    hub: MeterHub,
    apps: FactoryVecDeque<AppRow>,
    outputs: FactoryVecDeque<OutputRow>,
    remembered: FactoryVecDeque<RememberedRow>,
    remembered_group: adw::PreferencesGroup,
}

#[derive(Debug)]
pub enum AppsMsg {
    Update {
        snapshot: Arc<GraphSnapshot>,
        setup: Arc<Setup>,
    },
    RowEdit(Edit),
}

#[derive(Debug)]
pub enum AppsOutput {
    Edit(Edit),
}

impl SimpleComponent for AppsPage {
    type Init = MeterHub;
    type Input = AppsMsg;
    type Output = AppsOutput;
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
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let apps_list = boxed_list();
        apps_list.set_placeholder(Some(&placeholder(
            &i18n("Nothing is playing"),
            &i18n("Start playing audio in any app and it will show up here."),
        )));
        let outputs_list = boxed_list();
        outputs_list.set_placeholder(Some(&placeholder(&i18n("No outputs found"), "")));
        let remembered_list = boxed_list();

        let group = |title: &str, list: &gtk::ListBox| {
            let g = adw::PreferencesGroup::new();
            g.set_title(title);
            g.add(list);
            g
        };
        let apps_group = group(&i18n("Playing Now"), &apps_list);
        let outputs_group = group(&i18n("Outputs"), &outputs_list);
        let remembered_group = group(&i18n("Remembered Apps"), &remembered_list);
        remembered_group.set_description(Some(&i18n(
            "Apps that will play on their chosen output next time they start.",
        )));
        remembered_group.set_visible(false);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.append(&apps_group);
        content.append(&outputs_group);
        content.append(&remembered_group);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(900);
        clamp.set_child(Some(&content));
        root.set_child(Some(&clamp));

        let forward = |o: RowOutput| match o {
            RowOutput::Edit(e) => AppsMsg::RowEdit(e),
        };
        let model = AppsPage {
            hub,
            apps: FactoryVecDeque::builder()
                .launch(apps_list)
                .forward(sender.input_sender(), forward),
            outputs: FactoryVecDeque::builder()
                .launch(outputs_list)
                .forward(sender.input_sender(), forward),
            remembered: FactoryVecDeque::builder()
                .launch(remembered_list)
                .forward(sender.input_sender(), forward),
            remembered_group,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: AppsMsg, sender: ComponentSender<Self>) {
        match msg {
            AppsMsg::RowEdit(e) => {
                let _ = sender.output(AppsOutput::Edit(e));
            }
            AppsMsg::Update {
                snapshot: snap,
                setup,
            } => {
                let apps = snap
                    .apps
                    .iter()
                    .map(|g| {
                        let rule_target = setup
                            .app_rules
                            .iter()
                            .find(|r| r.app == g.key)
                            .map(|r| &r.target);
                        let extra: Vec<_> = std::iter::once(&g.current_target)
                            .chain(rule_target)
                            .collect();
                        let mut group = g.clone();
                        identity::resolve(&mut group);
                        AppRowData {
                            group,
                            choices: target_choices(&snap, &extra),
                            hub: self.hub.clone(),
                        }
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
                let remembered: Vec<RememberedRowData> = setup
                    .app_rules
                    .iter()
                    .filter(|r| !snap.apps.iter().any(|g| g.key == r.app))
                    .map(|r| RememberedRowData {
                        app: r.app.clone(),
                        display_name: if r.display_name.is_empty() {
                            r.app.0.clone()
                        } else {
                            r.display_name.clone()
                        },
                        target_label: target_label(&r.target, &snap),
                    })
                    .collect();
                self.remembered_group.set_visible(!remembered.is_empty());
                reconcile(&mut self.remembered, remembered);
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
