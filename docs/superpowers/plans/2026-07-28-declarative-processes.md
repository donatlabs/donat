# Durable Processes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development or superpowers:executing-plans to
> implement this plan task by task. Track the checkboxes in the task being
> executed; do not batch task commits.

**Goal:** Build source-local declarative durable processes inside the one
Donat Rust binary, with immutable executable revisions, typed
commands/connectors, execution-generation outboxes, timers, signed inbound
delivery, explicit-role command transitions, and crash-safe at-least-once
workers.

**Architecture:** Candidate construction publishes source-qualified command
and connector descriptors, compiles process definitions, creates a neutral
effect catalog, finalizes command effects, and compiles the serving schema.
Before publication, the server loads and hash-verifies the declared active
plus every non-terminal live-retired revision from each real Postgres source.
V6 supplies source-qualified journals, command invocation UUIDs, linked split
ingress audit, a durable token bucket, per-step provider-attempt records, and
the deploy-time validation helper. Serving performs reads only.

**Tech stack:** Rust 2024, `no_std + alloc` value contracts, Tokio, Axum,
serde YAML/JSON, Postgres 16, tokio-postgres, reqwest, SHA-256, UUID, insta,
and the native conformance harness.

## Global constraints

- Keep one `donat` binary and Postgres; crates/modules are not services.
- Add no workflow service, plugin runtime, JavaScript/WASM execution, dynamic
  connector, admin role, permission bypass, runtime DDL, distributed
  transaction, or process-management HTTP API.
- `donat-value-contract` is the only owner of `ValueType`, `TypeRef`,
  `TypedValue`, canonical sizing, and inline bytes. It is `no_std + alloc`;
  `donat-ir` only re-exports its value types.
- Executable connector effects are exactly headerless `ReadOnly` or
  per-side-effect-step `ProviderIdempotent` with a fixed binding, scope,
  minimum retention, positive clock margin, and complete bounded send
  horizon. There is no executable non-idempotent side-effect class.
- Commands contain no connector/provider business logic. A provider call can
  occur only from a durable activity after its job, lease, and applicable
  capacity intent commit.
- The five Process state variants are closed. Add no If/Switch/Merge/Code/Wait
  node model, loops, batching-as-workflow, item/paired-item model,
  subworkflows, or send-and-wait. Pagination is bounded connector transport
  inside one activity only.
- Every process, command transition, start/signal effect, worker, database
  clock, connector binding, journal key, and write is source-local to one
  exact metadata source.
- Every command remains one Postgres statement and returns the exact existing
  Donat error envelope.
- Process journal DML uses short explicit transactions; no connector or other
  external I/O occurs while a transaction is open.
- Preserve metadata-free refinery migration:
  `donat --database-url <url> migrate --migrations-dir <dir>` never requires
  metadata or `--source`.
- Metadata-aware migrate/validate selects one real Postgres source; serve
  validates every real source but performs no DDL or reconciliation.
- `migrations/V6__donat_processes.sql` is the only process migration and owns
  `donat.check_violation(text)`.
- `donat.command_invocations.invocation_id` identifies one execution
  generation; process semantic start dedupe remains separate.
- `lifecycle: retired` rejects new starts before domain DML while retaining
  executable dependencies for non-terminal instances.
- Every process command transition has a closed typed `session_variables`
  mapping and an explicit classic `run_as_role`.
- Every process-owned serialized value is bounded to 256 KiB.
- Copy no upstream source, fixture, generated artifact, schema, or large text.
  Keep source-level provenance under the existing reference porting register.
- Every externally visible behavior starts with its native conformance case
  inside the task that owns the implementation. Rebuild the server binary
  before each focused conformance GREEN run.
- Review every insta diff; never accept snapshots blindly.
- Do not invoke Judge. Request one independent review only after the complete
  implementation range passes final verification.

**Specification:**
[`specs/005-durable-processes.md`](../../../specs/005-durable-processes.md)

The 14 numbered tasks below are implementation units. Each repeats the exact
interfaces it creates or consumes so its extracted text is a complete brief.
Final no-change acceptance is outside the numbered task count.

---

### Task 1: Add the lower value contract and command descriptors

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/value-contract/Cargo.toml`
- Create: `crates/value-contract/src/lib.rs`
- Create: `crates/value-contract/tests/value_contract.rs`
- Modify: `crates/ir/Cargo.toml`
- Create: `crates/ir/src/value_contract.rs`
- Modify: `crates/ir/src/lib.rs`
- Create: `crates/ir/tests/value_contract_adapter.rs`
- Modify: `crates/schema/Cargo.toml`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/tests/commands.rs`

**Interfaces created here:**

~~~rust
// crates/value-contract/src/lib.rs
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

pub const VALUE_TYPE_LANGUAGE_VERSION: u16 = 1;

pub struct ValueContractCatalog {
    pub roots: BTreeMap<String, ValueContractField>,
    pub named_objects: BTreeMap<String, ValueObjectContract>,
}

pub struct ValueContractField {
    pub required: bool,
    pub type_ref: TypeRef,
}

pub struct TypeRef {
    pub nullable: bool,
    pub value_type: ValueType,
}

pub enum ValueScalar {
    Boolean,
    String,
    Int32,
    Int64,
    UInt64,
    Decimal,
    Uuid,
    Date,
    Timestamp,
    TimestampTz,
    Json,
    Custom { name: String },
}

pub enum ValueType {
    Scalar { scalar: ValueScalar },
    Enum { name: String, values: Vec<String> },
    Object { fields: BTreeMap<String, ValueContractField> },
    List { element: Box<TypeRef> },
    Ref { name: String },
}

pub struct ValueObjectContract {
    pub fields: BTreeMap<String, ValueContractField>,
}

pub enum CanonicalNumber {
    I64(i64),
    U64(u64),
    Decimal(String),
}

pub enum TypedValue {
    Null,
    Boolean(bool),
    String(String),
    Number(CanonicalNumber),
    List(Vec<TypedValue>),
    Object(BTreeMap<String, TypedValue>),
    InlineBytes(BoundedInlineBytes),
}

pub struct BoundedInlineBytes {
    bytes: Vec<u8>,
}

impl BoundedInlineBytes {
    pub fn try_new(
        bytes: Vec<u8>,
        maximum_bytes: usize,
    ) -> Result<Self, ValueContractError>;

    pub fn as_slice(&self) -> &[u8];
}

impl ValueContractCatalog {
    pub fn validate(
        &self,
        value: &TypedValue,
    ) -> Result<(), ValueContractError>;

    pub fn is_assignable_from(&self, source: &Self) -> bool;
}

// crates/ir/src/lib.rs and crates/ir/src/value_contract.rs
pub use donat_value_contract::{
    CanonicalNumber, TypeRef, TypedValue, ValueContractCatalog,
    ValueContractError, ValueContractField, ValueObjectContract, ValueScalar,
    ValueType, VALUE_TYPE_LANGUAGE_VERSION,
};

pub enum ProcessStartPolicy {
    Enabled,
    RejectRetired,
}

pub fn compile_value_contract_catalog(
    metadata: &donat_metadata::Metadata,
    fields: &BTreeMap<String, String>,
) -> Result<ValueContractCatalog, ValueContractError>;

// crates/schema/src/commands.rs
pub struct CommandDescriptor {
    pub source: String,
    pub name: String,
    pub arguments: ValueContractCatalog,
    pub result: ValueContractCatalog,
    pub allowed_roles: BTreeSet<String>,
    pub required_session_variables:
        BTreeMap<String, BTreeMap<String, TypeRef>>,
    pub definition_fingerprint: String,
}

impl CompiledCommand {
    pub fn descriptor(&self) -> &CommandDescriptor;
}

pub fn compile_command_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    catalog: &Catalog,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Result<CompiledSourceCommandCatalog, PlanError>;
~~~

`crates/value-contract/Cargo.toml` is unpublished, has empty default features,
no `std` feature, no build script, and no normal/build/procedural-macro
third-party dependency. Its parser implements
`type-ref = primary ["!"]` and
`primary = identifier | "[" type-ref "]"` with no whitespace. It owns all
value validation and canonical-size accounting. IR contains only the
metadata/Rule adapter and re-exports; it defines no second value type.
`crates/schema/Cargo.toml` adds exactly `sha2 = { workspace = true }`.

**Tests owned by this task:**

- `value_type_language_is_closed_and_canonical`
- `value_contract_distinguishes_required_from_nullable`
- `value_contract_resolves_recursive_object_refs`
- `value_contract_rejects_unknown_duplicate_and_invalid_refs`
- `value_contract_validates_every_closed_scalar_shape`
- `value_contract_assignability_is_nominal_except_json`
- `value_contract_no_std_boundary_is_mechanical`
- `ir_reexports_the_only_value_contract_types`
- `command_descriptor_exposes_exact_contract`
- `command_descriptor_fingerprint_is_pre_process_and_deterministic`
- `command_descriptor_rejects_incompatible_session_variable_uses`

- [ ] **Step 1: Add the failing lower-contract and boundary tests**

  Create the value-contract and IR adapter tests first. Use `[uuid!]!`, every
  scalar alias, a recursive object, a nominal enum, a custom scalar, missing
  versus null, canonical map order, and the 256-KiB size edge. Add a workspace
  policy assertion that the lower crate has no forbidden dependency/build
  edge and that IR re-exports the same Rust types.

- [ ] **Step 2: Add the failing command-descriptor tests**

  Add the three exact schema tests. The command fixture has a recursive
  argument, nullable result, two classic roles, role-distinct session
  variables, Rule use, idempotency, and raw `start_process`. Assert the
  fingerprint changes with raw effect shape but contains no resolved process
  revision, request value, environment value, credential, or secret.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-value-contract
  cargo test -p donat-ir --test value_contract_adapter
  cargo test -p donat-schema --test commands command_descriptor
  cargo check -p donat-value-contract --no-default-features \
    --target thumbv7em-none-eabi
  ~~~

  Expected: the lower crate, IR adapter/re-export, command descriptor, and
  schema SHA-256 dependency do not exist.

- [ ] **Step 4: Implement the lower crate and IR adapter**

  Add the crate to the workspace. Implement the closed parser, bounded
  constructors, canonical scalar/collection validation, deterministic object
  order, canonical sizing, exact assignability, and private checked inline
  byte construction. Keep JSON conversion in IR/server adapters; do not add
  `serde_json` to the lower crate.

- [ ] **Step 5: Publish immutable command descriptors**

  Move single-source command work into `compile_command_source_catalog`; make
  the global compiler compose it. Collect role-specific required session
  variables from effective permissions, idempotency scope, and raw effects.
  Hash versioned canonical JSON with raw effects but no resolved revision.

- [ ] **Step 6: Run GREEN**

  ~~~bash
  cargo test -p donat-value-contract
  cargo test -p donat-ir
  cargo test -p donat-schema --test commands
  cargo test -p donat-schema --test multi_source
  cargo check -p donat-value-contract --no-default-features \
    --target thumbv7em-none-eabi
  cargo metadata --locked --format-version 1 >/tmp/donat-process-metadata.json
  ~~~

  Inspect the metadata closure and confirm the value crate has no forbidden
  edge. Expected: all listed tests pass without changing command execution.

