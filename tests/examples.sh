#!/bin/bash
# Run the integration examples against the real service on a private bus, with
# a stand-in Share Sheet that records the request it was given. Nothing opens
# on screen and nothing reaches the real clipboard or network.
#
#   cargo build -p nyx-share-daemon && dbus-run-session -- tests/examples.sh
set -euo pipefail
[[ -n ${DBUS_SESSION_BUS_ADDRESS:-} ]] || { echo "run under dbus-run-session" >&2; exit 2; }
here=$(cd "$(dirname "$0")/.." && pwd)
daemon=$here/target/debug/nyx-share-daemon
[[ -x $daemon ]] || { echo "build nyx-share-daemon first" >&2; exit 2; }
tmp=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$tmp"' EXIT

mkdir -p "$tmp/config/nyx-share"
printf 'visibility = "off"\n' > "$tmp/config/nyx-share/nearby.toml"
printf '#!/bin/sh\nenv | grep "^NYX_SHARE_" > %s/sheet-env\n' "$tmp" > "$tmp/fake-sheet"
chmod +x "$tmp/fake-sheet"
env -u WAYLAND_DISPLAY -u INVOCATION_ID XDG_CONFIG_HOME="$tmp/config" XDG_STATE_HOME="$tmp/state" XDG_DATA_HOME="$tmp/data" \
    XDG_CACHE_HOME="$tmp/cache" NYX_SHARE_SHEET="$tmp/fake-sheet" FLUX_SOCKET="$tmp/no-flux.sock" NYX_SHARE_SECRETS_DIR="$tmp/secrets" \
    "$daemon" > "$tmp/daemon.log" 2>&1 &
for _ in $(seq 100); do
    busctl --user status io.github.design_nexus.nyx.Share1 >/dev/null 2>&1 && break
    sleep 0.1
done

fail() { echo "FAIL: $*" >&2; exit 1; }
# The request the stand-in sheet was started for, as the sheet would read it.
request() {
    for _ in $(seq 50); do [[ -s $tmp/sheet-env ]] && break; sleep 0.1; done
    # shellcheck disable=SC1091
    . <(sed 's/^/export /' "$tmp/sheet-env")
    rm -f "$tmp/sheet-env"
    busctl --user --json=short call io.github.design_nexus.nyx.Share1 /io/github/design_nexus/nyx/Share1 \
        io.github.design_nexus.nyx.ShareSheet1 GetRequest ss "$NYX_SHARE_REQUEST" "$NYX_SHARE_TOKEN" |
        python3 -c 'import json,sys; r = json.loads(json.load(sys.stdin)["data"][0]); print(" | ".join(i["name"] for i in r["items"]))'
}

printf 'one\n' > "$tmp/plain.txt"
printf 'two\n' > "$tmp/odd \"name\".txt"

"$here/examples/integration/share-sheet.sh" "$tmp/plain.txt" "$tmp/odd \"name\".txt" > /dev/null
got=$(request)
[[ $got == 'plain.txt | odd "name".txt' ]] || fail "share-sheet.sh: $got"
echo "ok: share-sheet.sh opened the sheet for: $got"

if command -v cc >/dev/null && pkg-config --exists gio-unix-2.0 json-glib-1.0; then
    cc "$here/examples/integration/share.c" -o "$tmp/share" $(pkg-config --cflags --libs gio-unix-2.0 json-glib-1.0)
    "$tmp/share" "$tmp/plain.txt" > /dev/null
    got=$(request)
    [[ $got == 'plain.txt' ]] || fail "share.c: $got"
    echo "ok: share.c opened the sheet for: $got"
    if "$tmp/share" /dev/null 2> "$tmp/err"; then fail "share.c accepted a device"; fi
    grep -q "Couldn't share" "$tmp/err" || fail "share.c: $(cat "$tmp/err")"
    echo "ok: share.c reports the service's refusal: $(cat "$tmp/err")"
else
    echo "skip: share.c (needs cc, gio-unix-2.0 and json-glib-1.0)"
fi

"$here/examples/integration/send-and-wait.sh" | grep -q '^qr ' || fail "send-and-wait.sh didn't list the QR code"
out=$("$here/examples/integration/send-and-wait.sh" qr "Hello")
[[ $out == "completed: QR code for the text" ]] || fail "send-and-wait.sh: $out"
echo "ok: send-and-wait.sh: $out"
echo "All examples work."
