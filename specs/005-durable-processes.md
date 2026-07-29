# Spec 005 — Durable declarative processes

Status: proposed. Processes orchestrate long-running business work after a
command commits. They are not microservices, serverless functions, an
administrator surface, or a replacement for the existing GraphQL Actions
endpoint.

## 1. Goal and hard boundary

A process is a metadata-defined durable state machine. A domain command writes
a pinned process start or signal request in its one Postgres CTE statement.
Source-local workers later consume that committed request, execute explicit
role-qualified commands, schedule timers, or lease connector activities.

The runtime remains part of the single `donat` Rust binary and stores all
durable process state in the owning Postgres source. Crates and modules are
compilation boundaries, not services. This feature adds no workflow service,
plugin runtime, JavaScript/WASM escape hatch, dynamic connector, runtime DDL,
admin role, permission bypass, generic process-management HTTP API, or
distributed transaction.

The serving binary never creates or reconciles DDL. A command remains exactly
one Postgres statement and assembles its response in Postgres. Background
process journal work may use several explicit journal statements inside a
short transaction, but it never performs row-by-row business-table mutation:
business data changes only through the existing declarative command statement.
No external request is made while a database transaction is open.

## 2. Cycle-free compilation and public contracts

### 2.1 Dependency ownership

The dependency direction is:

~~~text
donat-server -> donat-schema -> donat-ir -> donat-metadata
      |               |             ^
      +---------------+-------------+
~~~

`donat-sqlgen` also depends on `donat-ir`. `donat-schema` never depends on
`donat-server`.

The implementation adds `crates/ir/src/value_contract.rs` for the shared,
SQL-free normalized value contract. It exports these immutable types:

~~~rust
pub struct ValueContractCatalog {
    pub roots: BTreeMap<String, ValueContractField>,
    pub named_objects: BTreeMap<String, ValueObjectContract>,
}

pub struct ValueContractField {
    pub required: bool,
    pub value: ValueContract,
}

pub struct ValueContract {
    pub nullable: bool,
    pub shape: ValueContractShape,
}

pub enum ValueContractShape {
    Scalar { name: String },
    Enum { name: String, values: Vec<String> },
    Object { fields: BTreeMap<String, ValueContractField> },
    List { element: Box<ValueContract> },
    Ref { name: String },
}

pub struct ValueObjectContract {
    pub fields: BTreeMap<String, ValueContractField>,
}
~~~

`roots` and every object field retain missing-value requiredness separately
from value nullability. `Ref` may name only an entry in `named_objects`, which
makes recursive input objects finite and self-contained. Map order is
canonical lexical order; enum value order is declared order. Contract
assignment and JSON validation are implemented once in this lower module and
are reused by commands, processes, and connectors.

### 2.2 Command descriptors

`crates/schema/src/commands.rs` publishes one deterministic descriptor for
every source-qualified command accepted by the existing command compiler:

~~~rust
pub struct CommandDescriptor {
    pub source: String,
    pub name: String,
    pub arguments: ValueContractCatalog,
    pub result: ValueContractCatalog,
    pub allowed_roles: BTreeSet<String>,
    pub required_session_variables:
        BTreeMap<String, BTreeMap<String, ValueContract>>,
    pub definition_fingerprint: String,
}
~~~

The outer key of `required_session_variables` is an allowed explicit role; the
inner mapping contains every session-variable name and its single compatible
contract required by that role's effective table permissions, command
idempotency scope, and command-effect bindings. Incompatible uses of one name
are a deployment error.

`definition_fingerprint` is lowercase SHA-256 over versioned canonical JSON
containing the source, name, recursive argument/result contracts, allowed
roles, required session-variable contracts, steps, Rule artifact hashes,
idempotency declaration, and canonical raw effect declarations. The raw effect
material contains process/signal names and raw binding shapes, but no resolved
process revision. Consequently a process revision may include a command
fingerprint without creating a fingerprint cycle when that command's effect is
later pinned to the process revision. Descriptors contain no request value,
resolved session value, database credential, or secret.

The existing command compiler remains the only command compiler.
`CompiledCommand` owns its `CommandDescriptor`; process compilation never
reconstructs command types from raw metadata.

### 2.3 Connector descriptors

`crates/server/src/connectors/mod.rs` publishes immutable
`ConnectorOperationDescriptor` values:

~~~rust
pub struct ConnectorOperationDescriptor {
    pub instance_name: String,
    pub module_name: String,
    pub module_semantic_version: String,
    pub runtime_abi: u32,
    pub operation_name: String,
    pub operation_version: String,
    pub input: ValueContractCatalog,
    pub output: ValueContractCatalog,
    pub idempotency: ConnectorIdempotencySupport,
    pub capacity: ConnectorCapacityContract,
    pub endpoint_identity: String,
    pub credential_identity: String,
    pub configuration_fingerprint: String,
}

pub enum ConnectorIdempotencySupport {
    StableKeyHeader { name: String },
    Unsupported,
}

