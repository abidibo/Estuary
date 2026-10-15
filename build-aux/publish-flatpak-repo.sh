#!/bin/sh
# Turn a flatpak-builder repository into the signed static Flatpak repository served from
# GitHub Pages (https://abidibo.github.io/Estuary/), plus the .flatpakrepo/.flatpakref files
# users open to add it. The site is rebuilt from scratch on every release: Flatpak only needs
# each new commit to be newer than the installed one.
#
#   build-aux/publish-flatpak-repo.sh <builder-repo> <site-dir> <gpg-key-id> [gpg-homedir]
set -eu
cd "$(dirname "$0")/.."
SRC=${1:?builder repo}
SITE=${2:?site dir}
KEY=${3:?gpg key id}
GPG_HOME=${4:-}
ID=io.github.abidibo.Estuary
URL=https://abidibo.github.io/Estuary
HOMEPAGE=https://github.com/abidibo/Estuary

sign() { flatpak "$@" --gpg-sign="$KEY" ${GPG_HOME:+--gpg-homedir="$GPG_HOME"}; }

rm -rf "$SITE"
mkdir -p "$SITE"
cp -a "$SRC" "$SITE/repo"
# Debug symbols stay out of the public repository.
find "$SITE/repo/refs/heads/runtime" -path "*/$ID.Debug/*" -type f -delete
sign build-sign "$SITE/repo" "$ID"
sign build-sign --runtime "$SITE/repo" "$ID.Locale"
sign build-update-repo --title=Estuary --default-branch=master --generate-static-deltas --prune \
    "$SITE/repo"

GPGKEY=$(base64 -w0 build-aux/estuary-flatpak-repo.gpg)
cp "data/icons/hicolor/scalable/apps/$ID.svg" "$SITE/icon.svg"
cat > "$SITE/estuary.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Estuary
Url=$URL/repo/
Homepage=$HOMEPAGE
Comment=Route, mix and clean up your audio
Icon=$URL/icon.svg
GPGKey=$GPGKEY
EOF
cat > "$SITE/estuary.flatpakref" <<EOF
[Flatpak Ref]
Name=$ID
Branch=master
Title=Estuary
Url=$URL/repo/
SuggestRemoteName=estuary
Homepage=$HOMEPAGE
Icon=$URL/icon.svg
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
GPGKey=$GPGKEY
EOF
cat > "$SITE/index.html" <<EOF
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Estuary Flatpak repository</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 42rem; margin: 3rem auto; padding: 0 1rem;
         color: #1e1e1e; background: #fafafa; }
  @media (prefers-color-scheme: dark) { body { color: #eee; background: #242424; } a { color: #78aeed; } }
  pre { background: rgba(127,127,127,.15); padding: .75rem 1rem; border-radius: 8px; overflow-x: auto; }
  img { vertical-align: middle; }
</style>
<h1><img src="icon.svg" width="48" height="48" alt=""> Estuary</h1>
<p>Route, mix and clean up your audio on Linux. This is the Flatpak repository of
<a href="$HOMEPAGE">Estuary</a>.</p>
<p><a href="estuary.flatpakref"><strong>Install Estuary</strong></a>: opens in GNOME Software,
Pop!_Shop or KDE Discover. Updates arrive with your other apps.</p>
<p>Or from a terminal:</p>
<pre>flatpak remote-add --user --if-not-exists estuary $URL/estuary.flatpakrepo
flatpak install --user estuary $ID</pre>
<p>The GNOME runtime is installed from Flathub. See the <a href="$HOMEPAGE#readme">README</a> for
requirements and a quick start.</p>
</html>
EOF
touch "$SITE/.nojekyll"
