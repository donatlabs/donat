---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
---

# Durable process compatibility, activity lifecycle, and recovery contract

## Context

An immutable process definition alone is not enough to make a deployment safe.
An in-flight instance also relies on the process runtime interpretation,
commands, connector operation behavior, endpoint class, credential class, and
the activity worker's timeout and retry semantics. A rolling deployment can
otherwise let an older binary claim work it cannot interpret, or let a changed
connector send an old instance to a new provider endpoint without an auditable
revision boundary.

External calls also need a precise lifecycle. A single lease timeout conflates
queueing delay and a running request. Per-worker concurrency counters cause a
deployment with multiple binaries to exceed a provider limit. Generic process
management endpoints would reintroduce an authorization bypass that Donat does
not permit.

## Decision

Every deployed process revision stores the complete non-secret dependency
closure: canonical definition, rules, command fingerprints, connector module
and operation versions, runtime ABI, endpoint and credential identities, and
connector configuration fingerprint. A worker may claim an instance only when
its binary supports that ABI and every pinned operation. A rolling deployment
must drain or fence incompatible workers. `migrate --metadata-dir` rejects an
incompatible removal of a command or catalog dependency used by a non-terminal
revision, and database migrations stay backward compatible until those
instances finish. A wait/cancellation signal's name, correlation shape, and
payload type are likewise retained while an active revision can receive it.

An activity receives one logical activity ID at enqueue time. Executable
connector effects are closed to headerless `ReadOnly` or
`ProviderIdempotent`. The latter declares every side-effecting compiled step
with one fixed header/body binding, provider scope, conservative minimum
retention, and positive clock margin. The step key is derived from logical
activity ID, scope, and step ID and is unchanged by retry, worker crash, or
lease takeover. A database-clock `first_provider_attempt_at` is committed in
the source-local activity journal before that step's first network send.
Compilation pins a complete bounded retry/takeover send horizon and requires
it to fit the usable retention window. Each possible attempt contributes its
start-to-close bound plus one terminal takeover grace, including the final
attempt and `max_attempts = 1`. A takeover rotates lease generation but cannot
renew that attempt's deadline or increment its configured attempt ordinal.
Equality does not by itself expire either persisted bound; both comparisons
are strict and both bounds are still evaluated. When database time is later
than both, the provider usable-window check runs first and returns permanent
`connector_idempotency_window_exhausted`; when it is only later than the
compiled maximum-send deadline, the typed timeout route wins. Both routes
permanently refuse network I/O instead of rotating the key.

The runtime distinguishes schedule-to-start from start-to-close timeout using
the database clock. Phase 1 has no heartbeat extension: long interactions are
represented by a start activity followed by a timer or verified signal. Retry
delay uses deterministic jitter derived from logical activity ID and attempt.
All worker binaries reserve operation capacity and rate-limit permits through
Postgres before any outbound call. An operation may also declare a typed
same-resource serialization key using that durable reservation mechanism.
Commands commit only domain SQL plus process start/signal intent; connector or
provider business logic is invoked only by the durable activity after that
intent, lease, and applicable capacity reservation commit.

Connector and process failures use typed classes and ordered declared
`on_error` routes. Domain recovery and cancellation are ordinary explicit
commands with a typed `signal_process` effect and a declared process
transition. The effect writes a durable outbox row in the same command CTE;
the process worker consumes it idempotently. There is no generic runtime
cancel, retry, replay, definition-mutation API, mutating process-management
operator CLI, admin role, or permission bypass. Operators use deployment-owned
observability for the internal journal. The only CLI exceptions are
`donat process inspect --source <name> --instance <uuid>` and
`donat process verify-history --source <name> --instance <uuid>`; both are
read-only diagnostics and never mutate history or invoke a command or
connector.

Inbound connector webhooks are durable process ingress, not connector-route
or conformance responsibilities. The durable process ingress implementation
uses `process_inbound_events` only as the verified provider-event dedupe
ledger and appends one `process_inbound_deliveries` row for every delivery
attempt. Verified deliveries write audit plus dedupe atomically; invalid
signatures write audit only and need no trusted provider event ID. Correlation
selects at most one pinned process instance, and the route acknowledges a
provider only after the transaction commits. Until that implementation
exists, the connector route verifies raw bytes then returns `503` for a
verified event without acknowledgement; it adds no in-memory queue, duplicate
cache, or standalone persistence model. The connector conformance task proves
that temporary `503` boundary only. The process plan's **Task 12: Add timers,
command signals, and linked inbound audit** owns durable ingress,
audit, deduplication, correlation, and success acknowledgement. The exact
source-local journal and transaction contract is recorded in
[[009-durable-process-source-local-compilation-and-journal-contracts]].

The raw connector-verification matrix remains exact: unknown instance or no
verifier is empty `404`, an oversized raw body is empty `413`,
missing/malformed/expired/unsupported/invalid verification is empty `400`,
and successful verification before durable ingress exists is empty `503`.
After durable ingress exists, every successfully committed verified outcome—
`accepted`, `duplicate`, `unmatched`, `ambiguous`, `guard_false`, or
`unexpected_state`—returns empty `204 No Content`. A post-verification
persistence or transition database failure returns empty
`503 Service Unavailable`. No verified result is acknowledged before its
source-local audit/dedupe/transition transaction commits.

## Alternatives

| Option | Why not |
| --- | --- |
| Pin only process YAML | misses ABI, commands, connector behavior, and protocol-facing configuration required to replay an active instance safely |
| Use one activity lease timeout | cannot distinguish an unclaimed queue delay from a hung provider call |
| Use a per-process in-memory semaphore | does not enforce a provider limit across multiple Donat binaries or survive a restart |
| Let same-resource activities rely on best-effort ordering | cannot prevent two workers from concurrently changing one external provider resource |
| Add activity heartbeats immediately | adds a second lease protocol before short HTTP activities and signal/timer patterns are proven |
| Provide a generic process-admin endpoint or CLI | violates the explicit-role/no-admin boundary and bypasses the declarative domain contract |
| Automatically reverse prior SQL on cancellation | cannot safely reverse arbitrary committed business changes; compensation must be explicit |
| Treat one-attempt provider mutation as safe | worker loss can leave an ambiguous provider outcome, so every executable side-effecting step still needs provider idempotency |
| Invoke a connector from command execution | external I/O would escape the one-statement command transaction and could run before durable process intent commits |

## Consequences

Deployments and worker rollouts require compatibility tests and operational
discipline, but each in-flight instance has an auditable executable contract.
Activity implementation needs capacity-reservation tables, a controllable
database clock in tests, and more fault-injection coverage. In return, provider
idempotency, timeout behavior, retries, cancellation, and failure routing are
defined in metadata and tested without adding a workflow service or an
administrative escape hatch.
