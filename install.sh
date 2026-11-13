#!/usr/bin/env bash
# Builds EchoDock and installs it for the current user on Omarchy (Hyprland):
#   - echodock and echodock-settings into ~/.local/bin
#   - the EchoDock Settings launcher entry
#   - ~/.config/hypr/echodock.lua (autostart, layer rule, shortcuts), loaded from hyprland.lua
#   - Omarchy's own dock turned off in ~/.config/omarchy/shell.json
# Files it changes are backed up next to themselves with a .bak.echodock suffix.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"

cargo build --release

install -Dm755 target/release/echodock target/release/echodock-settings -t "$HOME/.local/bin"
install -Dm644 assets/echodock-settings.desktop -t "$HOME/.local/share/applications"
install -Dm644 assets/echodock.lua "$HOME/.config/hypr/echodock.lua"

hypr="$HOME/.config/hypr/hyprland.lua"
if [[ -f $hypr ]] && ! grep -q 'require("hypr.echodock")' "$hypr"; then
  cp "$hypr" "$hypr.bak.echodock"
  printf '\nrequire("hypr.echodock") -- EchoDock: dock, shortcuts, autostart\n' >> "$hypr"
fi

shell="$HOME/.config/omarchy/shell.json"
if [[ -f $shell ]] && command -v jq >/dev/null; then
  cp "$shell" "$shell.bak.echodock"
  jq '.disabledPlugins = ((.disabledPlugins // []) + ["omarchy.dock"] | unique)' "$shell" > "$shell.tmp" && mv "$shell.tmp" "$shell"
fi

hyprctl reload >/dev/null 2>&1 || true
echodock ctl quit >/dev/null 2>&1 || true
sleep 0.3
(command -v uwsm-app >/dev/null && uwsm-app -- echodock || setsid echodock) >/dev/null 2>&1 &
echo "EchoDock installed. Settings: echodock-settings · Shortcuts: Ctrl+Alt+1…9, Super+Alt+T, Super+Alt+D, Super+M"
