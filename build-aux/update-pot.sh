#!/bin/sh
# Regenerate po/estuary.pot and merge it into every language (packaging M12.4).
# Uses the GNOME SDK's xgettext when the SDK is installed (Rust parser, gettext >= 0.24),
# otherwise the host's, falling back to the C parser below 0.24 (Rust literals and calls are
# close enough to C for gettext calls).
set -eu
cd "$(dirname "$0")/.."
SDK="org.gnome.Sdk//51"
if flatpak info "$SDK" >/dev/null 2>&1; then
    run() { flatpak run --filesystem="$PWD" --cwd="$PWD" --command=sh "$SDK" -c "$*"; }
else
    run() { sh -c "$*"; }
fi
VER=$(run 'xgettext --version | head -n1 | sed "s/.* //"')
case "$VER" in
    0.2[4-9]*|0.[3-9]*|[1-9]*) LANG_RS="--language=Rust" ;;
    *) echo "xgettext $VER < 0.24: using the C parser" >&2; LANG_RS="--language=C" ;;
esac
# Our wrappers (crates/estuary/src/i18n.rs) plus the raw gettext-rs names.
KW="--keyword=i18n --keyword=i18n_f --keyword=ni18n_f:1,2 --keyword=pi18n:1c,2 \
    --keyword=gettext --keyword=pgettext:1c,2 --keyword=ngettext:1,2 --keyword=npgettext:1c,2,3"
TMP=$(mktemp -d "$PWD/po/.pot-XXXXXX"); trap 'rm -rf "$TMP"' EXIT
grep -E '\.rs$' po/POTFILES.in > "$TMP/rs.list"
run "xgettext $LANG_RS $KW --from-code=UTF-8 --add-comments=Translators \
      --package-name=estuary --msgid-bugs-address=https://github.com/abidibo/Estuary/issues \
      -f $TMP/rs.list -o $TMP/rust.pot"
run "xgettext --language=Desktop -k -kName -kGenericName -kComment -kKeywords \
      --add-comments=Translators data/io.github.abidibo.Estuary.desktop.in -o $TMP/desktop.pot"
ITS=/usr/share/gettext/its/metainfo.its
[ -f "$ITS" ] || ITS=$(ls /usr/share/gettext*/its/appdata.its 2>/dev/null | head -n1)
run "xgettext --its=$ITS data/io.github.abidibo.Estuary.metainfo.xml.in -o $TMP/metainfo.pot"
run "msgcat --use-first $TMP/rust.pot $TMP/desktop.pot $TMP/metainfo.pot -o po/estuary.pot"
for l in $(cat po/LINGUAS); do
    if [ -f "po/$l.po" ]; then
        run "msgmerge --quiet --update --backup=none po/$l.po po/estuary.pot"
    fi
done
echo "po/estuary.pot: $(grep -c '^msgid ' po/estuary.pot) messages"
