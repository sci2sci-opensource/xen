#!/bin/sh
# xen installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sci2sci-opensource/xen/master/install.sh | sh
#
# Detects OS+arch, downloads the matching static binary from the
# corresponding GitHub Release, and drops it in $XEN_INSTALL_DIR
# (defaults to ~/.local/bin). No prerequisites beyond curl, tar/unzip,
# and a POSIX shell.
#
# Override the version with: XEN_VERSION=v0.1.0 sh install.sh

set -eu

REPO="${XEN_REPO:-sci2sci-opensource/xen}"
INSTALL_DIR="${XEN_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${XEN_VERSION:-latest}"

err() { printf 'xen-install: %s\n' "$*" >&2; exit 1; }
say() { printf 'xen-install: %s\n' "$*"; }

uname_s=$(uname -s 2>/dev/null || echo unknown)
uname_m=$(uname -m 2>/dev/null || echo unknown)

case "$uname_s" in
  Linux)  os=unknown-linux-gnu ;;
  Darwin) os=apple-darwin ;;
  *)      err "unsupported OS: $uname_s" ;;
esac

case "$uname_m" in
  x86_64|amd64)        arch=x86_64 ;;
  arm64|aarch64)       arch=aarch64 ;;
  *)                   err "unsupported arch: $uname_m" ;;
esac

target="${arch}-${os}"

if [ "$VERSION" = "latest" ]; then
  # Resolve the latest release tag without depending on jq.
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
  [ -n "$VERSION" ] || err "could not resolve latest version"
fi

archive="xen-${VERSION}-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${VERSION}/${archive}"

say "downloading $url"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$url" -o "$tmp/$archive" || err "download failed"
tar -xzf "$tmp/$archive" -C "$tmp"

mkdir -p "$INSTALL_DIR"
mv "$tmp/xen-${VERSION}-${target}/xen" "$INSTALL_DIR/xen"
chmod +x "$INSTALL_DIR/xen"

say "installed xen ${VERSION} to $INSTALL_DIR/xen"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH" ;;
esac
