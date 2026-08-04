---
type: decision
status: accepted
date: 2026-06-14
features:
  - "[[multi-backend]]"
---

# SQLite mutations assemble the response in Rust (M4 carve-out)

## Context

The engine's M4 invariant is "one SQL statement per operation; the response
JSON is assembled inside the database (`json_build_object`/`json_agg`,
correlated subqueries); no row-by-row post-processing in Rust." Mutations
realize it with a CTE:

```sql
WITH "ins" AS (INSERT ... RETURNING *) SELECT <json + check> AS root
```

Verified against the bundled SQLite (~3.46): **SQLite forbids a DML statement
(INSERT/UPDATE/DELETE) as a CTE body or subquery** — `near "INSERT": syntax
error`. Further, SQLite's `RETURNING` works only at top level, may reference
only bare column names, and **cannot be aggregated** (you cannot wrap it in a
subquery / `json_group_array`). So the CTE-wrapped, in-database assembly used
for Postgres mutations is structurally impossible on SQLite. (SQLite *queries*
are unaffected — they keep full in-database assembly; this is mutations only.)

## Decision

For SQLite **mutations**, emit one top-level DML per mutation root with a
`RETURNING json_object(<bare output columns>), <check-flag>` clause, where the
check-flag is `CASE WHEN (<check expr over bare columns>) THEN 0 ELSE 1 END`.
Only SQL `TRUE` satisfies a permission check; both `FALSE` and `NULL` set the
violation flag. The SQLite mutation executor runs this inside a transaction,
iterates the returned rows to build the `returning` array and `affected_rows`
count, and — if any row's check-flag is set — rolls back and returns a
permission error.

This is a documented **per-backend carve-out** to M4's "assembled in the
database" clause, scoped to SQLite mutations only. M4's load-bearing
property — **one SQL statement per mutation root, no N+1, no result
stitching across statements** — is preserved: it is still a single DML
statement; the only Rust-side work is folding that one statement's `RETURNING`
rows into the response shape, which SQLite cannot do in SQL. Postgres mutations
and all SQLite queries are unchanged. The permission check is enforced
atomically (flag computed in the same DML, rollback in the same transaction),
so no permission bypass is introduced.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep the CTE-wrapped in-DB assembly for SQLite | Impossible — SQLite forbids DML in a CTE/subquery (verified). |
| Two statements (DML into a temp table, then `SELECT json_group_array`) | Breaks "one statement per root", adds per-mutation temp tables, more round-trips — a worse M4 deviation than folding one statement's RETURNING in Rust. |
| Register a `check_violation` scalar function (rusqlite `functions` feature) to raise in-SQL | Needs an extra rusqlite feature, and still cannot aggregate RETURNING, so Rust-side row folding is required regardless. The lazy `CASE`-flag column achieves the rollback without the dependency. |
| Defer SQLite mutations (query-only) | Valid, but leaves the SQLite binding incomplete; this carve-out is small and well-contained. |

## Consequences

**We get:** a complete SQLite mutation binding (insert/update/delete with
`returning`/`affected_rows` and enforced permission checks with rollback),
without a CTE the grammar rejects.

**We pay:** SQLite mutation response assembly lives in the Rust executor, not
in SQL — a documented divergence from Postgres (whose mutations stay fully
in-database). Future mutation features (e.g. `on_conflict`, nested inserts)
must be designed for both the Postgres CTE path and the SQLite top-level-DML
path. `on_conflict` on SQLite is deferred (SQLite has no `ON CONFLICT ON
CONSTRAINT <name>`; it uses `ON CONFLICT(<cols>)`).
