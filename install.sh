#!/bin/sh
# Loomux installer for macOS and Linux.
#   curl -fsSL https://raw.githubusercontent.com/willem445/loomux/main/install.sh | sh
set -eu

REPO="willem445/loomux"
API="https://api.github.com/repos/$REPO/releases/latest"

say() { printf '\033[1;34mloomux\033[0m %s\n' "$1"; }
die() { printf '\033[1;31mloomux\033[0m %s\n' "$1" >&2; exit 1; }

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
    curl -fSL --progress-bar "$url" -o "$tmp/loomux.dmg"
    say "installing to /Applications"
    mount=$(hdiutil attach -nobrowse -readonly "$tmp/loomux.dmg" | awk -F'\t' 'END{print $NF}')
    rm -rf /Applications/Loomux.app
    cp -R "$mount"/Loomux.app /Applications/
    hdiutil detach "$mount" -quiet
    rm -rf "$tmp"
    # The build is unsigned; clear the quarantine flag so Gatekeeper
    # doesn't report the app as damaged.
    xattr -cr /Applications/Loomux.app 2>/dev/null || true
    say "installed: /Applications/Loomux.app"
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
    curl -fSL --progress-bar "$url" -o "$bin/loomux"
    chmod +x "$bin/loomux"
    say "installed: $bin/loomux"
    case ":$PATH:" in
      *":$bin:"*) ;;
      *) say "note: add $bin to your PATH" ;;
    esac
    ;;
  *)
    die "unsupported OS: $os (on Windows, use install.ps1)"
    ;;
esac

say "done — run Loomux from your app launcher (or 'loomux' on Linux)"