pub struct ConnectorCapacityContract {
    pub max_in_flight: u32,
    pub rate_limit: ConnectorRateLimitContract,
    pub serialize_by: Option<String>,
}

pub struct ConnectorRateLimitContract {
    pub permits: u32,
    pub per_milliseconds: u64,
    pub burst: u32,
}

pub struct ConnectorInboundEventDescriptor {
    pub instance_name: String,
    pub module_name: String,
    pub module_semantic_version: String,
    pub runtime_abi: u32,
    pub event_name: String,
    pub event_version: String,
    pub output: ValueContractCatalog,
    pub provider_event_id_field: String,
    pub endpoint_identity: String,
    pub credential_identity: String,
    pub configuration_fingerprint: String,
}

pub struct ConnectorDescriptorCatalog {
    pub operations:
        BTreeMap<(String, String), ConnectorOperationDescriptor>,
    pub inbound_events:
        BTreeMap<(String, String), ConnectorInboundEventDescriptor>,
}
~~~

The capacity contract contains `max_in_flight`, the exact
`permits`/`per`/`burst` rate policy, and the optional typed scalar
`serialize_by` input name. The idempotency contract states whether a stable
activity key is supported and, when applicable, the fixed header identity.
The configuration fingerprint retains the current non-secret module,
operation, endpoint, credential-class, capacity, protocol, environment-name,
and resolved-HTTP-endpoint-digest material; it never retains a resolved secret
or raw environment-derived base URL.

Verified inbound events use a sibling
`ConnectorInboundEventDescriptor` containing instance/module/version/ABI,
event name and version, exact verified output contract, provider-event-ID
field, endpoint/credential identities, and the same non-secret configuration
fingerprint. This descriptor is produced by the same compiled connector
registry, not a process-owned protocol compiler.

Declarative HTTP operation metadata adds an explicit
`input: BTreeMap<String, String>` type mapping. Every
`{input.<name>}` path slot and every `{ input: <name> }` query, header, and
recursive body slot must name an entry in that mapping and be assignable to
its normalized contract. Dispatch rejects missing required inputs, null for a
non-null contract, type mismatch, and undeclared extra inputs before DNS or
request rendering. A declared input that is used only by `serialize_by` is
valid; every other declared input must be used by the fixed request template.
Response field type declarations form the output contract.

Stripe does not accept a metadata-defined HTTP schema. The built-in
`stripe` module publishes a fixed descriptor for
`checkout.create_session`, operation version `v1`, module semantic version
`0.1.0`, and runtime ABI `1`. Its input is the exact object
`{ mode: CheckoutMode!, success_url: string!, cancel_url: string!,
client_reference_id: uuid!, line_items: [CheckoutLineItem!]! }`, where
`CheckoutMode` is `payment | subscription | setup` and
`CheckoutLineItem` is `{ price: string!, quantity: uint64! }`. Its output is
`{ id: string!, url: string!, status: string!, expires_at: int64! }`.
The fixed verified `checkout.session.completed` event exposes
`{ provider_event_id: string!, checkout_session_id: string!,
client_reference_id: uuid!, payment_status: string! }`.

### 2.4 Process-effect catalog and candidate order

`crates/schema/src/process_effects.rs` owns the server-neutral,
source-qualified catalog:

~~~rust
pub struct ProcessEffectContractCatalog {
    pub sources:
        BTreeMap<String, BTreeMap<String, ProcessEffectContract>>,
}

pub struct ProcessEffectContract {
    pub process_name: String,
    pub active_revision: String,
    pub start_input: ValueContractCatalog,
    pub signals: BTreeMap<String, ProcessSignalEffectContract>,
}

pub struct ProcessSignalEffectContract {
    pub signal_name: String,
    pub contract_revision: String,
    pub correlation: ValueContractCatalog,
    pub payload: ValueContractCatalog,
    pub compatible_revisions: BTreeSet<String>,
}
~~~

For each process it contains the active revision and exact start input; for
each command signal it contains the signal name, correlation/payload
contracts, the revision whose signal contract was checked, and the set of
retained revisions with an identical signal-contract fingerprint. It contains
no runtime handle or journal access.

`crates/server/src/processes/definition.rs` owns the only process compiler and
its immutable `CompiledProcessCatalog`. Candidate construction in
`crates/server/src/state.rs` performs exactly this order:

1. `compile_rule_catalog` compiles Rules.
2. `compile_command_catalog` compiles source-qualified commands and their
   pre-process `CommandDescriptor` values without resolving process revisions.
3. `ConnectorRegistry::descriptors` supplies the already compiled connector
   operation and inbound-event descriptors.
4. `processes::definition::compile_process_catalog` compiles source-qualified
   process definitions against those command, Rule, and connector descriptors
   and derives immutable revisions.
5. `ProcessEffectContractCatalog::from_processes` creates the cycle-free
   schema contract.
