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

An activity receives one logical activity ID at enqueue time. Its provider
idempotency key is derived from that ID and is unchanged by retry, worker crash,
or lease takeover. The runtime distinguishes schedule-to-start from
start-to-close timeout using the database clock. Phase 1 has no heartbeat
extension: long interactions are represented by a start activity followed by a
timer or verified signal. Retry delay uses deterministic jitter derived from
logical activity ID and attempt. All worker binaries reserve operation capacity
and rate-limit permits through Postgres before any outbound call. An operation
may also declare a typed same-resource serialization key using that durable
reservation mechanism.

Connector and process failures use typed classes and ordered declared
`on_error` routes. Domain recovery and cancellation are ordinary explicit
commands with a typed `signal_process` effect and a declared process
transition. The effect writes a durable outbox row in the same command CTE;
the process worker consumes it idempotently. There is no generic runtime
cancel, retry, replay, definition-mutation API, operator CLI, admin role, or
permission bypass. Operators use direct database access and deployment-owned
observability for the internal journal. The binary may additionally expose only
read-only inspect and offline history-verification CLI subcommands; they never
mutate history or invoke a command or connector.

Inbound connector webhooks are durable process ingress, not connector-route
or conformance responsibilities. The durable process ingress implementation
must write `process_inbound_events`, deduplicate the verified provider event
identity, persist one redacted audit outcome, correlate at most one pinned
process instance, and acknowledge a provider only after that transaction
commits. Until that implementation exists, the connector route verifies raw
bytes then returns `503` for a verified event without acknowledgement; it adds
no in-memory queue, duplicate cache, or standalone persistence model. The
connector conformance task proves that temporary `503` boundary only. The
process plan's **Task 6: Process timers and verified inbound events** owns the
durable ingress, audit, deduplication, correlation, and success-acknowledgement
work.

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

## Consequences

Deployments and worker rollouts require compatibility tests and operational
discipline, but each in-flight instance has an auditable executable contract.
Activity implementation needs capacity-reservation tables, a controllable
database clock in tests, and more fault-injection coverage. In return, provider
idempotency, timeout behavior, retries, cancellation, and failure routing are
defined in metadata and tested without adding a workflow service or an
administrative escape hatch.
