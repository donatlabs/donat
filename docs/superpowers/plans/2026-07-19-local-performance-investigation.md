# Local Performance Investigation and Optimization Plan

**Goal:** Find and remove measured request-path bottlenecks across PostgreSQL,
SQLite, MySQL, ClickHouse, and mixed-source GraphQL without introducing SLO or
CI performance gates.

**Constraints:** Preserve explicit-role authorization, exact Donat error
shapes, capability skips, one native query statement per participating source,
and database-side JSON assembly. SQLite and MySQL mutation exceptions remain as
documented in their accepted ADRs.

## Work order

1. **Remove structurally proven request serialization.** Execute independent
   mixed-source reads concurrently while preserving plan-ordered responses and
   deterministic errors. Verify with focused async tests and the mixed-source
   conformance suite.
2. **Add a reproducible local matrix.** Provide `make perf
   BACKEND=postgres|sqlite|mysql|clickhouse`, `make perf-matrix`, and `make
   perf-mixed`. Use release builds, deterministic fixtures, response checks, a
   20-second warm-up, 60-second runs at concurrency 1/10/50, and three repeats.
   Store revision, machine details, throughput, latency percentiles, errors,
   CPU, RSS, response bytes, and connection counts. Do not define pass/fail
   thresholds.
3. **Measure shared request phases.** Behind opt-in local tracing, time auth,
   parse, allowlist/remote routing, planning, SQL generation, connection or
   blocking-worker wait, database execution, JSON decoding, remote joins, and
   response serialization. Keep the public GraphQL and metadata interfaces
   unchanged.
4. **Run the common backend workload.** Use equivalent cardinalities and query
   shapes for by-PK, filtered lists, relationships where supported, aggregates,
   permissions, variables, wide selections, and payload-size sweeps. Run
   mutations on PostgreSQL, SQLite, and MySQL; record ClickHouse mutation cases
   as capability skips.
5. **Test backend-specific hypotheses.** Measure PostgreSQL pool wait and
   parse/plan overhead; SQLite connection-open and `spawn_blocking` contention;
   MySQL connection handshake plus session setup; ClickHouse HTTP latency,
   buffering, and cancellation. For mixed-source reads, compare sequential
   baseline evidence with concurrent execution for PostgreSQL + ClickHouse and
   PostgreSQL + MySQL/SQLite.
6. **Optimize only confirmed hotspots.** Likely next candidates are compiled
   allowlist/REST query indexes, bounded reusable MySQL connections, an
   evidence-sized SQLite execution pool, subscription plan reuse, remote-join
   deduplication/bounded concurrency, and reduced large-response
   materialization. Each change starts with a failing regression or benchmark,
   keeps before/after evidence, and receives full unit/conformance verification.

## Deliverables

- A local result artifact for each backend and mixed-source run, plus a concise
  English report ranking cross-cutting and backend-specific bottlenecks by
  measured impact, effort, and regression risk.
- Focused implementation plans for the three highest-impact remaining issues.
- No SQL Server workload until a SQL Server runtime and conformance adapter
  actually exist.
