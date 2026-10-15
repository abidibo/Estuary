#!/bin/sh
# Regenerates build-aux/cargo-sources.json from Cargo.lock (packaging M12.7), with
# flatpak-builder-tools' cargo generator in a cached venv. Run after every Cargo.lock change and
# commit both together.
set -eu
cd "$(dirname "$0")/.."
TOOLS=${FLATPAK_BUILDER_TOOLS:-"$HOME/.cache/flatpak-builder-tools"}
[ -d "$TOOLS" ] || git clone --depth 1 https://github.com/flatpak/flatpak-builder-tools "$TOOLS"
[ -x "$TOOLS/.venv/bin/python" ] || python3 -m venv "$TOOLS/.venv"
"$TOOLS/.venv/bin/pip" install --quiet aiohttp tomlkit
"$TOOLS/.venv/bin/python" "$TOOLS/cargo/flatpak-cargo-generator.py" Cargo.lock -o build-aux/cargo-sources.json
