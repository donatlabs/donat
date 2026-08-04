# Local performance workloads

This harness is for bottleneck investigation, not an SLO or CI gate. It uses a
release server, persistent HTTP connections, response validation, warm-up, and
repeatable concurrency runs. Every run records throughput, latency percentiles
for all, successful, and failed attempts, errors, response bytes, server
CPU/RSS, inbound HTTP connection counts, and per-phase server trace summaries
under `benchmarks/perf/results/`. The socket count is deliberately labeled as
HTTP only; it is not a proxy for backend-pool occupancy.

Run the self-contained SQLite workload:

```bash
make perf BACKEND=sqlite
```

For PostgreSQL, MySQL, or ClickHouse, seed an equivalent source and provide its
metadata directory and URL:

```bash
make perf BACKEND=mysql \
  PERF_DATABASE_URL='mysql://...' \
  PERF_METADATA_DIR=/path/to/metadata
```

Compare the self-contained SQLite workload with configured external backends:

```bash
PERF_POSTGRES_DATABASE_URL='postgres://...' \
PERF_POSTGRES_METADATA_DIR=/path/to/postgres-metadata \
PERF_MYSQL_DATABASE_URL='mysql://...' \
PERF_MYSQL_METADATA_DIR=/path/to/mysql-metadata \
PERF_CLICKHOUSE_DATABASE_URL='http://...' \
PERF_CLICKHOUSE_METADATA_DIR=/path/to/clickhouse-metadata \
make perf-matrix
```

For a mixed-source comparison, provide metadata and a query that explicitly
name the participating sources:

```bash
PERF_MIXED_DATABASE_URL='postgres://...' \
PERF_MIXED_METADATA_DIR=/path/to/mixed-metadata \
PERF_MIXED_QUERY='{ authors { id } analytics { count } }' \
make perf-mixed
```

Defaults follow the investigation protocol: a 20-second warm-up, 60-second
runs at concurrency 1, 10, and 50, repeated three times. Short smoke runs can
override `PERF_WARMUP`, `PERF_DURATION`, `PERF_REPEATS`, and
`PERF_CONCURRENCY`. `PERF_QUERY` selects the GraphQL shape.
Set `PERF_RESULTS_DIR` to write artifacts outside the repository.

Each measurement run enables only `donat::perf` trace logs and embeds a summary
of parse, route, allowlist, planning, pool wait, SQL generation, execution,
and decoding phases in its JSON artifact. This is diagnostic evidence, not an
SLO or a CI pass/fail threshold.

Run the dependency-free harness regression tests with:

```bash
python3 -m unittest benchmarks/perf/test_load.py
```
