#!/usr/bin/env bash
set -euo pipefail

# Run the same local workload for every configured backend. Backend-specific
# variables avoid accidentally using a PostgreSQL URL with MySQL metadata:
# PERF_POSTGRES_DATABASE_URL, PERF_POSTGRES_METADATA_DIR, and so on.
backends="${PERF_MATRIX_BACKENDS:-postgres sqlite mysql clickhouse}"
built=0

for backend in ${backends}; do
  upper="$(printf '%s' "${backend}" | tr '[:lower:]' '[:upper:]')"
  database_variable="PERF_${upper}_DATABASE_URL"
  metadata_variable="PERF_${upper}_METADATA_DIR"
  database_url="${!database_variable:-${PERF_DATABASE_URL:-}}"
  metadata_dir="${!metadata_variable:-${PERF_METADATA_DIR:-}}"

  if [[ "${backend}" != "sqlite" && ( -z "${database_url}" || -z "${metadata_dir}" ) ]]; then
    echo "${backend} needs ${database_variable} and ${metadata_variable}" >&2
    exit 2
  fi

  BACKEND="${backend}" \
    PERF_DATABASE_URL="${database_url}" \
    PERF_METADATA_DIR="${metadata_dir}" \
    PERF_SKIP_BUILD="${built}" \
    benchmarks/perf/run.sh
  built=1
done
