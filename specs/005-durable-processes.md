# Spec 005 — Durable declarative processes

Status: proposed. Processes orchestrate long-running business work after a
command commits. They are not microservices, serverless functions, or a
replacement for the existing GraphQL Actions endpoint.

## 1. Goal and hard boundary

A process is a metadata-defined durable state machine. A domain command writes
a pinned process-start outbox request, and the start worker consumes that
request into the initial process event and instance. The instance then waits
for a connector result, an authenticated inbound signal, or a timer, and
transitions by running a declarative command or queuing another connector
activity.

The runtime is part of the single donat binary and stores its durable state in
Postgres. It reuses the established journal pattern: deploy-time DDL through
donat migrate --metadata-dir, a runtime Tokio poller, FOR UPDATE SKIP LOCKED,
at-least-once delivery, idempotent handlers, and per-attempt logs. The serving
binary never creates DDL.

An external request is never made while a database transaction is open. A
command transaction atomically writes only a pinned process-start outbox
request, never a process event or activity job. The start worker consumes that
request in a later short transaction; only a process state transition may
enqueue an activity job, and its poller performs I/O only after that transition
commits.

## 2. Metadata surface

`processes.yaml` is a top-level metadata section. Section 7 is its only
normative Phase-1 grammar; no earlier shorthand is accepted by the loader.
`start.command` causes the command's final CTE to insert an immutable
process-start request containing the compiled active revision, later consumed
into the pinned process event. It is valid only when the command result can be
bound to the declared immutable process input type. A transition invoking a
command names a `run_as_role`; it
must be explicitly listed on that command and retain every currently effective
table permission. There is no administrator role, implicit workflow identity,
or permission bypass.

Every command transition declares `on_rejection`. Every activity declares this
single ordered error-routing form; no list-only or `default` spelling is
accepted:

~~~yaml
on_error:
  routes:
    - kinds: [authentication, validation]
      next: manual_review
  fallback:
    next: failed
~~~

`routes` is a non-empty ordered list. `kinds` contains one or more distinct
`ActivityFailureKind` values and no value may appear in more than one route.
`fallback` is mandatory, has exactly one `next`, and handles every activity
failure not selected by an earlier route. It is therefore the declared outcome
for every non-retried connector class not named above, including `invariant`,
and for `retry_exhausted`. The closed kinds are the eight
`ConnectorErrorClass` values from Spec 006 (`transport`, `timeout`,
`http_429`, `http_5xx`, `authentication`, `validation`, `permanent`, and
`invariant`) plus the worker-only `retry_exhausted` outcome. `retry_on` may
name only `transport`, `timeout`, `http_429`, and `http_5xx`; the worker
retries a matching connector failure only while attempts remain. Metadata
validation rejects an absent `fallback`, an empty/duplicate/unknown route
kind, a duplicate route kind, a non-retryable `retry_on` entry, or an invalid
target. A state has exactly one kind discriminator: terminal, activity,
`wait_for_signal`, `wait_for_command`, or timer waiting. An activity's
`on_success`/`on_error` and a wait state's nested `on_signal`/`timeout` are
fields of that one kind, not additional state kinds. `set` may consume declared
input, state, result, signal, or Spec 004 rule values; it cannot read a table,
select a runtime role, or execute code.

A command or process may accept a decision value only into an exactly typed data destination
or map a declared enum at deploy time to fixed action/state targets; they
never bind a generic output to role/permission/command/connector identifiers.

## 3. Durable data model and execution

The deploy-time migration creates engine-internal tables in donat:

- process_events: append-only starts, signals, timer firings, and activity
  completions; unique (process_name, idempotency_key) where a source defines
  a key;
- process_instances: current state, state JSON, version, status, timestamps;
- process_activity_jobs: claimed connector invocation intent, result/error,
  attempts, retry time, and idempotency key;
- process_transition_logs: immutable audit record of every claimed event,
  state transition, command result, and connector attempt.

All transitions use optimistic instance versioning plus FOR UPDATE SKIP LOCKED.
Retried delivery can observe the same event more than once, so the transition
record has a unique (instance_id, event_id). A connector job has a logical
activity ID derived when the state transition enqueues it (the pinned process
revision, triggering event, and declared state name), not from an attempt.
Every attempt and lease takeover uses the one idempotency key derived from that
logical activity ID. A second claim becomes a no-op rather than a duplicated
business transition.

