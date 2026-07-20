#!/usr/bin/env bash
set -euo pipefail

backend="${BACKEND:-sqlite}"
duration="${PERF_DURATION:-60}"
warmup="${PERF_WARMUP:-20}"
repeats="${PERF_REPEATS:-3}"
concurrencies="${PERF_CONCURRENCY:-1 10 50}"
port="${PERF_PORT:-18080}"
query="${PERF_QUERY:-{ author(limit: 100) { id name score } }}"
metadata_dir="${PERF_METADATA_DIR:-}"
database_url="${PERF_DATABASE_URL:-}"
temporary_dir=""
server_pid=""

cleanup() {
  if [[ -n "${server_pid}" ]]; then
    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true
  fi
  if [[ -n "${temporary_dir}" ]]; then
    rm -rf -- "${temporary_dir}"
  fi
}
trap cleanup EXIT INT TERM

if [[ "${backend}" == "sqlite" && -z "${database_url}" ]]; then
  temporary_dir="$(mktemp -d)"
  database_url="${temporary_dir}/perf.sqlite"
  python3 - "${database_url}" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
connection.execute("CREATE TABLE author(id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL)")
connection.executemany(
    "INSERT INTO author(id, name, score) VALUES (?, ?, ?)",
    ((index, f"author-{index}", index / 10.0) for index in range(1, 100_001)),
)
connection.commit()
PY
  metadata_dir="benchmarks/perf/metadata"
fi

if [[ -z "${database_url}" || -z "${metadata_dir}" ]]; then
  echo "BACKEND=${backend} requires PERF_DATABASE_URL and PERF_METADATA_DIR with a seeded source" >&2
  exit 2
fi

if [[ "${PERF_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release -p donat-server --bin donat
fi

PERF_DATABASE_URL="${database_url}" \
  target/release/donat \
  --database-url "${database_url}" \
  --metadata-dir "${metadata_dir}" \
  --port "${port}" \
  >"${temporary_dir:-/tmp}/donat-perf-server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 100); do
  if python3 - "${port}" <<'PY'
import sys
import urllib.request
urllib.request.urlopen(f"http://127.0.0.1:{sys.argv[1]}/healthz", timeout=0.2).read()
PY
  then
    break
  fi
  sleep 0.1
done
kill -0 "${server_pid}"

mkdir -p benchmarks/perf/results
python3 benchmarks/perf/load.py \
  --url "http://127.0.0.1:${port}/v1/graphql" \
  --query "${query}" \
  --backend "${backend}" \
  --concurrency 1 \
  --duration "${warmup}" \
  --pid "${server_pid}" \
  --server-port "${port}" \
  --output "benchmarks/perf/results/${backend}-warmup.json" >/dev/null

for concurrency in ${concurrencies}; do
  for repeat in $(seq 1 "${repeats}"); do
    output="benchmarks/perf/results/${backend}-c${concurrency}-r${repeat}.json"
    python3 benchmarks/perf/load.py \
      --url "http://127.0.0.1:${port}/v1/graphql" \
      --query "${query}" \
      --backend "${backend}" \
      --concurrency "${concurrency}" \
      --duration "${duration}" \
      --pid "${server_pid}" \
      --server-port "${port}" \
      --output "${output}"
  done
done