- [ ] **Step 7: Commit**

  ~~~bash
  git add Cargo.toml Cargo.lock crates/value-contract crates/ir \
    crates/schema/Cargo.toml crates/schema/src crates/schema/tests
  git commit -m "feat(processes): publish closed command contracts"
  ~~~

---

### Task 2: Publish typed connector descriptors and closed effects

**Files:**

- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/metadata/tests/fixtures/connectors/`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/connectors/http.rs`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/tests/connectors_http.rs`
- Modify: `crates/server/tests/connectors_stripe.rs`
- Modify: `crates/server/tests/state.rs`
- Modify: `crates/conformance/tests/connectors.rs`
- Create: `crates/conformance/fixtures/connectors/http_typed_input.yaml`
- Create: `crates/conformance/fixtures/connectors/effect_contract.yaml`

**Interfaces consumed from Task 1:**

~~~rust
pub use donat_value_contract::{
    TypeRef, TypedValue, ValueContractCatalog, ValueType,
};

pub fn compile_value_contract_catalog(
    metadata: &Metadata,
    fields: &BTreeMap<String, String>,
) -> Result<ValueContractCatalog, ValueContractError>;
~~~

**Metadata interfaces created here:**

~~~rust
pub struct HttpConnectorOperation {
    pub version: String,
    pub input: BTreeMap<String, String>,
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, ConnectorInputBinding>,
    pub headers: Vec<ConnectorRequestHeader>,
    pub body: Option<serde_json::Value>,
    pub success_statuses: Vec<u16>,
    pub response: BTreeMap<String, ConnectorResponseBinding>,
    pub effect: Option<ConnectorOperationEffect>,
    pub idempotency: Option<LegacyConnectorIdempotency>,
    pub error_classification: ConnectorHttpErrorClassification,
}

pub struct ConnectorRequestHeader {
    pub name: String,
    #[serde(flatten)]
    pub value: ConnectorRequestHeaderValue,
}

#[serde(untagged, deny_unknown_fields)]
pub enum ConnectorRequestHeaderValue {
    Static { value: String },
    Input { input: String },
}

#[serde(untagged, deny_unknown_fields)]
pub enum ConnectorOperationEffect {
    ReadOnly { read_only: ConnectorReadOnlyEffect },
    ProviderIdempotent {
        provider_idempotent: ConnectorProviderIdempotentEffect,
    },
}

pub struct ConnectorReadOnlyEffect {}

pub struct ConnectorProviderIdempotentEffect {
    pub side_effect_steps: Vec<ConnectorProviderIdempotentStep>,
}

pub struct ConnectorProviderIdempotentStep {
    pub step: String,
    pub fixed_binding: ConnectorFixedIdempotencyBinding,
    pub scope: String,
    pub minimum_retention_ms: u64,
    pub clock_safety_margin_ms: u64,
}

#[serde(untagged, deny_unknown_fields)]
pub enum ConnectorFixedIdempotencyBinding {
    Header { header: String },
    BodyField { body_field: String },
}
~~~

`idempotency: { header: ... }` remains loadable v2 inventory metadata but
cannot publish an executable mutation descriptor because it lacks scope,
retention, and margin. It is never converted to an invented safe default.

**Descriptor interfaces created here:**

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
    pub effect: OperationEffect,
    pub capacity: ConnectorCapacityContract,
    pub endpoint_identity: String,
    pub credential_identity: String,
    pub configuration_fingerprint: String,
}

pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: NonEmptyVec<ProviderIdempotentStep>,
    },
}

pub struct ProviderIdempotentStep {
    pub step: CompiledStepId,
    pub fixed_binding: FixedIdempotencyBinding,
    pub scope: ProviderIdempotencyScope,
    pub minimum_retention_ms: NonZeroU64,
    pub clock_safety_margin_ms: NonZeroU64,
}

pub enum FixedIdempotencyBinding {
    Header { name: StaticHeaderName },
    BodyField { pointer: StaticBodyPointer },
}

pub struct NonEmptyVec<T> {
    pub first: T,
    pub rest: Vec<T>,
}

pub struct CompiledStepId(pub String);
pub struct StaticHeaderName(pub String);
pub struct StaticBodyPointer(pub String);
pub struct ProviderIdempotencyScope(pub String);

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

impl ConnectorRegistry {
    pub fn descriptors(&self) -> &ConnectorDescriptorCatalog;
}
~~~

Every descriptor newtype has a bounded checked constructor. Declarative HTTP
uses one compiled step ID, `request`. `ReadOnly` is explicit and headerless.
Every provider mutation requires exactly one complete `request` entry.
Generated/static modules may expose several compiled steps but must cover
every side-effecting step exactly once. Effect data enters the non-secret
configuration fingerprint. Stripe's fixed descriptor records its admitted
step binding/scope/retention/margin constants; do not infer them at runtime.

**Tests and conformance owned by this task:**

- `http_request_headers_preserve_static_yaml_and_add_typed_input`
- `http_operation_type_refs_use_shared_language`
- `http_template_slots_require_declared_types`
- `http_header_input_requires_scalar`
- `connector_descriptor_is_typed_and_non_secret`
- `connector_effect_model_is_closed_and_per_step`
- `connector_effect_rejects_inventory_only_mutation`
- `connector_effect_multistep_coverage_is_total`
- `stripe_descriptor_matches_fixed_checkout_contract`
- Conformance test `http_typed_input_contract_is_enforced`
- Conformance test `connector_effect_contract_is_enforced`
- Fixtures `http_typed_input.yaml` and `effect_contract.yaml`

- [ ] **Step 1: Add both failing native conformance cases**

  The typed-input fixture declares `order_id: uuid!`,
  `request_trace: string!`, legacy/static plus typed headers, and exact
  undeclared-slot failures. The effect fixture separately proves headerless
  read-only admission, complete provider-idempotent admission, incomplete
  legacy inventory rejection, duplicate/missing step rejection, zero margin,
  margin equal to retention, and a non-idempotent mutation rejection.

- [ ] **Step 2: Add all failing serde and descriptor tests**

  Round-trip legacy static-header YAML unchanged and reject both/neither
  header value forms. Assert effect serialization, exact fixed scope and
  numeric windows, complete multi-step coverage, no dynamic method/header
  name/body pointer, and no secret/resolved URL in any descriptor/hash.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-metadata --test types_serde connector_
  cargo test -p donat-server --test connectors_http
  cargo test -p donat-server --test connectors_stripe
  cargo test -p donat-conformance --test connectors \
    http_typed_input_contract_is_enforced
  cargo test -p donat-conformance --test connectors \
    connector_effect_contract_is_enforced
  ~~~

  Expected: typed inputs, compatible header serde, closed effect descriptors,
  and both conformance behaviors are absent.

- [ ] **Step 4: Implement metadata and descriptor validation**

  Compile every input/output type through the Task 1 adapter. Recursively
  validate path/query/header/body slots before dispatch. Implement only the
  two executable effects. Preserve old incomplete idempotency metadata for
  loading/reporting, but reject it from executable descriptor publication.

- [ ] **Step 5: Publish descriptors from existing connector compilers**

  Store immutable HTTP/Stripe operation and inbound descriptors. Reuse
  endpoint/credential fingerprint code and include effect evidence. Keep
  request rendering, credentials, transport, pagination, retry policy,
  process state, commands, timers, and database access out of descriptors.

- [ ] **Step 6: Run GREEN**

  ~~~bash
  cargo test -p donat-metadata --test types_serde
  cargo test -p donat-server --test connectors_http
  cargo test -p donat-server --test connectors_stripe
  cargo test -p donat-server --test state
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test connectors \
    http_typed_input_contract_is_enforced
  cargo test -p donat-conformance --test connectors \
    connector_effect_contract_is_enforced
  ~~~

  Expected: old metadata still loads, incomplete mutations are inventory-only,
  and executable descriptors use only the closed effect model.

- [ ] **Step 7: Commit**

  ~~~bash
  git add crates/metadata crates/server/src/connectors crates/server/tests \
    crates/conformance/tests/connectors.rs \
    crates/conformance/fixtures/connectors
  git commit -m "feat(connectors): publish closed operation effects"
  ~~~

---

### Task 3: Add the exact process grammar and source-local compiler

**Files:**

- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/src/loader.rs`
- Modify: `crates/metadata/src/lib.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/metadata/tests/load_fixture.rs`
- Create: `crates/metadata/tests/fixtures/processes/version.yaml`
- Create: `crates/metadata/tests/fixtures/processes/databases/databases.yaml`
- Create: `crates/metadata/tests/fixtures/processes/processes.yaml`
- Create: `crates/server/src/processes/mod.rs`
- Create: `crates/server/src/processes/definition.rs`
- Create: `crates/server/tests/process_definition.rs`
- Modify: `crates/server/src/lib.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/conformance/src/lib.rs`
- Create: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/metadata_contract.yaml`
- Create: `crates/conformance/fixtures/processes/source_locality.yaml`
- Create: `crates/conformance/fixtures/processes/effect_horizon.yaml`

**Interfaces consumed from Tasks 1-2:**

~~~rust
pub struct CommandDescriptor;
pub struct ConnectorDescriptorCatalog;
pub struct ValueContractCatalog;
pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: NonEmptyVec<ProviderIdempotentStep>,
    },
}

pub fn compile_command_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    catalog: &Catalog,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Result<CompiledSourceCommandCatalog, PlanError>;
~~~

**Complete metadata interfaces created here:**

~~~rust
pub struct ProcessDefinition {
    pub name: String,
    pub source: String,
    pub lifecycle: ProcessLifecycle,
    pub start: ProcessStart,
    pub input: BTreeMap<String, String>,
    pub state: BTreeMap<String, String>,
    pub initial_state: BTreeMap<String, ProcessValueBinding>,
    pub initial: String,
    pub cancellation: Option<ProcessCancellation>,
    pub states: BTreeMap<String, ProcessStateDefinition>,
}

pub enum ProcessLifecycle { Active, Retired }

pub struct ProcessStart {
    pub command: String,
    pub input: BTreeMap<String, ProcessStartBinding>,
    pub idempotency_key: ProcessStartBinding,
}

pub enum ProcessStartBinding {
    CommandArgument { command_argument: String },
    CommandResult { command_result: String },
}

pub struct ProcessCancellation {
    pub signal: String,
    pub correlate: BTreeMap<String, ProcessCommandCorrelation>,
    pub payload: BTreeMap<String, String>,
    pub on_cancel: String,
}

pub struct ProcessCommandCorrelation {
    pub type_ref: String,
    pub equals: ProcessValueBinding,
}

pub enum ProcessStateDefinition {
    Terminal(ProcessTerminalState),
    Activity(ProcessActivityState),
    WaitForSignal(ProcessVerifiedSignalState),
    WaitForCommand(ProcessCommandSignalState),
    Timer(ProcessTimerState),
}

pub struct ProcessTerminalState { pub terminal: bool }

pub struct ProcessActivityState {
    pub activity: ProcessActivity,
    pub on_success: ProcessTransition,
    pub on_error: ProcessActivityErrorRouting,
}

pub struct ProcessActivity {
    pub connector: String,
    pub operation: String,
    pub input: BTreeMap<String, ProcessValueBinding>,
    pub timeout: ProcessActivityTimeout,
    pub retry: ProcessRetry,
}

pub struct ProcessActivityTimeout {
    pub schedule_to_start: String,
    pub start_to_close: String,
}

pub struct ProcessRetry {
    pub retry_on: Vec<ProcessRetryableErrorKind>,
    pub max_attempts: u32,
    pub initial_interval: String,
    pub max_interval: String,
    pub jitter: ProcessRetryJitter,
}

pub enum ProcessRetryJitter { DeterministicFull }
pub enum ProcessRetryableErrorKind {
    Transport,
    Timeout,
    #[serde(rename = "http_429")]
    Http429,
    #[serde(rename = "http_5xx")]
    Http5xx,
}

pub struct ProcessActivityErrorRouting {
    pub routes: Vec<ProcessActivityErrorRoute>,
    pub fallback: ProcessActivityErrorFallback,
}

pub struct ProcessActivityErrorRoute {
    pub kinds: Vec<ProcessActivityErrorKind>,
    pub next: String,
}

pub struct ProcessActivityErrorFallback { pub next: String }

pub enum ProcessActivityErrorKind {
    Transport,
    Timeout,
    #[serde(rename = "http_429")]
    Http429,
    #[serde(rename = "http_5xx")]
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
    RetryExhausted,
}

pub struct ProcessVerifiedSignalState {
    pub wait_for_signal: ProcessVerifiedSignalWait,
    pub on_signal: ProcessTransition,
    pub timeout: Option<ProcessTimeout>,
}

pub struct ProcessVerifiedSignalWait {
    pub connector: String,
    pub event: String,
    pub provider_event_id: ProcessVerifiedEventField,
    pub correlate: BTreeMap<String, ProcessVerifiedEventCorrelation>,
}

pub struct ProcessVerifiedEventField { pub event: String }

pub struct ProcessVerifiedEventCorrelation {
    pub from: ProcessVerifiedEventField,
    pub equals: ProcessValueBinding,
}

pub struct ProcessCommandSignalState {
    pub wait_for_command: ProcessCommandSignalWait,
    pub on_signal: ProcessTransition,
    pub timeout: Option<ProcessTimeout>,
}

pub struct ProcessCommandSignalWait {
    pub signal: String,
    pub correlate: BTreeMap<String, ProcessCommandCorrelation>,
    pub payload: BTreeMap<String, String>,
}

pub struct ProcessTimerState { pub timer: ProcessTimer }
pub struct ProcessTimer {
    pub after: String,
    pub on_timeout: ProcessTransition,
}
pub struct ProcessTimeout {
    pub after: String,
    pub on_timeout: ProcessTransition,
}

pub struct ProcessTransition {
    pub guard: Option<ProcessRuleCall>,
    pub command: Option<ProcessCommandInvocation>,
    pub set: BTreeMap<String, ProcessValueBinding>,
    pub next: String,
    pub on_rejection: Option<String>,
}

pub struct ProcessRuleCall {
    pub rule: String,
    pub bindings: BTreeMap<String, ProcessValueBinding>,
}

pub struct ProcessCommandInvocation {
    pub name: String,
    pub run_as_role: String,
    pub input: BTreeMap<String, ProcessValueBinding>,
    pub session_variables: BTreeMap<String, ProcessValueBinding>,
}

pub enum ProcessValueBinding {
    Literal { literal: serde_json::Value },
    Input { input: String },
    State { state: String },
    ActivityResult { activity_result: String },
    VerifiedSignal { verified_signal: String },
    CommandResult { command_result: String },
    Rule {
        rule: String,
        bindings: BTreeMap<String, ProcessValueBinding>,
    },
}
~~~

Every struct/untagged enum denies unknown fields; lifecycle defaults to
`active`; enum spellings are `snake_case`; `type_ref` serializes as `type`;
Rule bindings serialize as `with`. Durations are non-zero decimal plus
`ms|s|m|h|d`.

**Compiler interfaces created here:**

~~~rust
pub const PROCESS_RUNTIME_ABI: u32 = 1;
pub const MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS: u64 = 5_000;

pub struct CompiledProcessCatalog {
    pub sources: BTreeMap<String, CompiledSourceProcessCatalog>,
    pub connector_sources: BTreeMap<String, String>,
}

pub struct CompiledSourceProcessCatalog {
    pub source_name: String,
    pub processes: BTreeMap<String, CompiledProcessDefinition>,
}

pub struct CompiledProcessDefinition {
    pub definition: ProcessDefinition,
    pub lifecycle: ProcessLifecycle,
    pub revision: String,
    pub canonical_definition: serde_json::Value,
    pub dependencies: ProcessDependencyDescriptors,
    pub signal_contract_fingerprints: BTreeMap<String, String>,
}

pub struct ProcessDependencyDescriptors {
    pub commands: BTreeMap<(String, String), CommandDescriptor>,
    pub rules: BTreeMap<String, PinnedRuleBundle>,
    pub connector_operations:
        BTreeMap<(String, String), PinnedConnectorOperation>,
    pub connector_inbound_events:
        BTreeMap<(String, String), PinnedConnectorInboundEvent>,
}

pub struct PinnedRuleBundle {
    pub profile_version: u16,
    pub declared_types: BTreeMap<String, donat_rules::RuleType>,
    pub definitions: BTreeMap<String, donat_rules::RuleDefinition>,
    pub canonical_ast_sha256: BTreeMap<String, String>,
    pub source_sha256: BTreeMap<String, String>,
}

pub struct PinnedConnectorOperation {
    pub source_name: String,
    pub descriptor: ConnectorOperationDescriptor,
    pub activity_send_horizons_ms:
        BTreeMap<String, BTreeMap<CompiledStepId, u64>>,
}

pub struct PinnedConnectorInboundEvent {
    pub source_name: String,
    pub descriptor: ConnectorInboundEventDescriptor,
}

pub fn compile_process_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    commands: &CompiledSourceCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledSourceProcessCatalog, PlanError>;

pub fn compile_process_catalog(
    metadata: &Metadata,
    commands: &CompiledCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledProcessCatalog, PlanError>;
~~~

For attempt `i`, the retry upper bound is
`min(max_interval, initial_interval * 2^(i-1))`. For each side-effecting step:

~~~text
maximum_send_horizon_ms =
  max_attempts * start_to_close
  + sum i=1..(max_attempts-1) of (
    MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS
    + retry_delay_upper_bound(i)
    + schedule_to_start
  )
~~~

Checked compilation requires that value to be at most
`minimum_retention_ms - clock_safety_margin_ms`. Capacity/rate/serialization
wait is bounded by each attempt's schedule-to-start deadline; `Retry-After`
cannot exceed the corresponding retry-delay upper bound. Missing, unbounded,
overflowed, or one-millisecond-over policies reject. Each activity state and
provider side-effect step is pinned. Each `start_to_close` term covers all
compiled step/page/call/redirect work within that attempt, including repeated
sends of the same step; the operation deadline must fit inside it and the HTTP
client has no hidden transport retry.

**Tests and conformance owned by this task:**

- Metadata/compiler tests:
  `process_metadata_round_trips_every_state_variant`,
  `process_metadata_requires_start`,
  `process_metadata_cancellation_is_exact`,
  `process_metadata_rejects_unknown_fields`,
  `process_metadata_rejects_mixed_state_kinds`,
  `process_metadata_wait_for_command_is_typed`,
  `process_metadata_rejects_invalid_duration_and_retry_kind`,
  `process_metadata_binding_context_is_closed`,
  `process_metadata_command_requires_session_and_rejection`,
  `process_grammar_has_no_workflow_nodes_or_items`,
  `process_rejects_cross_source_start_command`,
  `process_rejects_cross_source_transition_command`,
  `process_rejects_cross_source_command_effects`,
  `process_start_command_requires_matching_raw_effect`,
  `process_connector_instance_has_one_source`,
  `process_session_variables_are_closed`,
  `process_revision_contains_executable_dependency_closure`,
  `connector_effect_retention_boundary_is_exact`,
  and `connector_effect_multistep_horizon_is_independent`.
- Conformance `process_metadata_contract_is_exact` with
  `fixtures/processes/metadata_contract.yaml`.
- Conformance `process_source_locality_is_rejected_at_deploy` with
  `fixtures/processes/source_locality.yaml`.
- Conformance `process_effect_horizon_is_bounded` with
  `fixtures/processes/effect_horizon.yaml`.

- [ ] **Step 1: Add all three failing native conformance cases**

  The metadata fixture includes all five states and every binding plus
  unknown/mixed/node/item/loop/subworkflow negatives. The source fixture
  crosses start, transition, effects, and connector ownership separately.
  The horizon fixture has read-only, exact-equality, one-millisecond-over,
  overflow, missing-bound, and independent multi-step cases.

- [ ] **Step 2: Add every named metadata/compiler test**

  Keep the complete YAML in the metadata fixture. Assert full deterministic
  metadata paths/messages. Pin complete Rule source/type closure and connector
  effect/source/horizon data; hashes alone are insufficient.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-metadata processes
  cargo test -p donat-server --test process_definition
  cargo test -p donat-conformance --test processes \
    process_metadata_contract_is_exact
  cargo test -p donat-conformance --test processes \
    process_source_locality_is_rejected_at_deploy
  cargo test -p donat-conformance --test processes \
    process_effect_horizon_is_bounded
  ~~~

  Expected: process metadata, compiler, horizon validation, and conformance
  support do not exist.

- [ ] **Step 4: Implement exact serde and source-local compilation**

  Add absent/inline/quoted-include loading. Validate closed binding contexts,
  graph reachability/totality, matching raw start effects, roles/session
  mappings, Rule contexts, typed activity/signal contracts, total error
  routing, and one-source connector ownership. Reject workflow node/item
  spellings; do not translate them.

- [ ] **Step 5: Implement checked per-step horizon pinning**

  Calculate the complete formula with checked integers. Treat all schedule,
  capacity, rate, serialization, lease, takeover, retry, jitter, and
  `Retry-After` delays exactly as bounded above. A read-only operation stays
  headerless and needs no retention window.

- [ ] **Step 6: Expose the compiler module, not workers**

  Add `pub mod processes` to the server library and `mod processes` to the
  binary. Task 3 creates no effect-source trait, journal DML, connector call,
  worker loop, runtime route, or operator surface.

- [ ] **Step 7: Run GREEN**

  ~~~bash
  cargo test -p donat-metadata
  cargo test -p donat-server --test process_definition
  cargo test --workspace --no-run
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_metadata_contract_is_exact
  cargo test -p donat-conformance --test processes \
    process_source_locality_is_rejected_at_deploy
  cargo test -p donat-conformance --test processes \
    process_effect_horizon_is_bounded
  ~~~

  Expected: pure source-local compilation passes and no process row or worker
  exists.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/metadata crates/server/src/processes \
    crates/server/src/lib.rs crates/server/src/main.rs \
    crates/server/tests/process_definition.rs crates/conformance/src/lib.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes
  git commit -m "feat(processes): compile source-local definitions"
  ~~~

---

### Task 4: Build the neutral effect catalog and pure candidate stages

**Files:**

- Create: `crates/schema/src/process_effects.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/multi_source.rs`
- Create: `crates/schema/tests/process_effects.rs`
- Modify: `crates/server/src/processes/definition.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/tests/state.rs`

**Interfaces consumed from Tasks 1-3:**

~~~rust
pub enum ProcessStartPolicy { Enabled, RejectRetired }
pub struct CompiledCommandCatalog;
pub struct CompiledProcessCatalog;
pub struct CompiledProcessDefinition;
pub struct ConnectorDescriptorCatalog;

pub fn compile_process_catalog(
    metadata: &Metadata,
    commands: &CompiledCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledProcessCatalog, PlanError>;
~~~

**Interfaces created here:**

~~~rust
// crates/schema/src/process_effects.rs
pub struct ProcessEffectContractCatalog {
    pub sources:
        BTreeMap<String, BTreeMap<String, ProcessEffectContract>>,
}

pub struct ProcessEffectContract {
    pub process_name: String,
    pub current_revision: String,
    pub start_policy: ProcessStartPolicy,
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

pub trait ProcessEffectContractSource {
    fn process_effect_contract(
        &self,
        source: &str,
        process: &str,
    ) -> Option<&ProcessEffectContract>;
}

impl ProcessEffectContractSource for ProcessEffectContractCatalog { /* exact */ }

pub enum FinalizedCommandEffect {
    Start(FinalizedStartProcessEffect),
    Signal(FinalizedSignalProcessEffect),
}

pub struct FinalizedStartProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub start_policy: ProcessStartPolicy,
    pub input: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    pub effect_position: u32,
}

pub struct FinalizedSignalProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub signal_name: String,
    pub correlation: BTreeMap<String, CommandExecutionValue>,
    pub payload: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    pub effect_position: u32,
}

pub fn finalize_command_effects(
    commands: CompiledCommandCatalog,
    contracts: &dyn ProcessEffectContractSource,
) -> Result<FinalizedCommandCatalog, PlanError>;

impl CompiledMultiSourceSchema {
    pub fn compile_with_command_catalog_and_process_effects(
        metadata: &Metadata,
        catalogs: &HashMap<String, Catalog>,
        rules: &RuleCatalog,
        commands: &FinalizedCommandCatalog,
        process_effects: &ProcessEffectContractCatalog,
        infer_function_permissions: bool,
    ) -> Result<Self, PlanError>;
}

// crates/server/src/processes/definition.rs
impl ProcessEffectContractCatalog {
    pub fn from_processes(
        processes: &CompiledProcessCatalog,
    ) -> Result<Self, PlanError>;
}

pub struct PureEngineCandidate {
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    pub process_catalog: Arc<CompiledProcessCatalog>,
    pub process_effects: Arc<ProcessEffectContractCatalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
}
~~~

`PureEngineCandidate` performs only stages 1-7: Rules, pre-process commands,
connector descriptors, processes, neutral effects, effect finalization, and
schema compilation. It performs no catalog/journal I/O and is not yet
published as `Engine`; Task 6 adds read-only deployed validation first.
Task 4 creates the trait and its only process-catalog implementation together.

**Tests owned by this task:**

- `process_effect_contract_source_is_neutral_and_object_safe`
- `process_effect_catalog_retains_active_and_retired_policy`
- `process_effect_signal_compatibility_is_explicit`
- `process_candidate_build_is_cycle_free`
- `process_candidate_stages_are_pure`
- `process_candidate_failure_keeps_old_engine`
- `process_effect_finalization_preserves_pre_process_fingerprint`

- [ ] **Step 1: Add all failing schema/state tests**

  Build an active A, explicit retired A, compatible/incompatible signal
  revisions, and a raw command catalog. Assert exact stage order and that no
  database client or server connector implementation enters schema.

- [ ] **Step 2: Run RED**

  ~~~bash
  cargo test -p donat-schema --test process_effects
  cargo test -p donat-server --test state process_candidate
  ~~~

  Expected: the neutral source trait, finalized catalog, pure candidate, and
  new schema entry point are absent.

- [ ] **Step 3: Implement stages 1-7 and the trait**

  Add all repeated interfaces. `from_processes` maps `active` to `Enabled` and
  explicit `retired` to `RejectRetired`. Finalization validates exact
  source/type/binding/compatible revisions without changing the pre-process
  command fingerprint. Keep the process compiler server-owned.

- [ ] **Step 4: Remove stale state commentary**

  In the first edit to `state.rs`, replace the stale `sync_sources` comment
  saying "`run_sql` untracks" with English documentation that source refresh
  rebuilds a pure immutable candidate and runtime DDL/admin mutation APIs do
  not exist.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-schema --test process_effects
  cargo test -p donat-schema
  cargo test -p donat-server --test state process_candidate
  cargo test --workspace --no-run
  ~~~

  Expected: stages 1-7 are pure and cycle-free; Engine publication still has
  no process runtime.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/schema crates/server/src/processes/definition.rs \
    crates/server/src/state.rs crates/server/tests/state.rs
  git commit -m "feat(processes): compile neutral command effects"
  ~~~

---

### Task 5: Preserve refinery-only migration and select real metadata sources

**Files:**

- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/migrate.rs`
- Create: `crates/server/src/validate.rs`
- Modify: `crates/server/src/lib.rs`
- Create: `crates/server/tests/cli.rs`
- Create: `crates/server/tests/source_selection.rs`
- Modify: `crates/conformance/src/lib.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/source_selection/`

