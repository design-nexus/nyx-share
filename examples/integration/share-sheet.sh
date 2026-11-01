#!/bin/bash
# Open the Nyx Share Sheet for files, from a script, with nothing but busctl.
# Each file is opened read-only here and passed as a descriptor, so the service
# never needs your paths (and a sandboxed app could do the same).
#
#   share-sheet.sh FILE...
set -euo pipefail
[[ $# -gt 0 ]] || { echo "usage: $0 FILE..." >&2; exit 2; }

json_string() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    printf '"%s"' "$(printf '%s' "$s" | tr -d '\000-\037')"
}

items=() fds=() i=0
for file in "$@"; do
    [[ -f $file ]] || { echo "$file isn't a file" >&2; exit 1; }
    exec {fd}<"$file"
    items+=("{\"kind\":\"file\",\"fd\":$i,\"name\":$(json_string "$(basename -- "$file")")}")
    fds+=("$fd")
    i=$((i + 1))
done
json="[$(IFS=,; echo "${items[*]}")]"

# Share(s items, ah fds, s options) → s request id
busctl --user call io.github.design_nexus.nyx.Share1 /io/github/design_nexus/nyx/Share1 \
    io.github.design_nexus.nyx.Share1 Share sahs "$json" "${#fds[@]}" "${fds[@]}" '{"app_name":"My script"}'
