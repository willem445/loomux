#!/bin/sh
# Orrerix installer for macOS and Linux.
#   curl -fsSL https://raw.githubusercontent.com/willem445/loomux/main/install.sh | sh
#
# The repo slug stays as-is until the GitHub rename, which is a separate
# human step; GitHub redirects both raw.githubusercontent.com and the REST
# API afterwards, so this script keeps working on either side of it.
set -eu

REPO="willem445/loomux"
API="https://api.github.com/repos/$REPO/releases/latest"

say() { printf '\033[1;34morrerix\033[0m %s\n' "$1"; }
die() { printf '\033[1;31morrerix\033[0m %s\n' "$1" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"

os=$(uname -s)
arch=$(uname -m)
assets=$(curl -fsSL "$API" | grep -o '"browser_download_url": *"[^"]*"' | cut -d'"' -f4) \
  || die "could not query latest release"

pick() {
  echo "$assets" | grep -i "$1" | head -n 1
}

case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) url=$(pick '_aarch64\.dmg$') ;;
      *)             url=$(pick '_x64\.dmg$') ;;
    esac
    [ -n "$url" ] || die "no macOS build found in the latest release"
    tmp=$(mktemp -d)
    say "downloading $(basename "$url")"
    curl -fSL --progress-bar "$url" -o "$tmp/app.dmg"
    say "installing to /Applications"
    mount=$(hdiutil attach -nobrowse -readonly "$tmp/app.dmg" | awk -F'\t' 'END{print $NF}')
    # Install whatever bundle the image carries rather than a hardcoded name:
    # it is Orrerix.app from the first post-rename release on and Loomux.app on
    # every release before that, and this script always resolves /latest, so it
    # has to handle both. The destination takes the same name, so an install
    # replaces its own predecessor and never touches a bundle beside it.
    #
    # Current name first, then the pre-rename one, then anything else — the same
    # order npm/bin/orrerix.js uses, and for the same reason: if an image ever
    # carried two bundles, the current product is the one to install. A bare
    # `ls | head -1` answers that alphabetically, which is a different rule
    # dressed as the same one. Unreachable today (the bundler puts one .app in a
    # DMG) and cheap to keep aligned.
    app=
    for candidate in Orrerix.app Loomux.app; do
      if [ -d "$mount/$candidate" ]; then app="$mount/$candidate"; break; fi
    done
    if [ -z "$app" ]; then app=$(ls -d "$mount"/*.app 2>/dev/null | head -n 1); fi
    [ -n "$app" ] || die "no application bundle inside the disk image"
    dest="/Applications/$(basename "$app")"
    rm -rf "$dest"
    cp -R "$app" /Applications/
    hdiutil detach "$mount" -quiet
    rm -rf "$tmp"
    # The build is unsigned; clear the quarantine flag so Gatekeeper
    # doesn't report the app as damaged.
    xattr -cr "$dest" 2>/dev/null || true
    say "installed: $dest"
    ;;
  Linux)
    case "$arch" in
      x86_64) url=$(pick '_amd64\.AppImage$') ;;
      aarch64) url=$(pick '_aarch64\.AppImage$') ;;
      *) die "unsupported architecture: $arch" ;;
    esac
    [ -n "$url" ] || die "no Linux build found for $arch in the latest release"
    bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
    mkdir -p "$bin"
    say "downloading $(basename "$url")"
    curl -fSL --progress-bar "$url" -o "$bin/orrerix"
    chmod +x "$bin/orrerix"
    say "installed: $bin/orrerix"
    # A pre-rename install left a `loomux` binary in the same bin dir. It is
    # the user's file in the user's directory, so it is not ours to delete —
    # say so instead. Written as an `if`, not an `&&` list: under `set -e` a
    # trailing AND-OR list whose test fails takes the whole script down.
    if [ -e "$bin/loomux" ]; then
      say "note: the older $bin/loomux is still there; remove it when you are ready"
    fi
    case ":$PATH:" in
      *":$bin:"*) ;;
      *) say "note: add $bin to your PATH" ;;
    esac
    ;;
  *)
    die "unsupported OS: $os (on Windows, use install.ps1)"
    ;;
esac

say "done — run Orrerix from your app launcher (or 'orrerix' on Linux)"
