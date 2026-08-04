#!/usr/bin/env bash
# Measure serving latency for one engine binary, so a memory change can be
# shown not to have cost speed.
#
#   tests-system/measure-latency.sh <binary> <label>
#
# Boots the binary against the system-test database, waits until it serves,
# then reports boot time and the latency of a representative permission-filtered
# read, sequentially and under concurrency. Run against a stand raised by
# stack.sh (it reuses that database).

set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
binary="${1:-$repo/target/release/donat}"
label="${2:-current}"
metadata="$repo/examples/petshop/metadata"
port="${LATENCY_PORT:-8184}"

export DONAT_GRAPHQL_DATABASE_URL="${PETSHOP_PG_URL:-postgresql://postgres:postgres@127.0.0.1:15434/petshop_system}"
for var in PETSHOP_PAYMENT PETSHOP_TAX PETSHOP_NOTIFICATION PETSHOP_PAYOUT; do
  export "${var}_BASE_URL=http://127.0.0.1:8099" "${var}_API_TOKEN=measure"
done
export DONAT_MOCK_CARRIER_BASE_URL=http://127.0.0.1:8099 DONAT_MOCK_CARRIER_TOKEN=measure

log="$(mktemp)"
start=$(date +%s.%N)
DONAT_PORT="$port" DONAT_METADATA_DIR="$metadata" \
DONAT_GRAPHQL_ADMIN_SECRET=measure DONAT_GRAPHQL_UNAUTHORIZED_ROLE=anonymous \
DONAT_GRAPHQL_JWT_SECRET="{\"type\":\"HS256\",\"key\":\"${PETSHOP_JWT_KEY:-petshop-dev-jwt-key-change-me-32bytes+}\"}" \
  "$binary" --metadata-dir "$metadata" >"$log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT

until curl -fsS -m 2 -X POST "http://127.0.0.1:$port/v1/graphql" \
        -H 'content-type: application/json' -d '{"query":"{ __typename }"}' >/dev/null 2>&1; do
  kill -0 $pid 2>/dev/null || { echo "engine exited before serving:"; tail -20 "$log"; exit 1; }
  sleep 0.2
done
boot=$(echo "$(date +%s.%N) - $start" | bc)

rss=$(awk '/VmRSS/ {print $2}' /proc/$pid/status)
"$here/.venv/bin/python" - "$port" "$label" "$boot" "$rss" <<'PY'
import json, statistics, sys, time
from concurrent.futures import ThreadPoolExecutor
import requests

port, label, boot, rss = sys.argv[1], sys.argv[2], float(sys.argv[3]), int(sys.argv[4])
url = f"http://127.0.0.1:{port}/v1/graphql"
# A permission-filtered read with a relationship — the shape a storefront serves.
QUERY = {"query": "{ product(order_by: {id: asc}) { id slug title variants { id sku price_minor } } }"}
session = requests.Session()

def once():
    start = time.perf_counter()
    response = session.post(url, json=QUERY, timeout=15)
    elapsed = (time.perf_counter() - start) * 1000
    assert response.status_code == 200 and "errors" not in response.json(), response.text
    return elapsed

for _ in range(50):          # warm the pool and the plan cache
    once()

serial = sorted(once() for _ in range(300))
p50 = statistics.median(serial)
p95 = serial[int(len(serial) * 0.95)]

# Introspection is measured separately: it is the one path that materialises a
# whole schema document, so a change to how documents are stored shows up here
# and nowhere else.
INTROSPECTION = {"query": """query { __schema { queryType { name }
 types { kind name description fields { name description type { kind name ofType { kind name } } }
 inputFields { name } enumValues { name } } } }"""}

def introspect():
    start = time.perf_counter()
    response = session.post(url, json=INTROSPECTION, timeout=30)
    elapsed = (time.perf_counter() - start) * 1000
    assert response.status_code == 200 and "errors" not in response.json(), response.text[:300]
    return elapsed

for _ in range(5):
    introspect()
intro = sorted(introspect() for _ in range(40))
intro_p50 = statistics.median(intro)

# Throughput is measured across processes: with threads the client's own GIL
# becomes the bottleneck long before the engine does, and the number then says
# more about Python than about the change under test.
def burst(count):
    local = requests.Session()
    start = time.perf_counter()
    for _ in range(count):
        response = local.post(url, json=QUERY, timeout=15)
        assert response.status_code == 200
    return time.perf_counter() - start

if __name__ == "__main__":
    from multiprocessing import Pool

    requests_per_worker, workers = 500, 6
    start = time.perf_counter()
    with Pool(workers) as pool:
        pool.map(burst, [requests_per_worker] * workers)
    rps = (requests_per_worker * workers) / (time.perf_counter() - start)

    print(f"{label:12} boot {boot:5.2f}s  RSS {rss/1024:6.1f} MB  "
          f"p50 {p50:5.2f} ms  p95 {p95:5.2f} ms  {rps:6.0f} rps  introspect {intro_p50:6.2f} ms")
PY
