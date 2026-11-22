#!/usr/bin/env bash
# Probes a relay image: runs it with an extra relay flag and checks that it
# answers the health probe and serves the pairing page. The `relay-image` hook
# and `.github/workflows/publish-relay-image.yml` both call it.
#
#   bash container/relay-probe.sh <image>
set -euo pipefail

image=$1
container=$(podman run --detach --rm --publish 127.0.0.1::8787 "$image" \
  --client-address-header X-Real-IP)
trap 'podman stop --time 2 "$container" >/dev/null' EXIT
address=$(podman port "$container" 8787/tcp)

for _ in 1 2 3 4 5 6 7 8 9 10; do
  if curl --fail --silent --output /dev/null "http://$address/healthz"; then break; fi
  sleep 1
done

test "$(curl --fail --silent "http://$address/healthz")" = ok
page=$(curl --fail --silent "http://$address/")
[[ "$page" == *'name="agentty-transport" content="relay"'* ]]
