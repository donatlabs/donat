---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[operations]]"
---

# The deploy gate checks what starting needs, and the log is readable by a machine

## Context

Two small gaps, both of them the engine expecting an operator to make up the
difference.

`donat validate` is the documented step between `migrate` and serving, and it
checked that the metadata agreed with the database — including that every
declared attachment names a real `uuid` column. What it never did was resolve
a single credential. `StorageRegistry::build` and `ConnectorRegistry::build`,
which turn `value_from_env` declarations into actual secrets, ran only at
`serve`. So a deployment could pass the gate on metadata that was entirely
consistent and fail seconds later on an environment variable nobody set: the
most common deploy mistake there is, discovered in the one place least able to
report it usefully.

Separately, [[declarative-saas/decisions/002-durable-process-operational-contracts]]
gives the deployment the job of observing the internal journal — "operators use
deployment-owned observability" — and the engine handed it
`tracing_subscriber::fmt()`, the human format, and nothing else. Delegating
observability while emitting only prose a collector has to parse back apart is
delegating the work and withholding the means.

## Decision

`validate` builds both registries and reports what they cannot resolve as
ordinary validation problems, named by the variable that is missing. It stops
there: neither registry opens a connection, because "is the deployment
configured" is a question a deploy-time gate should answer and "is the provider
reachable right now" is not one it should block on.

`DONAT_LOG_FORMAT=json` switches the subscriber to structured output with the
current span attached, so the request span's fields — method, path, and the
caller's own `x-request-id` — arrive as object fields rather than as a prefix.
The default stays the human format, because the default reader is a terminal.

## Alternatives

| Option | Why Not |
|--------|---------|
| Have `validate` also reach the object store and each provider | Turns a deploy gate into a liveness check on third parties, and fails a deploy for someone else's outage. |
| Resolve secrets at `migrate` instead | `migrate` runs against the database; the credentials belong to the process that serves. |
| Default to JSON logs | The default reader is a person running `make run`, and prose is better for them. |
| Emit both formats | Doubles the volume to satisfy neither reader well. |
| Leave logs alone and add a `/metrics` endpoint | ADR 002 places operational observability with the deployment; giving it readable logs honours that, while a metrics surface would take the job back. |

## Consequences

A deployment missing a credential now learns it from `validate`, with the
variable named, instead of from a container that will not start. A deployment
shipping logs to a collector can have them structured, and the request id it
already sends comes back as a field it can index on.

The human format remains the default, so nothing changes for anyone who has
not asked. `tracing-subscriber` gains its `json` feature, which is the only
dependency cost.