6. `finalize_command_effects` validates and pins raw command effects without
   changing the pre-process command fingerprint.
7. `CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects`
   compiles the serving schema.
8. `processes::reconcile::validate_serving_catalogs` performs read-only checks
   that every compiled revision is deployed in its real owning source before
   the candidate `Engine` is published.

Each `Engine` snapshot retains the finalized command catalog and one immutable
source-qualified `CompiledProcessCatalog` beside the schema. No compiler is
duplicated and no dependency points from `donat-schema` to `donat-server`.

## 3. Canonical process metadata

`processes.yaml` is a top-level metadata section. The following shape is the
normative Phase-1 grammar:

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
    payload:
      reason: string!
    on_cancel: cancelled
  states:
    create_checkout:
      activity:
        connector: stripe
        operation: checkout.create_session
        input:
          mode: { literal: payment }
          success_url: { literal: "https://shop.example/success" }
          cancel_url: { literal: "https://shop.example/cancel" }
          client_reference_id: { input: order_id }
          line_items:
            literal:
              - { price: price_standard, quantity: 1 }
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
        provider_event_id: { event: provider_event_id }
        correlate:
          order_id: { event: client_reference_id }
        on_signal:
          guard:
            rule: payment_matches_order
            with:
              order_id: { state: order_id }
              checkout_order_id: { event: client_reference_id }
          command:
            name: mark_order_paid
            run_as_role: payment_worker
            input:
              order_id: { state: order_id }
            session_variables: {}
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
              session_variables: {}
            next: expired
            on_rejection: failed
    completed: { terminal: true }
    manual_review: { terminal: true }
    failed: { terminal: true }
    expired: { terminal: true }
    cancelled: { terminal: true }
~~~

`name`, `source`, `start`, `input`, `state`, `initial_state`, `initial`,
`cancellation`, and `states` use deny-unknown-fields metadata types.
`cancellation` is optional, but when present it requires the exact signal,
typed correlation, typed payload, and `on_cancel` target shown above.
`start` is required and has exactly one command in Phase 1.
That same-source command must contain exactly one raw `start_process` effect
targeting this process, and its canonical input/idempotency bindings must equal
the process `start` mapping. The compiler rejects a missing, duplicate, or
disagreeing declaration; neither side silently overrides the other.

A state has exactly one kind discriminator: `terminal`, `activity`,
`wait_for_signal`, `wait_for_command`, or `timer`. Activity
`on_success`/`on_error` and wait-state `on_signal`/nested `timeout` are fields
of that kind, not additional kinds. A timer-only state is
`timer: { after: <duration>, on_timeout: <transition> }`. Phase 1 does not
accept a sibling `after` or `on_signal` shorthand.

Every activity uses the one total ordered error form:

~~~yaml
on_error:
  routes:
    - kinds: [authentication, validation]
      next: manual_review
  fallback:
    next: failed
~~~

`routes` is non-empty. A kind may occur in only one route. `fallback` is
mandatory and handles every unmatched non-retried failure, including
`invariant` and worker-generated `retry_exhausted`. The closed connector kinds
are `transport`, `timeout`, `http_429`, `http_5xx`, `authentication`,
`validation`, `permanent`, and `invariant`; `retry_on` accepts only the first
four.

Every transition that invokes a command requires `name`, fixed
`run_as_role`, exact `input`, explicit `session_variables`, `next`, and
`on_rejection`. `session_variables` is a closed typed mapping: it contains
every name required for that role by `CommandDescriptor` and no other name.
Allowed bindings are literals and declared process input/state,
prior-command result, verified signal, or Rule data available in that
transition context. They never come from caller headers, ambient GraphQL
session data, connector-selected data outside the verified event descriptor,
or an event-selected role.

All start input, initial state, state `set`, command input/session variables,
activity input, signal payload/correlation, and Rule bindings are checked for
exact contract assignment at deployment. A decision value may flow only into
an exactly typed data destination or through a deploy-time fixed enum mapping;
it cannot select a role, permission, command, connector, source, or process.

Every process-owned input, state, activity input/result, signal payload, and
redacted error value is limited to 256 KiB after canonical serialization.
Connector modules enforce their raw-body limits before parsing.

## 4. Strict source locality and deployment commands

A process source is required and must name a Postgres source. The starting
command, every transition command, every `start_process` effect, and every
`signal_process` effect resolve only within that exact source. A same-named
command or process in another source is not a fallback. Cross-source
references fail deployment before a journal row can be written.

A connector instance referenced by a process activity or inbound wait is
bound to exactly one process source in a deployment. Referencing the same
connector instance from processes in two sources fails global candidate
compilation. This makes capacity coordination, webhook routing, database time,
and journal writes source-local. The runtime starts separate worker loops and
uses separate Postgres pools for each process-owning source.

Deployment uses these exact forms:

~~~text
donat migrate --migrations-dir migrations --metadata-dir <dir> --source <name>
donat validate --metadata-dir <dir> --source <name>
~~~

