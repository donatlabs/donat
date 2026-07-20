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
results_dir="${PERF_RESULTS_DIR:-benchmarks/perf/results}"
temporary_dir=""
server_pid=""
server_log=""

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

server_log="${PERF_SERVER_LOG:-${temporary_dir:-/tmp}/donat-perf-server.log}"

if [[ "${PERF_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release -p donat-server --bin donat
fi

RUST_LOG="${RUST_LOG:+${RUST_LOG},}donat::perf=trace" \
PERF_DATABASE_URL="${database_url}" \
  DONAT_GRAPHQL_DATABASE_URL="${database_url}" \
  target/release/donat \
  --metadata-dir "${metadata_dir}" \
  --port "${port}" \
  >"${server_log}" 2>&1 &
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

mkdir -p "${results_dir}"
: >"${server_log}"
python3 benchmarks/perf/load.py \
  --url "http://127.0.0.1:${port}/v1/graphql" \
  --query "${query}" \
  --backend "${backend}" \
  --concurrency 1 \
  --duration "${warmup}" \
  --pid "${server_pid}" \
  --server-port "${port}" \
  --phase-log "${server_log}" \
  --output "${results_dir}/${backend}-warmup.json" >/dev/null

for concurrency in ${concurrencies}; do
  for repeat in $(seq 1 "${repeats}"); do
    output="${results_dir}/${backend}-c${concurrency}-r${repeat}.json"
    : >"${server_log}"
    python3 benchmarks/perf/load.py \
      --url "http://127.0.0.1:${port}/v1/graphql" \
      --query "${query}" \
      --backend "${backend}" \
      --concurrency "${concurrency}" \
      --duration "${duration}" \
      --pid "${server_pid}" \
      --server-port "${port}" \
      --phase-log "${server_log}" \
      --output "${output}"
  done
done
