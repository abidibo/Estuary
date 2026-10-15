//! First-run welcome (UI M11.1): the recommended setup or an empty one.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::{adw, gtk};

use crate::config_build::APP_ID;
use crate::i18n::i18n;
use crate::state::preset::PresetKind;

/// Closing it (✕ or Escape, which AdwDialog doesn't tell apart) counts as "Start empty".
pub fn present(parent: &impl IsA<gtk::Widget>, on_choice: impl Fn(PresetKind) + 'static) {
    let dialog = adw::Dialog::new();
    dialog.set_content_width(480);
    dialog.set_can_close(false);
    let chosen = Rc::new(Cell::new(false));
    let on_choice = Rc::new(on_choice);
    let choose = {
        let (dialog, chosen, on_choice) = (dialog.clone(), chosen.clone(), on_choice.clone());
        move |kind| {
            if !chosen.replace(true) {
                on_choice(kind);
            }
            dialog.force_close();
        }
    };
    dialog.connect_close_attempt({
        let choose = choose.clone();
        move |_| choose(PresetKind::Empty)
    });

    let status = adw::StatusPage::new();
    let icon_theme = gtk::IconTheme::for_display(&parent.as_ref().display());
    status.set_icon_name(Some(if icon_theme.has_icon(APP_ID) {
        APP_ID
    } else {
        "view-grid-symbolic"
    }));
    status.set_title(&i18n("Welcome to Estuary"));
    status.set_description(Some(&i18n("How would you like to start?")));
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    for (kind, title, subtitle) in [
        (
            PresetKind::Recommended,
            i18n("Recommended setup"),
            i18n("Music, Games and Chat channels plus a virtual microphone with noise suppression"),
        ),
        (
            PresetKind::Empty,
            i18n("Start empty"),
            i18n("Only your real devices; build your own mix"),
        ),
    ] {
        let row = adw::ActionRow::new();
        row.set_title(&title);
        row.set_subtitle(&subtitle);
        row.set_activatable(true);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let choose = choose.clone();
        row.connect_activated(move |_| choose(kind));
        list.append(&row);
    }
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(420);
    clamp.set_child(Some(&list));
    status.set_child(Some(&clamp));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}
