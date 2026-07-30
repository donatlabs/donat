# Petshop Mock Payment Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive Petshop checkout through a minimal durable YAML Flow that
performs a real provider-neutral HTTP payment request, waits for a signed mock
callback or deadline, applies the existing store Commands, and recovers safely
across retry, disconnect, duplicate delivery, and process restart.

**Architecture:** A source-local Flow run is the single durable execution
record. `start_*` returns its handle immediately; `execute_*` attaches to the
same run for a bounded interval. A worker leases one state in a short committed
transaction, performs HTTP outside the transaction through the existing
compiled connector registry, then records the result with a lease fence.
Inbound events are durably accepted before matching so callback-before-wait is
safe. There are no provider-specific outboxes, two workflow engines, or
Petshop Rust handlers.

**Tech Stack:** Rust workspace, axum/tokio, Postgres 16, serde YAML,
`donat-connector-*`, native conformance harness, `insta`.

## Global Constraints

- Complete and verify the Petshop Store Core plan first.
- The shipped topology remains one `donat` binary plus Postgres.
- Flow states are exactly `command`, `request`, `wait`, `when`, `output`, and
  `fail` in this plan.
- No loops, parallel/fan-in, child flows, general code step, dynamic connector,
  runtime plugin, or public journal administration.
- Ordinary CRUD remains on the current data plane.
- A Flow command state invokes an already compiled declarative Command under
  either the persisted explicit caller role/session or one fixed internal
  service role named literally in metadata; it never constructs SQL.
- A fixed service role still needs ordinary Command and table permissions. It
  is never derived from Flow input, Rule output, event payload, or a session
  variable and is not an admin bypass.
- HTTP begins only after durable state commits and no transaction is held over
  the network.
- External request delivery is at least once with a stable idempotency key.
- `start_*`, `execute_*`, and status use the same run and owner policy.
- Disconnect never cancels a run.
- CI never calls a public mock service. Manual configuration may point the
  generic HTTP connector at RequestBin, webhook.site, or an equivalent mock.
- The mock callback secret is fixture-only and never establishes a
  provider-independent production signature standard.
- Do not dispatch the Judge after each commit; the user explicitly replaced
  that gate with a later whole-range code review.

## File Map

- `specs/008-product-derived-durable-flows.md` — normative minimal Flow
  contract; supersedes conflicting grammar in proposed Spec 005.
- `knowledgebase/declarative-saas/decisions/015-unified-durable-flow-runs.md`
  — one run model for attached and detached execution.
- `crates/metadata/src/types.rs`, `crates/metadata/src/loader.rs` —
  `flows.yaml` and internal Command visibility.
- `crates/flow/` — pure Flow definition compiler and deterministic transition
  reducer; no database, HTTP, or server dependency.
- `migrations/V6__donat_flows.sql` — run, state-attempt, and inbound-event
  journal.
- `crates/server/src/flows/` — Postgres repository, worker, GraphQL planning
  adapters, connector activity adapter, and declared event ingress.
- `crates/server/src/main.rs`, `crates/server/src/state.rs` — candidate
  compilation and worker lifecycle.
- `crates/conformance/src/mock_payment.rs` — deterministic recording provider.
- `crates/conformance/tests/petshop_payment.rs` — product E2E, concurrency,
  retry, callback, restart, and ownership cases.
- `examples/petshop/metadata/flows.yaml` — `checkout_payment`.
- `examples/petshop/metadata/connectors.yaml` — fixed generic HTTP mock-payment
  instance.

---

### Task 1: Freeze the mock-payment product contract

**Files:**

- Create: `crates/conformance/src/mock_payment.rs`
- Modify: `crates/conformance/src/lib.rs`
- Create: `crates/conformance/tests/petshop_payment.rs`
- Create:
  `crates/conformance/fixtures/petshop/payment_start_unknown_field.yaml`
- Create:
  `crates/conformance/fixtures/petshop/payment_owner_isolation.yaml`

**Interfaces:**

