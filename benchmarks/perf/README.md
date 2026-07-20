# Local performance workloads

This harness is for bottleneck investigation, not an SLO or CI gate. It uses a
release server, persistent HTTP connections, response validation, warm-up, and
repeatable concurrency runs. Every run records throughput, latency percentiles
for all, successful, and failed attempts, errors, response bytes, server
CPU/RSS, and observed connection counts under `benchmarks/perf/results/`.

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

Defaults follow the investigation protocol: a 20-second warm-up, 60-second
runs at concurrency 1, 10, and 50, repeated three times. Short smoke runs can
override `PERF_WARMUP`, `PERF_DURATION`, `PERF_REPEATS`, and
`PERF_CONCURRENCY`. `PERF_QUERY` selects the GraphQL shape.

Run the dependency-free harness regression tests with:

```bash
python3 -m unittest benchmarks/perf/test_load.py
```
