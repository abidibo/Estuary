//! Preferences (UI M11.2): what closing the window does, and start at login.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use estuary_engine::setup::CloseAction;
use relm4::{adw, gtk};

use crate::desktop::autostart::AutostartOutcome;
use crate::i18n::i18n;
use crate::state::surfaces::{autostart_row, autostart_tooltip};

/// An open Preferences dialog; `update` re-renders it without emitting edits.
#[derive(Debug, Clone)]
pub struct PreferencesHandle {
    dialog: adw::PreferencesDialog,
    close: adw::ComboRow,
    login: adw::SwitchRow,
    updating: Rc<Cell<bool>>,
}

pub fn present(
    parent: &impl IsA<gtk::Widget>,
    on_close_action: impl Fn(CloseAction) + 'static,
    on_start_at_login: impl Fn(bool) + 'static,
) -> PreferencesHandle {
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();
    page.set_title(&i18n("General"));
    page.set_icon_name(Some("preferences-system-symbolic"));
    let group = adw::PreferencesGroup::new();
    group.set_title(&i18n("Background"));
    let close = adw::ComboRow::new();
    close.set_title(&i18n("When closing the window"));
    close.set_model(Some(&gtk::StringList::new(&[
        &i18n("Keep running in the background"),
        &i18n("Quit"),
    ])));
    let login = adw::SwitchRow::new();
    login.set_title(&i18n("Start at login"));
    group.add(&close);
    group.add(&login);
    page.add(&group);
    dialog.add(&page);

    let updating = Rc::new(Cell::new(false));
    close.connect_selected_notify({
        let updating = updating.clone();
        move |row| {
            if !updating.get() {
                on_close_action(if row.selected() == 1 {
                    CloseAction::Quit
                } else {
                    CloseAction::Hide
                });
            }
        }
    });
    login.connect_active_notify({
        let updating = updating.clone();
        move |row| {
            if !updating.get() {
                on_start_at_login(row.is_active());
            }
        }
    });
    dialog.present(Some(parent));
    PreferencesHandle {
        dialog,
        close,
        login,
        updating,
    }
}

impl PreferencesHandle {
    /// The switch shows the intent; the subtitle explains the effective state.
    pub fn update(
        &self,
        close_action: CloseAction,
        start_at_login: bool,
        autostart: Option<&Result<AutostartOutcome, String>>,
    ) {
        self.updating.set(true);
        self.close
            .set_selected(u32::from(close_action == CloseAction::Quit));
        self.login.set_active(start_at_login);
        self.updating.set(false);
        let spec = autostart_row(start_at_login, autostart);
        self.login.set_sensitive(spec.sensitive);
        self.login
            .set_subtitle(spec.subtitle.as_deref().unwrap_or_default());
        self.login
            .set_tooltip_text(autostart_tooltip(autostart).as_deref());
    }

    pub fn is_open(&self) -> bool {
        self.dialog.root().is_some()
    }
}