Timer scheduling writes a process event with available_at; no in-memory sleep
is business state. Removing a process definition prevents new starts, but its
immutable deployed revision remains available until every pinned instance has
reached a terminal state; existing work is never silently discarded.

## 4. Inbound signals and connector completion

An inbound connector webhook is verified by that connector module before it
can create a process signal. The signal mapping is deploy-time metadata and
binds only declared payload fields. An unknown instance, duplicate provider
ID, failed signature, or guard denial records an audit outcome and does not
advance an instance.

Connector completion appends a process event in the same database transaction
that marks its activity job finished. The process runner consumes that event to
run on_success or the matching on_error route. Compensation is an ordinary
explicit state with a command or activity; it is never an implicit reverse-SQL
algorithm.

## 5. Test-first acceptance contract

| Behavior | First failing test | Regression proof |
| --- | --- | --- |
| Command starts one instance | commands/process conformance fixture | command result and pinned process-start request commit atomically; the start worker creates one initial event and instance |
| Worker crash after claim | process integration test | restart completes exactly one transition audit record |
| Connector retry | recording connector stub | stable idempotency key is reused and attempts are logged |
| Duplicate Stripe webhook | webhook conformance fixture | exactly one signal transition occurs |
| Timer fires after restart | integration test with controlled clock | process reaches timeout state without in-memory state |
| Compensation | order cancellation fixture | declared compensation command runs, no hidden rollback |
| Removed definition | migrate/reload integration test | pending event is auditable error, not silently dropped |

All tests build the actual donat binary and use a Postgres suite database. The
worker must be exercised with at least two engine processes to prove the claim
and idempotency rules.

## 6. Reference porting plan