**Interfaces consumed from Tasks 1-4:**

~~~rust
pub fn compile_command_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    catalog: &Catalog,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Result<CompiledSourceCommandCatalog, PlanError>;

pub fn compile_process_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    commands: &CompiledSourceCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledSourceProcessCatalog, PlanError>;
~~~

**Interfaces created here:**

~~~rust
pub enum DeploymentSelection {
    RefineryOnly {
        database_url: String,
        migrations_dir: PathBuf,
    },
    MetadataSource {
        metadata_dir: PathBuf,
        source_name: String,
        database_url: String,
        migrations_dir: Option<PathBuf>,
    },
}

pub fn resolve_migrate_selection(
    cli: &MigrateArgs,
    env: &dyn EnvironmentLookup,
) -> anyhow::Result<DeploymentSelection>;

pub fn resolve_validate_selection(
    cli: &ValidateArgs,
    env: &dyn EnvironmentLookup,
) -> anyhow::Result<MetadataSourceSelection>;

pub struct MetadataSourceSelection {
    pub metadata_dir: PathBuf,
    pub source_name: String,
    pub database_url: String,
}
~~~

Refinery-only mode is selected only when no metadata directory is supplied.
It accepts explicit `--database-url`, `--metadata-database-url`,
`DONAT_DATABASE_URL`, or the existing `DONAT_GRAPHQL_DATABASE_URL` alias,
rejects `--source`, applies pending migrations, and exits without loading
metadata or reconciling anything. Metadata-aware migrate/validate resolves
exactly one Postgres source and its own URL; omission of `--source` is valid
only for one unambiguous Postgres source. Validate always requires metadata.