```rust
pub struct MockPayment {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedPaymentRequest>>>,
    script: Arc<Mutex<VecDeque<MockPaymentResponse>>>,
}

pub enum MockPaymentResponse {
    Status { code: u16, body: serde_json::Value },
    Delay { duration: Duration, then: Box<MockPaymentResponse> },
    Disconnect,
}

pub struct RecordedPaymentRequest {
    pub idempotency_key: String,
    pub body: serde_json::Value,
}

impl Suite {
    pub fn with_mock_payment(self, script: Vec<MockPaymentResponse>) -> Self;
}
```

- [ ] **Step 1: Write recorder unit tests**

Prove that the stub:

- records exact method/path/body and one `Idempotency-Key`;
- returns scripted `500`, delayed response, `201`, and disconnect in order;
- can POST a signed callback to a supplied engine URL;
- exposes no secret in its debug output.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-conformance mock_payment
```

Expected: FAIL because the module and builder do not exist.

- [ ] **Step 3: Implement the in-process axum recorder**

Bind to `127.0.0.1:0`. Do not add a Docker service or test-only binary.
Expose the base URL to the engine as `PETSHOP_PAYMENT_BASE_URL` and the fixture
secret as `PETSHOP_PAYMENT_WEBHOOK_SECRET`.

- [ ] **Step 4: Add ignored-by-name product cases without ignoring tests**

Create test functions for:

```text
attached_success
detached_disconnect_and_poll
stable_key_across_500_timeout_success
duplicate_callback
callback_before_wait
deadline_survives_restart
owner_isolation
```

Each test must fail at the first missing public field or route. Do not use
`#[ignore]`; implementation tasks turn them green incrementally by running one
test name at a time.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p donat-conformance mock_payment
git add crates/conformance
git commit -m "test(petshop): define mock payment behavior"
```

---

### Task 2: Specify the product-derived Flow YAML and public contract

**Files:**

- Create: `specs/008-product-derived-durable-flows.md`
- Create:
  `knowledgebase/declarative-saas/decisions/015-unified-durable-flow-runs.md`
- Modify: `specs/005-durable-processes.md`
- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/src/loader.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/metadata/tests/load_fixture.rs`
- Create: `crates/metadata/tests/fixtures/flows/checkout_payment.yaml`

**Interfaces:**

```rust
pub struct FlowDefinition {
    pub name: String,
    pub source: String,
    pub permissions: Vec<FlowPermission>,
    pub input: Vec<FlowField>,
    pub output: Vec<FlowField>,
    pub start_at: String,
    pub states: Vec<FlowState>,
}

pub enum FlowStateKind {
    Command(FlowCommandState),
    Request(FlowRequestState),
    Wait(FlowWaitState),
    When(FlowWhenState),
    Output(FlowOutputState),
    Fail(FlowFailState),
}

pub enum CommandVisibility {
    Public,
    Internal,
}

pub enum FlowCommandRole {
    Caller,
    Fixed { role: String },
}
```

Canonical Petshop YAML:

```yaml
flows:
  - name: checkout_payment
    source: default
    permissions:
      - role: customer
        owner_session_variable: x-donat-user-id
    input:
      - { name: cart_id, type: bigint! }
      - { name: request_id, type: uuid! }
    output:
      - { name: order_id, type: uuid! }
      - { name: payment_id, type: uuid! }
      - { name: payment_status, type: String! }
    start_at: checkout
    states:
      - id: checkout
        command:
          name: begin_checkout
          run_as: caller
          arguments:
            cart_id: { input: cart_id }
            request_id: { input: request_id }
          next: create_payment
      - id: create_payment
        request:
          connector: mock_payment
          operation: create_payment
          input:
            order_id: { state: checkout, field: order_id }
            payment_id: { state: checkout, field: payment_id }
            amount_minor: { state: checkout, field: total_minor }
            currency: { state: checkout, field: currency }
          timeout: 2s
          retry:
            maximum_attempts: 3
            backoff: 100ms
            retry_on: [transport, timeout, http_429, http_5xx]
          next: await_outcome
          on_error: release_request_failure
      - id: await_outcome
        wait:
          event: payment_outcome
          correlate:
            payment_id: { state: checkout, field: payment_id }
          deadline: 15m
          next: route_outcome
          on_timeout: expire
      - id: route_outcome
        when:
          cases:
            - rule: payment_was_paid
              with:
                outcome: { event: outcome }
              next: record_paid_outcome
          default: record_failed_outcome
      - id: record_paid_outcome
        command:
          name: record_payment_outcome
          run_as: payment_worker
          arguments:
            payment_id: { state: checkout, field: payment_id }
            event_id: { event: event_id }
            outcome: { event: outcome }
            provider_reference: { event: provider_reference }
          next: paid
      - id: record_failed_outcome
        command:
          name: record_payment_outcome
          run_as: payment_worker
          arguments:
            payment_id: { state: checkout, field: payment_id }
            event_id: { event: event_id }
            outcome: { event: outcome }
            provider_reference: { event: provider_reference }
          next: payment_failed
      - id: release_request_failure
        command:
          name: expire_checkout
          run_as: payment_worker
          arguments:
            payment_id: { state: checkout, field: payment_id }
            deadline_key: { run: id }
          next: payment_request_failed
      - id: expire
        command:
          name: expire_checkout
          run_as: payment_worker
          arguments:
            payment_id: { state: checkout, field: payment_id }
            deadline_key: { run: id }
          next: payment_failed
      - id: paid
        output:
          values:
            order_id: { state: checkout, field: order_id }
            payment_id: { state: checkout, field: payment_id }
            payment_status: { literal: paid }
      - id: payment_request_failed
        fail: { code: payment_request_failed, message: Payment request failed }
      - id: payment_failed
        fail: { code: payment_failed, message: Payment was not completed }
```