`--source` selects one metadata source, requires it to be Postgres, resolves
that source's own `configuration.connection_info.database_url`, and uses the
global `--database-url`/`DONAT_DATABASE_URL` only as the existing explicit
fallback when the selected source has no connection URL. A missing
environment variable is an error; it is never silently replaced by another
source's URL.

Omitting `--source` is accepted only when the loaded metadata contains exactly
one Postgres source and its URL is unambiguous. Zero or multiple Postgres
sources is a usage error. `migrate` applies V1–V6 and reconciles event triggers
and process definitions only in the selected source. It never changes another
source. `validate` introspects only the selected source and reports that
source's catalog, command, process, deployed-revision, and compatibility
problems plus global metadata-only Rule/connector/source-binding problems.
It does not claim that unselected database catalogs were checked.

Consistency code constructs a catalog map containing only the selected source.
It must never clone one introspected `Catalog` across metadata source names.
`donat-schema::compile_command_source_catalog` and
`processes::definition::compile_process_source_catalog` are the source-scoped
compiler entry points used by migrate/validate; the global candidate compilers
compose the same entry points for all real source catalogs.
Serving startup separately resolves and introspects every real source URL,
compiles the complete candidate using those real catalogs, and performs
read-only deployed-revision validation. Serve performs no DDL or
reconciliation.

The native conformance harness always invokes migration as
`donat migrate --migrations-dir <workspace>/migrations --metadata-dir <suite>
--source default`. Multi-source deployment tests invoke the command once per
Postgres source with that source's real URL.

## 5. Immutable definitions and reconciliation

The compiler derives a lowercase SHA-256 revision from versioned canonical
JSON containing:

- the source-qualified canonical process definition;
- normalized process input/state/signal contracts;
- referenced Rule profile version and canonical AST hashes;
- referenced pre-process command descriptors and fingerprints;
- referenced connector operation/inbound descriptors and configuration
  fingerprints;
- the process runtime ABI.

The deploy-time reconciliation entry point is:

~~~rust
pub async fn reconcile(
    source_name: &str,
    database_url: &str,
    source_catalog: &donat_catalog::Catalog,
    compiled_processes: &CompiledSourceProcessCatalog,
    dependency_descriptors: &ProcessDependencyDescriptors,
) -> anyhow::Result<()>;
~~~

It inserts a missing immutable revision, leaves an identical deployment
unchanged, activates the selected revision, and retires omitted prior
revisions without deleting them. It rejects removal or an incompatible change
of a command, Rule, connector operation, runtime ABI, or wait/cancellation
signal contract still referenced by a non-terminal instance. A process
revision remains loadable until no live instance can use it.

A start request stores the active revision selected when the command statement
executes. The start worker never substitutes a newer active revision. A signal
request stores the revision whose signal contract finalized the effect.
It may match an instance on another retained revision only when reconciliation
recorded an identical signal name/correlation/payload contract fingerprint;
the worker never guesses compatibility from current metadata.

## 6. Execution-generation identity and resolved command effects

V6 adds `invocation_id uuid` to `donat.command_invocations`. Existing rows are
backfilled with distinct UUIDs and the column becomes non-null and unique.
The command's first execution generates a new UUID. An unexpired exact replay
returns the stored result and preserves that UUID. Reclaiming an expired
idempotency tuple is a new execution generation: it writes a new UUID even
though `(command_identity, scope_hash, key)` is reused.

`crates/ir/src/lib.rs` replaces `CommandEffectKind` with these closed resolved
values:

~~~rust
pub enum ResolvedCommandEffect {
    StartProcess(ResolvedStartProcessEffect),
    SignalProcess(ResolvedSignalProcessEffect),
}

pub struct ResolvedStartProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub input: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    pub command_invocation_id: CommandInvocationIdSource,
    pub effect_position: u32,
}

pub struct ResolvedSignalProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub signal_name: String,
    pub correlation: BTreeMap<String, CommandExecutionValue>,
    pub payload: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    pub command_invocation_id: CommandInvocationIdSource,
    pub effect_position: u32,
}

pub enum CommandInvocationIdSource {
    CurrentExecution,
}
~~~

`CurrentExecution` is a trusted closed reference, not metadata. SQLgen
resolves it to the concrete UUID returned by the command invocation CTE.
`effect_position` is the zero-based position in canonical command effect
order. All other fields are fully source- and type-resolved by the planner.

`plan_mutation.rs` lowers finalized effects into this IR. SQLgen inserts each
start or signal request from the same successful command CTE statement and
only from the `first` claim path. Every outbox row copies the concrete
`invocation_id` and is unique on
`(command_invocation_id, effect_position)`. Replay, changed-input rejection,
guard rejection, or any database error inserts no second outbox row.