**Tests and conformance owned by this task:**

- `metadata_free_migrate_preserves_refinery_only_mode`
- `metadata_free_migrate_rejects_source`
- `metadata_aware_migrate_selects_source_url_before_connect`
- `metadata_aware_migrate_rejects_ambiguous_omission`
- `validate_requires_metadata`
- `validate_checks_only_selected_real_catalog`
- Conformance `process_deployment_selects_one_real_source`
- Fixture directory `fixtures/processes/source_selection/`

- [ ] **Step 1: Add the failing native conformance case**

  Create two metadata Postgres sources with distinct real URLs and one
  same-named table mismatch. Prove selecting `secondary` never connects to or
  reports the `default` catalog. Add the ambiguous no-source CLI failure.

- [ ] **Step 2: Add the failing CLI/integration tests**

  Run refinery-only migration against an empty temporary database with no
  metadata tree. Assert `--source` is rejected there. For metadata-aware
  cases use recording connection factories to prove URL selection occurs
  before introspection.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test cli metadata_free_migrate
  cargo test -p donat-server --test source_selection
  cargo test -p donat-conformance --test processes \
    process_deployment_selects_one_real_source
  ~~~

  Expected: one global URL/catalog is still reused and the exact selection
  interfaces do not exist.

- [ ] **Step 4: Implement the two migrate modes**

  Resolve selection before opening a connection. In metadata mode compile
  only the selected source with the two source-scoped compiler entry points.
  Never clone one `Catalog` across source names. Update the native harness to
  invoke metadata suites with
  `migrate --metadata-dir <suite> --source default`.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test cli
  cargo test -p donat-server --test source_selection
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_deployment_selects_one_real_source
  ~~~

  Expected: refinery-only migration remains metadata-free and metadata-aware
  operations use only the selected real source.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/main.rs crates/server/src/migrate.rs \
    crates/server/src/validate.rs crates/server/src/lib.rs \
    crates/server/tests crates/conformance/src/lib.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/source_selection
  git commit -m "fix(migrate): select exact metadata sources"
  ~~~

---

### Task 6: Add V6, deployment reconciliation, and read-only serving load

**Files:**

- Create: `migrations/V6__donat_processes.sql`
- Create: `crates/server/src/processes/reconcile.rs`
- Create: `crates/server/src/processes/catalog.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/migrate.rs`
- Modify: `crates/server/src/validate.rs`
- Modify: `crates/server/src/state.rs`
- Create: `crates/server/tests/process_migration.rs`
- Create: `crates/server/tests/process_reconcile.rs`
- Modify: `crates/server/tests/state.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/deployed_revisions/`
- Create: `crates/conformance/fixtures/processes/shared_database/`

**Interfaces consumed from Tasks 3-5:**

~~~rust
pub struct PureEngineCandidate {
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    pub process_catalog: Arc<CompiledProcessCatalog>,
    pub process_effects: Arc<ProcessEffectContractCatalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
}

pub struct CompiledSourceProcessCatalog;
pub struct CompiledProcessCatalog;
pub struct ConnectorRegistry;
pub struct SourceRuntime;
pub enum DeploymentSelection;
~~~

**Interfaces created here:**

