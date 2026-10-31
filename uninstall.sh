#!/usr/bin/env bash
# Usage: uninstall.sh [--purge]   --purge also removes the destination arrangement.
set -euo pipefail
share="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
if command -v systemctl >/dev/null; then systemctl --user stop nyx-share.service 2>/dev/null || true; fi
rm -f "$HOME/.local/bin/nyx-share" "$HOME/.local/bin/nyx-share-daemon" "$HOME/.local/bin/nyx-share-sheet"
rm -f "$share/dbus-1/services/io.github.design_nexus.nyx.Share1.service"
rm -f "$config/systemd/user/nyx-share.service"
rm -f "$share/applications/io.github.design_nexus.nyx.Share.desktop"
rm -f "$share/icons/hicolor/scalable/apps/io.github.design_nexus.nyx.Share.svg"
if command -v update-desktop-database >/dev/null; then update-desktop-database "$share/applications"; fi
if command -v systemctl >/dev/null; then systemctl --user daemon-reload 2>/dev/null || true; fi
if [[ ${1:-} == --purge ]]; then rm -rf "$config/nyx-share"; fi
printf 'Uninstalled Share.\n'
