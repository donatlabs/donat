# donat — architecture, milestones, decision log

The map of the engine: what the parts are, which invariants hold across all of
them, where the work stands, and where each decision is written down.

Two neighbours own things this file deliberately does not repeat. The
[knowledge base](knowledgebase/_index.md) holds the *why* — 77 ADRs across four
domains — and this file links to it rather than restating it. The
[conformance crate](crates/conformance) holds the *what* — the executable
contract for request and error behaviour — and no prose here overrides a
fixture.

## The shape of a request

A request never reaches SQL directly. It resolves through a fixed sequence, and
every surface enters that sequence at the same point:

```
GraphQL / REST / MCP
        │  (REST and MCP translate to GraphQL; they are not parallel stacks)
        ▼
   session          role and session variables, from headers, a JWT, or an
        │           auth webhook — never from a permission bypass
        ▼
   plan             per-role schema, permission predicates folded in,
        │           unknown fields refused here
        ▼
    IR              the SQL-free boundary: the last representation that
        │           knows nothing about a backend
        ▼
  backend           one statement per root operation
        ▼
   Postgres
```

| Crate | What it owns |
|---|---|
| `crates/metadata` | Donat v2 metadata types, YAML directory loader (`!include`) |
| `crates/catalog` | Postgres introspection (`pg_catalog`) |
| `crates/schema` | Per-role GraphQL schema generation, planning, introspection |
| `crates/ir` | The intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one Postgres statement (insta snapshots) |
| `crates/backend` | Dialects for the preview backends |
| `crates/rules` | Declarative rules and decision tables |
| `crates/connector-abi`, `crates/connector-catalog` | Connector boundary and canonical connector sources |
| `crates/storage` | File attachments: resolved object store, URL signing |
| `crates/server` | axum server, all surfaces, workers, `migrate` / `validate` / `process` |
| `crates/conformance` | The native harness and its fixtures |
| `crates/value-contract` | Shared value semantics across the boundary |

## Invariants

These hold everywhere. A diff that breaks one is wrong even when its tests
pass.

**No admin role, and no admin secret.** There is no permission-bypass role and
no admin-over-HTTP surface: the runtime admin/`run_sql` API was deleted, the
admin data role removed, and `DONAT_GRAPHQL_ADMIN_SECRET` with it. Every access
resolves through an explicit per-role permission, including the ones a Process
makes on a caller's behalf. A role is established by a verified JWT or an
authentication hook and by nothing else — no header names one — and the engine
can serve that login itself without owning any identity
([[api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]]). The read-only `donat process inspect` / `verify-history` commands
are the only operator entry points, and they are permitted by name in
[[declarative-saas/decisions/002-durable-process-operational-contracts]].

**One statement per operation.** Response JSON is assembled in the database.
The documented carve-out is SQLite *mutations*, which fold one statement's
`RETURNING` rows in Rust because SQLite forbids DML inside a CTE —
[[multi-backend/decisions/003-sqlite-mutation-rust-assembly]]. Postgres
mutations and all SQLite queries keep full in-database assembly.

**Deploy-time configuration.** `migrate` applies DDL and deploys Process
revisions; the serving engine runs neither. Metadata is YAML, read at boot.

**Exact error shapes.** Error `code`, `path`, message text and HTTP status are
part of the conformance contract.

**Escaping, not formatting.** sqlgen renders literals inline through
`quote_lit` / `quote_ident`. Parameterised execution is a planned refactor
(see the header of `crates/sqlgen/src/lib.rs`); until then no user input
reaches SQL by any other route.

**Bounded by default.** Every pooled session carries a `statement_timeout`,
every request-response surface carries a deadline, every upstream response is
read against a ceiling, a panicking handler becomes a response, and `SIGTERM`
reports the replica not ready before it drains — see
[[operations/decisions/001-bounded-and-drainable-by-default]],
[[operations/decisions/003-a-replica-announces-its-own-readiness]] and
[[operations/decisions/004-the-other-end-of-a-socket-is-not-trusted-to-behave]].

## Backends

Postgres is the reference. The others are CI-tested previews with declared
capability boundaries, and the conformance matrix runs each one only against
the fixtures its capabilities support.

| Backend | Status | Limits |
|---|---|---|
| Postgres + PostGIS | Supported reference | Full feature set |
| SQLite | Preview | No Relay, `DISTINCT ON`, upsert, nested inserts; JSON1 not JSONB |
| MySQL 8.0.14+ | Preview | No Relay, `RETURNING`, upsert, `DISTINCT ON`, nested inserts |
| ClickHouse | Preview, read-only | No mutations, relationships, JSON operators, geo, Relay |

Commands and Processes are Postgres-only.

## Milestones

| # | Milestone | Status |
|---|---|---|
| M1 | Metadata loading, introspection, per-role schema | done |
| M2 | Queries, relationships, aggregates, Relay | done |
| M3 | Mutations, permissions, presets, session variables | done |
| M4 | One statement per operation, in-database JSON assembly | done |
| M5 | REST and MCP surfaces over the same pipeline | done — [[api-surfaces/_index]] |
| M6 | Multi-backend matrix (SQLite, MySQL, ClickHouse) | preview — [[multi-backend/_index]] |
| M7 | Declarative services: rules, commands, processes, connectors | done — [[declarative-saas/_index]] |
| M8 | File attachments on S3-compatible storage | done — `specs/008-file-attachments.md` |
| M9 | Deployability: TLS to Postgres, bounded requests, readiness and drain | done — [[operations/_index]] |
| — | Embedded SDK and native hooks | deferred — [[embedded-sdk/_index]] |

Feature specifications live in `specs/`, one per delivered capability
(`001`–`008`).

## Decision log

Decisions are ADRs in the knowledge base, not entries here. Each domain's
`_index.md` lists its own; the counts are the shape of the record:

| Domain | ADRs | Covers |
|---|---|---|
| [[declarative-saas/_index]] | 36 | Commands, rules, durable processes, connectors, files |
| [[multi-backend/_index]] | 15 | The backend trait, per-dialect assembly, the matrix, multi-source |
| [[api-surfaces/_index]] | 12 | REST and MCP, session compatibility, identity boundaries |
| [[embedded-sdk/_index]] | 7 | Embedding and native hooks (deferred) |
| [[operations/_index]] | 5 | Bounded requests, TLS posture, drain and readiness, upstream ceilings, the deploy gate |

Cross-cutting: [[security-audit]] ranks the security findings and their
resolution under the deployment threat model.

## How the work is done

Engine behaviour starts from a failing conformance case — a fixture in
`crates/conformance/fixtures` plus a call in `crates/conformance/tests` — then
unit and insta tests in the touched crate, then the conformance crate green
against a rebuilt binary. Commands are in `CLAUDE.md`; fixture conventions are
in `crates/conformance/PORTING.md`.
