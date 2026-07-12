---
type: decision
status: accepted
date: 2026-06-14
features:
  - "[[multi-backend]]"
---

# MySQL mutations: DML + companion SELECT (no RETURNING)

## Context

MySQL has **no `RETURNING` clause** (any version) and its CTEs are read-only
(no DML in a `WITH`). So neither the Postgres CTE-wrapped in-DB assembly nor the
SQLite top-level-`RETURNING` approach (ADR 003) works. A mutation's `returning`
set must be recovered by a **separate `SELECT`** after (or, for delete, before)
the DML, in the same transaction. This extends the SQLite M4 carve-out (Rust-
side mutation assembly) to MySQL.

## Decision

Per mutation root, the MySQL executor runs, inside one transaction:

- **insert**: `INSERT ...`; recover the new rows by `last_insert_id()` when the
  table has a single auto-increment PK (the N rows occupy
  `[last_insert_id(), last_insert_id()+N-1]` for one multi-row insert under the
  default `auto_increment_increment=1`), else by the explicitly-supplied PK
  values; then `SELECT json_object(<returning fields>), <check-flag>` over those
  rows.
- **update**: `UPDATE ... WHERE <pred>`; re-`SELECT` the rows matching `<pred>`
  for `returning` + `<check-flag>`.
- **delete**: `SELECT json_object(<returning fields>) ... WHERE <pred>` FIRST
  (capture the rows), then `DELETE ... WHERE <pred>`.

`affected_rows` comes from the DML's row count. The permission check is a
`CASE WHEN (<check>) THEN 0 ELSE 1 END` flag in the companion SELECT, so only
SQL `TRUE` passes and both `FALSE` and `NULL` violate the permission. Any set
flag → `ROLLBACK` + permission-error (same body as Postgres/SQLite). The whole
sequence is one transaction, so nothing partially persists.

## Alternatives

| Option | Why Not |
|--------|---------|
| `RETURNING` (SQLite/Postgres style) | MySQL has none. |
| DML inside a CTE | MySQL CTEs are read-only. |
| Trigger-based capture into a temp table | Heavyweight, stateful, worse than a companion SELECT. |

## Consequences

**We get:** complete MySQL mutations (insert/update/delete with `returning` /
`affected_rows` and enforced checks + rollback).

**We pay:** insert `returning` relies on `last_insert_id()` recovery, which is
well-defined only for a single auto-increment PK and a single INSERT statement
(documented limitation; non-auto-increment PKs use the supplied values). Update
`returning` re-selects by the predicate, so an update that changes columns the
predicate filters on could re-select a different set — acceptable for the
common case, flagged for the rare one. `on_conflict` (MySQL `ON DUPLICATE KEY
UPDATE`) is deferred. This is more Rust-side orchestration than Postgres, scoped
to MySQL mutations only; Postgres and all read queries are unaffected.