| Upstream | Immutable revision | Files/behavior used | License and treatment |
| --- | --- | --- | --- |
| [temporalio/sdk-rust](https://github.com/temporalio/sdk-rust/tree/d2769368df9077a311537431ff4594c9c14db4e7) | d2769368df9077a311537431ff4594c9c14db4e7 | activity idempotency, durable history, replay-oriented test thinking | MIT; behavioral reference only in Phase 1; no Temporal client, protocol, or source files are imported because Donat must not require a Temporal service |
| Donat crates/server/src/events.rs and cron.rs | current Donat revision | deploy-time reconcile versus runtime poller, retry logging, SKIP LOCKED ownership | native implementation reference and direct extension point |

Any future source port from Temporal must name exact files and retain its MIT
notice. The first implementation is deliberately a smaller independent state
machine over the Donat journal; importing Temporal's service dependency would
violate the one-binary architecture.


## 7. Canonical metadata and definition revision

The following is the only canonical Phase-1 shape and resolves all bindings
explicitly:

~~~yaml
- name: checkout_order
  source: default
  start:
    command: create_order
    input:
      order_id: { command_result: order_id }
      request_id: { command_argument: request_id }
    idempotency_key: { command_argument: request_id }
  input:
    order_id: uuid!
    request_id: uuid!
  state:
    order_id: uuid!
    checkout_url: string
  initial_state:
    order_id: { input: order_id }
  initial: create_checkout
  cancellation:
    signal: cancel_checkout
    correlate:
      order_id: uuid!
    payload: { reason: string! }
    on_cancel: cancelled
  states:
    create_checkout:
      activity:
        connector: stripe
        operation: create_checkout_session
        input:
          client_reference_id: { input: order_id }
        timeout:
          schedule_to_start: 5m
          start_to_close: 30s
        retry:
          retry_on: [transport, timeout, http_429, http_5xx]
          max_attempts: 5
          initial_interval: 10s
          max_interval: 5m
          jitter: deterministic_full
      on_success:
        set:
          checkout_url: { result: url }
        next: awaiting_payment
      on_error:
        routes:
          - kinds: [authentication, validation]
            next: manual_review
          - kinds: [permanent, invariant, timeout, retry_exhausted]
            next: failed
        fallback:
          next: failed
    awaiting_payment:
      wait_for_signal:
        connector: stripe
        event: checkout.session.completed
        provider_event_id: { event: id }
        correlate:
          order_id: { event: data.object.client_reference_id }
        on_signal:
          guard:
            rule: payment_matches_order
            with:
              order_id: { state: order_id }
              checkout_order_id: { event: data.object.client_reference_id }
          command:
            name: mark_order_paid
            run_as_role: payment_worker
            input:
              order_id: { state: order_id }
          next: completed
          on_rejection: failed
        timeout:
          after: 30d
          on_timeout:
            command:
              name: expire_order
              run_as_role: order_worker
              input:
                order_id: { state: order_id }
            next: expired
            on_rejection: failed
    completed: { terminal: true }
    manual_review: { terminal: true }
    failed: { terminal: true }
    expired: { terminal: true }
    cancelled: { terminal: true }
~~~

source is required and must be the same Postgres source used by the starting
command. input describes immutable instance input. state describes the only
mutable user-visible process data. initial_state binds every required state
field from input, literals, or Spec 004 rules; it cannot read a table or
environment variable.

The canonical form has exactly one start command in Phase 1. A command may
start multiple processes through separate Spec 003 effects, but one process
definition cannot have multiple start sources. A state has exactly one kind
discriminator; activity transition fields do not create another kind.
`wait_for_signal` contains one verified connector signal mapping and one
`on_signal` transition; its optional nested `timeout` has exactly `after` and
`on_timeout` transition keys. `wait_for_command` has the same nested
`on_signal` and optional `timeout` form, but additionally declares its exact
command-signal name, typed payload, and correlation. A timer-only state uses
`timer: { after: <duration>, on_timeout: <transition> }`; `after` is never a
sibling of `wait_for_signal`, `wait_for_command`, or another state kind. The
optional cancellation block is similarly typed and is accepted while the
instance is non-terminal. Only a validated Spec 003 `signal_process` effect
may append a command signal or cancellation; this is the domain-safe mechanism
for approval, correction, and cancellation without a generic
process-management endpoint. Start input, every state set, activity input,
command input, signal payload, correlation field, and rule binding is checked
against its declared type at metadata validation time.

During donat migrate --metadata-dir, the engine canonicalizes each valid
process definition and derives a revision from its canonical JSON plus the
rules, command-definition fingerprints, connector module/operation versions,
connector endpoint and credential identities, and non-secret connector
configuration fingerprints it references. It inserts immutable rows
into donat.process_definition_versions:

| Column | Meaning |
| --- | --- |
| process_name, revision | immutable primary key |
| canonical_definition | validated canonical JSON, including resolved rule AST hashes |
| source_name | owning Postgres source |
| runtime_abi | minimum compatible process-runtime ABI |
| status | active or retired |
| deployed_at | deployment audit timestamp |

A new metadata definition creates a new revision; it never overwrites a
revision used by an instance. A compiled command effect writes the active
revision into its start request in the same CTE as the command result; the
start worker must use that stored revision, never resolve a newer active one.
An instance stores process_name and revision, so reload, rollback, and removal
cannot reinterpret in-flight work. A worker may claim an event only when its
binary supports the instance runtime ABI and every pinned module/operation
version; deployment drains or fences incompatible workers before activation.
Retiring a definition prevents new starts but retains its revision until no
non-terminal instance references it. The serve command performs no definition
reconciliation or DDL; validate reports a metadata revision that has not been
deployed by migrate.

Every individual process input, state, activity input/result, signal payload,
and redacted error JSON is bounded to 256 KiB after canonical serialization.
Inbound raw-body limits are set by the connector module before parsing. A
deployment must keep database migrations backward-compatible with every active
process revision. `migrate --metadata-dir` verifies active pinned command and
catalog references before publishing the metadata snapshot; it fails rather
than serving an instance whose referenced command, table, or column disappeared.

## 8. Durable tables and state transitions

The migration creates the following internal tables. They are engine catalog
tables, are not automatically tracked, and have no GraphQL, REST, MCP, or
runtime-management mutation surface.

| Table | Essential columns and unique keys | Purpose |
| --- | --- | --- |
| donat.process_instances | id, process_name, revision, source_event_id, status, current_state, state_json, input_json, version, created_at, updated_at; unique(process_name, revision, source_event_id) | one durable state-machine instance |
| donat.process_events | id, instance_id nullable, process_name, revision, kind, payload_json, idempotency_key, available_at, status, attempts, created_at; unique(process_name, revision, idempotency_key) when key is non-null | immutable start, signal, timer, and completion events |
| donat.process_start_requests | id, process_name, revision, input_json, command_invocation_id, effect_position, idempotency_key, status, created_at; unique(command_invocation_id, effect_position) | idempotent command-to-process start hand-off pinned at command execution |
| donat.process_signal_requests | id, process_name, signal_name, correlation_json, payload_json, command_invocation_id, effect_position, idempotency_key, status, created_at; unique(command_invocation_id, effect_position) | idempotent typed command-to-process signal hand-off |
| donat.process_activity_jobs | id, instance_id, enqueued_from_event_id, state_name, logical_activity_id, connector_instance, operation, serialization_key nullable, input_json, idempotency_key, status, attempts, available_at, schedule_to_start_deadline, start_to_close_deadline, lease_token, lease_expires_at, last_error_json; unique(instance_id, logical_activity_id) | intent to perform one external operation |
| donat.process_transition_logs | id, instance_id, event_id nullable, activity_job_id nullable, from_state, to_state, outcome, definition_revision, redacted_context, created_at; unique(instance_id, event_id, outcome) | append-only audit trail |
| donat.process_inbound_events | connector_instance, provider_event_id, received_at, payload_digest, verification_outcome; unique(connector_instance, provider_event_id) | provider webhook deduplication and audit |

A worker transition uses a short database transaction: lock one due event with
FOR UPDATE SKIP LOCKED, verify its instance revision and status, apply the
state-machine transition with instance version equality, insert the transition
log, enqueue a timer or activity job if required, and mark the event consumed.
The transition transaction contains no HTTP and no wait for another worker.
It gives every job a logical_activity_id once; retries never create a new
logical activity or provider idempotency key.

An activity job is claimed separately. In one short committed transaction the
worker reserves an operation-scoped, Postgres-coordinated capacity permit,
sets status=running, a random lease_token, start_to_close_deadline, and a lease
that outlives that deadline by a fixed safety margin. If no permit is available,
the job remains scheduled; the worker does not hold a transaction while waiting.
The worker then calls the connector outside a transaction. It records a result
only with an UPDATE whose lease_token still matches and, in that same
transaction, inserts the single activity-completed process event. A worker that
wakes after its lease is lost cannot transition the instance; it records a
stale-attempt audit entry. Another worker may retry after lease expiry. This
guarantees at-least-once execution, never exactly-once external delivery.

The migration owns internal capacity-reservation rows keyed by connector
instance and operation, with an optional canonical serialization key. Every
engine process consults them before an outbound call, so configured
max-in-flight, rate limits, and same-resource serialization are global to the
deployment, not per worker. A reservation expires with its activity lease; an
operation is never rate-limited or serialized by an in-memory counter alone.

schedule_to_start is measured from job enqueue with the database clock. If a
scheduled job reaches that deadline before a claim, the worker appends a typed
timeout event without calling the provider. start_to_close is measured from a
successful claim; ActivityContext carries that deadline and a completion after
it is stale. Phase 1 has no activity heartbeats: a long-running interaction is
modelled by a connector start activity plus a timer or verified signal, not by
an indefinitely extended HTTP call.

The initial retry policy is explicit on every activity. max_attempts includes
the first attempt. The retry schedule is min(max_interval,
initial_interval * 2^(attempt-1)); deterministic full jitter is derived from a
stable hash of logical_activity_id and attempt. A provider Retry-After value
can postpone, but never accelerate, the next attempt. Reaching the limit
appends a typed retry_exhausted event. There is no infinite implicit retry.

## 9. Signals, time, and authorization

The connector endpoint is POST /v1/connectors/{connector_instance}/webhooks.
It accepts raw bytes and headers, invokes the compiled module's verifier before
JSON parsing, and stores the verification outcome. A valid provider event ID is
inserted into process_inbound_events before a correlated process signal is
created; the unique key makes duplicate delivery harmless. An invalid signature
is recorded with redacted metadata, returns the connector-defined rejection
status, and never reaches a process instance.

correlate maps explicit verified event fields to declared process state fields.
It must identify at most one non-terminal instance pinned to the active or
retired definition revision. Zero or multiple candidates are logged as
unmatched or ambiguous and do not transition a process. Process state values
are never inferred from a provider event without a declared mapping.

An ordinary domain command may also deliver a typed signal through the
validated Spec 003 signal_process effect. It has the same exact correlation,
payload typing, idempotency, and append-only audit rules as a verified inbound
signal. It is the only Phase-1 recovery and cancellation path. A process that
declares a cancellation signal must declare an on_cancel transition. Consuming
that signal moves the instance to the declared state in one transition,
cancels still-scheduled activity jobs, and makes later in-flight completions
audit-only; it never attempts to retract a request already accepted by a
provider. Compensation, if required, is an explicit state after on_cancel.

Because a command signal may correlate an active instance on an older
definition revision, `migrate --metadata-dir` rejects a changed or removed
wait/cancellation signal contract while a non-terminal pinned revision still
declares it. A new revision may add a signal, but it may not reinterpret the
name, correlation shape, or payload type accepted by older live instances.

Phase 1 does not buffer signals. If a correlated non-terminal instance is not
currently receptive to that exact wait/cancellation signal, the inbound event
or command-signal request receives the immutable unexpected_state audit outcome
and does not create a future transition. A terminal or unknown instance is
similarly audit-only. This keeps signal order visible and deterministic; a
later buffering/TTL feature must introduce an explicit persisted contract.

Due timers and leases compare against the database clock in the owning source.
Tests use a controllable database time abstraction rather than wall-clock
sleep. The process worker synthesizes only the run_as_role configured on the
transition; that role must appear in the target command's permissions and have
the needed table permissions. It has no X-Donat-Admin-Secret-derived bypass,
does not forward caller headers, and cannot choose a role from event data.

There is no generic runtime process cancel, retry, replay, or definition
mutation API. Operators inspect internal journal rows and metrics through
direct database access or deployment-owned observability; no role or HTTP
endpoint bypasses a process definition. A product exposes customer-visible
progress through ordinary tracked business tables updated by declarative
commands.

The binary may provide two read-only operational subcommands:
`donat process inspect --source <name> --instance <uuid>` emits a redacted
timeline from the internal journal, and `donat process verify-history` applies
stored events and stored command/activity results to the pinned definition and
checks recorded before/after state hashes. Neither command invokes a connector
or command, changes a lease or journal row, accepts arbitrary SQL, or exposes
an HTTP surface. A mismatch is an operational invariant failure with a
non-zero exit status, not a replay or recovery action.

## 10. Failure taxonomy and completion semantics

| Outcome | Event/job action | Process result |
| --- | --- | --- |
| guard evaluates false | consume event and log guard_false | remains in current state; no retry |
| command business rejection | consume event and log command_rejected | follows required explicitly declared on_rejection |
| connector retryable error | release job to scheduled with computed retry time | state remains unchanged |
| schedule-to-start or start-to-close timeout | append typed timeout event | follows matching on_error route or mandatory fallback |
| connector authentication, validation, permanent, or invariant error | append typed activity-failed event | follows matching on_error route or mandatory fallback |
| retry limit reached | append typed retry_exhausted event | follows matching on_error route or mandatory fallback |
| capacity unavailable | retain scheduled job without a provider call | state remains unchanged |
| serialization key busy | retain scheduled job without a provider call | state remains unchanged |
| domain cancellation signal | consume signal, cancel unclaimed jobs | follows declared on_cancel route |
| early or late domain/provider signal | record unexpected_state audit outcome | no buffered future transition |
| worker crash before job result commit | lease expires | activity is retried with the same idempotency key |
| duplicate provider signal | unique insert conflicts | recorded duplicate; no second process event |
| stale worker completion | lease-token update affects zero rows | logged stale; no state transition |
| retired definition with instance | load pinned revision | continues normally |
| missing pinned revision | worker stops that event and logs invariant_failure | never invents a new transition |

A terminal instance rejects all later signals and timers as audit-only events.
A compensating state is a normal declared state. It receives the same retry,
idempotency, role, and audit rules as every other state; compensation never
tries to reverse arbitrary prior SQL.

## 11. Expanded test-first matrix

| Test ID | Harness level | Required proof |
| --- | --- | --- |
| process_definition_revision_is_pinned | migrate plus integration | instance started on revision A completes with A after definition B deploys |
| process_start_request_pins_revision | command plus deployment integration | command CTE produced under A starts A even when its outbox is consumed after B deploys |
| process_revision_runtime_abi_is_fenced | rolling-deploy integration | an incompatible binary cannot claim an instance until compatible workers drain or are available |
| process_active_dependency_is_not_removed | migrate validation | removal or incompatible change of a pinned command/catalog reference fails deployment |
| process_retire_blocks_new_starts | conformance | removed process cannot be newly started; existing instance remains runnable |
| process_activity_does_not_hold_tx | integration with recording connector | database lock is released before connector server is allowed to respond |
| process_lease_takeover_is_safe | two engine processes | expired lease causes retry; stale first completion cannot transition state |
| process_retry_key_is_stable | connector stub | all attempts and lease takeovers use one logical activity idempotency key |
| process_activity_timeouts_are_distinct | controlled DB clock and connector stub | schedule-to-start makes no call; start-to-close yields stale completion |
| process_activity_capacity_is_global | two engine processes and recording connector | per-operation limit and rate limit hold across workers |
| process_retry_jitter_is_reproducible | unit plus integration | same activity ID and attempt produces the same delayed retry |
| process_error_routes_are_total | metadata plus activity integration | the only `on_error` grammar has routes plus fallback; every non-retried class, including invariant, and retry exhaustion take a declared path |
| process_wait_signal_timeout_is_one_state_kind | metadata plus timer integration | a wait state accepts nested timeout; a sibling `on_signal`/`after` shape and multiple outer state kinds are rejected |
| process_signal_signature_before_parse | HTTP endpoint test | malformed JSON with bad signature is rejected before event JSON handling |
| process_signal_deduplicates | two identical webhook requests | one inbound row and one transition |
| process_correlation_is_unique | integration | zero and multiple candidate instances are audit-only |
| process_signal_is_not_buffered | command plus integration | early/late signal records unexpected_state and cannot advance a later wait state |
| process_domain_cancel_is_declarative | command plus integration | typed command signal cancels unclaimed work and takes declared on_cancel only |
| process_operational_history_is_read_only | CLI integration | inspect redacts payloads; verify-history detects tampered state hash without command/connector I/O |
| process_timer_survives_restart | controlled DB clock and restart | due timer transitions exactly once |
| process_run_as_role_is_checked | metadata plus conformance | undeclared or under-permitted role fails validation or command permission |
| process_no_management_api | route and schema test | no process mutation/query field is published |

The process integration suite runs the real donat binary against Postgres with
two independently started engine processes. It must run after cargo build -p
donat-server --bin donat, then the complete conformance crate.

## 12. Reference extraction ledger

| Reference | Immutable paths | Treatment | Donat test mapping |
| --- | --- | --- | --- |
| temporalio/sdk-rust at d2769368df9077a311537431ff4594c9c14db4e7 | ARCHITECTURE.md, crates/sdk-core, crates/workflow | MIT behavioral reference for replay, activity failure, and history discipline; no client or protocol port | process revision, retry, and stale-completion integration tests |
| Donat crates/server/src/events.rs | current source | durable event capture, retry-log naming, and deployment split | event-to-process journal migration tests |
| Donat crates/server/src/cron.rs | current source | due-time materialization and multi-instance claim precedent | timer and clock tests |
| Donat crates/server/src/migrate.rs | current source | migrate/validate boundary | definition revision deployment tests |

Temporal is intentionally not a dependency. If a future change ports an MIT
source file, its exact file path, copyright notice, destination path, and
upstream-to-Donat test mapping must be added to the central reference register
before import.


## 13. Component ownership boundaries

| Area | Required ownership | Prohibited shortcut |
| --- | --- | --- |
| Metadata and deployment validation | crates/metadata plus migrate/validate integration | reading process YAML separately in a background task |
| Durable runtime | crates/server process module and server state | a separate workflow service or in-memory task state |
| Tables and helper functions | refinery migrations invoked only by donat migrate | DDL at serve boot or a runtime process-admin endpoint |
| Command execution | existing schema/IR/sqlgen command path under explicit run_as_role | direct hand-written SQL from process state |
| Activities and webhooks | connector registry plus process job/signal tables | holding a transaction while HTTP waits |
| Time, concurrency, and capacity | database clock, leases, SKIP LOCKED, and Postgres-coordinated permits | Tokio sleep as durable state, an in-memory limiter, or exactly-once claims |
| Operational diagnosis | read-only process CLI over redacted pinned journal data | a process-management HTTP endpoint, replay action, or journal mutation |
| Proof | native conformance plus two-process Postgres integration suite | a single-worker mocked unit test |

The process module is allowed to issue ordinary journal DML statements because
it is a background runtime, but every transition and connector completion must
be a short explicit transaction with a reviewed query. It never performs
row-by-row business-table mutations outside a declarative command.