~~~rust
// crates/server/src/processes/reconcile.rs
pub async fn reconcile(
    source_name: &str,
    database_url: &str,
    source_catalog: &donat_catalog::Catalog,
    compiled_processes: &CompiledSourceProcessCatalog,
) -> anyhow::Result<()>;

pub async fn validate_check_violation_helper(
    client: &tokio_postgres::Client,
) -> anyhow::Result<()>;

// crates/server/src/processes/catalog.rs
pub struct DeployedProcessCatalog {
    pub sources: BTreeMap<String, DeployedSourceProcessCatalog>,
}

pub struct DeployedSourceProcessCatalog {
    pub active:
        BTreeMap<String, Arc<CompiledProcessDefinition>>,
    pub live_retired:
        BTreeMap<(String, String), Arc<CompiledProcessDefinition>>,
}

pub async fn validate_serving_catalogs(
    runtimes: &HashMap<String, SourceRuntime>,
    process_catalog: &CompiledProcessCatalog,
    command_catalog: &CompiledCommandCatalog,
    connectors: &ConnectorRegistry,
) -> anyhow::Result<DeployedProcessCatalog>;

// crates/server/src/state.rs
pub struct Engine {
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub process_catalog: Arc<CompiledProcessCatalog>,
    pub deployed_process_catalog: Arc<DeployedProcessCatalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
    // existing fields remain unchanged
}
~~~

V6 owns `donat.check_violation(text)`, adds unique non-null
`command_invocations.invocation_id`, and creates these source-qualified
tables exactly:

~~~text
process_definition_versions
process_start_requests
process_instances
process_events
process_signal_requests
process_activity_jobs
process_activity_provider_steps
process_transition_logs
process_capacity_reservations
process_capacity_buckets
process_inbound_deliveries
process_inbound_events
~~~

Every primary/foreign/semantic/partial/due/reconciliation key and predicate
begins with `source_name`. `process_activity_provider_steps` is keyed by
`(source_name, logical_activity_id, compiled_step_id)` and stores the fixed
key, database-clock `first_provider_attempt_at`, compiled
`maximum_send_deadline_at`, and usable-window deadline.
Accepted inbound deliveries have source-qualified non-null instance/event
foreign keys; every other outcome has both null. No process table references
the command journal.

Reconciliation inserts immutable executable definition/dependency JSON,
activates current revisions, explicitly retires prior/current declarations,
rejects omission while non-terminal work exists, and rejects incompatible
command/Rule/connector/ABI/signal/source-binding removal. It checks all
metadata source names sharing one physical database before connector rebind.
The loader hash-verifies and recompiles active plus non-terminal live-retired
revisions. Serve calls only `validate_serving_catalogs` and the helper
compatibility read; it never calls `reconcile`.

**Tests and conformance owned by this task:**

- `process_v6_schema_is_exact`
- `process_sources_sharing_database_are_isolated`
- `process_reconcile_is_idempotent_and_source_local`
- `process_live_connector_rebind_is_rejected`
- `process_retired_revision_reloads_and_completes`
- `process_persisted_rule_bundle_recompiles_exactly`
- `serve_with_readonly_role_issues_no_ddl`
- `serve_rejects_missing_or_incompatible_check_helper`
- `process_candidate_failure_keeps_old_engine`
- Conformance `process_deployed_revision_contract_is_enforced`
- Conformance `process_shared_database_sources_are_isolated`
- Fixture directories `deployed_revisions/` and `shared_database/`

- [ ] **Step 1: Add both failing native conformance suites**

  Add active A, replacement B, explicit retirement, omission-while-live,
  fresh-binary reload, connector source rebind, and two source names sharing
  one physical database. Assert exact deployment errors and that retired A
  remains executable.

- [ ] **Step 2: Add failing migration/reconcile/serve tests**

  Inspect every V6 column, check, FK, index, partial unique, and source-first
  predicate. Start serve with a role that has connect/select/execute but no
  create/alter permission and capture all SQL; assert no DDL/DML. Separately
  prove a missing/incompatible helper produces the migration instruction.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_migration
  cargo test -p donat-server --test process_reconcile
  cargo test -p donat-server --test state serve_with_readonly_role_issues_no_ddl
  cargo test -p donat-conformance --test processes \
    process_deployed_revision_contract_is_enforced
  cargo test -p donat-conformance --test processes \
    process_shared_database_sources_are_isolated
  ~~~

  Expected: V6, reconciliation, deployed loader, and read-only startup
  validation are absent; serve still installs the helper.

- [ ] **Step 4: Implement V6 and metadata-aware reconciliation**

  Put all DDL, including `create or replace function
  donat.check_violation(text)`, in V6. Implement source-local reconcile after
  selected-source migration/introspection. Persist executable definitions,
  complete Rule source/type closure, connector source/effect/horizons, hashes,
  and runtime ABI.

- [ ] **Step 5: Implement read-only serving publication**

  Delete `ensure_check_violation_helper` and every serving call to it. Build
  the pure candidate, call the exact `validate_serving_catalogs` signature,
  and atomically publish the four exact Engine fields only on success. Workers
  and verification later consume only `deployed_process_catalog`.

- [ ] **Step 6: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_migration
  cargo test -p donat-server --test process_reconcile
  cargo test -p donat-server --test state
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_deployed_revision_contract_is_enforced
  cargo test -p donat-conformance --test processes \
    process_shared_database_sources_are_isolated
  ~~~

  Expected: deploy performs all writes; serving under the read-only role
  validates and publishes executable catalogs with no DDL/DML.

- [ ] **Step 7: Commit**

  ~~~bash
  git add migrations/V6__donat_processes.sql crates/server/src/processes \
    crates/server/src/migrate.rs crates/server/src/validate.rs \
    crates/server/src/state.rs crates/server/tests \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/deployed_revisions \
    crates/conformance/fixtures/processes/shared_database
  git commit -m "feat(processes): deploy source-local journals"
  ~~~

---

### Task 7: Give command executions durable generation UUIDs

**Files:**

- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/sqlgen/src/lib.rs`
- Modify: `crates/sqlgen/tests/commands.rs`
- Modify: `crates/sqlgen/tests/snapshots/`
- Create: `crates/server/src/commands.rs`
- Modify: `crates/server/src/lib.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/gql.rs`
- Create: `crates/server/tests/commands.rs`
- Modify: `crates/conformance/tests/commands.rs`
- Create: `crates/conformance/fixtures/commands/invocation_generation/`

**Interfaces/schema consumed from Task 6:**

~~~text
donat.command_invocations.invocation_id uuid not null unique
~~~

~~~rust
pub struct CommandDescriptor;
pub struct FinalizedCommandCatalog;
~~~

**Interfaces created here:**

~~~rust
pub struct CommandInvocationGeneration {
    pub invocation_id: uuid::Uuid,
    pub replayed: bool,
}

pub struct CommandExecutionResult {
    pub invocation: CommandInvocationGeneration,
    pub result_json: serde_json::Value,
}
~~~

The first claim inserts a fresh database-generated UUID. An unexpired exact
replay returns the stored UUID/result. Reclaiming an expired tuple is a new
generation and replaces it with a new UUID even when
`(command_identity, scope_hash, key)` is reused. Changed-input and guard
rejection create no successful generation.

**Tests and conformance owned by this task:**

- `command_invocation_id_replays_unchanged`
- `command_invocation_id_changes_after_expiry`
- `command_changed_input_has_no_new_generation`
- `command_guard_rejection_has_no_generation`
- `command_invocation_sql_is_one_statement`
- Conformance `command_invocation_generation_is_stable`
- Fixture directory `fixtures/commands/invocation_generation/`

- [ ] **Step 1: Add the failing native conformance case**

  Execute first claim, exact replay, changed-input rejection, advance DB time
  beyond expiry, and re-execute. Query the journal through the test DB
  connection and assert UUID equality/inequality without adding an API field.

- [ ] **Step 2: Add failing SQLgen/Postgres tests and snapshots**

  Require the invocation UUID in the one command statement result. Snapshot
  insert, replay, and expiry reclaim branches. Read every insta diff.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-sqlgen --test commands command_invocation
  cargo test -p donat-server --test commands command_invocation
  cargo test -p donat-conformance --test commands \
    command_invocation_generation_is_stable
  ~~~

  Expected: the executor does not return/preserve a generation UUID.

- [ ] **Step 4: Implement generation claim/replay semantics**

  Extend the existing command CTE and decoder; do not add a second SQL
  statement. Keep the UUID internal and do not add an HTTP/admin field.

- [ ] **Step 5: Run GREEN and review snapshots**

  ~~~bash
  cargo test -p donat-sqlgen --test commands
  cargo insta test -p donat-sqlgen
  cargo insta review
  cargo test -p donat-server --test commands
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test commands \
    command_invocation_generation_is_stable
  ~~~

  Expected: exact replay preserves UUID, expired re-execution changes it, and
  command execution remains one statement.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/ir crates/sqlgen crates/server/src/commands.rs \
    crates/server/tests/commands.rs crates/conformance/tests/commands.rs \
    crates/conformance/fixtures/commands/invocation_generation
  git commit -m "feat(commands): persist execution generation ids"
  ~~~

---

### Task 8: Lower finalized effects into atomic process outboxes

**Files:**

- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/process_effects.rs`
- Modify: `crates/schema/src/plan_mutation.rs`
- Modify: `crates/schema/tests/process_effects.rs`
- Modify: `crates/sqlgen/tests/commands.rs`
- Modify: `crates/sqlgen/tests/snapshots/`
- Modify: `crates/server/tests/commands.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/command_effects/`

**Interfaces consumed from Tasks 4, 6, and 7:**

~~~rust
pub enum ProcessStartPolicy { Enabled, RejectRetired }
pub enum FinalizedCommandEffect {
    Start(FinalizedStartProcessEffect),
    Signal(FinalizedSignalProcessEffect),
}
pub struct CommandInvocationGeneration {
    pub invocation_id: uuid::Uuid,
    pub replayed: bool,
}
~~~

~~~text
process_start_requests unique(source_name, command_invocation_id, effect_position)
process_signal_requests unique(source_name, command_invocation_id, effect_position)
~~~

**Interfaces created here:**

~~~rust
pub enum ResolvedCommandEffect {
    StartProcess(ResolvedStartProcessEffect),
    SignalProcess(ResolvedSignalProcessEffect),
}

pub struct ResolvedStartProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub start_policy: ProcessStartPolicy,
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

pub fn lower_finalized_command_effects(
    command: &FinalizedCompiledCommand,
) -> Result<Vec<ResolvedCommandEffect>, PlanError>;
~~~

`CurrentExecution` lowers to the concrete UUID returned by the same command
claim CTE. Effects insert only on the first successful claim branch and share
that generation UUID. Exact replay, changed input, guard rejection, or any
database failure inserts no second outbox row. A `RejectRetired` start adds a
materialized pre-DML gate that raises SQLSTATE `P0D01` with path
`$.selectionSet.<command-field>` and message
`process '<source>.<process>' does not accept new starts` before the
idempotency claim and every domain CTE.

