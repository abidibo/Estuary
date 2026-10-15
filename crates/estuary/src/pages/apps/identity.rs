//! An app's name and icon as the user knows them. Electron apps often report themselves as
//! "Chromium" with a `chromium-browser` icon that isn't installed; their binary is honest, so
//! the installed desktop entry for it gives the real name and icon.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use estuary_engine::AppGroup;
use relm4::gtk;
use relm4::gtk::prelude::*;

/// Fallback when nothing better is installed: a generic app icon, never a broken image.
const GENERIC_ICON: &str = "application-x-executable-symbolic";

/// A desktop entry's name and icon, or `None` when no entry matched (cached too).
type Found = Option<(String, Option<String>)>;

thread_local! {
    static CACHE: RefCell<HashMap<String, Found>> = RefCell::default();
}

/// Rewrites `group`'s display name and icon when its own metadata doesn't hold up.
pub fn resolve(group: &mut AppGroup) {
    let has_icon = |name: &str| {
        gtk::gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(name))
    };
    let reported_ok = group.icon_name.as_deref().is_some_and(has_icon);
    let generic_name = group.display_name == "Chromium" || group.display_name == "Electron";
    if reported_ok && !generic_name {
        return;
    }
    let binary = group.key.0.strip_prefix("bin:");
    if let Some((name, icon)) = binary.and_then(desktop_entry) {
        group.display_name = name;
        if let Some(icon) = icon.filter(|i| has_icon(i)) {
            group.icon_name = Some(icon);
            return;
        }
    }
    if !reported_ok {
        group.icon_name = Some(GENERIC_ICON.to_owned());
    }
}

/// The installed desktop entry whose id or executable matches `binary` (cached).
fn desktop_entry(binary: &str) -> Option<(String, Option<String>)> {
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(binary.to_owned())
            .or_insert_with(|| lookup(binary))
            .clone()
    })
}

fn lookup(binary: &str) -> Option<(String, Option<String>)> {
    gio_unix::DesktopAppInfo::search(binary)
        .into_iter()
        .flatten()
        .filter_map(|id| gio_unix::DesktopAppInfo::new(&id).map(|a| (id, a)))
        .find(|(id, app)| {
            let stem = id.trim_end_matches(".desktop");
            let exe = app.executable();
            let exe_name = Path::new(&exe).file_name().and_then(|n| n.to_str());
            stem == binary || exe_name == Some(binary)
        })
        .map(|(_, app)| {
            let icon = app
                .icon()
                .and_then(|i| i.to_string())
                .map(|s| s.to_string());
            (app.name().to_string(), icon)
        })
}