The command's execution-generation identity and the process's semantic start
dedupe are different contracts. The former identifies one actual command
execution; the latter is `unique(process_name, start_idempotency_key)` on
instances and prevents two distinct command executions from starting the same
business process. Process audit and outbox rows have no foreign key to
`donat.command_invocations`, so command retention cannot delete or invalidate
process history.

There is no `enqueue_start` API. `processes/start.rs` and
`processes/inbound.rs` expose worker-side consumers only; no post-command
Rust insert, SQL function, GraphQL resolver, or HTTP handler can start or
signal a process directly.

## 7. Exact V6 journal schema

`migrations/V6__donat_processes.sql` is deploy-time-only. All tables are in
the `donat` schema, are not tracked automatically, and have no GraphQL, REST,
MCP, or management mutation surface. UUID primary keys default to
`gen_random_uuid()`. Every process-owned JSONB payload has
`check (pg_column_size(<column>) <= 262144)`.

The migration first alters the command journal:

~~~text
donat.command_invocations
  add invocation_id uuid;
  backfill every existing row with gen_random_uuid();
  set invocation_id not null;
  unique(invocation_id).
~~~

It then creates these exact process tables:

~~~text
donat.process_definition_versions(
  process_name text not null,
  revision text not null,
  source_name text not null,
  canonical_definition jsonb not null,
  dependency_descriptors jsonb not null,
  runtime_abi integer not null check (runtime_abi > 0),
  status text not null check (status in ('active','retired')),
  deployed_at timestamptz not null default now(),
  retired_at timestamptz,
  primary key(process_name, revision)
)
unique(process_name) where status = 'active';

donat.process_start_requests(
  id uuid primary key,
  source_name text not null,
  process_name text not null,
  revision text not null,
  input_json jsonb not null,
  command_invocation_id uuid not null,
  effect_position integer not null check (effect_position >= 0),
  idempotency_key text not null,
  status text not null
    check (status in ('pending','consumed','duplicate','failed')),
  instance_id uuid,
  created_at timestamptz not null default now(),
  consumed_at timestamptz,
  unique(command_invocation_id, effect_position),
  foreign key(process_name, revision)
    references donat.process_definition_versions(process_name, revision)
);

donat.process_instances(
  id uuid primary key,
  source_name text not null,
  process_name text not null,
  revision text not null,
  source_request_id uuid not null,
  start_idempotency_key text not null,
  status text not null
    check (status in ('running','terminal','failed','cancelled')),
  current_state text not null,
  input_json jsonb not null,
  state_json jsonb not null,
  version bigint not null default 0 check (version >= 0),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique(process_name, start_idempotency_key),
  unique(source_request_id),
  foreign key(process_name, revision)
    references donat.process_definition_versions(process_name, revision)
);

donat.process_events(
  id uuid primary key,
  instance_id uuid not null references donat.process_instances(id),
  process_name text not null,
  revision text not null,
  kind text not null check (kind in (
    'start','signal','timer','activity_succeeded','activity_failed',
    'retry_exhausted','command_rejected','cancellation'
  )),
  payload_json jsonb not null,
  idempotency_key text,
  available_at timestamptz not null default now(),
  status text not null check (status in ('pending','consumed','failed')),
  attempts integer not null default 0 check (attempts >= 0),
  created_at timestamptz not null default now(),
  consumed_at timestamptz,
  foreign key(process_name, revision)
    references donat.process_definition_versions(process_name, revision)
)
unique(process_name, revision, kind, idempotency_key)
  where idempotency_key is not null;

donat.process_signal_requests(
  id uuid primary key,
  source_name text not null,
  process_name text not null,
  process_revision text not null,
  signal_name text not null,
  correlation_json jsonb not null,
  payload_json jsonb not null,
  command_invocation_id uuid not null,
  effect_position integer not null check (effect_position >= 0),
  idempotency_key text not null,
  status text not null check (status in (
    'pending','consumed','duplicate','unmatched','ambiguous',
    'guard_false','unexpected_state','failed'
  )),
  created_at timestamptz not null default now(),
  consumed_at timestamptz,
  unique(command_invocation_id, effect_position),
  foreign key(process_name, process_revision)
    references donat.process_definition_versions(process_name, revision)
);

donat.process_activity_jobs(
  id uuid primary key,
  instance_id uuid not null references donat.process_instances(id),
  enqueued_from_event_id uuid not null references donat.process_events(id),
  state_name text not null,
  logical_activity_id text not null,
  connector_instance text not null,
  operation text not null,
  serialization_key_hash bytea,
  input_json jsonb not null,
  result_json jsonb,
  idempotency_key text not null,
  status text not null check (status in (
    'scheduled','running','succeeded','failed','cancelled'
  )),
  attempts integer not null default 0 check (attempts >= 0),
  available_at timestamptz not null default now(),
  schedule_to_start_deadline timestamptz not null,
  start_to_close_deadline timestamptz,
  lease_token uuid,
  lease_expires_at timestamptz,
  last_error_json jsonb,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique(instance_id, logical_activity_id)
);

