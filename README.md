<div align="center">

# donat

## A platform team for your SaaS, in a box.

**Vibe-code your SaaS. Ship it to production.** You describe what the
business needs; your agent declares it — schema, permissions, payments,
refunds, and the tests beside them; the engine **refuses anything wrong
before it reaches production**. No hardening pass, because there is nothing
to harden: there is no code.

[![CI](https://github.com/donatlabs/donat/actions/workflows/ci.yml/badge.svg)](https://github.com/donatlabs/donat/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/donatlabs/donat?display_name=tag&label=release)](https://github.com/donatlabs/donat/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

[Why it is safe](#safe-to-vibe-code) · [Run the Petshop example](#get-started) · [See what you declare](#what-you-declare) · [Explore the architecture](#one-execution-path)

</div>

---

## Safe to vibe-code

2026 taught everyone the same lesson: AI writes applications faster than
anyone can review them, and the wreckage is documented — leaked keys by the
million, endpoints nobody authenticated, production tables dropped by an
agent that was told not to. The going advice is a "hardening pass" before
AI-built software touches production data.

Donat's answer is structural, not procedural. An application here is not
code the agent wrote — it is **declarations the engine either accepts whole
or refuses**, the way a platform team's golden path works: the paved road is
the easy one, and the guardrails do not move.

| What keeps happening to vibe-coded apps | Why it cannot happen here |
|---|---|
| Hardcoded secrets shipped in code | There is no code to ship them in. Credentials are deploy-time environment, named by the metadata, resolved before the engine will even bind its port. |
| An endpoint someone forgot to authenticate | Access exists only as a declared per-role permission. No permission — no field in the schema at all. And there is **no admin role** to find: the bypass does not exist. |
| SQL injection through a clever input | Nobody — including the agent — writes SQL into a request path. Inputs bind as typed values; the [injection attempts are tests now](examples/petshop/metadata/databases/default/tables/public_product_test.yaml), and they stay search terms. |
| A wrong declaration discovered in production | `donat validate` is a compiler: a permission naming a missing column, a validator that cannot type-check, an unreachable process branch — each is a **deploy failure**, never a request failure. |
| Untested behaviour nobody noticed | A test is a `*_test.yaml` file beside the declaration it proves, run by `donat test` on a fresh database each. A table that grants a role something **without a test beside it fails CI**. |
| A migration or "quick fix" run by hand against prod | The runtime has no `run_sql`, no metadata API, no admin surface — deleted, not disabled. Change arrives as `migrate` + metadata, through review. |

So the promise is not "the agent will not make mistakes". It is: **everything
the agent gets wrong is refused before production — by the engine, not by a
reviewer's attention.**

## Built to be driven by an agent

The intended user is not a developer with an editor — it is a founder,
analyst or operator working with a coding agent. You say what the business
needs; the agent reads this repository's skills and writes the declarations;
you read the result and recognise your own requirement in it, because a
declaration reads as a sentence, and so does its test:

```yaml
tests:
  - name: a shopper cannot check out another shopper's cart
    steps:
      - as: { role: customer, user: customer-2 }
      - graphql: 'mutation { start_checkout(cart_id: 1, request_id: "…") { cart_id } }'
        expect:
          errors:
            - extensions: { code: validation-failed }
```

This repository is a [Claude Code plugin marketplace](plugins/donat):

```
/plugin marketplace add donatlabs/donat
/plugin install donat@donat
```

It installs skills for the whole declarative surface — tables and
permissions, validators, rules, commands, durable Processes, connectors,
files, the REST and MCP surfaces, testing and deployment — each pointing at
the worked example in this tree, plus the rules that are not negotiable. For
OpenAI Codex, [`plugins/donat/codex`](plugins/donat/codex) ships the same
material as an `AGENTS.md` section and prompts.

## The whiteboard is the source

In a system design interview you draw a checkout in two minutes. A box that
takes the order. An arrow to the payment provider. A note that says *retry with
backoff*. A second arrow for *what if it times out and we don't know whether it
charged*. A box that waits for the warehouse to confirm. A fan-out, one per
shipment.

Everyone in the room understands it. Then it becomes months of glue: outbox
tables, a job runner, correlation ids, an idempotency table, a reconciliation
script somebody runs by hand.

Donat is the bet that the drawing was already the specification.

```yaml
- id: request_authorization
  request:
    connector: mock_payment
    operation: authorize
    idempotency_key: { stable: { run: id, state: request_authorization } }
    retry:
      retry_on: [transport, timeout, http_429, http_5xx]
      max_attempts: 3
      initial_interval: 100ms
      jitter: deterministic_full
    next: route_authorization_response
    on_error:
      # A transport error does not prove the provider skipped the charge.
      fallback: { next: reconcile_authorization }
```

That is the arrow, the retry note, and the "what if it times out" branch. No
worker to write, no queue to run, no correlation id to invent.

## What you declare

| You draw | You declare | You do not write |
|---|---|---|
| A box that changes state | A **Command** — ordered steps in one transaction, with guards, batch writes and an idempotency key | Transaction plumbing, replay handling, a re-select after every write |
| A decision | A **Rule** or decision table — typed inputs, an expression or ordered rows, unit-testable in metadata | A tangle of `if` in three services that disagree |
| A long-running flow | A **Process** — states, waits, timers, signals, bounded fan-out, compensation, pinned revisions | An outbox, a scheduler, a status column that lies |
| An external system | A **Connector** — declared operations with request and response contracts, error classes, retry, rate limits, idempotency headers | A hand-rolled client per provider, and the reconciliation script |
| A file on a record | An **Attachment** — a uuid column declared beside the table's permissions, stored in S3 | An upload service, a second permission model, a cleanup cron |
| Who may see what | Per-role row filters, column sets and presets | An authorization layer per transport |

All of it is YAML in review, applied at deploy time. The engine is one Rust
binary that reads it and serves.

## Durable by construction

A Process is not a background job with a status column. Its journal lives in
the same database as your data, so a state transition and the rows it wrote
commit together or not at all.

- **Waits** park on a verified signal or a timer, for minutes or for a month.
  A signal committed before its wait became receptive is auditable, not lost.
- **Activities** hold a lease, retry on the classes you declared, and stop at
  the attempt limit.
- **Ambiguity is a first-class branch.** A timeout does not mean "it did not
  happen". Declare the read-only lookup that decides, and route on its answer.
- **Fan-out is bounded.** A per-item journal with a declared ceiling, so a
  hundred shipments cannot become an unbounded queue.
- **Revisions are pinned.** A running instance keeps the definition it started
  under; deploying a new one does not rewrite history mid-flight.

## It runs, not just parses

[`examples/petshop`](examples/petshop) is not a toy. It is a store with **11
durable Processes**, **73 Commands**, **60 rules and 10 decision tables** over
**41 declared types**, and **5 connectors**: checkout with tax and
authorization, cancellation racing an in-flight charge, partial fulfilment
with per-shipment capture, returns with three human approvals, subscription
dunning, B2B credit approval, marketplace payouts, prescription review and
payment reconciliation.

And it is tested the way it is built — declaratively. **142 test cases live
beside the metadata they prove** (`*_test.yaml` next to each table, flow and
endpoint), run by `donat test` against the real binary on a fresh database
per case: declines, retries, idempotency replays, cross-shopper refusals,
provider outages, human approvals. The whole store — payments, returns, B2B
credit, subscriptions, marketplace payouts — with **zero lines of code,
tests included**.

## Explicit access, no way around it

There is **no admin role**. Not "disabled by default" — the permission-bypass
role does not exist, and neither does a runtime metadata API or a `run_sql`
endpoint. Every data access resolves through an explicit per-role permission,
including the ones a Process makes on your behalf: a Process step runs as a
declared role, or as the caller whose session it captured.

Configuration is deploy-time. `donat migrate` applies DDL, a second `migrate`
deploys the Process revisions, and the serving engine runs neither.

## Multitenant without writing a filter

A tenant is not a column you remember to compare in every permission of every
table. It is one declaration, `tenancy.yaml`, naming the source, the claim that
carries the tenant, the key column, and the registry of tenants:

```yaml
source: default
variable: X-Donat-Tenant-Id
key: tenant_id
registry:
  table: { schema: public, name: tenant }
  key: id
  status: { column: status, serving: [active] }
exempt:
  - table: { schema: public, name: plan }   # platform reference data
    shared: read_only
```

From there the compiler does it. Every read — roots, nested relationships,
aggregates, Relay, subscriptions, REST, MCP — is bounded by the caller's
tenant, so a permission with `filter: {}` means *every row of my tenant*. Every
write is bounded the same way and presets the tenant column, so an insert
naming somebody else's id lands in the caller's own tenant instead of theirs.
Commands and durable processes are included: a process persists its tenant with
the instance, and the tenant joins every idempotency scope, because otherwise
two tenants that pick the same key would replay each other's results.

Two properties are the point:

- **Forgetting a table stops the deployment.** Every tracked table either
  carries the key or says why it does not. A table that says neither fails
  `donat validate` and refuses to boot, naming itself. Adding the hundred and
  fifty-ninth table cannot quietly serve everyone's rows.
- **The tenant is a claim, never a header.** It arrives the way a role does —
  from a verified token or an auth hook. `X-Donat-Role` selects among roles a
  token already granted; there is nothing for a tenant header to select among,
  so no header names one. A request without a tenant is refused, not answered
  with an empty page.

`extends:` composes one metadata directory onto another, so the platform layer
sits on top of a business domain whose YAML it never edits — collisions are
refused rather than silently overridden. `examples/pethub` is that worked
example: it composes `examples/petshop` and adds tenancy, grants and ceilings in
four files. `git diff examples/petshop` staying empty is the acceptance
criterion, and `crates/conformance/tests/pethub.rs` asserts it rather than
assuming it.

**Inside a tenant, `iam.yaml` lets the tenant decide.** A compiled role is the
shape — which tables and operations exist. A grant is the scope: rows a tenant
writes for itself, saying which `service:action` its own roles hold. The
compiler turns those rows into the predicate on every root the compiled role
exposes, so two people with the same role differ by what they were granted, and
adding a table governs it. `cancel_order` can require `order:cancel` while
`orders:update` is not enough, and the actions that belong to the platform are
barred by the database rather than by whichever command writes the grant.

**`quotas.yaml` caps what a tenant holds** without editing the domain's insert
permission — the counter moves inside the statement that performs the write, so
the ceiling holds when two writers arrive at once rather than when they happen
to arrive apart.

## An unbounded permission says so

A tenant can be compiled in because it is one value on one column, the same for
every table and every role. Ownership cannot: the path to an owner differs per
table, and whether a table has one at all differs per role — a marketplace's
catalogue belongs to every shopper and to no seller. So the rule stays in the
permission's own `filter`, where it can vary.

What *is* uniform is the guarantee. `filter: {}` is a legitimate thing to write
and it is also what a forgotten bound looks like, so a deployment can require
the difference to be written down:

```yaml
# permissions.yaml
unbounded_permissions: declared
```

```yaml
- role: support
  permission: {columns: "*", filter: {}, unbounded: operator}
```

Every permission that admits rows it does not bound to the caller then has to
name a reason — `catalogue` (the rows are nobody's in particular), `operator`
(the role is a desk), `worker` (a fixed role a process runs as, with no session
to bound against) or `command` (the row is chosen by a command step). Nothing is
injected and no query changes; forgetting simply stops looking like deciding.

Unbounded is a question about reaching TRUE, not about being empty: `{status:
{_eq: 'paid'}}` still shows every caller the same rows. `{_or: [{owner: {_eq:
X-Donat-User-Id}}, {}]}` names a session variable and admits everything, and is
read as unbounded. A tenant bound is deliberately not a caller bound — scoping
only by tenant is every seller's order in one marketplace, which is the case
worth seeing. `command` is checked rather than believed: it is accepted only
where no generic root reaches the permission.

The default is `unchecked`, so metadata exported from an existing Donat project
loads unchanged. An overlay cannot relax a base that asked for it.

See the decisions for what is deliberately out of scope:
[tenancy](knowledgebase/declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered.md),
[in-tenant grants](knowledgebase/declarative-saas/decisions/098-a-compiled-role-is-the-shape-and-a-grant-is-the-scope.md)
and
[declared bounds](knowledgebase/declarative-saas/decisions/099-an-unbounded-permission-says-so.md).

## One execution path

A request resolves into a SQL-free intermediate representation before the
backend generates a query. On the Postgres reference backend the response JSON
is assembled in the database: one statement per root operation, permission
predicates inside the plan, no N+1.

REST and MCP are not parallel stacks — they translate into that same pipeline,
so filters, error contracts and policy cannot drift by transport.

<p align="center">
  <img src="docs/assets/data-plane-overview.png" alt="SQL migrations and metadata feed the Donat engine, which serves GraphQL, REST, MCP, and automation interfaces." width="1200" />
</p>

| Surface | What it is for |
|---|---|
| **GraphQL** | Queries, mutations, subscriptions, Relay, aggregates, relationships, computed fields, JSONB and PostGIS. |
| **REST** | Metadata-declared endpoints over saved operations; path, query and body become variables. |
| **MCP** | Permission-aware tools. A separate `mcp.yaml` publishes a small, role-scoped agent contract. |
| **Commands & Processes** | Transactional domain operations and durable flows, exposed as ordinary mutations. |
| **Events & actions** | Event triggers, durable cron delivery, verified inbound webhooks, typed synchronous actions. |
| **Files** | Presigned upload and download URLs for declared file columns on any S3-compatible store, public files served straight from a CDN, and orphaned objects collected in the background. No file byte passes through the engine. |

All request-facing surfaces are on by default; restrict them at deploy time
with `DONAT_GRAPHQL_ENABLED_APIS=graphql`. A mounted route still needs an
explicit role and a matching permission.

## Get started

```sh
make up                           # http://localhost:5180
```

One command, and nothing to fill in. It prints the password to sign in as
`operator@example.com` with.

That is the default deployment, built from this tree: Postgres, an identity
provider, object storage, and the engine — which serves the UI
itself, so there is one container answering on one origin rather than a
reverse proxy in front of two.

Every other credential it uses is generated on this machine the first time,
into `.env`. None of them is a value anybody chooses: they are secrets two
programs use to recognise each other, and the only reason to see one is to
rotate it.

It has **no application in it** — no tables, no metadata beyond a source and a
store — because an application is what you add on top. What it has is
everything you would otherwise assemble first: a way to sign in, a way to say
who may do what, and somewhere for files to go. The panel's Identity section
manages the provider's own accounts, roles, groups, scopes, applications,
attributes, blocked addresses and sessions — none of which is metadata you
write; see
[apps/ui](apps/ui) and
[ADR platform/003](knowledgebase/platform/decisions/003-the-identity-adapter-ships-in-the-binary-and-grants-nothing.md).

Add tables to `deploy/metadata/databases/default/tables/` with per-role
permissions beside them, and the panel renders them as resources.

### An application to look at

```sh
docker build -t ghcr.io/donatlabs/donat:latest .
cd examples/petshop
docker compose up
```

The Petshop brings up Postgres, applies the schema, deploys the Process
revisions, answers the five declared providers with a local fixture, and
serves:

- **GraphQL** — <http://localhost:8080/v1/graphql>
- **REST** — <http://localhost:8080/api/rest/>
- **MCP** — <http://localhost:8080/mcp>

For focused surfaces, see [REST-only](examples/petshop-rest) and
[MCP-only](examples/petshop-mcp).

To embed the engine **inside a Go application** instead of running it beside
one, start with the [Go SDK](sdk/go) — it documents where your code plugs in,
and what the embedded host deliberately does not do. The worked example is
[lending-golang](examples/lending-golang): a small library whose
lending decisions are declared in YAML and compiled into single statements,
whose side effects are ordinary Go functions called in-process, and which
serves GraphQL from the same binary with no cgo. Its
[system tests](tests-system-lending) run every case against both the
standalone engine and the embedded host, and fail if the two disagree.

### From source

```sh
make build
make test
make run
```

Run the compatibility suite with `make conformance`; add
`make conformance-matrix` for the preview backend contract matrix.

### The crates

One line divides this workspace: whether a crate can reach the outside world.
Everything that turns a request into SQL is pure, so it compiles to `wasm32`
and can run inside a host that is not Rust. Everything that opens a socket
stays behind that line. That is what lets the same planner serve both the
standalone binary and an embedded Go host without either forking its
behaviour — the SQL is the same SQL because it is the same code.

**The compile path** — pure, no I/O, all of it reaches wasm:

| Crate | What it owns |
| --- | --- |
| [`metadata`](crates/metadata) | The Donat v2 metadata types and the YAML directory loader, `!include` included. The format is the compatibility surface: an export from an existing project must load unconverted. |
| [`catalog-types`](crates/catalog-types) | The catalog snapshot as plain serde types — tables, columns, keys. Split out from `catalog` precisely so the planner could reach wasm without the drivers behind it. |
| [`rules`](crates/rules) | The restricted CEL-inspired expression language a declaration uses for its arithmetic and thresholds, so a business rule is data rather than code. |
| [`value-contract`](crates/value-contract) | The closed, SQL-free value types shared by commands, processes and connectors. One owner, so those three cannot drift apart on what a value is. |
| [`action`](crates/action) | Actions minus their transport: routing an operation, checking role visibility, binding arguments, shaping the result. Shared so a webhook and an in-process Go function answer identically. |
| [`schema`](crates/schema) | The planner. GraphQL + metadata + catalog → IR, with permissions woven in rather than checked afterwards. The largest of the pure crates, and the one that decides what a role may see. |
| [`ir`](crates/ir) | The SQL-free boundary: a validated, permission-resolved plan. Everything upstream is about meaning; everything downstream is about syntax. |
| [`backend`](crates/backend) | What differs between databases — logical scalar types, capability descriptors, dialect rendering — kept in one place so `sqlgen` is written once. |
| [`sqlgen`](crates/sqlgen) | IR → **one** statement that returns the finished JSON. The response is assembled in the database, never row by row in Rust. |
| [`storage`](crates/storage) | File attachments: the resolved store and the URL signing the planner and the server share. Its secrets are a caller's argument, which is what lets it work off-host. |
| [`wasm-core`](crates/wasm-core) | The above, compiled to `wasm32` behind a memory ABI: `(query, vars, session) → PlanV1`. The blob the Go SDK embeds. |

**The host path** — opens sockets, so it stops at the line:

| Crate | What it owns |
| --- | --- |
| [`catalog`](crates/catalog) | Reading `pg_catalog`. The only place that talks to it; everything downstream takes the snapshot. |
| [`connector-abi`](crates/connector-abi) | Neutral IDs, bounded envelopes and host traits for compiled connectors. |
| [`connector-catalog`](crates/connector-catalog) | Reviewed source records and the checked-in static catalog. Connectors are a deploy-time artifact — nothing is loaded at runtime. |
| [`server`](crates/server) | The axum binary: `/v1/graphql` (+ws), Relay, `/api/rest`, `/mcp`, cron, durable processes, plus `migrate`, `validate` and `codegen`. There is no admin API; configuration is deploy-time. |
| [`conformance`](crates/conformance) | The harness and its fixtures — the executable source of truth. It builds and spawns the engine itself. |

The Go host lives in [`sdk/go`](sdk/go/README.md): it embeds `core.wasm`
(built by `make wasm-core`; it ships inside the released module rather than in
the branches),
drives it through wazero with no cgo, and owns the pool, the HTTP surface and
the functions you write for logic no declaration can express.

## Designed to be operated, not bypassed

<p align="center">
  <img src="docs/assets/deployment-flow.png" alt="A deploy-time flow from migrations and metadata through validation to a protected Donat engine serving controlled interfaces." width="1200" />
</p>

1. Apply versioned DDL with `donat migrate`.
2. Deploy Process revisions and check the metadata against the migrated
   database.
3. Start the engine behind your normal TLS, auth, rate-limit and observability
   edge.

It connects to a managed database the way anything else does: `sslmode` in the
connection URL, `verify-full` included, with a private CA named in
`DONAT_PG_SSL_ROOT_CERT`. And it stops the way an orchestrator expects — on
`SIGTERM` it reports itself **not ready** while still serving, so your balancer
can take it out of rotation, and only then drains the requests in flight and
lets each background worker finish the item it holds.

Point your readiness probe at `/readyz` and your liveness probe at `/healthz`.
Neither touches the database on purpose: a liveness probe that fails on an
unreachable database asks for a restart that cannot help, and a readiness probe
that follows a blip empties every replica at once.

| Knob | Default | What it bounds |
|---|---|---|
| `pool_settings.statement_timeout` (per source) | 30s | Any single statement. `0` removes it; `DONAT_PG_STATEMENT_TIMEOUT_SECONDS` sets the default. |
| `pool_settings.verify_connections` (per source) | `true` | Proves a pooled connection alive before handing it out — one round trip, and no failed query when a proxy reaps an idle socket. |
| `DONAT_REQUEST_TIMEOUT_SECONDS` | 60 | Every request-response surface. Websocket subscriptions are exempt. `0` removes it. |
| `DONAT_UPSTREAM_MAX_BODY_BYTES` | 16 MiB | How much an action handler or remote schema may return. Session and key-set responses are fixed at 1 MiB. |
| `DONAT_SHUTDOWN_READINESS_DELAY_SECONDS` | 5 | How long the replica keeps serving after reporting not ready. `0` stops accepting immediately. |
| `DONAT_SHUTDOWN_GRACE_SECONDS` | 25 | How long draining may take before the process exits anyway. |
| `DONAT_PG_SSL_ROOT_CERT` | host trust store | The certificates a Postgres TLS connection will accept. |
| `DONAT_LOG_FORMAT` | human | `json` emits structured logs, with the request span — and your own `x-request-id` — as fields. |

Nothing here needs an admin surface. A stuck instance is read with
`donat process inspect --source <name> --instance <uuid>`, and
`donat process verify-history` checks that its recorded history still agrees
with itself. Both are read-only, and there is deliberately no CLI that cancels,
retries or replays: recovery is an explicit declared command, called the way
any other caller calls one.

Webhook handlers should be idempotent: event and cron delivery is at least
once. So is a Process activity — which is why an idempotency key is part of
the declaration rather than an afterthought.

## Evidence over assertions

A behaviour change starts from a failing fixture. The result must then pass
unit and snapshot tests plus the native harness against real services.

- [CI](https://github.com/donatlabs/donat/actions/workflows/ci.yml) runs the
  workspace against Postgres, full reference conformance, the backend contract
  matrix, live MySQL and ClickHouse paths, and `cargo audit`.
- Security fixtures cover SQL injection, IDOR, hidden columns, preset
  enforcement and missing session variables.
- The [conformance crate](crates/conformance) is the executable source of truth
  for request and error behaviour.
- Design decisions, including the ones that were wrong first, live as ADRs in
  the [knowledge base](knowledgebase/).

## Backend support

Postgres is the supported reference backend. SQLite, MySQL and ClickHouse are
CI-tested preview targets with explicit capability boundaries — not labels for
drop-in equivalence. Commands and Processes are Postgres-only.

| Backend | Status | Important limits |
|---|---|---|
| **Postgres + PostGIS** | Supported reference | Full feature set, including Relay, JSONB, geo, upsert, nested inserts and subscriptions. |
| **SQLite** | Preview | No Relay, `DISTINCT ON`, upsert or nested inserts; JSON is JSON1 rather than JSONB. |
| **MySQL 8.0.14+** | Preview | No Relay, `RETURNING`, upsert, `DISTINCT ON` or nested inserts. |
| **ClickHouse** | Preview, read-only | No mutations, relationships, JSON operators, geo or Relay. |

The [matrix](.github/workflows/ci.yml) makes those limits executable: each
backend runs only the fixtures its declared capabilities support.

## Is Donat a fit?

Choose Donat when the flow between your services is the hard part — when the
same data must serve app clients, REST consumers and AI tools, and when
"charged but not recorded" is a bug you cannot ship. And choose it when the
person who owns the requirements is not the person who would have written the
code: an analyst with an agent gets the platform team's guardrails without
hiring the platform team.

Donat is not an ORM, a hosted platform, or a place for genuinely computational
work. What it gives you is declarations with closed grammars: bounded,
reviewable, and refused at deploy time when they do not hold together. If your
logic wants a general-purpose language, keep it in a service and reach it
through a connector.

## Agentic contribution

Much of the work here is done by coding agents, and the repository is built so
that an agent writing code is answerable to the same things a person is — not
to a reviewer's attention, which does not scale, but to gates that refuse. The
premise is stated in
[`knowledgebase/engineering`](knowledgebase/engineering/_index.md): a model
does not learn between sessions; the repository does, and only on the rungs
that refuse. So every lesson is pushed down to the lowest rung that can hold
it — a test or a fixture before a lint, a lint before a CI gate, a gate before
a sentence in a document.

**The rules an agent reads.** [`CLAUDE.md`](CLAUDE.md) and
[`AGENTS.md`](AGENTS.md) carry the same content for Claude Code and Codex: how
work is done (a failing conformance case first, then the code), what is never
negotiable (no admin role, fixtures are ground truth, snapshots are read not
accepted), and how a red build is classified before it is touched.

**The gate that reads the diff.** `make gate`
([`scripts/check_change_gate.py`](scripts/check_change_gate.py), and the
`Change gate` workflow on every pull request) is the mechanical twin of the
rules that can be checked from a diff. A retired admin-role name in engine
sources, or a committed `.snap.new`, fails outright. A change that is sometimes
right and always worth a sentence — an existing fixture or snapshot rewritten,
the toolchain bumped, an advisory excused, a `sleep` added to a conformance
test, a test ignored, a skill edited — is admitted only when the pull request
says why, one line per kind: `gate:<kind> <reason>`. A new fixture or snapshot
is free; rewriting an existing one is what needs a reason. The gate checks that
the change was named; a person reads whether the reason is good.

**Measured, not asserted.** A change to a plugin skill names a measurement that
the edit helps an agent build a better service — where a corpus to run one
exists — or says why it needs none. The gate makes an unmeasured skill edit say
so rather than pass in silence.

**Unattended work still arrives as a pull request.** A nightly loop
(`make setup-loop-infrastructure`; `scripts/loop.sh` and
[`.claude/skills/fix-advisories`](.claude/skills/fix-advisories/SKILL.md)) runs
jobs in a worktree of its own and opens a pull request — never a push to
`main`, never a self-approved merge. The advisory check that feeds it runs
[nightly in CI](.github/workflows/advisories.yml) as well. Whatever an agent
did overnight is read, and merged, by a person.

The shape of it: an agent can do the work, and the repository decides what it
is allowed to have done quietly. Nothing here writes to `main` on its own.

## Get involved

Star or watch the [repository](https://github.com/donatlabs/donat), report an
[issue](https://github.com/donatlabs/donat/issues), or start with the
[examples](examples/). Fixture conventions are documented in the
[conformance harness](crates/conformance/PORTING.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Some conformance
fixtures are derived from a third-party Apache-2.0 test suite; its license and
attribution are retained in `crates/conformance/fixtures/LICENSE.hasura`.