- [ ] **Step 1: Add metadata RED tests**

Test exact round-trip plus rejection of:

- duplicate state ID;
- missing `start_at` target;
- unknown `next`/error/timeout target;
- cycle in this first acyclic profile;
- dynamic command/connector/state name;
- request without timeout/retry/error route;
- wait without deadline/timeout route;
- unreachable state;
- output mismatch;
- public Flow referencing an unauthorized Command;
- a fixed `run_as` role not permitted on the Command or its tables;
- `run_as` sourced from a binding instead of the literal `caller` or a static
  classic role;
- `internal` Command invoked directly from GraphQL.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-metadata flows
```

- [ ] **Step 3: Write Spec 008 and supersession note**

Spec 008 must define:

- one durable run for both start and execute;
- immutable definition revision hash pinned by each run;
- the six closed states and acyclic first profile;
- stable state IDs and typed bindings;
- at-least-once request attempts and lease fencing;
- persist-before-match inbound events;
- owner-scoped status;
- exact retry/error/deadline behavior;
- no process-specific outbox;
- no generic admin/cancel endpoint.

Add a clear note to proposed Spec 005 that Spec 008 replaces its grammar and
journal for the first implementation. Retain useful operational requirements
that do not conflict.

- [ ] **Step 4: Implement metadata loading only**

Load optional `flows.yaml` as one wrapper. A directory without it remains
valid. Add `visibility: public|internal` to Commands with default `public`;
schema introspection omits internal Commands but the candidate compiler keeps
them available to Flow validation.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p donat-metadata
git add specs knowledgebase/declarative-saas crates/metadata
git commit -m "feat(flows): define the product-derived YAML contract"
```

---

### Task 3: Compile immutable typed Flow revisions

**Files:**

- Modify: root `Cargo.toml`
- Create: `crates/flow/Cargo.toml`
- Create: `crates/flow/src/lib.rs`
- Create: `crates/flow/src/compiler.rs`
- Create: `crates/flow/src/reducer.rs`
- Create: `crates/flow/tests/compiler.rs`
- Create: `crates/flow/tests/reducer.rs`
- Modify: `crates/server/src/state.rs`

**Interfaces:**

```rust
pub struct CompiledFlow {
    pub identity: FlowIdentity,
    pub revision: FlowRevision,
    pub input: ValueContractCatalog,
    pub output: ValueContractCatalog,
    pub start_at: StateId,
    pub states: BTreeMap<StateId, CompiledState>,
    pub permissions: Vec<CompiledFlowPermission>,
}

pub struct FlowRevision([u8; 32]);

pub enum ReducerInput {
    Start,
    CommandCompleted(TypedValue),
    RequestCompleted(TypedValue),
    RequestFailed(ConnectorFailureClass),
    Event(TypedValue),
    Deadline,
}

pub enum ReducerDecision {
    ScheduleCommand { state: StateId, invocation: CommandInvocation },
    ScheduleRequest { state: StateId, activity: ConnectorActivity },
    Wait { state: StateId, subscription: EventSubscription, deadline: Timestamp },
    Complete { output: TypedValue },
    Fail { code: StaticErrorCode, message: StaticSafeMessage },
}
```

