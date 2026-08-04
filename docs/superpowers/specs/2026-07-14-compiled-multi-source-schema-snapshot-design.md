# Compiled Multi-Source Schema Snapshot Design

**Date:** 2026-07-14

**Status:** Approved

## Problem

`MultiSourcePlanner::new` currently performs schema composition on the GraphQL
request path. For every ordinary operation it builds a role-independent schema,
discovers every metadata role, and composes both frontend and backend-only
projections for each role. `execute_multi_source_introspection` then builds the
request role schema before it has established that the operation is an
introspection query.

With the tandt metadata this repeats more than twenty full schema compositions
per request. A local request with no network hop spends about 1.4 seconds in
schema preparation while the generated PostgreSQL statement executes in less
than one millisecond. Concurrent requests queue behind CPU work and can take
four seconds or more.

## Goals

- Move schema composition and role-projection validation out of the request
  path.
- Keep metadata, catalogs, root ownership, validation state, and introspection
  schemas in one immutable, internally consistent snapshot together with the
  source planner indexes and backend routing handles they describe.
- Reject an invalid candidate before it can replace the serving snapshot.
- Preserve role, backend-only permission, Relay, capability, error-shaping, and
  no-admin-role behavior.
- Make ordinary GraphQL planning proportional to the requested operation plus
  the small number of configured sources, not to all configured roles and
  tables.

## Non-Goals

- A TTL cache, process-global mutable cache, or metadata hash lookup.
- A permission bypass, admin role, or admin-secret data access path.
- Changes to JWT verification, IAM seeding, generated SQL, or trigger delivery.
- Runtime metadata mutation APIs.

## Architecture

### Immutable compiled schema

The schema crate exposes `CompiledMultiSourceSchema`. Compilation receives a
metadata/catalog pair and the function-permission inference setting. It creates
temporary source-local planners and produces:

- base query and mutation root ownership maps;
- immutable per-source planner indexes for tables, functions, and CRUD roots;
- the role-independent schema template used for response-shape validation;
- the set of known metadata roles;
- prebuilt standard and Relay introspection schemas for each
  `(role, backend_request)` pair;
- prevalidated standard and Relay root/type collision results. A Relay-only
  collision is stored in the snapshot so ordinary GraphQL remains available;
  enabling Relay returns that cached validation error immediately.

Compilation is the only API that performs whole-schema composition or builds
table/function root indexes. A `MultiSourcePlanner` is created from metadata,
catalogs, and an already compiled schema. It creates only lightweight
source-local planner views over immutable indexes and borrows ownership/schema
data. Relay mode selects prevalidated Relay ownership and schema data instead of
recompiling them.

Unknown roles remain denied by the existing permission logic. Their rare
introspection request is served from one precompiled empty-role projection;
session variable values never participate in schema shape.

### Engine snapshot lifecycle

An `Arc<Engine>` owns metadata, catalogs, an
`Arc<CompiledMultiSourceSchema>`, and a source-name map of backend runtime handles
as one snapshot. `AppState` publishes it through `RwLock<Arc<Engine>>`. A runtime handle
contains the resolved URL and the backend-specific cloneable connection state
(PostgreSQL pool, SQLite path, MySQL URL, or ClickHouse URL). During source
synchronization, the server:

1. reads and clones the candidate metadata;
2. stages/reuses candidate runtime handles and introspects all candidate
   catalogs without holding the engine write lock;
3. prunes missing tracked objects in the candidate metadata;
4. compiles and validates the candidate schema;
5. wraps the candidate in an Arc, acquires the write lock, and swaps that one
   pointer so metadata, catalogs, compiled schema, and runtime handles change
   together.

If connection, introspection, or compilation fails, the previous snapshot and
all of its routing handles remain unchanged. The server does not publish
partially updated metadata, catalogs, schemas, or routing. Bootstrap uses an
explicit uninitialized compiled state; serving starts only after the first
successful synchronization.

### Request path

After parsing, a GraphQL request clones one `Arc<Engine>` and releases the read
lock. It creates lightweight planner views from that snapshot's compiled source
indexes, selects standard or Relay mode, and plans the operation. No metadata
clone, table/function index rebuild, role enumeration, or schema composition
occurs. The same Arc is carried through action webhooks, action relationship
queries, local execution, and remote joins, so publication during I/O cannot
mix metadata or permissions from a newer snapshot into the in-flight request.

Introspection detection is split from schema projection. The executor first
selects the operation and flattens only its roots. It requests a cached role
schema only when `__schema` or `__type` is actually present. Ordinary queries,
validation errors, and root `__typename` therefore never materialize an
introspection schema.

## Failure Semantics

- Missing catalogs and standard GraphQL root/type collisions fail candidate
  compilation.
- Relay collisions are detected during candidate compilation and stored as a
  deterministic Relay-mode error, not rediscovered on each Relay request.
- A failed candidate leaves the previous serving snapshot, including backend
  routing handles, unchanged.
- Calling the request planner before successful bootstrap returns an explicit
  initialization error; it never falls back to stale or empty catalogs.
- GraphQL validation and permission error bodies remain unchanged.

## Verification

1. Schema tests prove compilation catches standard, role-specific, backend-only,
   and Relay collisions.
2. A regression test proves ordinary operations do not request or compose an
   introspection schema.
3. Server state tests prove a failed candidate does not mutate the current
   snapshot or runtime routing and a valid candidate swaps all parts together.
4. Existing multi-source, Relay, permission, runtime, and conformance tests stay
   green.
5. A benchmark using the complete tandt L3 metadata compares repeated ordinary
   requests before and after the change and confirms schema work is absent from
   the request path.

## Compatibility

The external GraphQL contract, metadata format, environment variables, role
claims, and database behavior do not change. Startup performs more deliberate
work once, while steady-state requests reuse the result. No deployment
configuration change is required.
