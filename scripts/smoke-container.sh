#!/usr/bin/env bash
# Start the built image with a throwaway token and data dir, then check that
# the server answers its health check, serves the UI and widget, and enforces
# auth on the API. Requires the image to be loaded into Docker.
set -euo pipefail
image=${1:-ghcr.io/fruitiex/feedback-hub:main}
port=18431
token=smoke-token-$RANDOM$RANDOM$RANDOM
cid=$(docker run -d --rm --read-only --tmpfs /data:uid=10001 --tmpfs /tmp \
  -e FEEDBACK_HUB_ADMIN_TOKEN="$token" -p 127.0.0.1:$port:8080 "$image")
trap 'docker logs "$cid" 2>&1 | tail -20; docker stop "$cid" >/dev/null' EXIT
base=http://127.0.0.1:$port
for _ in $(seq 30); do curl -fsS "$base/healthz" >/dev/null 2>&1 && break; sleep 1; done
check() { # expected-status path [curl args...]
  local want=$1 path=$2; shift 2
  local got
  got=$(curl -s -o /dev/null -w '%{http_code}' "$@" "$base$path")
  [[ $got == "$want" ]] || { echo "GET $path: expected $want, got $got" >&2; exit 1; }
}
check 200 /healthz
check 200 /
check 200 /widget.js
check 200 /app.js
check 401 /api/feedback
check 200 /api/feedback -H "Authorization: Bearer $token"
echo "container smoke ok"