**Tests and conformance owned by this task:**

- `command_effect_positions_share_generation`
- `command_effect_replay_writes_no_second_outbox`
- `command_effect_failure_writes_no_outbox`
- `process_start_request_pins_revision`
- `process_signal_request_pins_contract_revision`
- `process_retired_start_rejects_before_domain_dml`
- `process_commands_cannot_invoke_connectors`
- Conformance `process_command_effects_are_atomic`
- Fixture directory `fixtures/processes/command_effects/`

- [ ] **Step 1: Add the failing native conformance case**

  Cover two canonical effect positions, replay, changed input, guard false,
  domain constraint failure, A request consumed after B deployment, signal
  revision anchor, and explicit retirement. Assert exact body/status/path and
  DB rows. Add a negative metadata case attempting provider execution from a
  command and assert the field is unknown.

- [ ] **Step 2: Add failing IR/schema/SQLgen tests**

  Require the exact interfaces above and snapshot the materialized retired
  gate before all domain CTEs. Assert no process table has a command-journal
  FK and no post-command Rust insert function exists.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-schema process_effect
  cargo test -p donat-sqlgen --test commands command_effect
  cargo test -p donat-server --test commands process_
  cargo test -p donat-conformance --test processes \
    process_command_effects_are_atomic
  ~~~

  Expected: finalized effects do not lower to one-statement outboxes.

- [ ] **Step 4: Implement exact IR/planner/SQL lowering**

  Reuse the existing command planner and literal escaping. Do not create
  `enqueue_start`, raw SQL, a connector call, or a second statement. Preserve
  the existing error decoder and one-statement response assembly.

- [ ] **Step 5: Run GREEN and review snapshots**

  ~~~bash
  cargo test -p donat-schema process_effect
  cargo test -p donat-sqlgen --test commands
  cargo insta test -p donat-sqlgen
  cargo insta review
  cargo test -p donat-server --test commands
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_command_effects_are_atomic
  ~~~

  Expected: domain result, command generation, and each process outbox commit
  atomically in one statement.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/ir crates/schema crates/sqlgen \
    crates/server/tests/commands.rs crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/command_effects
  git commit -m "feat(processes): write atomic command outboxes"
  ~~~

---

### Task 9: Consume starts through the pinned deployed catalog

**Files:**

- Create: `crates/server/src/processes/runtime.rs`
- Create: `crates/server/src/processes/start.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/src/main.rs`
- Create: `crates/server/tests/process_start.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/start_worker/`

**Interfaces/schema consumed from Tasks 6 and 8:**

~~~rust
pub struct DeployedProcessCatalog {
    pub sources: BTreeMap<String, DeployedSourceProcessCatalog>,
}

pub struct DeployedSourceProcessCatalog {
    pub active:
        BTreeMap<String, Arc<CompiledProcessDefinition>>,
    pub live_retired:
        BTreeMap<(String, String), Arc<CompiledProcessDefinition>>,
}
~~~

~~~text
process_start_requests(
  source_name, id, process_name, revision, input_json,
  command_invocation_id, effect_position, idempotency_key, status, instance_id
)
process_instances unique(source_name, process_name, start_idempotency_key)
process_events
process_transition_logs
~~~

**Interfaces created here:**

~~~rust
pub struct ProcessRuntime {
    pub source_name: String,
    pub pool: SourcePool,
    pub deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub connector_registry: Arc<ConnectorRegistry>,
}

pub enum StartConsumption {
    NoWork,
    Started { request_id: Uuid, instance_id: Uuid },
    Duplicate { request_id: Uuid, instance_id: Uuid },
}

impl ProcessRuntime {
    pub async fn consume_one_start(
        &self,
    ) -> anyhow::Result<StartConsumption>;
}
~~~

The worker selects one pending source-local request with
`FOR UPDATE SKIP LOCKED`. In one short transaction it resolves the exact
pinned revision from `deployed_catalog`, validates typed input, inserts or
finds the semantic instance, appends start history for a new instance, records
duplicate outcome otherwise, and consumes the request. It never compiles
metadata or substitutes current metadata. There is no public enqueue/start
function or route.

**Tests and conformance owned by this task:**

- `process_start_request_pins_revision`
- `process_start_semantic_dedupe_is_separate`
- `process_start_crash_before_commit_retries`
- `process_start_crash_after_commit_does_not_duplicate`
- `process_start_refuses_missing_deployed_revision`
- `process_workers_are_source_local`
- Conformance `process_start_worker_is_durable`
- Fixture directory `fixtures/processes/start_worker/`

- [ ] **Step 1: Add the failing native conformance case**

  Start A, deploy B before consumption, and prove the worker creates A. Use
  two distinct command generations with one semantic key and prove one
  instance. Inject failures before/after commit and restart the binary.

- [ ] **Step 2: Add failing runtime/Postgres tests**

  Use two source pools with identical UUID/name/key values. Assert every SQL
  predicate starts with source and a missing deployed revision stops the claim
  with invariant audit rather than current-catalog fallback.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_start
  cargo test -p donat-conformance --test processes \
    process_start_worker_is_durable
  ~~~

  Expected: no process runtime or start consumer exists.

- [ ] **Step 4: Implement start consumption and lifecycle wiring**

  Spawn one loop per process-owning source from the published Engine snapshot.
  Tokio polling is wake-up only. Use the exact short transaction and return
  values above. Shutdown cleanly with the existing server cancellation path.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_start
  cargo test -p donat-server --test state
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_start_worker_is_durable
  ~~~

  Expected: committed starts are consumed once by the pinned source/revision.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/state.rs \
    crates/server/src/main.rs crates/server/tests/process_start.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/start_worker
  git commit -m "feat(processes): consume pinned start requests"
  ~~~

---

### Task 10: Execute guarded command transitions with a savepoint

**Files:**

- Create: `crates/server/src/processes/transition.rs`
- Create: `crates/server/src/processes/command.rs`
- Modify: `crates/server/src/processes/runtime.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/commands.rs`
- Create: `crates/server/tests/process_transition.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/command_transition/`

**Interfaces consumed from Tasks 1, 3, 6, and 9:**

~~~rust
pub struct ProcessRuntime;
pub struct CompiledProcessDefinition;
pub struct ProcessCommandInvocation {
    pub name: String,
    pub run_as_role: String,
    pub input: BTreeMap<String, ProcessValueBinding>,
    pub session_variables: BTreeMap<String, ProcessValueBinding>,
}
pub struct CommandDescriptor;
~~~

**Interfaces created here:**

~~~rust
pub enum TransitionConsumption {
    NoWork,
    Advanced { instance_id: Uuid, event_id: Uuid },
    GuardFalse { instance_id: Uuid, event_id: Uuid },
    CommandRejected {
        instance_id: Uuid,
        event_id: Uuid,
        error: GraphQlErrorEnvelope,
    },
}

pub enum ProcessCommandOutcome {
    Applied { result: TypedValue },
    Rejected { error: GraphQlErrorEnvelope },
}

impl ProcessRuntime {
    pub async fn consume_one_transition(
        &self,
    ) -> anyhow::Result<TransitionConsumption>;
}

pub async fn execute_process_command_in_savepoint(
    transaction: &tokio_postgres::Transaction<'_>,
    command: &CompiledCommand,
    role: &str,
    arguments: TypedValue,
    session_variables: BTreeMap<String, TypedValue>,
) -> anyhow::Result<ProcessCommandOutcome>;
~~~

The outer transition locks the due event and instance, checks the deployed
revision/runtime ABI and optimistic version, evaluates a closed Rule guard,
and writes history/next work atomically. Command execution uses the existing
planner, one-statement renderer, result decoder, and exact classic role. It
runs inside `SAVEPOINT donat_process_command`. Only SQLSTATE `P0D01` with a
valid exact `donat.graphql-error.v1` envelope becomes `Rejected`: roll back
and release the savepoint, append one `command_rejected`, follow
`on_rejection`, and commit the outer transaction. Every other failure aborts
the outer transaction. No ambient GraphQL headers/session and no connector
call are available.

**Tests and conformance owned by this task:**

- `process_session_variables_are_closed`
- `process_command_later_assert_commits_on_rejection`
- `process_command_guard_prevents_first_write`
- `process_command_database_error_aborts_outer`
- `process_command_malformed_reserved_envelope_aborts_outer`
- `process_command_permission_error_aborts_outer`
- `process_transition_uses_deployed_revision_only`
- `process_provider_logic_is_activity_only`
- Conformance `process_command_transition_savepoint_is_exact`
- Fixture directory `fixtures/processes/command_transition/`

- [ ] **Step 1: Add the failing native conformance case**

  Use two separate commands. The later-assert command performs a valid first
  domain write and then a valid `assert` step whose predicate is false; prove
  domain DML/journal/effect CTEs roll back while one rejection transition
  commits. The guard case uses a process transition Rule guard that is false
  before the command and proves the first domain CTE is never reached. Also
  cover malformed `P0D01`, permission `23514`, and a real constraint error.

- [ ] **Step 2: Add failing transaction/session tests**

  Assert missing/extra/ambient session variables reject, only compiled
  mappings reach the command, and a command cannot construct/invoke a
  connector or provider request.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_transition
  cargo test -p donat-conformance --test processes \
    process_command_transition_savepoint_is_exact
  ~~~

  Expected: ordinary transitions and savepoint rejection handling are absent.

- [ ] **Step 4: Implement transition and command execution**

  Reuse the exact command internals; do not add raw SQL, a new rejection
  envelope, row-by-row business mutation, or post-command outbox insert.
  Validate all typed mappings before opening the transaction.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_transition
  cargo test -p donat-server --test commands
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_command_transition_savepoint_is_exact
  ~~~

  Expected: only a valid structured business rejection follows
  `on_rejection`; all other failures commit nothing.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/commands.rs \
    crates/server/tests/process_transition.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/command_transition
  git commit -m "feat(processes): execute guarded command transitions"
  ~~~

---

### Task 11: Lease connector activities with exact capacity and step windows

**Files:**

- Create: `crates/server/src/processes/activity.rs`
- Create: `crates/server/src/processes/capacity.rs`
- Modify: `crates/server/src/processes/runtime.rs`
- Modify: `crates/server/src/processes/transition.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/connectors/http.rs`
- Create: `crates/server/tests/process_activity.rs`
- Create: `crates/server/tests/process_capacity.rs`
- Create: `crates/server/tests/process_activity_takeover.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/activity_runtime/`

**Interfaces consumed from Tasks 2, 3, 6, and 10:**

~~~rust
pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: NonEmptyVec<ProviderIdempotentStep>,
    },
}

pub struct ProviderIdempotentStep {
    pub step: CompiledStepId,
    pub fixed_binding: FixedIdempotencyBinding,
    pub scope: ProviderIdempotencyScope,
    pub minimum_retention_ms: NonZeroU64,
    pub clock_safety_margin_ms: NonZeroU64,
}

