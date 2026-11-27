#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.local/bin"
ICON_DIR="$HOME/.local/share/icons/hicolor/512x512/apps"
APP_DIR="$HOME/.local/share/applications"

mkdir -p "$BIN_DIR" "$ICON_DIR" "$APP_DIR"

cp "$SCRIPT_DIR/reelget-gui" "$BIN_DIR/reelget-gui"
cp "$SCRIPT_DIR/reelget" "$BIN_DIR/reelget"
chmod +x "$BIN_DIR/reelget-gui" "$BIN_DIR/reelget"

cp "$SCRIPT_DIR/com.reelget.app.png" "$ICON_DIR/com.reelget.app.png"

sed "s|^Exec=.*|Exec=$BIN_DIR/reelget-gui|" "$SCRIPT_DIR/com.reelget.app.desktop" > "$APP_DIR/com.reelget.app.desktop"
chmod +x "$APP_DIR/com.reelget.app.desktop"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

echo "Installed ReelGet GUI, CLI, and Do icon to ~/.local/"
