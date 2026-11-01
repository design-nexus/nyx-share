#!/bin/bash
# Send a text straight to a destination, without the Share Sheet, and wait for
# the job to finish. Lists the destinations when none is given.
#
#   send-and-wait.sh                      # what can take a text?
#   send-and-wait.sh DESTINATION "TEXT"   # e.g. clipboard "Hello"
set -euo pipefail
call() {
    busctl --user --json=short call io.github.design_nexus.nyx.Share1 /io/github/design_nexus/nyx/Share1 \
        io.github.design_nexus.nyx.Share1 "$@"
}
# Structured values travel as JSON strings: unwrap busctl's reply with python3.
first() { python3 -c 'import json,sys; print(json.load(sys.stdin)["data"][0])'; }

if [[ $# -eq 0 ]]; then
    # ListDestinations(as kinds, u count) → s JSON array
    call ListDestinations asu 1 text 1 | first | python3 -c '
import json, sys
for d in json.load(sys.stdin):
    if d["available"]:
        print("%-40s %s" % (d["id"], d["name"]))'
    exit 0
fi

destination=$1 text=$2
items=$(python3 -c 'import json,sys; print(json.dumps([{"kind": "text", "text": sys.argv[1]}]))' "$text")
# ShareTo(s destination, s items, ah fds, s options) → s job id
job=$(call ShareTo ssahs "$destination" "$items" 0 '{"notify":false}' | first)
while :; do
    # GetJob(s id) → s JSON job
    state=$(call GetJob s "$job" | first)
    case $(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["state"])' "$state") in
        completed | failed | cancelled)
            python3 -c 'import json,sys; j = json.loads(sys.argv[1]); print(j["state"] + ":", j.get("message", ""))' "$state"
            exit 0 ;;
    esac
    sleep 0.2
done
