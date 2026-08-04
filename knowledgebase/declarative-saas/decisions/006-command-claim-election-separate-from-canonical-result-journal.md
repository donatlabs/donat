---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Command claim election is separate from the canonical result journal

## Context

An idempotent command must execute its domain-write CTEs once, retain one
canonical result, replay that result for an identical retry, and reject a
different canonical input under the same compound key. The original V3
`donat.command_invocations` catalog correctly owns the completed input
fingerprint and result, but a Postgres data-modifying CTE cannot reliably
update a row that an earlier data-modifying CTE inserted in the same statement.
Postgres executes each CTE once under the statement snapshot; attempting to
insert a `running` journal row and then update that same row to store the
result can leave the invocation uncompleted. An exact retry could then execute
the domain write again.

The command invariant still requires one Postgres statement and one
transaction. Adding a second runtime round trip, a lock service, a worker, or
runtime DDL would violate that boundary.

## Decision

V3 remains the sole canonical completed-invocation journal. It stores the
compound key `(command_name, scope_hash, key)`, input fingerprint, full result,
and expiry. V4 introduces the internal
`donat.command_invocation_claims` table with the same compound key, expiry,
and a bounded `first`/`replay` election marker. It stores no raw scope/input
value, role, result, metadata, or SQL.

At the beginning of the one command statement, SQLgen performs an
`INSERT .. ON CONFLICT DO UPDATE` on the claim row. A new or expired claim
returns `first`; an active claim returns `replay`. Only `first` gates domain
CTEs. An expired claim refreshes its expiry and becomes `first` again; the
first-result journal upsert then replaces the matching expired V3 entry.

After the canonical result CTE is assembled, mutually exclusive journal CTEs
run: a `first` claim inserts or replaces the V3 fingerprint/result, while a
`replay` claim performs a no-op conflict update that returns the stored V3
fingerprint/result. The conflict check therefore always compares the requested
fingerprint with the stored fingerprint, and the projection always reads the
stored canonical JSON. Any guard, assertion, permission, or idempotency
rejection aborts the statement, rolling back its claim, domain writes, and
journal change together.

## Alternatives

| Option | Why not |
| --- | --- |
| Insert a `running` V3 row then update it in a later data-modifying CTE | PostgreSQL's same-statement snapshot does not make that update reliable; it can leave a row without the completed result. |
| Execute claim, domain write, and result update as separate SQL statements | Breaks the single-statement atomicity contract and admits retry races. |
| Add Redis/advisory-lock service or an idempotency microservice | Adds a second system of record and violates the single Rust binary/deploy-time catalog architecture. |
| Store inputs/results in the claim row too | Duplicates V3's canonical journal and unnecessarily widens the retention surface. |

## Consequences

The migration catalog has one small internal table in addition to V3. Its
expiry index supports paired retention cleanup, while statement-time expiry
handling makes reclamation safe if cleanup is delayed. Renderer tests exercise
first execution, exact replay, changed-input rejection, failed-command
rollback, expiration reclamation, and a deterministic concurrent retry. The
renderer remains Postgres-only, emits one statement, and gains no admin role,
runtime migration path, public API, or external service.
