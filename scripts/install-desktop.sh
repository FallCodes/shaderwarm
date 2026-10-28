#!/usr/bin/env bash
# Installs the GUI's menu entry and icon for the current user (the package does this
# system-wide). Run again after moving the binaries; remove with --uninstall.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
APPS=$DATA/applications
ICONS=$DATA/icons/hicolor

if [[ ${1:-} == --uninstall ]]; then
    rm -f "$APPS/shaderwarm.desktop" "$ICONS/scalable/apps/shaderwarm.svg" "$ICONS/256x256/apps/shaderwarm.png"
else
    install -Dm644 "$ROOT/packaging/shaderwarm.desktop" "$APPS/shaderwarm.desktop"
    install -Dm644 "$ROOT/assets/shaderwarm.svg" "$ICONS/scalable/apps/shaderwarm.svg"
    install -Dm644 "$ROOT/assets/shaderwarm-256.png" "$ICONS/256x256/apps/shaderwarm.png"
fi
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$ICONS" || true
echo "${1:---install} done"