donat.process_transition_logs(
  id uuid primary key,
  instance_id uuid not null references donat.process_instances(id),
  event_id uuid references donat.process_events(id),
  activity_job_id uuid references donat.process_activity_jobs(id),
  activity_attempt integer,
  from_state text,
  to_state text,
  outcome text not null,
  definition_revision text not null,
  command_result_json jsonb,
  before_state_hash bytea,
  after_state_hash bytea,
  redacted_context jsonb not null,
  created_at timestamptz not null default now()
)
unique(instance_id, event_id) where event_id is not null;
unique(activity_job_id, activity_attempt, outcome)
  where activity_job_id is not null and activity_attempt is not null;

donat.process_capacity_reservations(
  id uuid primary key,
  activity_job_id uuid not null references donat.process_activity_jobs(id),
  connector_instance text not null,
  operation text not null,
  serialization_key_hash bytea,
  lease_token uuid not null,
  reserved_at timestamptz not null,
  expires_at timestamptz not null,
  released_at timestamptz,
  unique(activity_job_id, lease_token)
);
index(connector_instance, operation, expires_at);
index(connector_instance, operation, serialization_key_hash, expires_at);

donat.process_inbound_deliveries(
  id uuid primary key,
  connector_instance text not null,
  provider_event_id text,
  payload_digest bytea not null,
  signature_status text not null check (signature_status in (
    'verified','missing','invalid','expired','malformed','unsupported'
  )),
  outcome text not null check (outcome in (
    'accepted','duplicate','unmatched','ambiguous','guard_false',
    'unexpected_state','invalid_signature'
  )),
  redacted_metadata jsonb not null,
  received_at timestamptz not null default now()
);

donat.process_inbound_events(
  id uuid primary key,
  connector_instance text not null,
  provider_event_id text not null,
  first_delivery_id uuid not null
    references donat.process_inbound_deliveries(id)
    deferrable initially deferred,
  payload_digest bytea not null,
  verified_at timestamptz not null,
  unique(connector_instance, provider_event_id)
);
~~~

No process table has a foreign key to `donat.command_invocations` or
`donat.command_invocation_claims`. Indexes cover due event/start/signal/job
status and `available_at`, activity lease expiry, instance process/state
correlation, definition status, inbound received time, and capacity expiry.

## 8. Start consumption and command transitions

The start worker claims one pending request with
`FOR UPDATE SKIP LOCKED` in the request's source pool. In one short
transaction it loads the exact stored revision, validates the canonical input,
inserts or finds the instance by
`(process_name, start_idempotency_key)`, appends the start event and transition
log for a new instance, records duplicate outcome for an existing semantic
instance, and marks the request consumed. A crash before commit leaves the
request pending; a crash after commit cannot create a second instance.

Every ordinary transition locks one due event and its instance, verifies the
pinned definition/runtime ABI, evaluates its closed guard context, compares
the optimistic instance version, writes the transition log and next
event/timer/activity, and marks the event consumed in one short outer
transaction.

When a transition invokes a command, the process transition owns that outer
Postgres transaction. The worker builds:

~~~rust
Session {
    role: transition.run_as_role.clone(),
    vars: compiled_session_variables_only,
    backend_request: false,
}
~~~

The mapping is evaluated only from the compiled sources allowed in Section 3;
no request headers or ambient GraphQL session are available.

The command's one SQL statement executes inside a nested savepoint. Only a
database error with SQLSTATE `P0D01` and a valid exact
`donat.graphql-error.v1` envelope is a command business rejection. For that
error, the worker rolls back to and releases the savepoint, decodes the
existing Donat error body, appends exactly one `command_rejected` audit
event/transition, follows `on_rejection`, and commits the outer transaction.
The command's domain DML and its command journal/outbox CTEs remain rolled
back. A malformed reserved envelope, permission SQLSTATE `23514`, connection
error, constraint error, decode error, or any other database failure aborts
the outer transaction; it cannot commit `on_rejection`.

GraphQL and process execution share the same internal command planner,
single-statement renderer, result decoder, and strict rejection decoder.
Neither path accepts raw SQL or constructs a new error envelope.

## 9. Activities, leases, retries, and capacity

A state transition gives each activity one deterministic
`logical_activity_id` from the process revision, instance, triggering event,
and state name. Its provider idempotency key is derived once from that ID and
is unchanged by retries, crash recovery, or lease takeover.

An activity claim transaction:

1. locks one scheduled due job with `FOR UPDATE SKIP LOCKED`;
2. compares `schedule_to_start_deadline` with the owning source's database
   clock;
3. under a source-local Postgres coordination lock, checks unexpired
   `process_capacity_reservations` against `max_in_flight`, the rolling
   permits/per/burst policy, and the optional serialization-key hash;
4. inserts one reservation, assigns a random lease token, increments attempt,
   and stores start-to-close/lease deadlines;
