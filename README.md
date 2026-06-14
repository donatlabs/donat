<div align="center">

# Donat

### Your schema is the backend.

**Donat generates permissioned GraphQL, REST, and MCP APIs, event & scheduled
triggers, and webhook automations from your Postgres schema — no application
code, no admin backdoor, one binary.**

[Quick start](#quick-start) · [How it works](#how-it-works) · [Example workflow](#example-workflow) · [Comparison](#how-donat-compares) · [FAQ](#faq)

</div>

---

## What is Donat?

Every team rebuilds the same backend: APIs, authentication, permissions,
events, integrations, automations. Most tools solve **one** layer — a GraphQL
server, a REST framework, an event bus, a workflow engine — and you wire the
rest together by hand and keep it all in sync.

Donat takes a different starting point: **your data model is the source of
truth.** You describe your schema (SQL migrations) and your access rules and
endpoints (declarative YAML) once. Donat derives a complete, permissioned
backend from it:

- **APIs** — a per-role GraphQL API (`/v1/graphql`, with subscriptions and
  Relay), the same model exposed as RESTified endpoints (`/api/rest`), and an
  MCP server (`/mcp`) for AI tools — all over **one** permission model.
- **Events** — webhooks on row insert/update/delete and on a cron schedule,
  with retries, invocation logs, and multi-pod-safe delivery.
- **Automations** — synchronous webhook *actions* with their own typed inputs
  and outputs, and relationships back into your tracked tables.
- **Integrations** — remote GraphQL schemas stitched in under role-scoped
  permissions.
- **Native handlers** *(in progress)* — write trigger logic as **typed
  functions in your own service** via a generated SDK, not only as HTTP
  webhooks. A Go SDK is landing first — a code generator emits typed row structs
  from your catalog, and you register handlers like
  `donat.On("on_order_placed", func(ctx, ev) {...})` — with an in-process WASM
  runtime and Node.js/Python SDKs to follow. See [Roadmap](#roadmap).

It is **not** another GraphQL server, ORM, or API gateway. It is a
database-native backend platform: define the data, get the backend.

> **Status — read this first.** Donat is built TDD-style against a native
> conformance harness (18 test modules, run with `make conformance`). The
> capabilities below are the ones that harness verifies on every commit
> against a real Postgres. **Postgres is the supported backend today.** See
> [Roadmap](#roadmap) for what is scaffolded but not yet shipping
> (multi-database GA, OpenAPI export, async actions, event streaming) — we
> keep that line explicit on purpose.

---

## Why Donat

| Pain | How Donat removes it |
|---|---|
| Rebuilding CRUD + auth + permissions on every project | Derived from your schema + declarative permissions — no handwritten resolvers |
| Stitching an API layer, event bus, cron, and workflow engine together | APIs, events, and automations live in **one** engine over one model |
| Permission logic scattered across app code → IDOR holes | Row- and column-level rules are declarative and enforced in-engine; covered by an IDOR/SQL-injection security suite |
| Runtime admin consoles / `run_sql` as an attack surface | **No admin role and no runtime admin API at all** — configuration is deploy-time only |
| N+1 and over-fetching from ORMs and naive GraphQL servers | **One SQL statement per operation** — the response is assembled inside Postgres |

---

## Quick start

The fastest path is the [`examples/petshop`](examples/petshop) project — a
catalogue, customers, and orders with a realistic public/shopper/staff
permission set.

```sh
cd examples/petshop
docker compose up
```

This runs the deploy model end to end:

1. **`donat migrate`** applies the versioned DDL in `migrations/` (refinery).
2. **`donat validate`** checks the YAML metadata against the migrated DB and
   fails the deploy if anything tracked is missing.
3. **`donat serve`** serves the data plane on `:8080`:
   - GraphQL → <http://localhost:8080/v1/graphql>
   - REST → <http://localhost:8080/api/rest/>
   - MCP → <http://localhost:8080/mcp>

Working from source instead:

```sh
make build
make test           # unit + snapshot tests (no database needed)
make run            # serves :8080 with the fixture metadata
make conformance    # full conformance suite (needs Postgres — see below)
```

---

## Example workflow

**1. Define the schema** (a migration, the only thing that runs DDL):

```sql
-- migrations/V2__create_pet.sql
CREATE TABLE pet (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name        text NOT NULL,
  category_id bigint REFERENCES category(id),
  owner_id    text NOT NULL          -- the customer who owns this row
);
```

**2. Declare access** in metadata (per-role row filter + column mask):

```yaml
# metadata/databases/default/tables/public_pet.yaml
table: { schema: public, name: pet }
select_permissions:
  - role: shopper
    permission:
      columns: [id, name, category_id]
      filter: { owner_id: { _eq: "X-Donat-User-Id" } }    # row-level rule
```

**3. Use it** — the same model, three surfaces:

```graphql
# POST /v1/graphql   (X-Donat-Role: shopper)
query { pet { id name category { name } } }   # only this shopper's pets
```

```sh
# A saved query exposed as REST via metadata/rest_endpoints.yaml
curl localhost:8080/api/rest/pets -H 'X-Donat-Role: shopper'
```

**4. React to changes** — add an event trigger and a nightly cron job in YAML;
Donat captures the row change in-transaction and delivers a webhook with
retries and invocation logs, safely across multiple pods.

No resolvers, no event-bus glue, no admin console.

---

## How it works

```mermaid
flowchart LR
  subgraph deploy["Deploy-time"]
    M["SQL migrations"] -->|donat migrate| DB[("Postgres")]
    Y["YAML metadata"] -->|donat validate| DB
  end
  DB --> ENG{{"Donat engine"}}
  Y --> ENG
  ENG --> GQL["/v1/graphql + ws"]
  ENG --> REST["/api/rest/"]
  ENG --> MCP["/mcp"]
  ENG --> EVT["event & cron triggers → webhooks"]
  ENG --> ACT["actions → webhooks"]
```

A request becomes an intermediate representation (IR), which compiles to **one
Postgres statement** that assembles the entire JSON response in the database
(`json_build_object` / `json_agg`, correlated subqueries) — no per-row
post-processing in the app, no N+1. Permissions are merged into that statement,
so a role can never see a row or column it isn't granted.

| Crate | Purpose |
|---|---|
| `crates/metadata` | Donat metadata types + YAML directory loader (`!include`) |
| `crates/catalog` | Database introspection |
| `crates/schema` | Per-role GraphQL schema generation + introspection |
| `crates/ir` | Intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one SQL statement (snapshot-tested) |
| `crates/backend` | Pluggable `Dialect` abstraction (Postgres today) |
| `crates/server` | axum server: GraphQL/REST/MCP, auth, events, actions; `migrate`/`validate` |
| `crates/conformance` | Native conformance harness + fixtures |

> **Architecture diagram tip:** the Mermaid block above renders natively on
> GitHub. For a richer landing page, replace it with an SVG showing the same
> three-stage flow: *Schema + YAML → engine → (GraphQL · REST · MCP · events ·
> actions)*.

---

## Capabilities

All of the following are verified by a passing module in the native
conformance harness (`crates/conformance/tests/`).

### APIs
- **GraphQL** — per-role row filters and column masks, relationships (FK +
  manual), aggregates, computed fields, `_by_pk`/`_one`, jsonb & PostGIS
  operators, Relay connections, and live-query **subscriptions** over
  WebSocket.
- **Mutations** — insert (with `on_conflict`/upsert, presets, check
  expressions), update, delete, `returning` — all in one transaction.
- **REST** — RESTified endpoints: a method + URL template maps to a saved
  query; path/query/body bind its variables. Runs through the same per-role
  pipeline.
- **MCP** — Model Context Protocol over streamable HTTP: generic,
  table-parameterized CRUD tools (`list_tables`, `describe_table`, `query`,
  `insert`, `update`, `delete`), each running under the request's role.
- **Surface selection** — mount any subset with `--enabled-apis`
  (`DONAT_GRAPHQL_ENABLED_APIS=graphql,rest,mcp`); a disabled surface is simply
  not registered.

### Auth & permissions
- **JWT** (RS/ES/EdDSA/HS families) including **JWK fetch with Cache-Control
  refresh**; webhook auth hook with unauthorized-role fallback.
- **Row- and column-level permissions** per role, **inherited roles** with
  cell-level NULLing and cycle detection, **allowlist / query collections**.
- **No admin role.** No `run_sql`, no metadata mutation over HTTP. The
  admin-secret is API-level auth only and never bypasses permissions. This is a
  deliberate security posture — see [Security](#security).

### Events & automations
- **Event triggers** — insert/update/delete webhooks, captured in-transaction,
  delivered with the Donat envelope, `retry_conf`, and per-attempt invocation
  logs.
- **Cron (scheduled) triggers** — recurring webhooks from YAML with retries and
  tolerance windows.
- Both are **multi-pod safe with no leader election** (`ON CONFLICT`
  materialization + `FOR UPDATE SKIP LOCKED` → at-least-once; handlers must be
  idempotent).
- **Actions (synchronous)** — webhook handlers with a custom type system
  (input/output objects, scalars, enums), output validation, handler-error
  surfacing, and output → tracked-table relationships resolved under the
  calling role.

### Integrations
- **Remote schemas** — role-scoped SDL permissions and execution, schema
  customization (namespace/prefix translation), and `@preset` arguments (static
  + session).

### Deploy
- **`migrate`** (refinery DDL) + **`validate`** (metadata vs DB) +
  boot-from-YAML; multi-source metadata with per-source pools.

---

## Security

Donat's security model is a feature, not an afterthought:

- **No permission-bypass role and no admin-over-HTTP surface.** A trusted
  request with no `X-Donat-Role` is denied. Configuration is deploy-time only
  (`migrate` + YAML).
- **Declarative, in-engine permissions** are merged directly into the single
  generated SQL statement.
- **Adversarial test coverage** — `crates/conformance/tests/security.rs`
  exercises IDOR / broken-access-control attacks and SQL-injection payloads;
  there is a query-depth DoS guard.

---

## Roadmap

We keep "shipping today" and "not yet" honest — credibility matters more than a
longer feature list.

**Scaffolded, not yet shipping:**
- **Native handler SDKs** — the Go SDK lands the handler contract + codegen and
  defers the in-process **WASM** transport (pure-Go `wazero`, no cgo) behind a
  single `Dispatch` seam. **Node.js and Python SDKs** follow the same contract.
  The goal: write business logic as typed native functions in any language,
  with HTTP webhooks as the fallback rather than the only option.
- **Multi-database GA** — a pluggable `Dialect` abstraction exists with SQLite
  and MySQL introspection + SQL generation, but neither is conformance-tested
  in the running engine yet. **Postgres is the supported backend today.**
- **Async actions** and request/response (Kriti) transforms — sync actions ship
  now; async is future work.
- **Remote relationships across the full native harness** — per-row remote
  relationships are implemented but currently only exercised by the legacy
  cross-check; mixed local+remote root queries remain incomplete.
- **Event-trigger session-variable capture** and column-filtered payloads.

**Planned:**
- **OpenAPI / Swagger export** — generate an OpenAPI document from the declared
  REST endpoints. Not implemented yet; REST endpoints are declared in YAML
  today.
- **Event streaming** — first-class delivery to streaming backends
  (Kafka/NATS) alongside today's webhook delivery.

> Note: there is intentionally **no admin role and no runtime admin API**
> (`run_sql`, metadata mutation). That is a permanent design stance, not a
> roadmap item — see [Security](#security).

---

## How Donat compares

| | Donat | Hasura | Supabase | PostgREST | GraphJin | Convex |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| Schema-first (DB = source of truth) | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ (code-first) |
| GraphQL + REST + MCP from one model | ✅ | partial | partial | REST only | GraphQL only | n/a |
| Declarative RBAC + inherited roles | ✅ | ✅ | partial | DB roles | basic | code |
| Events + cron + automations built in | ✅ | ✅ | partial | ❌ | ❌ | ✅ |
| Typed native handlers (not just webhooks) | 🚧 Go landing (WASM/Node/Py next) | ❌ webhook-only | edge functions | ❌ | ❌ | ✅ TS |
| **No runtime admin API / `run_sql`** | ✅ | ❌ | ❌ | n/a | n/a | n/a |
| One SQL statement per operation | ✅ | partial | n/a | ✅ | ✅ | n/a |
| Self-hosted single binary | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ |

*Temporal isn't in the table: it's a durable-workflow engine that complements
Donat's data-bound webhooks rather than competing with them.*

---

## FAQ

**Is this a Hasura clone?**
No. Donat is compatible with the Donat v2 metadata format and API shape (so
existing metadata loads without conversion), but it is a distinct engine with a
different security posture — **no admin role, no runtime admin API** — written
in Rust, configured only at deploy time.

**Do I write any application code?**
For the core backend, no. You write SQL migrations and YAML metadata. Custom
business logic lives in *actions* and *trigger handlers* — your own code, not
the engine.

**Do I have to use webhooks for handlers?**
No. Webhooks work everywhere, but you can also register **typed native
handlers** through a generated SDK and skip the HTTP round-trip. The Go SDK is
first; an in-process WASM runtime and Node.js/Python SDKs are on the
[Roadmap](#roadmap).

**How are permissions enforced?**
Row filters and column masks are declared per role and merged into the single
generated SQL statement, so a role physically cannot select data it isn't
granted. There is no bypass role.

**Which databases are supported?**
Postgres today. A pluggable backend abstraction exists with SQLite/MySQL
groundwork — see [Roadmap](#roadmap).

**Can I run only GraphQL (or only REST)?**
Yes — `DONAT_GRAPHQL_ENABLED_APIS=graphql`. Surfaces left out aren't mounted.

**How does it run in production?**
A single binary, multiple pods. Event and cron delivery is safe under
concurrent instances with no leader election (at-least-once; design your
webhook handlers to be idempotent).

---

## Contributing & docs

- Architecture decisions and design notes live in [`knowledgebase/`](knowledgebase).
- Engine work is TDD against the conformance harness — see
  [`crates/conformance/PORTING.md`](crates/conformance/PORTING.md).
- Run `make conformance` (needs Postgres `postgis/postgis:16-3.4` at `PG_URL`,
  default `postgresql://postgres:postgres@127.0.0.1:15432/postgres`). Each
  suite spawns its own engine on a fresh database — hermetic and parallel.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Some conformance
fixtures are derived from a third-party Apache-2.0 test suite; that upstream
license and attribution are retained in
`crates/conformance/fixtures/LICENSE.hasura`.
