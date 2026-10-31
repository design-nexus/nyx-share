#!/usr/bin/env bash
set -euo pipefail
# Piped from curl (`curl -fsSL …/install.sh | bash`): no checkout here, so fetch
# the source and run its installer from there.
if [[ -z ${BASH_SOURCE[0]:-} ]]; then
  command -v git >/dev/null || { echo "Git is needed to install nyx-share." >&2; exit 1; }
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  git clone -q --depth 1 https://github.com/design-nexus/nyx-share.git "$work/src"
  bash "$work/src/install.sh" "$@"
  exit
fi
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# A release archive (.prebuilt) brings its binaries.
if [[ ${NYX_SKIP_BUILD:-0} != 1 && ! -f $root/.prebuilt ]]; then
  CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --release --locked --manifest-path "$root/Cargo.toml" \
    -p nyx-share -p nyx-share-daemon -p nyx-share-sheet
fi
share="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
target="${CARGO_TARGET_DIR:-$root/target}/release"
for bin in nyx-share nyx-share-daemon nyx-share-sheet; do
  [[ -f $target/$bin ]] || { echo "Release binary missing: $target/$bin" >&2; exit 1; }
  # Replace by rename so a running service keeps its old binary until it exits.
  install -Dm755 "$target/$bin" "$HOME/.local/bin/$bin.new"
  mv -f "$HOME/.local/bin/$bin.new" "$HOME/.local/bin/$bin"
done
install -Dm644 "$root/data/dbus/io.github.design_nexus.nyx.Share1.service" "$share/dbus-1/services/io.github.design_nexus.nyx.Share1.service"
install -Dm644 "$root/data/systemd/nyx-share.service" "$config/systemd/user/nyx-share.service"
install -Dm644 "$root/data/desktop/io.github.design_nexus.nyx.Share.desktop" "$share/applications/io.github.design_nexus.nyx.Share.desktop"
install -Dm644 "$root/data/icons/io.github.design_nexus.nyx.Share.svg" "$share/icons/hicolor/scalable/apps/io.github.design_nexus.nyx.Share.svg"
if command -v update-desktop-database >/dev/null; then update-desktop-database "$share/applications"; fi
if command -v systemctl >/dev/null; then systemctl --user daemon-reload 2>/dev/null || true; fi

# Nyx Files: Share… and Send to → Share. Files reads providers when it starts.
providers="$config/nyx-files/providers/nyx-share"
install -d -m755 "$providers"
install -m644 "$root/data/files-provider/share.png" "$providers/share.png"
home_json=${HOME//\\/\\\\}
home_json=${home_json//\"/\\\"}
cat > "$providers/provider.json.new" <<EOF
{
  "version": 1,
  "id": "nyx-share",
  "name": "Share",
  "command": ["$home_json/.local/bin/nyx-share", "files-provider"],
  "icon": "share",
  "icons": {"share": "share.png"},
  "category": "other"
}
EOF
chmod 644 "$providers/provider.json.new"
mv -f "$providers/provider.json.new" "$providers/provider.json"

# The service starts when an app shares and exits after a minute idle, so a
# running copy picks up this version once its transfers finish.
printf 'Installed Share without opening it. Try: nyx-share --help\n'