- [ ] **Step 1: Write compiler and reducer RED tests**

Cover deterministic revision bytes, command and connector existence/type
checking, original-role permission checking, typed state/event/run bindings,
unreachable-state rejection, and one transition for every YAML edge.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-flow
```

- [ ] **Step 3: Implement the dependency-free compiler/reducer**

`donat-flow` may depend on metadata, value-contract, rules, connector ABI, and
IR-level identifiers. It may not depend on server, axum, tokio-postgres, or
HTTP clients. Canonical revision material includes the complete normalized
definition and referenced Command/Rule/connector contract identities.

- [ ] **Step 4: Compile Flow catalogs with candidate state**

In `server/state.rs`, compile Rules, Commands, connectors, and then Flows in
that order. Validation reports `flows.yaml.states[n]...` paths and prevents the
server from publishing a partially valid candidate.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p donat-flow
cargo test -p donat-server state
cargo test --workspace --no-run
git add Cargo.toml crates/flow crates/server/src/state.rs
git commit -m "feat(flows): compile immutable flow revisions"
```

---

### Task 4: Add the normalized Flow journal

**Files:**

- Create: `migrations/V6__donat_flows.sql`
- Modify: `migrations/README.md`
- Modify: `crates/server/src/migrate.rs`
- Create: `crates/server/src/flows/mod.rs`
- Create: `crates/server/src/flows/repository.rs`
- Create: `crates/server/tests/flows_repository.rs`

**Interfaces:**

Tables:

```sql
donat.flow_runs
  (id uuid primary key, source text, flow_name text, revision bytea,
   owner_role text, owner_subject_hash bytea, input jsonb,
   status text, current_state text, output jsonb, failure jsonb,
   created_at timestamptz, updated_at timestamptz)

donat.flow_state_attempts
  (run_id uuid, state_id text, attempt integer, status text,
   available_at timestamptz, lease_token uuid, lease_until timestamptz,
   idempotency_key text, input jsonb, result jsonb, failure jsonb,
   primary key (run_id, state_id, attempt))

donat.flow_events
  (source text, flow_name text, event_name text, event_id text,
   correlation_hash bytea, payload jsonb, received_at timestamptz,
   consumed_run_id uuid, consumed_state_id text,
   primary key (source, flow_name, event_name, event_id))
```

Raw owner/correlation inputs and credentials are not stored. JSON values are
validated bounded typed values before insertion.

- [ ] **Step 1: Write migration and repository RED tests**

Cover:

- same start idempotency key/input returns one run;
- same key/changed input conflicts;
- lease acquire/renew/complete requires the current token;
- stale completion changes nothing;
- event insert is unique;
- event may exist before a matching wait;
- exactly one wait consumes one event;
- terminal run cannot schedule another state.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-server --test flows_repository
```

- [ ] **Step 3: Add migration and repository**

Use short explicit transactions. The repository accepts already typed,
compiled values; it does not interpret YAML. State leasing uses
`FOR UPDATE SKIP LOCKED` and a bounded lease expiry.

- [ ] **Step 4: Register deploy-time migration**

`donat migrate` applies V6. Serving and `validate` never issue DDL.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p donat-server --test flows_repository
cargo test -p donat-server migrate
git add migrations crates/server
git commit -m "feat(flows): add the normalized durable journal"
```

---

### Task 5: Expose one run through start, execute, and owner-scoped status

**Files:**

- Create: `crates/server/src/flows/graphql.rs`
- Create: `crates/server/src/flows/worker.rs`
- Modify: `crates/schema/src/introspection.rs`
- Modify: `crates/schema/src/plan.rs`
- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/server/src/gql.rs`
- Modify: `crates/server/src/main.rs`
- Test: `crates/schema/tests/flows.rs`
- Test: `crates/conformance/tests/petshop_payment.rs`

**Interfaces:**

For `checkout_payment`, generate:

```graphql
start_checkout_payment(
  input: CheckoutPaymentInput!
  idempotency_key: uuid!
): CheckoutPaymentHandle!

