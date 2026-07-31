---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
---

# Declarative SaaS runtime boundaries and upstream-porting policy

## Context

Donat already provides the Hasura-compatible data plane: tracked tables and
views, per-role GraphQL schemas and permissions, relationships, Actions, REST,
MCP, cron triggers, and table event journals. Recreating that surface as a new
application model would add duplicate configuration and weaken existing
permission guarantees.

The desired next layer must express real SaaS business behavior with minimal
custom code while remaining one Rust binary backed by Postgres. Existing
projects offer useful reference behavior and test suites, but copying code
without an explicit license and provenance record is unsafe.

## Decision

Build above the existing data plane in four separated layers:

1. multi-relation domain commands compiled to one SQL statement;
2. typed rules and decision tables with no general scripting runtime;
3. Postgres-journal-backed durable processes;
4. compiled Rust connector modules for external protocols.

Commands are atomic database work and never perform external I/O. Processes
and connectors perform I/O only after a durable intent commits. Deploy-time
metadata and donat migrate --metadata-dir define the system; the serving binary
does not issue DDL or expose an administrative API. All data access remains
under an explicit role and no layer creates a permission bypass.

Every command is source-local and compiles to one Postgres statement. A
process instance pins an immutable deployed definition revision, and an
activity is leased in a short committed transaction before I/O occurs outside
that transaction. This deliberately gives at-least-once external delivery with
stable idempotency keys, rather than incorrectly holding a database row lock
over HTTP or claiming exactly-once behavior. Rule expressions are a strict,
typed CEL profile with no scripting, implicit database reads, or nondeterminism.

The HTTP connector originally classified destination addresses against the
IANA IPv6 Special-Purpose Address Registry to deny non-globally-reachable
hosts. That policy has been withdrawn: destination reachability belongs to the
deployment's network layer. See [[026-connector-egress-is-a-network-concern]].

Every specification and implementation change that uses an external reference
must record the upstream URL, immutable revision, exact source or fixture
paths, license, mandatory notices, Rust destination, and the mapping from
upstream behavior to a Donat-owned TDD test. Source-level ports are limited to
Apache-2.0, MIT, or BSD-compatible licenses and retain all required notices.
Non-permissive projects may be inspected only as behavior/documentation
references; their source and fixtures are not copied.

[[reference-porting-register]] is the authoritative detailed record. It also
requires a source hash, destination, failing-first Donat test, and reviewer
before any eligible upstream file is imported. The first copied third-party
artifact creates or updates root THIRD_PARTY_NOTICES.md with its required
notice.

## Alternatives

| Option | Why not |
| --- | --- |
| Rebuild entities, views, permissions, and CRUD in a separate DSL | duplicates existing metadata and creates two authorization models |
| Let GraphQL Actions contain all business logic | requires external handlers, lacks durable orchestration, and makes business behavior opaque |
| Adopt Temporal as a required workflow service | violates the one-binary/Postgres operating model |
| Allow arbitrary JavaScript, WASM, or Rust snippets in metadata | reintroduces application code, prevents static validation, and expands the trust boundary |
| Copy code from any useful repository | risks incompatible licensing and loses traceable test provenance |
| Keep a database transaction open while an activity calls HTTP | blocks unrelated workers and still cannot provide exactly-once external delivery |
| Reinterpret running processes after a metadata reload | makes a deployed workflow's history nondeterministic and unauditable |

## Consequences

The framework adds compiler and journal complexity, but SaaS applications gain
auditable, deploy-time business behavior without custom microservices. The
four specs are intentionally independent so each can be implemented and
conformance-tested in a separately reviewable series. The connector policy
creates some upfront research work, but makes later Rust ports reproducible and
legally reviewable.

The additional process-definition and job tables increase the deploy-time
migration surface. They preserve a stronger operational contract: a deployment
is auditable by revision, stale worker completions are harmless, and a future
metadata change cannot change an instance already in flight.
