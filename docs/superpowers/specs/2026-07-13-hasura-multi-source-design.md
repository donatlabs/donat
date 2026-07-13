# Hasura-Compatible Multi-Source GraphQL Design

**Date:** 2026-07-13

**Status:** Approved

## Problem

Donat loads every source from Hasura v2 metadata and maintains one catalog per
source, but GraphQL planning and execution use only the source named `default`.
This makes valid Hasura metadata with a Postgres `default` source and a
ClickHouse `clickhouse` source start successfully while silently omitting all
ClickHouse roots from the GraphQL schema.

The ClickHouse source used by tandt-backend tracks tables from both the
`analytics` and `logs` databases. Donat currently introspects one database from
the ClickHouse URL, defaulting to `default`, then prunes every tracked table not
found in that single database. A URL such as `http://clickhouse:8123` therefore
removes all tracked `analytics.*` and `logs.*` tables.

## Goals

- Expose every permitted root field from every metadata source through one
  Hasura-compatible GraphQL schema.
- Plan and execute operations containing root fields from one or multiple
  sources, including mixed Postgres and ClickHouse queries.
- Preserve aliases, top-level field order, Hasura validation errors, session
  permissions, and backend capability boundaries.
- Introspect every ClickHouse database referenced by tracked tables, without
  requiring a `database=` query parameter.
- Keep ClickHouse read-only and preserve the existing no-admin-role invariant.
- Cover the exact ClickHouse query shapes used by tandt-backend: flat reads,
  aliases, `where`, `_and`, `_like`, ordering, pagination, aggregates, aggregate
  nodes, Date/DateTime variables, JSON/Map/Tuple/Array output, and multiple root
  fields in one operation.

## Non-Goals

- Cross-source relationships. Hasura models those as remote relationships;
  ordinary table relationships remain source-local.
- Distributed transactions across mutation-capable sources.
- ClickHouse mutations, relay, or Postgres-only operators.
- A runtime admin role, permission bypass, metadata mutation endpoint, or admin
  secret based data access.

## Architecture

### Composite planning

The existing `Planner` remains the authoritative planner for one source. A new
`MultiSourcePlanner` owns one child `Planner` per metadata source and an index
from GraphQL root field name to source name.

For each request the composite planner performs the GraphQL operation and
variable selection once, expands top-level fragments and directives, and
partitions the top-level fields by owning source. It creates a source-local
operation containing the selected fields plus the original nested fragments
and variable definitions, then delegates validation and IR generation to the
existing child planner. This preserves all existing permission, naming,
predicate, aggregate, and capability logic.

The resulting query plan is:

```rust
pub struct SourceQueryPlan {
    pub source: String,
    pub roots: Vec<donat_ir::RootField>,
}

pub enum QueryResponseSlot {
    SourceField { key: String },
    LocalTypename { key: String, value: String },
}

pub enum MultiSourcePlan {
    Query {
        sources: Vec<SourceQueryPlan>,
        response: Vec<QueryResponseSlot>,
    },
    Mutation {
        source: Option<String>,
        roots: Vec<donat_ir::MutationRoot>,
        response: Vec<QueryResponseSlot>,
    },
}
```

`response` contains response keys after aliases. Root `__typename` fields are
synthetic local slots and require no datasource, so typename-only query and
mutation operations do not perform a backend call. A mutation source is `None`
only for a typename-only operation.

Top-level fields are collected in first-seen response-key order using GraphQL's
field-selection merging rules. Repeated compatible fields with the same field
name and equivalent arguments merge their nested selection sets, including
fields introduced through fragments. A response-key collision with different
field names or incompatible arguments returns `validation-failed` before any
source planner or backend call.

Mutations containing datasource fields must resolve to exactly one source;
typename-only mutations remain source-less. This preserves each backend's
existing atomic mutation semantics and avoids pretending to provide a
distributed transaction. ClickHouse never owns mutation roots because its
capabilities are read-only.

### Composite introspection

Each child planner builds its role-specific schema using its own capabilities.
The composite schema merger:

- merges `query_root` and `mutation_root` fields in metadata source order;
- deduplicates identical shared scalar, enum, and input types;
- retains all source-specific table and aggregate types;
- rejects conflicting root or type definitions during planner construction;
- reports a mutation root only when at least one source exposes a permitted
  mutation.

Introspection projection continues to use the existing GraphQL introspection
executor after the merged schema value is built.

### Source-aware execution

`AppState` gains source-addressed helpers for source kind, URL, Postgres pool,
SQLite path, and MySQL URL. `execute_source_query_json(source, roots)` selects
the backend and connection by source name instead of falling back to `default`.

One SQL statement is generated and executed per participating source. Source
plans execute in metadata source order, which keeps failure selection
deterministic without creating a cross-source transaction. The server applies
existing remote-join post-processing to each source result, then merges the
returned data objects in response-slot order and inserts local typename values
without a backend call. A backend failure retains the existing
Hasura-compatible GraphQL error body and fails the operation.

### ClickHouse multi-database introspection

For a ClickHouse source, `sync_sources` derives a stable, deduplicated database
list from the source's tracked table metadata. If no tables are tracked, it
falls back to the URL database and finally `default` for backward compatibility.

The `system.columns` request selects the `database` column and filters with an
Array(String) request parameter. Catalog parsing uses each row's database value
as the table schema, producing one catalog containing all tracked databases.
Only tables genuinely absent from that combined catalog are pruned.

## Error Handling

- Unknown roots keep the exact `validation-failed` message and path produced by
  the current planner.
- Duplicate root ownership or incompatible duplicate GraphQL type definitions
  fail startup with source and root/type names in the diagnostic.
- Cross-source mutations return `validation-failed` before any backend call.
- Compatible repeated fields merge according to GraphQL semantics; conflicting
  aliases or arguments return `validation-failed` before any backend call.
- Missing source connections return the existing `unexpected` query error and
  never fall back to another source.
- ClickHouse introspection transport and response-size limits remain unchanged.

## Verification

1. Unit tests prove root ownership, operation partitioning, schema merging,
   source-specific capability handling, response ordering, direct and fragment
   field merging, conflicting alias/argument rejection, and collision rejection.
2. Runtime tests use one Postgres source and one ClickHouse HTTP stub to prove
   source routing and a mixed-source response.
3. A real ClickHouse test creates both `analytics` and `logs` databases, uses a
   base URL without `database=`, and verifies reads and aggregates from both.
4. A tandt contract fixture uses Hasura `configuration.template` and the tracked
   table/root names from production metadata. It runs all twelve project query
   documents with representative variables.
5. Existing workspace unit tests and the full conformance suite remain green.
6. The real ClickHouse backend matrix remains green against ClickHouse 25.8.
7. Negative authorization tests prove that role-specific root visibility and
   session-variable row filters are enforced independently for Postgres and
   ClickHouse. Requests without a role and requests naming `admin` cannot read
   either source unless explicit metadata permissions grant that exact role.
8. Typename-only query/mutation operations and mixed typename plus datasource
   operations preserve response order and make only the required backend calls.

## Compatibility

Single-source metadata follows the same child planner and SQL generation paths
as before. Existing source names, root naming, permissions, JWT claims, action
and trigger behavior remain unchanged. Deployments do not need metadata or URL
rewrites.