execute_checkout_payment(
  input: CheckoutPaymentInput!
  idempotency_key: uuid!
  wait_timeout_ms: Int = 2000
): CheckoutPaymentExecution!

checkout_payment_run(id: uuid!): CheckoutPaymentExecution
```

`Handle` contains `id` and `status`. `Execution` contains `id`, `status`,
nullable typed `output`, and nullable safe failure. `wait_timeout_ms` is
bounded to `0..10000`; timeout returns `RUNNING` or `WAITING`, not an HTTP
error. Status is absent for a non-owner exactly as an invisible row would be.

- [ ] **Step 1: Add schema and owner-isolation RED tests**

Prove fields exist only for a declared role, input/output types are static,
unknown input is rejected, customer B cannot see customer A's run, and
start/execute with the same key return the same UUID.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-schema --test flows
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment owner_isolation
```

- [ ] **Step 3: Implement start and status planning**

Persist the explicit role and a hash of the configured owner session variable.
Do not accept owner fields in GraphQL input. Start inserts the run and first
attempt in one short transaction.

- [ ] **Step 4: Implement attached execution**

After the start transaction commits, wait on an in-process notification plus
periodic bounded status read. A client disconnect drops only the waiter.
Execution never owns or cancels the run.

- [ ] **Step 5: Execute Command states**

The worker resolves the pinned compiled state, invokes the existing Command
planner/executor with either the persisted role/session snapshot or the
statically compiled internal service role, records its typed result, and
reduces to the next state. Fixed roles use only their declared Command and
table permissions. Internal Commands remain absent from public GraphQL.

- [ ] **Step 6: Verify and commit**

```bash
cargo test -p donat-schema --test flows
cargo test -p donat-server flows
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment attached_success
git add crates/ir crates/schema crates/server crates/conformance
git commit -m "feat(flows): start attach and advance durable runs"
```

---

### Task 6: Execute retryable HTTP request states

**Files:**

- Create: `crates/server/src/flows/activity.rs`
- Modify: `crates/server/src/flows/worker.rs`
- Modify: `crates/server/src/connectors/mod.rs` only to expose the existing
  registry execution boundary to the Flow adapter.
- Modify: `crates/conformance/tests/petshop_payment.rs`

**Interfaces:**

```rust
pub async fn execute_activity(
    registry: &ConnectorRegistry,
    activity: CompiledConnectorActivity,
    stable_idempotency_key: &str,
    deadline: Instant,
) -> Result<TypedValue, ConnectorFailure>;
```

- [ ] **Step 1: Add retry/idempotency RED tests**

Script `500`, delayed timeout, then `201`. Assert three recorded requests,
byte-identical JSON bodies, and one identical idempotency key. Add a `400` case
that records one request and follows `on_error` without retry.

- [ ] **Step 2: Run RED**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment stable_key
```

- [ ] **Step 3: Lease before HTTP and complete with a fence**

Commit the lease first, call the registry with no database transaction, then
complete only where `(run_id, state_id, attempt, lease_token)` still matches.
Retry scheduling creates the next attempt with the same stable activity key
and a calculated `available_at`.

- [ ] **Step 4: Add restart recovery**

An expired `leased` attempt becomes claimable. A crash after remote acceptance
may resend, so the provider idempotency key derives from
`flow revision + run id + state id`, never attempt number or process ID.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p donat-server flows
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment stable_key
cargo test -p donat-conformance --test petshop_payment restart
git add crates/server crates/conformance
git commit -m "feat(flows): run idempotent connector activities"
```

---

### Task 7: Persist callbacks, deadlines, and event-vs-timeout decisions

**Files:**

- Create: `crates/server/src/flows/ingress.rs`
- Modify: `crates/server/src/flows/worker.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/conformance/tests/petshop_payment.rs`

**Interfaces:**

Declared route:

```text
POST /v1/flows/checkout_payment/events/payment_outcome
```

Headers:

```text
X-Donat-Event-Id: <stable provider event id>
X-Donat-Signature: v1=<hex hmac-sha256>
```