5. commits before calling the connector.

Unavailable capacity leaves the job scheduled and closes the transaction.
The connector receives immutable typed input, stable idempotency key, and
deadline only. It receives no pool, role, mutable process, arbitrary transport
request, or secret-bearing logger.

Completion uses a new short transaction and updates only a job whose current
lease token still matches. It stores the result/failure, releases the
reservation, and appends one completion event atomically. A stale completion
cannot advance the instance and receives an append-only attempt audit.

`schedule_to_start` is measured from enqueue and makes no provider call when
expired. `start_to_close` is measured from claim and makes a late completion
stale. Phase 1 has no heartbeat extension.

`max_attempts` includes the first attempt. Retry delay is
`min(max_interval, initial_interval * 2^(attempt-1))`, with deterministic full
jitter derived from logical activity ID and attempt. `Retry-After` may
postpone, never accelerate. Exhaustion appends `retry_exhausted`. No retry is
implicit or infinite.

## 10. Timers, command signals, and verified inbound delivery

Timers are source-local journal events due by the owning Postgres clock.
Tokio polling is only a wake-up mechanism; no sleep or in-memory wheel is
business state.

The connector endpoint remains
`POST /v1/connectors/{connector_instance}/webhooks`. The compiled connector
binding selects exactly one process source. The connector verifies the raw
bytes before parsing and returns only its typed verified event contract.

Inbound persistence is deliberately split:

- `process_inbound_events` is only the verified provider-event dedupe ledger,
  unique on `(connector_instance, provider_event_id)`;
- `process_inbound_deliveries` is append-only audit for every delivery attempt.

A verified delivery writes its delivery audit row and inserts or observes the
dedupe row in one source-local transaction. A first verified event then
correlates at most one non-terminal compatible instance and records exactly
one of `accepted`, `unmatched`, `ambiguous`, `guard_false`, or
`unexpected_state`. A repeated verified event writes a distinct delivery row
with `duplicate` while preserving the original dedupe row and transition.
An invalid signature has no trusted provider ID requirement and writes only
one redacted delivery row with `invalid_signature`; it never writes the dedupe
ledger or process state.

The route acknowledges a verified provider event only after the complete
transaction commits. A post-verification database failure is not
acknowledged, so the provider can retry. Payload digests and redacted metadata
are retained; raw bodies and secrets are not retained by this Phase-1 schema.

The command-signal worker consumes only a typed
`process_signal_requests` outbox row. It uses the stored source, process,
revision compatibility anchor, signal name, correlation/payload, semantic key,
invocation UUID, and effect position. No operator endpoint can manufacture
that row.

Signals are not buffered. A verified or command signal delivered while the
instance is not receptive records `unexpected_state` and cannot advance a
later wait. Zero or multiple correlation matches are audit-only. A declared
cancellation signal cancels only still-scheduled jobs and takes `on_cancel`;
an in-flight provider request is not retracted and its later completion is
audit-only. Compensation is an explicit state, never reverse SQL.

## 11. Failure and completion semantics

| Outcome | Durable result |
| --- | --- |
| guard false | event consumed, `guard_false` audit, state unchanged |
| valid structured command rejection | command savepoint rolled back; exactly one `on_rejection` transition commits |
| any other command/database error | outer transition aborts; no rejection transition commits |
| retryable selected connector error | job rescheduled with deterministic delay and same key |
| non-retried connector error | typed completion event follows the first matching route or fallback |
| retry limit | `retry_exhausted` follows the declared route or fallback |
| capacity or serialization unavailable | job remains scheduled; no provider call |
| schedule-to-start timeout | typed timeout event; no provider call |
| start-to-close timeout or stale completion | current lease may be retried; stale completion is audit-only |
| duplicate start semantic key | request is duplicate; existing instance is unchanged |
| duplicate verified provider event | new delivery audit plus existing dedupe row; no second process event |
| invalid signature | delivery audit only |
| unmatched/ambiguous/unexpected signal | delivery/request audit only |
| cancellation | declared transition; only unclaimed jobs cancelled |
| missing pinned definition/ABI | invariant audit and claim stop; no invented transition |

A terminal instance treats later signals, timers, and completions as
audit-only. Removing a definition blocks new starts but never reinterprets or
silently discards a live instance.

## 12. Test-first acceptance matrix

Every behavior starts with a failing crate test and, where externally visible,
a native conformance fixture. The focused identifiers are normative:

