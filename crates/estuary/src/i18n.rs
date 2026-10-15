//! Translation helpers (UI M3.2).
//!
//! Conventions for every user-visible string:
//! - wrap every literal: `i18n("Music")`;
//! - never `format!` a translated template; use `i18n_f` / `ni18n_f` with `{}` placeholders,
//!   which are filled after translation so translators can reorder words around them;
//! - put a `// Translators:` comment on the line before any string whose meaning or
//!   placeholders aren't obvious out of context.

use crate::config_build::{GETTEXT_PACKAGE, LOCALEDIR};

pub fn init() {
    // No explicit setlocale: it is `unsafe` in gettext-rs 0.8 (not thread-safe) and this crate
    // forbids unsafe code. GTK's own initialization calls setlocale(LC_ALL, "") before any UI
    // string is translated, which is all gettext needs.
    // Failures are non-fatal: the UI falls back to English source strings.
    let _ = gettextrs::bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR);
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(GETTEXT_PACKAGE);
}

pub fn i18n(msgid: &str) -> String {
    gettextrs::gettext(msgid)
}

/// Translates `msgid` in a disambiguating context (`pgettext`).
pub fn pi18n(context: &str, msgid: &str) -> String {
    gettextrs::pgettext(context, msgid)
}

/// Translates `msgid`, then replaces each `{}` in order with `args` (translators keep the `{}`).
pub fn i18n_f(msgid: &str, args: &[&str]) -> String {
    fill(&gettextrs::gettext(msgid), args)
}

/// Plural form of [`i18n_f`] (`ngettext` + placeholders).
pub fn ni18n_f(single: &str, plural: &str, n: u32, args: &[&str]) -> String {
    fill(&gettextrs::ngettext(single, plural, n), args)
}

fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut args = args.iter();
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        out.push_str(args.next().copied().unwrap_or("{}"));
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders_in_order() {
        assert_eq!(
            fill("{} is using “{}”.", &["Discord", "Virtual mic"]),
            "Discord is using “Virtual mic”."
        );
        assert_eq!(fill("no placeholders", &["x"]), "no placeholders");
        assert_eq!(
            fill("{} and {}", &["one"]),
            "one and {}",
            "missing args keep the placeholder"
        );
    }

    #[test]
    fn untranslated_strings_pass_through() {
        assert_eq!(i18n_f("Found {}", &["0.4.17"]), "Found 0.4.17");
        assert_eq!(ni18n_f("{} stream", "{} streams", 1, &["1"]), "1 stream");
        assert_eq!(ni18n_f("{} stream", "{} streams", 3, &["3"]), "3 streams");
    }
}
