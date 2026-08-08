---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[operations]]"
---

# Every request is bounded, and stopping is something the engine does on purpose

## Context

The engine had no ceiling anywhere on the request path. There was no
`statement_timeout` on any pooled session — the string did not appear in the
repository — no deadline on any HTTP surface, no `tower_http` in the
dependency tree at all, and therefore no panic layer. Dropping an HTTP request
does not cancel the statement it started, so one query the planner accepted but
the database could not answer cheaply held a backend open until an operator
noticed. A panicking handler dropped its connection, which a caller cannot
distinguish from a network fault.

Stopping was equally undefined. `axum::serve(listener, app).await` handled no
signal, so `SIGTERM` — what every rolling deployment sends — killed the process
where it stood. Requests in flight became transport errors. A durable activity
that had just taken a lease kept it until the lease expired, which is precisely
what [[declarative-saas/decisions/002-durable-process-operational-contracts]]
tells a deployment to avoid: "a rolling deployment must drain or fence
incompatible workers." There was no mechanism with which to comply.

The earlier posture ([[security-audit]], June 2026) modelled donat as an
internal component behind a mesh, with the fronting layer owning timeouts. The
product it is now — a public image, a documented `docker compose up`, a
Kubernetes deployment — has no such guaranteed neighbour, and a fronting proxy
that times out a request cannot cancel the statement behind it anyway.

## Decision

Every pooled Postgres session starts with a `statement_timeout`, carried as a
connection option so no call site has to remember it. It defaults to 30
seconds, a source may set its own with `pool_settings.statement_timeout`, `0`
removes it, and a URL that already carries libpq `options` keeps its own —
`deadpool` replaces that string wholesale, and silently dropping a deployment's
`search_path` in order to add our ceiling would be worse than having none.

Every request-response surface carries a deadline, default 60 seconds,
removable with `DONAT_REQUEST_TIMEOUT_SECONDS=0`. It sits *above* the statement
ceiling deliberately: a slow query should surface as its own GraphQL error
first, and the deadline is the backstop for a request stuck somewhere else. The
websocket upgrades are outside it, because a subscription is supposed to
outlive any deadline; the layer is applied to the methods registered before the
upgrade is added, and a test asserts that composition rather than trusting it.
A panicking handler becomes a 500 carrying the ordinary donat error shape, with
the panic text going to the log and never to the caller.

`SIGTERM` and `SIGINT` cancel one token. The HTTP server stops accepting and
finishes what it holds; every background loop — cron, events, the file
collector, and each Process transition and source worker — finishes its current
item and declines to claim another. The Process supervisor, whose whole job is
to start workers again, learns to distinguish a worker that died from one that
drained, because otherwise it is the one component that can undo a drain. The
wait is bounded by `DONAT_SHUTDOWN_GRACE_SECONDS`, default 25 seconds, chosen
to sit under Kubernetes' own 30-second default so the drain finishes before
`SIGKILL` rather than being cut off by it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Leave timeouts to the fronting proxy | A proxy timing out a request does not cancel the statement behind it; the backend stays occupied either way. |
| One deadline for every route, websockets included | Kills subscriptions at the deadline, which is the one thing they exist to outlive. |
| A `statement_timeout` per query instead of per connection | Every call site becomes a place to forget it, and the workers would be exempt by accident. |
| No default statement ceiling, opt in per deployment | The deployments that most need it are the ones that will not know to set it. |
| Abort background workers at `SIGTERM` instead of draining | Abandons a lease mid-flight — the failure the drain exists to prevent. |
| Wait indefinitely for workers to drain | A drain that never ends is a deploy that never finishes; the orchestrator resolves it with `SIGKILL`, which is where we started. |
| Track shutdown in `AppState` | Only `main` starts the workers and only `main` waits for them; threading the token through the two functions that use it keeps it out of every test's construction of `AppState`. |

## Consequences

A query that used to run unbounded now fails at 30 seconds, which is a
behaviour change for any deployment that had one — it is visible as an ordinary
error rather than as a backend nobody can account for, and the ceiling is
configurable per source. A rolling deployment now completes without stranded
leases or truncated responses, at the cost of up to 25 seconds per replica.

The engine gained a `tower-http` dependency for two layers and `tokio-util`
for the token and the task tracker. `donat_server::shutdown::idle` is now the
idiom for a polling loop: a bare `tokio::time::sleep` in a worker is a loop
that cannot be drained, and should be read as a defect.
