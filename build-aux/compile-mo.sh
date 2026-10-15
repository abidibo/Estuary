#!/bin/sh
# Compile translations into <stagedir> (packaging M12.4):
#   share/locale/<lang>/LC_MESSAGES/estuary.mo, plus the translated desktop file and MetaInfo.
# `build-aux/compile-mo.sh target` produces target/locale for `LANGUAGE=it cargo run`.
set -eu
cd "$(dirname "$0")/.."
STAGE=${1:?stagedir}
ID=io.github.abidibo.Estuary
for l in $(cat po/LINGUAS); do
    install -d "$STAGE/share/locale/$l/LC_MESSAGES"
    msgfmt -c --check-format -o "$STAGE/share/locale/$l/LC_MESSAGES/estuary.mo" "po/$l.po"
done
# `cargo run` reads target/locale (config_build::LOCALEDIR fallback).
if [ "$(basename "$STAGE")" = "target" ]; then
    rm -rf "$STAGE/locale"
    cp -r "$STAGE/share/locale" "$STAGE/locale"
fi
install -d "$STAGE/share/applications" "$STAGE/share/metainfo"
msgfmt --desktop --template="data/$ID.desktop.in" -d po -o "$STAGE/share/applications/$ID.desktop"
msgfmt --xml --template="data/$ID.metainfo.xml.in" -d po -o "$STAGE/share/metainfo/$ID.metainfo.xml"
