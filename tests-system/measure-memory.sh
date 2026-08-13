#!/usr/bin/env bash
# Measure the engine's resident memory for one metadata directory.
#
#   tests-system/measure-memory.sh [metadata-dir] [label]
#
# Boots the release binary against the running system-test database, waits
# until it actually serves, lets it settle, then reports RSS split into heap
# (anonymous) and file-backed pages. Run against a stand raised by stack.sh.

set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
metadata="${1:-$repo/examples/petshop/metadata}"
label="${2:-petshop}"
port="${MEASURE_PORT:-8181}"

export DONAT_GRAPHQL_DATABASE_URL="${PETSHOP_PG_URL:-postgresql://postgres:postgres@127.0.0.1:15434/petshop_system}"
for var in PETSHOP_PAYMENT PETSHOP_TAX PETSHOP_NOTIFICATION PETSHOP_PAYOUT; do
  export "${var}_BASE_URL=http://127.0.0.1:8099"
  export "${var}_API_TOKEN=measure"
done
export DONAT_MOCK_CARRIER_BASE_URL=http://127.0.0.1:8099 DONAT_MOCK_CARRIER_TOKEN=measure

log="$(mktemp)"
DONAT_PORT="$port" DONAT_METADATA_DIR="$metadata" \
DONAT_GRAPHQL_UNAUTHORIZED_ROLE=anonymous \
  "$repo/target/release/donat" --metadata-dir "$metadata" >"$log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT

for _ in $(seq 1 90); do
  if curl -fsS -m 2 -X POST "http://127.0.0.1:$port/v1/graphql" \
       -H 'content-type: application/json' -d '{"query":"{ __typename }"}' >/dev/null 2>&1; then
    break
  fi
  kill -0 $pid 2>/dev/null || { echo "engine exited before serving:"; tail -20 "$log"; exit 1; }
  sleep 1
done
curl -fsS -m 5 -X POST "http://127.0.0.1:$port/v1/graphql" \
  -H 'content-type: application/json' -d '{"query":"{ __typename }"}' >/dev/null || {
    echo "engine never served; log:"; tail -20 "$log"; exit 1; }

sleep 3
rss=$(awk '/VmRSS/ {print $2}' /proc/$pid/status)
hwm=$(awk '/VmHWM/ {print $2}' /proc/$pid/status)
anon=$(awk '/^Anonymous/ {print $2}' /proc/$pid/smaps_rollup)
printf '%-28s RSS %6.1f MB  peak %6.1f MB  heap %6.1f MB\n' \
  "$label" "$(echo "$rss/1024" | bc -l)" "$(echo "$hwm/1024" | bc -l)" "$(echo "$anon/1024" | bc -l)"
