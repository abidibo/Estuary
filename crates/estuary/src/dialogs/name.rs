//! Add or rename a channel or virtual mic (UI M9.6).

use adw::prelude::*;
use estuary_engine::{ChannelId, VirtualMicId};
use relm4::{adw, gtk};

use crate::i18n::i18n;
use crate::state::edit::{Edit, MAX_NAME_CHARS, default_name};

#[derive(Debug, Clone)]
pub enum NameDialogKind {
    AddChannel,
    AddMic,
    RenameChannel(ChannelId, String),
    RenameMic(VirtualMicId, String),
}

/// `taken`: names already in use, to prefill the first free "Channel N" / "Virtual mic N".
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    kind: NameDialogKind,
    taken: &[&str],
    on_done: impl Fn(Edit) + 'static,
) {
    let (heading, action, text) = match &kind {
        NameDialogKind::AddChannel => (
            i18n("New Channel"),
            i18n("Add"),
            default_name(&i18n("Channel"), taken),
        ),
        NameDialogKind::AddMic => (
            i18n("New Virtual Mic"),
            i18n("Add"),
            default_name(&i18n("Virtual mic"), taken),
        ),
        NameDialogKind::RenameChannel(_, name) => {
            (i18n("Rename Channel"), i18n("Rename"), name.clone())
        }
        NameDialogKind::RenameMic(_, name) => {
            (i18n("Rename Virtual Mic"), i18n("Rename"), name.clone())
        }
    };
    let dialog = adw::AlertDialog::new(Some(&heading), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", &action);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");

    let entry = adw::EntryRow::new();
    entry.set_title(&i18n("Name"));
    entry.set_text(&text);
    entry.set_activates_default(true);
    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    dialog.set_extra_child(Some(&group));

    let valid = |t: &str| {
        let t = t.trim();
        !t.is_empty() && t.chars().count() <= MAX_NAME_CHARS
    };
    dialog.set_response_enabled("ok", valid(&text));
    entry.connect_changed({
        let dialog = dialog.clone();
        move |e| dialog.set_response_enabled("ok", valid(&e.text()))
    });
    dialog.connect_response(None, move |_, response| {
        if response != "ok" {
            return;
        }
        let name = entry.text().trim().to_owned();
        on_done(match &kind {
            NameDialogKind::AddChannel => Edit::AddChannel { name },
            NameDialogKind::AddMic => Edit::AddMic { name },
            NameDialogKind::RenameChannel(id, _) => Edit::RenameChannel { id: *id, name },
            NameDialogKind::RenameMic(id, _) => Edit::RenameMic { id: *id, name },
        });
    });
    dialog.present(Some(parent));
}