The signature covers the unmodified body. The route exists only for a
declared event ingress and has no list/update/delete behavior.

- [ ] **Step 1: Add ingress RED tests**

Cover invalid signature, missing event ID, malformed typed payload, duplicate
event, unmatched event, callback-before-wait, callback/deadline race, and late
paid event after expiry.

- [ ] **Step 2: Run RED**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment callback
```

- [ ] **Step 3: Implement persist-before-match ingress**

Validate body limit, signature, event ID, and typed payload, then insert
`flow_events` before attempting correlation. A valid unmatched event returns
`204` and remains available. Invalid authentication returns a bounded error and
writes no event.

- [ ] **Step 4: Implement wait registration and consumption**

Register the wait and deadline in one transaction, then first search existing
events. Event consumption and transition scheduling are one transaction.
The correlation hash includes flow identity, event name, and canonical
correlation values.

- [ ] **Step 5: Implement durable deadline election**

The worker claims an expired wait only if no event is consumed. Event and
deadline updates fence on the same current-state version so exactly one route
wins. Late events remain audit records and never reopen a terminal run.

- [ ] **Step 6: Verify and commit**

```bash
cargo test -p donat-server flows
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_payment callback
cargo test -p donat-conformance --test petshop_payment deadline
git add crates/server crates/conformance
git commit -m "feat(flows): add durable event and deadline waits"
```

---

### Task 8: Wire Petshop YAML to the mock endpoint and publish the example

**Files:**

- Create: `examples/petshop/metadata/flows.yaml`
- Create: `examples/petshop/metadata/connectors.yaml`
- Modify: `examples/petshop/metadata/commands.yaml`
- Modify: `examples/petshop/metadata/rules.yaml`
- Modify: `examples/petshop/metadata/query_collections.yaml`
- Modify: `examples/petshop/metadata/rest_endpoints.yaml`
- Modify: `examples/petshop/docker-compose.yml`
- Modify: `examples/petshop/README.md`
- Modify: root `README.md`
- Modify: `crates/conformance/tests/petshop_payment.rs`

**Interfaces:**

The HTTP connector instance has one fixed configured origin:

```yaml
- name: mock_payment
  module: http
  config:
    endpoint_identity: petshop_mock_payment_v1
    credential_identity: petshop_mock_payment_fixture
    base_url: { value_from_env: PETSHOP_PAYMENT_BASE_URL }
  operations:
    - name: create_payment
      method: POST
      path: /payments
      success_statuses: [200, 201, 202]
      idempotency: { header: Idempotency-Key }
      capacity:
        max_in_flight: 8
        rate_limit: { permits: 20, per: 1s, burst: 8 }
```

No Flow input can change scheme, host, port, path, or header names.

- [ ] **Step 1: Mark lifecycle Commands internal**

`begin_checkout`, `record_payment_outcome`, and `expire_checkout` become
`visibility: internal`. The public checkout surface is the generated Flow
field; direct catalog/cart CRUD remains unchanged.

- [ ] **Step 2: Add REST and MCP parity**

REST saved operations expose start, execute, and status over the same generated
GraphQL fields. MCP exposes task-level adapters through that same schema
pipeline; it does not call the engine over HTTP or reveal the journal.

- [ ] **Step 3: Document manual RequestBin-like usage**

README explains:

1. configure `PETSHOP_PAYMENT_BASE_URL` to a mock API capable of returning the
   fixed response contract;
2. begin checkout;
3. inspect the captured request and stable idempotency header;
4. send a signed callback with the provided fixture script/curl example.

State explicitly that capture-only endpoints need a configured response and
manual callback; they are not payment providers.

- [ ] **Step 4: Run focused verification**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop
cargo test -p donat-conformance --test petshop_payment
cargo test -p donat-conformance --test connectors
```

- [ ] **Step 5: Run broad verification**

```bash
cargo test -p donat-metadata
cargo test -p donat-flow
cargo test -p donat-schema
cargo test -p donat-sqlgen
cargo test -p donat-server
cargo test -p donat-conformance
```

Inspect every new `insta` snapshot and verify the engine made no live internet
request.

- [ ] **Step 6: Commit**

```bash
git add examples/petshop crates/conformance README.md
git commit -m "feat(petshop): publish the mock payment flow"
```