pub const MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS: u64 = 5_000;
pub struct ProcessRuntime;
pub struct PinnedConnectorOperation {
    pub source_name: String,
    pub descriptor: ConnectorOperationDescriptor,
    pub activity_send_horizons_ms:
        BTreeMap<String, BTreeMap<CompiledStepId, u64>>,
}
~~~

**Interfaces created here:**

~~~rust
pub struct ClaimedActivity {
    pub source_name: String,
    pub job_id: Uuid,
    pub instance_id: Uuid,
    pub state_name: String,
    pub logical_activity_id: String,
    pub lease_token: Uuid,
    pub attempt: u32,
    pub input: TypedValue,
    pub request_fingerprint: String,
    pub descriptor: Arc<ConnectorOperationDescriptor>,
    pub start_to_close_deadline: DateTime<Utc>,
}

pub enum ActivityClaim {
    NoWork,
    Claimed(ClaimedActivity),
    ScheduleToStartTimedOut { job_id: Uuid },
    CapacityDeferred { job_id: Uuid, available_at: DateTime<Utc> },
}

pub enum ProviderStepAuthorization {
    ReadOnly,
    ProviderIdempotent {
        key: String,
        binding: FixedIdempotencyBinding,
        first_provider_attempt_at: DateTime<Utc>,
        maximum_send_deadline_at: DateTime<Utc>,
        usable_window_expires_at: DateTime<Utc>,
    },
}

impl ProcessRuntime {
    pub async fn claim_one_activity(
        &self,
    ) -> anyhow::Result<ActivityClaim>;

    pub async fn authorize_provider_step(
        &self,
        activity: &ClaimedActivity,
        step: &CompiledStepId,
    ) -> Result<ProviderStepAuthorization, ConnectorFailure>;

    pub async fn complete_activity(
        &self,
        activity: &ClaimedActivity,
        outcome: Result<TypedValue, ConnectorFailure>,
    ) -> anyhow::Result<()>;
}
~~~

Claim protocol is exact: lock one due job; read
`statement_timestamp()` once; handle schedule timeout; create/lock one
source/connector/operation token bucket in job-then-bucket order; release
expired reservations; enforce global max-in-flight and typed serialization;
refill exact numeric tokens; defer without consuming when unavailable; or
consume one token, insert reservation, assign lease, increment attempt, and
commit. Provider observation starts only after another Postgres connection
can see committed job/lease/reservation state.

For each provider-idempotent step,
`authorize_provider_step` derives:

~~~text
base64url_no_pad(SHA256(
  "donat.connector.idempotency.step.v1\0" ||
  JCS({ "logical_activity_id": logical_activity_id,
        "scope": scope,
        "step": compiled_step_id })
))
~~~

It inserts/reads the source-local provider-step row in a separate short
transaction committed before the first network send. Equality with
both `first_provider_attempt_at + maximum_send_horizon_ms` and
`first_provider_attempt_at + minimum_retention_ms -
clock_safety_margin_ms` is allowed. A later DB-clock instant refuses I/O:
the compiled deadline takes the typed timeout path, and the provider deadline
returns `connector_idempotency_window_exhausted`. Neither rotates the key.
Read-only calls are headerless. The HTTP client has no hidden retry.
Connector pagination, when used, remains inside one activity's compiled
page/item/call/byte/deadline budget and cannot create process states,
branches, retries, timers, commands, or database writes.

**Tests and conformance owned by this task:**

- `process_activity_does_not_hold_tx`
- `process_provider_logic_is_activity_only`
- `process_readonly_takeover_is_headerless`
- `process_lease_takeover_is_safe`
- `process_late_takeover_refuses_before_io`
- `process_activity_capacity_is_global`
- `process_capacity_bucket_serializes_two_claimers`
- `process_activity_rate_bucket_refill_is_exact`
- `process_activity_serialization_key_is_global`
- `process_activity_stale_completion_is_audit_only`
- `process_activity_retry_is_bounded_and_deterministic`
- `connector_pagination_is_bounded_transport_only`
- Conformance `process_activity_runtime_is_durable`
- Fixture directory `fixtures/processes/activity_runtime/`

- [ ] **Step 1: Add the failing native conformance case**

  Use a Donat-owned provider stub that blocks until a separate DB connection
  observes committed intent/reservation. Cover read-only takeover, provider
  mutation takeover with stable per-step keys, multi-step independent keys,
  exact usable-window equality, one millisecond late refusal, retry
  exhaustion, schedule/start timeout, and bounded pagination.

- [ ] **Step 2: Add failing two-connection and two-binary tests**

  Put a barrier after two claimers have selected work. Prove the bucket lock
  prevents oversubscribing the final token/slot/serialization key. Control the
  database clock and inject stale completions/worker loss. Assert no provider
  call occurs under an open journal transaction.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_activity
  cargo test -p donat-server --test process_capacity
  cargo test -p donat-server --test process_activity_takeover
  cargo test -p donat-conformance --test processes \
    process_activity_runtime_is_durable
  ~~~

  Expected: activity jobs are not claimed or dispatched.

- [ ] **Step 4: Implement capacity and lease transactions**

  Use exact numeric refill and source-first predicates. A token is never
  refunded; release/expiry restores only in-flight capacity. Never hold more
  than one bucket lock. Commit before any connector or provider logic.

- [ ] **Step 5: Implement step authorization and connector dispatch**

  Resolve only the pinned descriptor/catalog. Commit first-attempt time before
  each side-effecting step's first send, recheck database time before every
  later send, and inject the key into only the fixed binding. Keep retry,
  process state, commands, timers, and DB handles out of connector code.

- [ ] **Step 6: Implement completion/retry**

  Match the current lease token in a new short transaction; release the
  reservation and append completion atomically. Stale completion is
  append-only audit. Apply finite deterministic jitter and declared typed
  error routing only.

- [ ] **Step 7: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_activity
  cargo test -p donat-server --test process_capacity
  cargo test -p donat-server --test process_activity_takeover
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_activity_runtime_is_durable
  ~~~

  Expected: committed activities are at-least-once only within the fixed
  provider window, and global capacity cannot be oversubscribed.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/connectors \
    crates/server/tests/process_activity.rs \
    crates/server/tests/process_capacity.rs \
    crates/server/tests/process_activity_takeover.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/activity_runtime
  git commit -m "feat(processes): run durable connector activities"
  ~~~

---

### Task 12: Add timers, command signals, and linked inbound audit

**Files:**

- Create: `crates/server/src/processes/timer.rs`
- Create: `crates/server/src/processes/signal.rs`
- Create: `crates/server/src/processes/inbound.rs`
- Modify: `crates/server/src/processes/runtime.rs`
- Modify: `crates/server/src/processes/transition.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/connectors/stripe.rs`
- Create: `crates/server/tests/process_timer.rs`
- Create: `crates/server/tests/process_signal.rs`
- Create: `crates/server/tests/process_inbound.rs`
- Modify: `crates/conformance/tests/connectors.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/signals_and_ingress/`

**Interfaces/schema consumed from Tasks 6, 8, and 9:**

~~~text
process_signal_requests
process_events
process_inbound_deliveries(
  source_name, id, connector_instance, provider_event_id, payload_digest,
  signature_status, outcome, instance_id, process_event_id,
  redacted_metadata, received_at
)
process_inbound_events(
  source_name, id, connector_instance, provider_event_id, first_delivery_id,
  payload_digest, verified_at
)
~~~

~~~rust
pub struct ProcessRuntime;
pub struct DeployedSourceProcessCatalog;
pub struct ConnectorInboundEventDescriptor;
~~~

**Interfaces created here:**

~~~rust
pub enum SignalConsumption {
    NoWork,
    Accepted { request_id: Uuid, instance_id: Uuid, event_id: Uuid },
    Duplicate { request_id: Uuid },
    Unmatched { request_id: Uuid },
    Ambiguous { request_id: Uuid },
    GuardFalse { request_id: Uuid },
    UnexpectedState { request_id: Uuid },
}

pub struct VerifiedInboundEvent {
    pub connector_instance: String,
    pub event_name: String,
    pub provider_event_id: String,
    pub payload: TypedValue,
    pub payload_digest: [u8; 32],
    pub redacted_metadata: BTreeMap<String, TypedValue>,
}

pub enum InboundPersistence {
    Accepted {
        delivery_id: Uuid,
        instance_id: Uuid,
        process_event_id: Uuid,
    },
    Duplicate { delivery_id: Uuid },
    Unmatched { delivery_id: Uuid },
    Ambiguous { delivery_id: Uuid },
    GuardFalse { delivery_id: Uuid },
    UnexpectedState { delivery_id: Uuid },
}

impl ProcessRuntime {
    pub async fn consume_one_signal(
        &self,
    ) -> anyhow::Result<SignalConsumption>;

    pub async fn consume_one_due_timer(
        &self,
    ) -> anyhow::Result<TransitionConsumption>;

    pub async fn persist_verified_inbound(
        &self,
        event: VerifiedInboundEvent,
    ) -> anyhow::Result<InboundPersistence>;

    pub async fn persist_invalid_inbound(
        &self,
        connector_instance: &str,
        signature_status: SignatureStatus,
        payload_digest: [u8; 32],
        redacted_metadata: BTreeMap<String, TypedValue>,
    ) -> anyhow::Result<Uuid>;
}
~~~

Timers use the owning database clock; Tokio is wake-up only. Command signals
consume only typed atomic outbox rows. Verified inbound raw bytes are
authenticated before parsing, then audit plus dedupe/correlation commit in
one source-local transaction. Accepted delivery creates the process event and
sets both relational links before commit. Duplicate/other outcomes have null
links. Invalid signature writes delivery audit only and requires no trusted
provider ID. The route acknowledges verified input only after commit. Signals
are never buffered for a future state.

**Tests and conformance owned by this task:**

- `process_timer_survives_restart`
- `process_signal_is_not_buffered`
- `process_signal_revision_compatibility_is_explicit`
- `process_inbound_audit_is_split`
- `process_accepted_delivery_links_instance_history`
- `process_invalid_signature_is_audit_only`
- `process_inbound_database_failure_is_not_acknowledged`
- `process_inbound_connector_source_is_exact`
- `process_cancellation_only_cancels_scheduled_jobs`
- Conformance `process_timers_and_signals_are_durable`
- Conformance `process_verified_inbound_is_linked_and_atomic`
- Fixture directory `fixtures/processes/signals_and_ingress/`

- [ ] **Step 1: Add both failing native conformance cases**

  Cover timer restart, early/late command signal, compatible/incompatible
  retained revisions, cancellation, verified accepted+duplicate, unmatched,
  ambiguous, guard false, unexpected state, invalid/missing signature, and an
  injected post-verification DB failure. Assert exact HTTP response/status and
  all ledger/audit/link counts.