| Test ID | Level | Required proof |
| --- | --- | --- |
| `command_descriptor_exposes_exact_contract` | schema unit | recursive argument/result contracts, roles, session variables, and deterministic pre-process fingerprint are public |
| `connector_descriptor_is_typed_and_non_secret` | server unit | HTTP/Stripe descriptors contain exact contracts and no secret values |
| `http_template_slots_require_declared_types` | metadata/server unit | path/query/header/body slots reject undeclared or incompatible input |
| `process_metadata_requires_start_and_cancellation_shape` | metadata unit | canonical grammar includes start and typed cancellation |
| `process_candidate_build_is_cycle_free` | state unit | descriptors, process revisions, effect finalization, and schema compile in the required order |
| `process_rejects_cross_source_command` | process compiler | start, transition, start effect, and signal effect cannot cross source |
| `process_connector_instance_has_one_source` | process compiler | one connector instance cannot coordinate two process sources |
| `process_deployment_selects_one_real_source` | migrate integration | selected source alone is introspected/reconciled; omitted ambiguous source fails |
| `process_v6_schema_is_exact` | migrate integration | every Section 7 column/key/index exists and no command-journal FK exists |
| `command_invocation_id_replays_unchanged` | SQLgen/Postgres | exact replay preserves UUID |
| `command_invocation_id_changes_after_expiry` | SQLgen/Postgres | expired-key execution gets a new UUID |
| `command_effect_positions_share_generation` | SQLgen/Postgres | multiple positions copy one invocation UUID and remain individually unique |
| `process_start_request_pins_revision` | command/process integration | request under A consumed after B deploy starts A |
| `process_start_semantic_dedupe_is_separate` | process integration | distinct invocation UUIDs with one semantic key create one instance |
| `process_session_variables_are_closed` | compiler/runtime | missing/extra/ambient mappings reject; worker uses only compiled values |
| `process_command_rejection_commits_on_rejection` | Postgres integration | domain DML rolls back while one rejection transition commits |
| `process_command_database_error_aborts_outer` | Postgres integration | non-P0D01 error commits no process writes |
| `process_activity_does_not_hold_tx` | recording connector | provider wait begins only after lease transaction commits |
| `process_lease_takeover_is_safe` | two binaries | stable key, one transition, stale audit |
| `process_activity_capacity_is_global` | two binaries | max/rate/serialization policies hold in one source |
| `process_timer_survives_restart` | controlled DB clock | timer fires once without in-memory state |
| `process_inbound_audit_is_split` | webhook integration | accepted plus duplicate create two deliveries, one dedupe row, one transition |
| `process_invalid_signature_is_audit_only` | webhook integration | invalid signature creates delivery only and permits absent provider ID |
| `process_signal_is_not_buffered` | command/webhook integration | early/late signal cannot advance a later wait |
| `process_revision_runtime_abi_is_fenced` | rolling deployment | incompatible worker cannot claim pinned work |
| `process_no_management_api` | route/schema test | no process management field or route is published |

Focused tests rebuild `cargo build -p donat-server --bin donat` before native
conformance. Final acceptance runs all workspace tests and the full
conformance crate against Postgres, with process lifecycle cases using two
independent engine processes.

## 13. Read-only operational diagnosis

The binary may expose:

~~~text
donat process inspect --source <name> --instance <uuid>
donat process verify-history --source <name> --instance <uuid>
~~~

Both are read-only. Inspect emits a redacted journal timeline.
Verify-history reapplies stored events and stored command/activity results to
the pinned definition and exits non-zero on a before/after-state-hash
mismatch. Neither subcommand invokes a command or connector, changes a row or
lease, accepts arbitrary SQL or a role, or publishes an HTTP surface. They are
not retry, replay, repair, cancellation, or definition-mutation tools.

## 14. Provenance and no-copy treatment

Temporal, AWS Step Functions, Inngest, Stripe, and Airbyte remain
behavior/test-category references under
`knowledgebase/declarative-saas/reference-porting-register.md`.
This process contract copies no upstream source, fixture, generated artifact,
schema, or large text. Stripe Checkout contracts are independently expressed
by the fixed Rust descriptor already admitted under the existing Stripe
records. No n8n-derived code, fixture, module, or reference is added.

A future source-level import requires a separately admitted exact source file,
immutable revision, hash, license/notice, destination, failing-first Donat
test, and reviewer before the imported artifact lands.

## 15. Component ownership

| Area | Required owner | Prohibited shortcut |
| --- | --- | --- |
| normalized value contracts | `donat-ir` | separate command/process/connector type checkers |
| command descriptors/effect catalog | `donat-schema` | schema dependency on server or post-plan raw metadata lookup |
| process compiler/runtime | `donat-server::processes` and one `Engine` snapshot catalog | separate workflow service or mutable global definition |
| connector contracts | existing compiled connector registry | duplicated protocol compiler, dynamic module, or n8n port |
| DDL/reconciliation | V6 plus source-qualified `donat migrate` | serve-time DDL or reconciliation |
| business writes | existing explicit-role command IR/SQLgen path | hand-written process business SQL or admin role |
| process journals | short source-local Postgres transactions | distributed transaction or external I/O under transaction |
| ingress | connector verifier plus split source-local ledger/audit | parse-before-verify, in-memory dedupe, or one-row audit overwrite |
| proof | crate tests, native conformance, two-binary Postgres integration | mocked single-worker proof of durability |
