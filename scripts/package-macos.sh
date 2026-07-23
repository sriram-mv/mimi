#!/usr/bin/env bash
# Build mimi and package it as Mimi.app.
#
# Usage: scripts/package-macos.sh [--universal]
#
# Without --universal, builds only for the host arch. With it, builds both
# aarch64-apple-darwin and x86_64-apple-darwin and lipo's them together.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "package-macos.sh must run on macOS (need lipo, codesign, the Metal backend)." >&2
    exit 1
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

UNIVERSAL=0
[[ "${1:-}" == "--universal" ]] && UNIVERSAL=1

APP="dist/Mimi.app"
MACOS_DIR="$APP/Contents/MacOS"
RESOURCES_DIR="$APP/Contents/Resources"
SHARE_DIR="$RESOURCES_DIR/share"

rm -rf "$APP"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR" "$SHARE_DIR/fish/vendor_conf.d"

build_target() {
    local target="$1"
    rustup target add "$target" >/dev/null 2>&1 || true
    cargo build --release --target "$target" -p mimi-app
}

if [[ "$UNIVERSAL" == "1" ]]; then
    build_target aarch64-apple-darwin
    build_target x86_64-apple-darwin
    lipo -create -output "$MACOS_DIR/mimi" \
        target/aarch64-apple-darwin/release/mimi \
        target/x86_64-apple-darwin/release/mimi
else
    cargo build --release -p mimi-app
    cp target/release/mimi "$MACOS_DIR/mimi"
fi

cp shell/vendor_conf.d/mimi.fish "$SHARE_DIR/fish/vendor_conf.d/mimi.fish"
cp Info.plist "$APP/Contents/Info.plist"

# Ad-hoc sign so Gatekeeper allows a local launch. Distributing outside your
# own machine needs a Developer ID signature + notarization; this script
# does not attempt that.
codesign --force --deep --sign - "$APP"

echo "Built $APP"
echo "Run it:      open $APP"
echo "Install it:  cp -R $APP /Applications/"
