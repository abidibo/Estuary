#!/bin/sh
# Installs data files under <prefix> (packaging M12.5): the single source of truth for the
# data layout. Usage: build-aux/install-data.sh <prefix> [destdir]
# With ESTUARY_PROFILE=devel the striped Devel icon is installed under the regular name (the
# app ID doesn't change, M12.6).
set -eu
PREFIX=${1:?prefix}; DEST=${2:-}
ID=io.github.abidibo.Estuary
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE=$(mktemp -d); trap 'rm -rf "$STAGE"' EXIT
"$ROOT/build-aux/compile-mo.sh" "$STAGE"

ICON="$ROOT/data/icons/hicolor/scalable/apps/$ID.svg"
[ "${ESTUARY_PROFILE:-default}" = devel ] && ICON="$ROOT/data/icons/hicolor/scalable/apps/$ID.Devel.svg"

install -Dm0644 "$STAGE/share/applications/$ID.desktop"  "$DEST$PREFIX/share/applications/$ID.desktop"
install -Dm0644 "$STAGE/share/metainfo/$ID.metainfo.xml" "$DEST$PREFIX/share/metainfo/$ID.metainfo.xml"
install -Dm0644 "$ICON" "$DEST$PREFIX/share/icons/hicolor/scalable/apps/$ID.svg"
install -Dm0644 "$ROOT/data/icons/hicolor/symbolic/apps/$ID-symbolic.svg" \
                "$DEST$PREFIX/share/icons/hicolor/symbolic/apps/$ID-symbolic.svg"
( cd "$STAGE/share/locale" && find . -name '*.mo' | while read -r mo; do
    install -Dm0644 "$mo" "$DEST$PREFIX/share/locale/$mo"
  done )
install -Dm0644 "$ROOT/LICENSE-MIT"    "$DEST$PREFIX/share/licenses/$ID/LICENSE-MIT"
install -Dm0644 "$ROOT/LICENSE-APACHE" "$DEST$PREFIX/share/licenses/$ID/LICENSE-APACHE"
