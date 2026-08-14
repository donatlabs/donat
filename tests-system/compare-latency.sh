#!/usr/bin/env bash
# Compare two engine binaries fairly: both serve at the same time and the
# measurement alternates between them in short bursts, so machine drift hits
# both equally instead of whichever ran second.
#
#   tests-system/compare-latency.sh <binary-a> <binary-b> [label-a] [label-b]

set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
a="${1:?usage: compare-latency.sh <binary-a> <binary-b> [label-a] [label-b]}"
b="${2:?}"
label_a="${3:-A}"
label_b="${4:-B}"
metadata="$repo/examples/petshop/metadata"

export DONAT_GRAPHQL_DATABASE_URL="${PETSHOP_PG_URL:-postgresql://postgres:postgres@127.0.0.1:15434/petshop_system}"
for var in PETSHOP_PAYMENT PETSHOP_TAX PETSHOP_NOTIFICATION PETSHOP_PAYOUT; do
  export "${var}_BASE_URL=http://127.0.0.1:8099" "${var}_API_TOKEN=measure"
done
export DONAT_MOCK_CARRIER_BASE_URL=http://127.0.0.1:8099 DONAT_MOCK_CARRIER_TOKEN=measure

start_engine() {
  local binary="$1" port="$2"
  DONAT_PORT="$port" DONAT_METADATA_DIR="$metadata" \
  DONAT_GRAPHQL_UNAUTHORIZED_ROLE=anonymous \
    "$binary" --metadata-dir "$metadata" >/dev/null 2>&1 &
  echo $!
}

pid_a=$(start_engine "$a" 8186)
pid_b=$(start_engine "$b" 8187)
trap 'kill $pid_a $pid_b 2>/dev/null || true' EXIT

for port in 8186 8187; do
  until curl -fsS -m 2 -X POST "http://127.0.0.1:$port/v1/graphql" \
          -H 'content-type: application/json' -d '{"query":"{ __typename }"}' >/dev/null 2>&1; do
    sleep 0.2
  done
done

"$here/.venv/bin/python" - "$label_a" "$label_b" <<'PY'
import statistics, sys, time
import requests

label_a, label_b = sys.argv[1], sys.argv[2]
QUERY = {"query": "{ product(order_by: {id: asc}) { id slug title variants { id sku price_minor } } }"}
INTROSPECT = {"query": "query { __schema { types { kind name fields { name type { kind name } } } } }"}
engines = {label_a: ("http://127.0.0.1:8186/v1/graphql", requests.Session()),
           label_b: ("http://127.0.0.1:8187/v1/graphql", requests.Session())}

def burst(url, session, body, count):
    samples = []
    for _ in range(count):
        start = time.perf_counter()
        response = session.post(url, json=body, timeout=30)
        samples.append((time.perf_counter() - start) * 1000)
        assert response.status_code == 200, response.text[:200]
    return samples

for url, session in engines.values():          # warm both
    burst(url, session, QUERY, 100)
    burst(url, session, INTROSPECT, 5)

query = {label: [] for label in engines}
introspect = {label: [] for label in engines}
for round_index in range(20):                  # alternate, both directions
    order = list(engines) if round_index % 2 == 0 else list(engines)[::-1]
    for label in order:
        url, session = engines[label]
        query[label] += burst(url, session, QUERY, 50)
        introspect[label] += burst(url, session, INTROSPECT, 3)

for label in engines:
    q, i = sorted(query[label]), sorted(introspect[label])
    print(f"{label:12} query p50 {statistics.median(q):5.2f} ms  p95 {q[int(len(q)*0.95)]:5.2f} ms  "
          f"(n={len(q)})   introspect p50 {statistics.median(i):5.2f} ms")
PY