- [ ] **Step 2: Add failing timer/signal/inbound tests**

  Control DB time and use two source pools. Prove accepted inspection can join
  only on `(source_name, instance_id)` and never on provider/redacted text.
  Assert the original accepted delivery links the event; duplicates do not
  create or link a new event.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_timer
  cargo test -p donat-server --test process_signal
  cargo test -p donat-server --test process_inbound
  cargo test -p donat-conformance --test processes \
    process_timers_and_signals_are_durable
  cargo test -p donat-conformance --test processes \
    process_verified_inbound_is_linked_and_atomic
  ~~~

  Expected: timers, signal consumer, and durable ingress do not exist; verified
  webhook behavior remains the temporary `503`.

- [ ] **Step 4: Implement timers and command signals**

  Use source-first `FOR UPDATE SKIP LOCKED`, deployed definitions, exact typed
  correlation, and short transitions. Record all non-accepted outcomes; never
  buffer or invent a future match.

- [ ] **Step 5: Implement durable verified ingress**

  Preserve raw verification before parsing. Bind the connector to exactly one
  process source, commit linked audit/dedupe/event atomically, and only then
  replace the temporary verified-event `503` with the specified success
  acknowledgement. Keep raw bodies/secrets out of persistence.

- [ ] **Step 6: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_timer
  cargo test -p donat-server --test process_signal
  cargo test -p donat-server --test process_inbound
  cargo test -p donat-server --test connectors_stripe
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_timers_and_signals_are_durable
  cargo test -p donat-conformance --test processes \
    process_verified_inbound_is_linked_and_atomic
  ~~~

  Expected: timers/signals survive restart and every delivery attempt is
  auditable without weakening signature or source locality.

- [ ] **Step 7: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/connectors \
    crates/server/tests/process_timer.rs \
    crates/server/tests/process_signal.rs \
    crates/server/tests/process_inbound.rs \
    crates/conformance/tests/connectors.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/signals_and_ingress
  git commit -m "feat(processes): persist timers signals and ingress"
  ~~~

---

### Task 13: Add read-only process diagnosis and prove no management API

**Files:**

- Create: `crates/server/src/processes/inspect.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/main.rs`
- Create: `crates/server/tests/process_inspect.rs`
- Create: `crates/server/tests/routes.rs`
- Create: `crates/server/tests/schema.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/diagnostics/`

**Interfaces consumed from Tasks 6 and 12:**

~~~rust
pub struct DeployedProcessCatalog;
pub struct DeployedSourceProcessCatalog;
~~~

~~~text
process_instances
process_events
process_transition_logs
process_activity_jobs
process_activity_provider_steps
process_inbound_deliveries(source_name, instance_id, process_event_id, ...)
~~~

**Interfaces created here:**

~~~rust
pub struct ProcessInspectArgs {
    pub source: String,
    pub instance: Uuid,
}

pub struct RedactedProcessTimeline {
    pub source_name: String,
    pub instance_id: Uuid,
    pub process_name: String,
    pub revision: String,
    pub status: String,
    pub entries: Vec<RedactedTimelineEntry>,
}

pub async fn inspect_process(
    runtime: &SourceRuntime,
    deployed: &DeployedSourceProcessCatalog,
    instance_id: Uuid,
) -> anyhow::Result<RedactedProcessTimeline>;

pub async fn verify_process_history(
    runtime: &SourceRuntime,
    deployed: &DeployedSourceProcessCatalog,
    instance_id: Uuid,
) -> anyhow::Result<HistoryVerification>;

pub enum HistoryVerification {
    Valid,
    HashMismatch {
        transition_id: Uuid,
        expected: [u8; 32],
        actual: [u8; 32],
    },
}
~~~

The CLI surface is exactly:

~~~text
donat process inspect --source <name> --instance <uuid>
donat process verify-history --source <name> --instance <uuid>
~~~

Both commands select the source's real URL, use the hash-verified deployed
revision, join deliveries only by indexed `(source_name, instance_id)`, redact
values, and perform no write, lock claim, command, connector, retry, replay,
repair, cancellation, or definition mutation. No GraphQL/REST/MCP/admin
process-management field or route is added.

**Tests and conformance owned by this task:**

- `process_inspect_is_redacted_and_source_qualified`
- `process_inspect_uses_relational_inbound_links`
- `process_verify_history_uses_deployed_revision`
- `process_verify_history_detects_hash_mismatch`
- `process_diagnostics_issue_no_writes_or_claim_locks`
- `process_no_management_api`
- Conformance `process_diagnostics_are_read_only`
- Fixture directory `fixtures/processes/diagnostics/`

- [ ] **Step 1: Add the failing native conformance case**

  Build one accepted inbound activity/command timeline, run both CLI commands,
  mutate a stored state hash through the test DB, and rerun verification.
  Assert redaction, source links, exit codes, and exact absence of secrets/raw
  bodies. Probe GraphQL, REST, MCP, and former admin paths for exact existing
  not-found/validation behavior.

- [ ] **Step 2: Add failing SQL-capture and route/schema tests**

  Capture all diagnostic SQL and reject DDL/DML, `FOR UPDATE`, claim/update
  functions, or a connector call. Assert instance UUID without source is not a
  valid CLI form.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_inspect
  cargo test -p donat-server --test routes process_no_management_api
  cargo test -p donat-server --test schema process_no_management_api
  cargo test -p donat-conformance --test processes \
    process_diagnostics_are_read_only
  ~~~

  Expected: the two read-only CLI commands do not exist.

- [ ] **Step 4: Implement diagnostics only**

  Reapply stored events/results using the deployed catalog and compare
  before/after hashes. Use existing environment-name redaction. Do not expose
  raw journal JSON wholesale or add a mutable helper.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_inspect
  cargo test -p donat-server --test routes
  cargo test -p donat-server --test schema
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_diagnostics_are_read_only
  ~~~

  Expected: both CLI commands are read-only and no process management API is
  published.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/main.rs \
    crates/server/tests/process_inspect.rs \
    crates/server/tests/routes.rs crates/server/tests/schema.rs \
    crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/diagnostics
  git commit -m "feat(processes): add read-only diagnostics"
  ~~~

---

### Task 14: Prove restart, takeover, and rolling-binary compatibility

**Files:**

- Create: `crates/conformance/src/process_cluster.rs`
- Modify: `crates/conformance/src/lib.rs`
- Modify: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/rolling_runtime/`
- Create: `crates/server/tests/process_rolling_runtime.rs`
- Modify: `Makefile`
- Modify: `crates/conformance/PORTING.md`

**Production interfaces consumed from Tasks 6 and 9-13:**

~~~rust
pub struct Engine {
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub process_catalog: Arc<CompiledProcessCatalog>,
    pub deployed_process_catalog: Arc<DeployedProcessCatalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
}

pub const PROCESS_RUNTIME_ABI: u32 = 1;
pub const MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS: u64 = 5_000;
~~~

**Test-harness interfaces created here:**

~~~rust
pub struct ProcessCluster {
    pub database_url: String,
    pub engines: Vec<SpawnedEngine>,
    pub provider: RecordingProvider,
}

pub struct EngineBuild {
    pub binary: PathBuf,
    pub expected_runtime_abi: u32,
}

impl ProcessCluster {
    pub async fn spawn(
        metadata_dir: &Path,
        builds: Vec<EngineBuild>,
    ) -> anyhow::Result<Self>;

    pub async fn kill_engine(&mut self, index: usize) -> anyhow::Result<()>;

    pub async fn restart_engine(
        &mut self,
        index: usize,
        build: EngineBuild,
    ) -> anyhow::Result<()>;

    pub async fn barrier_provider_after_committed_intent(
        &self,
    ) -> anyhow::Result<()>;
}
~~~

The harness uses real independent `donat` processes, real Postgres
connections, controlled database time, and a Donat-owned provider stub. It
does not mock leases, catalogs, clocks, or connector outcomes in-process. The
test build mechanism may vary runtime ABI only through an explicit
test-only compile configuration and never changes production compatibility
rules.

**Tests and conformance owned by this task:**

- `process_revision_runtime_abi_is_fenced`
- `process_retired_revision_reloads_and_completes`
- `process_timer_survives_restart`
- `process_lease_takeover_is_safe`
- `process_late_takeover_refuses_before_io`
- `process_sources_sharing_database_are_isolated`
- `process_inbound_database_failure_is_not_acknowledged`
- `process_two_binaries_produce_one_transition`
- Conformance `process_rolling_runtime_contract_is_exact`
- Fixture directory `fixtures/processes/rolling_runtime/`

- [ ] **Step 1: Add the failing rolling-runtime conformance suite**

  Add the exact test and fixture before the cluster helper. Start compatible
  old/new binaries, then an incompatible ABI binary. Cover A active, explicit
  retirement, B current, A still live; timer restart; read-only takeover;
  provider mutation takeover; late idempotency refusal; duplicate webhook
  after DB failure; and same physical DB with two source namespaces.

- [ ] **Step 2: Add the failing server integration test**

  Require incompatible workers to fence before claiming any pinned work and
  compatible workers to produce one transition. Assert provider observations,
  journal rows, and stale audit from independent connections.

- [ ] **Step 3: Run RED**

  ~~~bash
  cargo test -p donat-server --test process_rolling_runtime
  cargo test -p donat-conformance --test processes \
    process_rolling_runtime_contract_is_exact
  ~~~

  Expected: the multi-process harness and complete rolling proof are absent.

- [ ] **Step 4: Implement only the test harness and compatibility gaps**

  Add process spawning, ports, database-time control, provider barriers, and
  ABI test builds. If a production gap appears, add the smallest change in the
  owning existing module with a failing focused test first; do not create a
  second lease/catalog/runtime path.

- [ ] **Step 5: Run GREEN**

  ~~~bash
  cargo test -p donat-server --test process_rolling_runtime
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes \
    process_rolling_runtime_contract_is_exact
  ~~~

  Expected: real binaries prove source isolation, durable restart, safe
  takeover, retained execution, and ABI fencing.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/conformance/src crates/conformance/tests/processes.rs \
    crates/conformance/fixtures/processes/rolling_runtime \
    crates/conformance/PORTING.md \
    crates/server/tests/process_rolling_runtime.rs Makefile
  git commit -m "test(processes): prove rolling runtime safety"
  ~~~

---

## Final acceptance (not an implementation task)

Perform this only after Tasks 1-14 each have their own reviewed commit. Make
no code or snapshot change during acceptance:

~~~bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build -p donat-server --bin donat
cargo test -p donat-conformance
make conformance
git status --short
~~~

Also rerun the value-contract no-OS check and inspect `cargo metadata --locked`
for the lower runtime closure. Review every insta diff; acceptance is invalid
if any snapshot is pending or blindly accepted. Confirm the working tree is
clean, serving SQL capture contains no DDL/DML, the route/schema inventory has
no process-management surface, and the complete connector effect/horizon,
two-binary takeover, retired-revision, and shared-database proofs passed.

Request one independent review of the complete commit range. Do not invoke
Judge. If review or acceptance finds a defect, return to the owning numbered
task, add a failing regression first, fix it in a new focused commit, rerun
that task's GREEN commands, then repeat final acceptance.
