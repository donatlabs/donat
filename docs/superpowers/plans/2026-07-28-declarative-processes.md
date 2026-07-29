# Durable Processes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build source-local declarative durable processes inside the one
Donat Rust binary, with immutable revisions, typed commands/connectors,
execution-generation outboxes, timers, signed inbound delivery, explicit-role
command transitions, and crash-safe at-least-once workers.

**Architecture:** Candidate construction first publishes source-qualified
command and connector descriptors, then the server-owned process compiler
derives immutable revisions and a schema-owned neutral effect catalog, and
only then finalizes command effects and compiles the serving schema. Every
process and worker is bound to one Postgres source. V6 supplies the exact
journal, command invocation UUID, split inbound persistence, and durable
capacity rows; commands keep one CTE statement while process journal work uses
short explicit transactions with no external I/O.

**Tech Stack:** Rust 2024, Tokio, Axum, serde YAML/JSON, Postgres 16,
tokio-postgres, reqwest, SHA-256, UUID, insta, and the native conformance
harness.

## Global Constraints

- Keep one `donat` binary and Postgres; crates/modules are not services.
- Add no workflow service, plugin runtime, JavaScript/WASM execution, dynamic
  connector, admin role, permission bypass, runtime DDL, distributed
  transaction, or process-management HTTP API.
- Every process, command transition, start/signal effect, worker, database
  clock, connector binding, and journal write is source-local to one exact
  Postgres source.
- Every command still executes as one Postgres statement and returns the exact
  existing Donat error envelope.
- Process journal DML uses short explicit transactions; no connector or other
  external I/O occurs while a transaction is open.
- `migrations/V6__donat_processes.sql` is the only process migration and uses
  `process_name` consistently.
- `donat.command_invocations.invocation_id` identifies one execution
  generation; process semantic start dedupe remains a separate key.
- Every process command transition has a closed typed `session_variables`
  mapping and an explicit classic `run_as_role`.
- Every process-owned serialized value is bounded to 256 KiB.
- Temporal, AWS Step Functions, Inngest, Stripe, and Airbyte remain
  behavior/test-category references under the existing register. Copy no
  upstream source, fixture, generated artifact, schema, or large text.
- Add no n8n-derived code, fixture, module, or reference.
- Review every insta diff; never accept snapshots blindly.
- This plan has no per-commit Judge gate. Request one independent review only
  after the complete implementation range passes final verification.

**Specification:**
[`specs/005-durable-processes.md`](../../../specs/005-durable-processes.md)

## File and interface map

The following ownership and signatures are fixed for every task.

### Lower shared contracts (`donat-ir`)

~~~rust
// crates/ir/src/value_contract.rs
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

`ValueContractCatalog` owns exact JSON validation and assignability. A `Ref`
must resolve in `named_objects`; requiredness and nullability are independent.

### Public descriptors

~~~rust
// crates/schema/src/commands.rs
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

// crates/server/src/connectors/mod.rs
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

### Process compiler and neutral effect catalog

~~~rust
// crates/server/src/processes/definition.rs
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
    pub revision: String,
    pub canonical_definition: serde_json::Value,
    pub dependencies: ProcessDependencyDescriptors,
    pub signal_contract_fingerprints: BTreeMap<String, String>,
}

pub struct ProcessDependencyDescriptors {
    pub commands: BTreeMap<(String, String), CommandDescriptor>,
    pub rules: BTreeMap<String, RuleDependencyDescriptor>,
    pub connector_operations:
        BTreeMap<(String, String), ConnectorOperationDescriptor>,
    pub connector_inbound_events:
        BTreeMap<(String, String), ConnectorInboundEventDescriptor>,
}

pub struct RuleDependencyDescriptor {
    pub name: String,
    pub profile_version: u16,
    pub canonical_ast_sha256: String,
    pub source_sha256: String,
    pub bindings: BTreeMap<String, ValueContract>,
    pub result: ValueContract,
}

pub fn compile_process_catalog(
    metadata: &Metadata,
    commands: &CompiledCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledProcessCatalog, PlanError>;

pub fn compile_process_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    commands: &CompiledSourceCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorDescriptorCatalog,
) -> Result<CompiledSourceProcessCatalog, PlanError>;

// crates/schema/src/process_effects.rs
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

pub trait ProcessEffectContractSource {
    fn effect_contracts(
        &self,
    ) -> BTreeMap<String, BTreeMap<String, ProcessEffectContract>>;
}

impl ProcessEffectContractCatalog {
    pub fn from_processes(
        processes: &dyn ProcessEffectContractSource,
    ) -> Result<Self, PlanError>;
}

pub fn finalize_command_effects(
    commands: &CompiledCommandCatalog,
    effects: &ProcessEffectContractCatalog,
) -> Result<CompiledCommandCatalog, PlanError>;
~~~

`ProcessEffectContractSource` is defined in `donat-schema`; the server
implements it for `CompiledProcessCatalog`. This keeps the schema independent
of server types.

### Candidate and deployment calls

~~~rust
// crates/server/src/state.rs, in Engine::compiled
pub fn compiled(
    metadata: Metadata,
    catalogs: HashMap<String, Catalog>,
    runtimes: HashMap<String, SourceRuntime>,
    connector_descriptors: &ConnectorDescriptorCatalog,
    infer_function_permissions: bool,
) -> Result<Engine, PlanError>;

let rules = Arc::new(compile_rule_catalog(&metadata)?);
let pre_process_commands = Arc::new(compile_command_catalog(
    &metadata, &catalogs, &rules, infer_function_permissions,
)?);
let processes = Arc::new(processes::definition::compile_process_catalog(
    &metadata, &pre_process_commands, &rules, connector_descriptors,
)?);
let effect_contracts =
    Arc::new(ProcessEffectContractCatalog::from_processes(processes.as_ref())?);
let commands = Arc::new(finalize_command_effects(
    pre_process_commands.as_ref(), effect_contracts.as_ref(),
)?);
let schema = Arc::new(
    CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects(
        &metadata,
        &catalogs,
        commands.clone(),
        effect_contracts.clone(),
        infer_function_permissions,
    )?,
);
~~~

`AppState::sync_candidate` passes
`self.connectors.descriptors()` to `Engine::compiled`, then awaits
`processes::reconcile::validate_serving_catalogs` before publishing the
candidate. Add exact `Engine` fields
`command_catalog: Arc<CompiledCommandCatalog>` and
`process_catalog: Arc<CompiledProcessCatalog>`; the existing
`compiled: Option<Arc<CompiledMultiSourceSchema>>` retains the schema.

~~~rust
// crates/server/src/processes/reconcile.rs
pub async fn reconcile(
    source_name: &str,
    database_url: &str,
    source_catalog: &donat_catalog::Catalog,
    compiled_processes: &CompiledSourceProcessCatalog,
    dependency_descriptors: &ProcessDependencyDescriptors,
) -> anyhow::Result<()>;
~~~

### Resolved effect IR

~~~rust
// crates/ir/src/lib.rs
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

### Runtime-only consumers

~~~rust
// crates/server/src/processes/start.rs
pub(crate) async fn consume_one_start(
    state: &AppState,
    source_name: &str,
) -> anyhow::Result<bool>;

// crates/server/src/processes/inbound.rs
pub(crate) async fn consume_one_signal(
    state: &AppState,
    source_name: &str,
) -> anyhow::Result<bool>;

// crates/server/src/processes/mod.rs
pub fn spawn(state: SharedState);
~~~

There is no command-callable `enqueue_start` or `enqueue_signal` function.

---

### Task 1: Add normalized value contracts and public command descriptors

**Files:**

- Create: `crates/ir/src/value_contract.rs`
- Create: `crates/ir/tests/value_contract.rs`
- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/tests/commands.rs`

**Interfaces:**

- Consumes: existing Donat command/custom-type metadata and compiled
  `RuleCatalog`.
- Produces: the lower contract types and `CommandDescriptor` interface from
  the file map plus `compile_command_source_catalog`; the existing global
  compiler composes that function for every real source catalog.

- [ ] **Step 1: Write failing normalized-contract tests**

  Add `value_contract_distinguishes_required_from_nullable`,
  `value_contract_resolves_recursive_object_refs`, and
  `value_contract_rejects_missing_extra_null_and_wrong_shape` to
  `crates/ir/tests/value_contract.rs`. Construct one recursive named object and
  assert exact assignment and JSON-validation outcomes.

- [ ] **Step 2: Write failing command-descriptor tests**

  Add `command_descriptor_exposes_exact_contract` and
  `command_descriptor_fingerprint_is_pre_process_and_deterministic` to
  `crates/schema/tests/commands.rs`. The fixture must contain a recursive
  argument, nullable result, two roles with distinct required session
  variables, Rule use, idempotency, and a raw `start_process` effect. Assert
  that changing effect process name changes the fingerprint while supplying a
  resolved revision is impossible.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-ir --test value_contract
  cargo test -p donat-schema --test commands command_descriptor
  ~~~

  Expected: compilation fails because the contract module and descriptor API
  do not exist.

- [ ] **Step 4: Implement the lower contract module**

  Add the exact types from the file map, a versioned type-string normalizer,
  finite named-object references, `is_assignable_from`, and
  `validate_json_object`. Keep the module SQL-free and deterministic.

- [ ] **Step 5: Publish descriptors from the existing command compiler**

  Replace private `StaticType` output at the descriptor boundary with the
  shared contracts. Collect effective role-specific session variables from
  table permissions, idempotency scope, and raw effect bindings. Hash
  versioned canonical JSON containing raw effects but no process revision.
  Store the immutable descriptor on `CompiledCommand`. Move one-source work
  into `compile_command_source_catalog` and make the global compiler compose
  it without changing diagnostics.

- [ ] **Step 6: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-ir
  cargo test -p donat-schema --test commands
  cargo test -p donat-schema --test multi_source
  ~~~

  Expected: all tests pass and existing command planning remains unchanged.

- [ ] **Step 7: Commit**

  ~~~bash
  git add crates/ir crates/schema
  git commit -m "feat(processes): publish typed command descriptors"
  ~~~

### Task 2: Publish typed connector operation and inbound descriptors

**Files:**

- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/metadata/tests/fixtures/connectors/instances/logistics.yaml`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/connectors/http.rs`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/tests/connectors_http.rs`
- Modify: `crates/server/tests/connectors_stripe.rs`
- Modify: `crates/server/tests/state.rs`

**Interfaces:**

- Consumes: `ValueContractCatalog` and the existing connector compilers.
- Produces: `ConnectorDescriptorCatalog`,
  `ConnectorOperationDescriptor`, and `ConnectorInboundEventDescriptor`.

- [ ] **Step 1: Add failing HTTP metadata tests**

  Add `http_operation_requires_typed_input_contract`,
  `http_template_slots_require_declared_types`, and
  `http_header_input_slot_is_typed` to `types_serde.rs` and
  `connectors_http.rs`. Cover path `{input.order_id}`, query, dynamic header
  value, nested body, nullable optional input, undeclared slot, unused
  non-serialization input, missing required value, null mismatch, and extra
  dispatch input.

- [ ] **Step 2: Add failing descriptor tests**

  Add `connector_descriptor_is_typed_and_non_secret` to
  `connectors_http.rs` and
  `stripe_checkout_descriptor_is_fixed_and_recursive` to
  `connectors_stripe.rs`. Assert module semantic version, runtime ABI,
  operation version, exact input/output, idempotency, capacity/serialization,
  endpoint/credential identities, and configuration fingerprint. Put a secret
  sentinel in the environment and assert it is absent from descriptor JSON and
  debug output.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-metadata http_operation
  cargo test -p donat-server --test connectors_http descriptor
  cargo test -p donat-server --test connectors_stripe descriptor
  ~~~

  Expected: the typed input mapping and public descriptors are absent.

- [ ] **Step 4: Extend declarative HTTP metadata**

  Add `input: BTreeMap<String, String>` to
  `HttpConnectorOperation`. Make operation header values a closed static-or-
  input enum. Normalize all declared input/output types, validate every
  path/query/header/body slot once in `ValidatedHttpOperation`, and retain the
  normalized contracts for registry publication and dispatch validation.

- [ ] **Step 5: Build the descriptor catalog in the existing registry**

  Store one descriptor beside every `CompiledHttpOperation` and
  `CompiledStripeOperation`. Publish fixed Stripe
  `checkout.create_session`/`v1` and
  `checkout.session.completed` contracts from Rust constants. Do not add a
  second connector compiler or any dynamic module path.

- [ ] **Step 6: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-metadata
  cargo test -p donat-server --test connectors_http
  cargo test -p donat-server --test connectors_stripe
  cargo test -p donat-server --test state
  ~~~

  Expected: all descriptor and existing connector boundary tests pass.

- [ ] **Step 7: Commit**

  ~~~bash
  git add crates/metadata crates/server/src/connectors crates/server/tests
  git commit -m "feat(processes): publish typed connector descriptors"
  ~~~

### Task 3: Add canonical process metadata and the source-local process compiler

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

**Interfaces:**

- Consumes: command, Rule, connector operation, and connector inbound-event
  descriptors.
- Produces: `CompiledProcessCatalog`,
  `CompiledSourceProcessCatalog`, `ProcessDependencyDescriptors`, and the
  `ProcessEffectContractSource` implementation. The global compiler composes
  `compile_process_source_catalog` and then validates connector-to-source
  uniqueness.

- [ ] **Step 1: Add failing metadata grammar tests**

  Add tests for absent and quoted-included `processes.yaml`, required
  `start`, typed optional `cancellation`, duplicate process/state names,
  missing/non-Postgres source, multiple state kinds, invalid nested timeout,
  missing `on_error.fallback`, duplicate/unknown error kind, invalid
  `retry_on`, unknown target, invalid start/initial bindings, and a command
  transition missing explicit `session_variables` or `on_rejection`.

- [ ] **Step 2: Add failing compiler tests**

  In `process_definition.rs`, add
  `process_rejects_cross_source_start_command`,
  `process_rejects_cross_source_transition_command`,
  `process_rejects_cross_source_command_effects`,
  `process_start_command_requires_matching_raw_effect`,
  `process_connector_instance_has_one_source`,
  `process_session_variables_are_closed`,
  `process_activity_contract_is_exact`, and
  `process_revision_contains_complete_dependency_closure`.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-metadata processes
  cargo test -p donat-server --test process_definition
  ~~~

  Expected: no process metadata field, module tree, or compiler exists.

- [ ] **Step 4: Implement the canonical metadata structs**

  Add `Metadata.processes`, `ProcessDefinition` with required `start` and
  optional typed `cancellation`, the closed state union, ordered total
  activity error routes, wait/timer forms, exact transitions, typed bindings,
  and mandatory command `session_variables`. Use deny-unknown-fields on the
  normative process structs and load/write `processes.yaml` through the
  existing directory-loader and conformance metadata builder.

- [ ] **Step 5: Implement source-local process compilation**

  Resolve every unqualified command/process name only in
  `ProcessDefinition.source`; reject non-Postgres and cross-source references.
  Validate graph totality, role availability, exact command input/session
  mapping, Rule bindings, connector input/output/event contracts, stable
  idempotency, and the one-source connector-instance rule. Derive the
  canonical revision and retained signal-contract fingerprints without
  database access. Put one-source work in
  `compile_process_source_catalog`; make the global compiler compose those
  source catalogs and enforce connector-source uniqueness.

- [ ] **Step 6: Expose both server module trees**

  Add `pub mod processes` to `lib.rs` and `mod processes` to `main.rs`.
  Keep all runtime consumers absent so process effects remain fail-closed.

- [ ] **Step 7: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-metadata
  cargo test -p donat-server --test process_definition
  cargo test --workspace --no-run
  ~~~

  Expected: canonical metadata and pure process compilation pass without
  journal or worker code.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/metadata crates/server/src/processes crates/server/src/lib.rs crates/server/src/main.rs crates/server/tests/process_definition.rs crates/conformance/src/lib.rs
  git commit -m "feat(processes): compile source-local definitions"
  ~~~

### Task 4: Build the two-stage candidate and finalize process effects

**Files:**

- Create: `crates/schema/src/process_effects.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/multi_source.rs`
- Modify: `crates/schema/tests/commands.rs`
- Modify: `crates/schema/tests/multi_source.rs`
- Modify: `crates/server/src/processes/definition.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/tests/state.rs`
- Modify: `crates/server/tests/process_definition.rs`

**Interfaces:**

- Consumes: all descriptor and compiler interfaces from Tasks 1–3.
- Produces: `ProcessEffectContractCatalog`, finalized immutable commands,
  `Engine.commands`, `Engine.processes`, and the exact candidate-build order.

- [ ] **Step 1: Add failing effect-finalization tests**

  Add `process_effect_catalog_is_source_qualified`,
  `start_effect_pins_active_revision`,
  `signal_effect_retains_contract_compatibility_anchor`,
  `effect_finalization_rejects_missing_or_wrong_source_process`, and
  `command_fingerprint_does_not_change_after_effect_finalization` to schema
  command tests.

- [ ] **Step 2: Add a failing candidate-order test**

  Add `process_candidate_build_is_cycle_free` to `server/tests/state.rs`.
  Construct Rules, one command descriptor, one connector descriptor, one
  process, and one command start effect. Assert the published `Engine`
  contains the same source-qualified finalized command, process revision, and
  schema effect contract.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-schema process_effect
  cargo test -p donat-server --test state process_candidate
  ~~~

  Expected: the neutral catalog, finalization function, and Engine fields do
  not exist.

- [ ] **Step 4: Implement the neutral schema contract**

  Add `ProcessEffectContractSource` and
  `ProcessEffectContractCatalog` in `donat-schema`. Validate exact start,
  signal, correlation, payload, revision, idempotency, and source contracts in
  `finalize_command_effects`; clone immutable compiled commands with finalized
  effects while retaining their pre-process descriptor fingerprint.

- [ ] **Step 5: Change schema compilation entry point**

  Replace `compile_with_command_catalog` with
  `compile_with_command_catalog_and_process_effects`. Store both immutable
  catalogs and verify they cover every metadata command/process declaration.
  Keep schema compilation independent of server types.

- [ ] **Step 6: Change actual candidate assembly**

  Implement the exact call sequence from the file map in
  `Engine::compiled`. Pass `ConnectorRegistry::descriptors()` from
  `AppState::sync_candidate`, retain command/process catalogs on `Engine`, and
  update all in-tree test construction sites to pass an empty descriptor
  catalog when metadata has no connectors/processes.

- [ ] **Step 7: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-schema
  cargo test -p donat-server --test process_definition
  cargo test -p donat-server --test state
  cargo test --workspace --no-run
  ~~~

  Expected: candidate construction is deterministic and has no schema/server
  dependency cycle.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/schema crates/server/src/state.rs crates/server/src/main.rs crates/server/src/processes/definition.rs crates/server/tests
  git commit -m "feat(processes): add two-stage candidate compilation"
  ~~~

### Task 5: Make migrate and validate source-qualified

**Files:**

- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/migrate.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/tests/migrate.rs`
- Modify: `crates/conformance/src/lib.rs`
- Modify: `crates/conformance/tests/migrate.rs`

**Interfaces:**

- Consumes: metadata source connection configuration and existing catalog
  introspection.
- Produces:

  ~~~rust
  pub(crate) struct SelectedPostgresSource {
      pub name: String,
      pub database_url: String,
  }

  pub(crate) fn select_postgres_source(
      metadata: &Metadata,
      requested: Option<&str>,
      default_url: Option<&str>,
  ) -> anyhow::Result<SelectedPostgresSource>;

  pub async fn check_consistency(
      metadata_dir: &Path,
      source: &SelectedPostgresSource,
  ) -> anyhow::Result<Vec<String>>;
  ~~~

  `check_consistency` calls `compile_command_source_catalog` and
  `compile_process_source_catalog`; it never supplies a selected catalog to a
  declaration owned by another source.

- [ ] **Step 1: Add failing CLI-selection tests**

  Add `migrate_requires_source_when_multiple_postgres_sources`,
  `validate_requires_source_when_multiple_postgres_sources`,
  `single_postgres_source_may_be_inferred`,
  `selected_source_requires_its_real_url`, and
  `non_postgres_source_is_rejected` to `server/tests/migrate.rs`.

- [ ] **Step 2: Add the catalog-isolation regression**

  Add `validate_introspects_only_selected_source` using two Postgres
  databases with different tables. Assert a table present only in source A
  does not validate source B and that no cloned catalog entry exists for the
  unselected source.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-server --test migrate source
  cargo test -p donat-conformance --test migrate
  ~~~

  Expected: `--source` is unavailable and consistency clones one catalog
  across sources.

- [ ] **Step 4: Add exact source-qualified CLI behavior**

  Add `source: Option<String>` to `MigrateArgs` and `ValidateArgs`. Load
  metadata before selecting the database. Require an exact Postgres source;
  permit omission only for one unambiguous Postgres source; make a missing
  `from_env` value fail instead of falling back to another source URL.

- [ ] **Step 5: Isolate consistency and reconciliation inputs**

  Introspect one selected database and construct only
  `{ selected_source_name: selected_catalog }`. Validate selected-source
  commands/processes through the source-scoped compiler entry points and
  validate global metadata-only facts without reporting unselected databases
  as checked. Keep serve-time `sync_candidate` as the path that introspects all
  real source URLs.

- [ ] **Step 6: Update native conformance invocation**

  In `Suite::ensure_engine`, append `.arg("--source").arg("default")` to
  `donat migrate`. Add a multi-source deployment test that invokes migration
  once per source and proves each source receives only its own selected
  reconciliation.

- [ ] **Step 7: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test migrate
  cargo test -p donat-conformance --test migrate
  cargo test -p donat-conformance --test multi_source
  ~~~

  Expected: source selection and catalog isolation pass.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/server/src/main.rs crates/server/src/migrate.rs crates/server/src/state.rs crates/server/tests/migrate.rs crates/conformance
  git commit -m "feat(processes): qualify deployment by source"
  ~~~

### Task 6: Add V6 and reconcile immutable source-local revisions

**Files:**

- Create: `migrations/V6__donat_processes.sql`
- Create: `crates/server/src/processes/reconcile.rs`
- Create: `crates/server/tests/process_reconcile.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/migrate.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/tests/migrate.rs`

**Interfaces:**

- Consumes: the selected real source catalog and compiled source process
  catalog.
- Produces: the exact `reconcile` signature from the file map and
  `validate_serving_catalogs`.

- [ ] **Step 1: Add the failing exact-schema test**

  Add `process_v6_schema_is_exact` to `process_reconcile.rs`. Apply V1–V6 to a
  clean database and assert every Section 7 table, column type, check,
  primary/unique/partial key, and due/lease/capacity index. Assert both
  command-effect outboxes have
  `unique(command_invocation_id, effect_position)` and no foreign key to
  either command journal.

- [ ] **Step 2: Add failing command-journal migration tests**

  Add `v6_backfills_unique_invocation_ids` and
  `v6_is_idempotent_after_refinery_history` to `server/tests/migrate.rs`.
  Seed two V5 invocation rows and assert distinct non-null UUIDs after V6.

- [ ] **Step 3: Add failing reconciliation tests**

  Add `process_reconcile_creates_and_reuses_revision`,
  `process_reconcile_retires_without_deleting`,
  `process_reconcile_changes_only_selected_source`,
  `process_reconcile_rejects_live_dependency_removal`,
  `process_reconcile_rejects_live_signal_contract_change`, and
  `serve_rejects_undeployed_revision_without_writing` to
  `process_reconcile.rs`.

- [ ] **Step 4: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_reconcile
  cargo test -p donat-server --test migrate v6
  ~~~

  Expected: V6 and reconciliation modules do not exist.

- [ ] **Step 5: Implement the exact V6 schema**

  Translate Spec 005 Section 7 literally. Use `process_name`, split inbound
  ledger/deliveries, capacity reservations, UUID effect identity, 256 KiB
  JSON checks, due/lease indexes, and no retention-coupling command FK.

- [ ] **Step 6: Implement selected-source reconciliation**

  Build the connector descriptor catalog, Rules catalog, selected-source
  command catalog, and selected-source process catalog from the selected real
  catalog. Canonicalize and insert immutable definitions, atomically activate
  one revision per process, retire omitted definitions, and query live
  instances before accepting incompatible dependency or signal changes. Call
  it only from `migrate --metadata-dir --source` after event reconciliation.

- [ ] **Step 7: Add read-only serving validation**

  In `AppState::sync_candidate`, read the deployed definition rows for every
  Postgres process source and compare them to the candidate before
  `publish_candidate`. Do not create a schema/table, activate a revision, or
  update a row from this path.

- [ ] **Step 8: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_reconcile
  cargo test -p donat-server --test migrate
  cargo build -p donat-server --bin donat
  ~~~

  Expected: schema, reconciliation, and serve-read-only proofs pass.

- [ ] **Step 9: Commit**

  ~~~bash
  git add migrations/V6__donat_processes.sql crates/server/src/processes crates/server/src/main.rs crates/server/src/migrate.rs crates/server/src/state.rs crates/server/tests
  git commit -m "feat(processes): add source-local V6 journals"
  ~~~

### Task 7: Give each command execution generation a durable UUID

**Files:**

- Modify: `crates/sqlgen/src/lib.rs`
- Modify: `crates/sqlgen/tests/commands.rs`
- Modify: `crates/sqlgen/tests/snapshots/commands__command_renderer_lowers_guard_and_session_scoped_idempotency.snap`

**Interfaces:**

- Consumes: V6 `command_invocations.invocation_id`.
- Produces: `_cmd_invocation(invocation_id, result, input_fingerprint,
  is_first)` in the command CTE pipeline.

- [ ] **Step 1: Add failing Postgres execution tests**

  Add `command_invocation_id_replays_unchanged`,
  `command_invocation_id_changes_after_expiry`, and
  `command_first_executions_have_distinct_invocation_ids` to
  `crates/sqlgen/tests/commands.rs`. Apply V6 in the command-catalog helper.
  Use a zero/expired retention test transaction to reclaim the same qualified
  tuple and assert a new UUID.

- [ ] **Step 2: Add a failing SQL snapshot assertion**

  Extend `command_renderer_lowers_guard_and_session_scoped_idempotency` to
  assert `gen_random_uuid()`, returned `invocation_id`, and an explicit
  `is_first` gate are present.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-sqlgen --test commands command_invocation_id
  cargo test -p donat-sqlgen --test commands command_renderer_lowers_guard
  ~~~

  Expected: the renderer neither writes nor returns an invocation UUID.

- [ ] **Step 4: Implement generation semantics**

  On a first claim insert `gen_random_uuid()`. On an expired-key first claim,
  replace the old journal row's `invocation_id` with a new UUID. On replay,
  preserve and return the stored UUID. Carry `is_first` through
  `_cmd_invocation` without changing result replay or changed-input rejection.

- [ ] **Step 5: Review and run GREEN**

  Run:

  ~~~bash
  cargo insta test -p donat-sqlgen --test commands
  cargo insta review
  cargo test -p donat-sqlgen --test commands
  ~~~

  Expected: the reviewed snapshot contains one statement and all command
  execution tests pass.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/sqlgen
  git commit -m "feat(commands): add execution generation ids"
  ~~~

### Task 8: Consume pinned starts with separate semantic dedupe

**Files:**

- Create: `crates/server/src/processes/start.rs`
- Create: `crates/server/tests/process_start.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/state.rs`

**Interfaces:**

- Consumes: V6 start requests and `Engine.processes`.
- Produces: `consume_one_start` and one per-source bounded start worker.

- [ ] **Step 1: Add failing start-consumer tests**

  Add `process_start_request_pins_revision`,
  `process_start_semantic_dedupe_is_separate`,
  `process_start_crash_before_commit_retries`,
  `process_start_crash_after_commit_is_duplicate`,
  `process_start_rejects_wrong_source_or_input`, and
  `process_retired_revision_starts_only_when_already_pinned`.

- [ ] **Step 2: Run the RED test**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_start
  ~~~

  Expected: no worker-side start consumer exists.

- [ ] **Step 3: Implement one transactional consumption**

  Lock one pending request with `FOR UPDATE SKIP LOCKED`, verify source and
  exact stored revision, validate input, insert/find by
  `(process_name, start_idempotency_key)`, append the initial event/log for a
  new instance, record duplicate for an existing semantic instance, and mark
  the request consumed in one short transaction.

- [ ] **Step 4: Add per-source bounded polling**

  `processes::spawn` enumerates Postgres process sources from the current
  `Engine` snapshot and starts a bounded loop per source pool. A loop never
  uses `default_pool` for a non-default source and releases every transaction
  before polling again.

- [ ] **Step 5: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_start
  cargo test -p donat-server --test process_reconcile
  cargo test -p donat-server --test state
  ~~~

  Expected: pinned revision and semantic dedupe tests pass.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/main.rs crates/server/src/state.rs crates/server/tests/process_start.rs
  git commit -m "feat(processes): consume pinned start requests"
  ~~~

### Task 9: Execute process commands with closed sessions and a rejection savepoint

**Files:**

- Create: `crates/server/src/command_executor.rs`
- Create: `crates/server/src/processes/transition.rs`
- Create: `crates/server/tests/process_transition.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/server/src/gql.rs`
- Modify: `crates/server/src/lib.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/tests/state.rs`

**Interfaces:**

- Consumes: finalized `CompiledCommand`, explicit internal input, and compiled
  transition session-variable mappings.
- Produces:

  ~~~rust
  pub fn plan_internal_command(
      command: &CompiledCommand,
      input: &serde_json::Map<String, serde_json::Value>,
      session: &Session,
      error_path: &str,
  ) -> Result<CommandMutation, PlanError>;

  pub(crate) enum CommandStatementError {
      Rejected(CommandRejection),
      Database(tokio_postgres::Error),
      Decode(String),
  }

  pub(crate) struct CommandRejection {
      pub code: String,
      pub path: String,
      pub message: String,
      pub body: serde_json::Value,
  }

  pub(crate) async fn execute_process_command_in_savepoint(
      tx: &tokio_postgres::Transaction<'_>,
      command: &CommandMutation,
  ) -> Result<serde_json::Value, CommandStatementError>;
  ~~~

- [ ] **Step 1: Add failing session-closure tests**

  Add `process_worker_builds_only_compiled_session_variables`,
  `process_worker_cannot_use_graphql_session_headers`,
  `process_worker_uses_only_fixed_run_as_role`, and
  `process_session_mapping_type_mismatch_fails_before_sql`.

- [ ] **Step 2: Add failing savepoint tests**

  Add `process_command_rejection_commits_on_rejection` with a command whose
  first CTE writes domain data and a later guard raises valid `P0D01`. Assert
  domain row/journal/outbox absence and exactly one committed rejection
  event/transition. Add `process_command_database_error_aborts_outer` for
  malformed `P0D01`, `23514`, and an ordinary constraint error.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_transition
  ~~~

  Expected: no internal planner, savepoint executor, or transition worker
  exists.

- [ ] **Step 4: Extract one shared internal command path**

  Move strict P0D01 envelope decoding and command row decoding to
  `command_executor.rs`. Add `plan_internal_command` in `donat-schema`; it
  resolves the full canonical declared result without constructing a GraphQL
  request. Keep GraphQL execution and exact error bodies unchanged.

- [ ] **Step 5: Implement the savepoint protocol**

  The process transition owns the outer transaction. Issue `SAVEPOINT`, run
  the one command SQL statement, and `RELEASE` on success. For only valid
  `P0D01`/`donat.graphql-error.v1`, issue `ROLLBACK TO SAVEPOINT` then
  `RELEASE SAVEPOINT`, return `Rejected`, append one rejection
  event/transition, and follow `on_rejection`. Return every other failure so
  the outer transaction drops without commit.

- [ ] **Step 6: Implement transition journal updates**

  Lock event/instance, validate pinned revision and optimistic version,
  evaluate guards, execute the optional command with the compiled `Session`,
  apply `set`/`next`, insert next work and transition log, and consume the
  event in the same short outer transaction.

- [ ] **Step 7: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_transition
  cargo test -p donat-schema --test commands
  cargo test -p donat-server gql::tests
  ~~~

  Expected: rollback/commit boundaries and exact error decoding pass.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/schema crates/server/src/command_executor.rs crates/server/src/processes crates/server/src/gql.rs crates/server/src/lib.rs crates/server/src/main.rs crates/server/tests
  git commit -m "feat(processes): execute commands in rejection savepoints"
  ~~~

### Task 10: Lease connector activities without transactions over I/O

**Files:**

- Create: `crates/server/src/processes/activity.rs`
- Create: `crates/server/tests/process_activity.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/connectors/mod.rs`

**Interfaces:**

- Consumes: V6 activity/capacity tables, pinned connector descriptors, and
  existing `ConnectorRegistry::execute`.
- Produces: source-local activity claim and completion loops.

- [ ] **Step 1: Add failing claim/lease tests**

  Add `process_activity_claims_once`,
  `process_activity_lease_takeover_is_safe`,
  `process_activity_stale_completion_is_audit_only`,
  `process_activity_schedule_timeout_makes_no_call`, and
  `process_activity_start_timeout_makes_late_completion_stale`.

- [ ] **Step 2: Add failing policy tests**

  Add `process_activity_capacity_is_global`,
  `process_activity_serialization_is_global`,
  `process_activity_rate_policy_is_global`,
  `process_retry_key_is_stable`,
  `process_retry_jitter_is_reproducible`, and
  `process_error_routes_are_total`. Use two independently started engine
  states sharing one source database and a controlled recording connector.

- [ ] **Step 3: Add the transaction boundary test**

  Add `process_activity_does_not_hold_tx`. Block the connector response,
  acquire a second database connection, and prove the leased journal row is
  committed and unlockable while the provider call is blocked.

- [ ] **Step 4: Run the RED test**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_activity
  ~~~

  Expected: no activity worker or capacity reservation protocol exists.

- [ ] **Step 5: Implement claim and capacity reservation**

  Lock one due job, reject schedule expiry, use source-local Postgres
  coordination to evaluate unexpired reservations against max/rate/burst and
  optional serialization hash, insert the reservation, assign lease/deadline,
  increment attempt, and commit. Leave the job scheduled when no permit is
  available.

- [ ] **Step 6: Execute outside and complete conditionally**

  Validate input against the pinned descriptor, call the registry after
  commit, then begin a new transaction. Update only the matching lease token,
  persist result/failure, release reservation, and append one completion
  event. Log a stale attempt without transitioning when ownership is lost.

- [ ] **Step 7: Implement deterministic retry and routing**

  Use the pinned retry policy, logical activity ID, persisted attempt, and
  database clock. Route all eight connector classes plus
  `retry_exhausted` through ordered routes/fallback. Keep configuration and
  webhook failures outside this activity taxonomy.

- [ ] **Step 8: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_activity
  cargo test -p donat-server --test connectors_http
  cargo test -p donat-server --test connectors_stripe
  ~~~

  Expected: lease, capacity, retry, and no-open-transaction tests pass.

- [ ] **Step 9: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/connectors/mod.rs crates/server/tests/process_activity.rs
  git commit -m "feat(processes): lease source-local activities"
  ~~~

### Task 11: Add timers, split inbound audit, and command-signal consumption

**Files:**

- Create: `crates/server/src/processes/timer.rs`
- Create: `crates/server/src/processes/inbound.rs`
- Create: `crates/server/tests/process_timer.rs`
- Create: `crates/server/tests/process_inbound.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/connector_webhook.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/tests/connector_webhook.rs`

**Interfaces:**

- Consumes: source-bound connector inbound descriptors and V6 split ingress
  tables.
- Produces: timer loop, `consume_one_signal`, and durable webhook acceptance.

- [ ] **Step 1: Add failing timer tests**

  Add `process_timer_uses_database_clock`,
  `process_timer_survives_restart`, and
  `process_wait_timeout_is_not_a_second_state_kind`. Use controlled database
  time and two workers; use no wall-clock sleeps for correctness assertions.

- [ ] **Step 2: Add failing split-audit tests**

  Add `process_inbound_audit_is_split`,
  `process_invalid_signature_is_audit_only`,
  `process_inbound_unmatched_ambiguous_guard_false_are_distinct`,
  `process_inbound_unexpected_state_is_not_buffered`, and
  `process_inbound_database_failure_is_not_acknowledged`.
  For accepted then duplicate delivery assert two delivery rows, one dedupe
  row, and one process transition.

- [ ] **Step 3: Add failing command-signal tests**

  Add `process_signal_request_uses_stored_source_and_revision`,
  `process_signal_contract_compatibility_is_explicit`,
  `process_signal_is_not_buffered`, and
  `process_domain_cancel_is_declarative`.

- [ ] **Step 4: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_timer
  cargo test -p donat-server --test process_inbound
  cargo test -p donat-server --test connector_webhook
  ~~~

  Expected: no timer/signal consumer and verified webhooks still stop at the
  temporary non-acknowledgement boundary.

- [ ] **Step 5: Implement database-clock timers**

  Materialize and claim due timer events in the owning source pool, apply the
  common short transition transaction, and make duplicate claim/restart a
  no-op through event and transition uniqueness.

- [ ] **Step 6: Implement split inbound persistence**

  Verify raw bytes first. Use the compiled connector source binding to select
  one pool. For valid signatures, insert a delivery plus dedupe ledger and
  correlate/apply one outcome atomically. On dedupe conflict insert a distinct
  duplicate delivery. For invalid signatures insert only a redacted delivery
  with optional provider ID. Acknowledge verified delivery only after commit.

- [ ] **Step 7: Implement the signal outbox consumer**

  Lock one source-local signal request, load the stored compatibility anchor,
  validate exact typed correlation/payload, select at most one receptive
  instance, append one event or immutable audit outcome, and consume the
  request. Cancellation updates only scheduled jobs; in-flight completion
  stays audit-only.

- [ ] **Step 8: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_timer
  cargo test -p donat-server --test process_inbound
  cargo test -p donat-server --test connector_webhook
  cargo test -p donat-server
  ~~~

  Expected: timer, split audit, acknowledgement, and signal/cancellation tests
  pass.

- [ ] **Step 9: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/connector_webhook.rs crates/server/src/main.rs crates/server/tests
  git commit -m "feat(processes): persist timers and split ingress audit"
  ~~~

### Task 12: Activate resolved start and signal effects in the command statement

**Files:**

- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/ir/tests/ir_structure.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/plan_mutation.rs`
- Modify: `crates/schema/src/multi_source.rs`
- Modify: `crates/schema/tests/commands.rs`
- Modify: `crates/sqlgen/src/lib.rs`
- Modify: `crates/sqlgen/tests/commands.rs`
- Modify: `crates/server/src/gql.rs`
- Modify: `crates/server/tests/process_start.rs`
- Modify: `crates/server/tests/process_inbound.rs`

**Interfaces:**

- Consumes: finalized effect contracts, V6, invocation UUID CTE, and
  worker-only consumers.
- Produces: the exact `ResolvedCommandEffect` IR and one-statement outbox CTEs.

- [ ] **Step 1: Add failing IR/lowering tests**

  Add `command_effect_lowering_is_fully_resolved` to IR/schema tests. Assert
  source, process/revision, signal, typed maps, semantic key,
  `CurrentExecution`, and zero-based effect positions. Assert raw metadata
  names or `CommandEffectKind` cannot cross the IR boundary.

- [ ] **Step 2: Add failing SQL/Postgres tests**

  Add `command_effect_outbox_is_written_once_per_execution_generation`,
  `command_effect_positions_share_generation`,
  `command_effect_replay_writes_no_second_row`,
  `command_effect_expired_reexecution_uses_new_generation`, and
  `command_effect_rejection_writes_no_row`. Cover both start and signal
  tables and inspect their concrete UUID/position pairs.

- [ ] **Step 3: Run the RED tests**

  Run:

  ~~~bash
  cargo test -p donat-schema --test commands process_effect
  cargo test -p donat-sqlgen --test commands command_effect
  cargo test -p donat-server --test process_start command_effect
  ~~~

  Expected: planner discards effect bindings and SQLgen still rejects all
  effect-bearing IR.

- [ ] **Step 4: Lower exact resolved effect IR**

  Replace `CommandEffectKind` with the closed resolved structs from the file
  map. Resolve all bindings in `plan_mutation.rs` using the same typed command
  value path as steps/results and the finalized source-qualified effect
  contract.

- [ ] **Step 5: Add outbox CTEs to SQLgen**

  Insert one CTE per canonical effect position after the successful
  invocation/result CTE. Select only where `_cmd_invocation.is_first`; copy
  `_cmd_invocation.invocation_id`; write source/process/revision/typed
  payload/semantic key; rely on
  `(command_invocation_id, effect_position)` uniqueness. Keep the final
  response projection inside the same statement.

- [ ] **Step 6: Remove the temporary serving fail-closed gate**

  Delete `command_execution_rejection` only after the renderer and both
  consumers exist. Retain SQLgen's Postgres-only command assertion and add an
  unreachable defensive rejection for resolved effects on a command IR whose
  `idempotency` is absent.

- [ ] **Step 7: Review snapshots and run GREEN**

  Run:

  ~~~bash
  cargo insta test -p donat-sqlgen --test commands
  cargo insta review
  cargo test -p donat-ir
  cargo test -p donat-schema --test commands
  cargo test -p donat-sqlgen --test commands
  cargo test -p donat-server --test process_start
  cargo test -p donat-server --test process_inbound
  ~~~

  Expected: reviewed SQL remains one statement and all replay/rejection cases
  pass.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/ir crates/schema crates/sqlgen crates/server/src/gql.rs crates/server/tests
  git commit -m "feat(processes): activate atomic command effects"
  ~~~

### Task 13: Add read-only process inspection and history verification

**Files:**

- Create: `crates/server/src/processes/inspect.rs`
- Create: `crates/server/src/processes/verify_history.rs`
- Create: `crates/server/tests/process_inspect.rs`
- Modify: `crates/server/src/processes/mod.rs`
- Modify: `crates/server/src/main.rs`

**Interfaces:**

- Consumes: selected source, pinned definition, and process journals.
- Produces exact CLI:
  `donat process inspect --source <name> --instance <uuid>` and
  `donat process verify-history --source <name> --instance <uuid>`.

- [ ] **Step 1: Add failing read-only tests**

  Add `process_inspect_is_redacted_and_read_only`,
  `process_verify_history_detects_state_hash_mismatch`, and
  `process_diagnostics_never_execute_command_connector_or_write`.
  Capture SQL and assert every statement begins with the read-only transaction
  setup or `SELECT`.

- [ ] **Step 2: Run the RED test**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_inspect
  ~~~

  Expected: the process CLI group and modules do not exist.

- [ ] **Step 3: Implement inspect**

  Select the exact source, read instance/events/jobs/logs/deliveries, redact
  payloads and error details, and emit deterministic JSON with lease/retry
  metadata. Accept no role, arbitrary SQL, mutation, or provider option.

- [ ] **Step 4: Implement verify-history**

  Load the pinned immutable definition and stored events/results, reapply the
  pure transition function, compare every before/after hash, and exit non-zero
  on mismatch. Do not invoke command planning, SQLgen mutation, connector
  execution, or journal updates.

- [ ] **Step 5: Run GREEN verification**

  Run:

  ~~~bash
  cargo test -p donat-server --test process_inspect
  cargo build -p donat-server --bin donat
  ~~~

  Expected: redaction, mismatch, and read-only SQL assertions pass.

- [ ] **Step 6: Commit**

  ~~~bash
  git add crates/server/src/processes crates/server/src/main.rs crates/server/tests/process_inspect.rs
  git commit -m "feat(processes): add read-only process diagnostics"
  ~~~

### Task 14: Prove source locality, crashes, effects, and ingress in conformance

**Files:**

- Create: `crates/conformance/tests/processes.rs`
- Create: `crates/conformance/fixtures/processes/version.yaml`
- Create: `crates/conformance/fixtures/processes/databases/databases.yaml`
- Create: `crates/conformance/fixtures/processes/processes.yaml`
- Create: `crates/conformance/fixtures/processes/commands.yaml`
- Create: `crates/conformance/fixtures/processes/rules.yaml`
- Create: `crates/conformance/fixtures/processes/connectors.yaml`
- Create: `crates/conformance/fixtures/processes/requests.yaml`
- Create: `crates/conformance/fixtures/processes/expected.yaml`
- Create: `crates/conformance/fixtures/processes/webhooks.yaml`
- Create: `crates/conformance/fixtures/processes/deployments.yaml`
- Create: `crates/conformance/fixtures/processes/failures.yaml`
- Modify: `crates/conformance/src/lib.rs`

**Interfaces:**

- Consumes: the complete runtime and exact source-qualified migration
  invocation.
- Produces: native exact-body process conformance with two-engine controls.

- [ ] **Step 1: Add failing definition/deployment cases**

  Add native tests for required start/cancellation, cross-source start,
  transition, effect, and connector rejection; source-qualified migrate;
  revision A pinning across B; retired start rejection; ABI fencing; live
  dependency/signal incompatibility; and closed session-variable mappings.

- [ ] **Step 2: Add failing command/effect cases**

  Add exact GraphQL fixtures for one start, replay, expired-key
  re-execution, two effect positions, changed-input rejection, command domain
  rejection with committed `on_rejection`, and non-P0D01 outer abort. Freeze
  exact status, code, path, message, and body.

- [ ] **Step 3: Add failing worker/activity cases**

  Add happy-path Checkout, false guard, timer restart, distinct timeouts,
  deterministic retry, all connector failure classes, retry exhaustion,
  two-engine lease takeover, stale completion, global capacity/rate/
  serialization, cancellation, and no transaction over a blocked connector.

- [ ] **Step 4: Add failing ingress cases**

  Add signature-before-parse, accepted, duplicate, unmatched, ambiguous,
  guard-false, unexpected-state, invalid-signature-without-provider-ID,
  post-verification database retry, non-buffering, and split audit row counts.

- [ ] **Step 5: Run the RED suite**

  Run:

  ~~~bash
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes -- --test-threads=1
  ~~~

  Expected: each newly added fixture fails at its unimplemented or mismatched
  contract before the corresponding correction.

- [ ] **Step 6: Add only harness controls required by the tests**

  Reuse real Postgres suite databases, local recording connector endpoints,
  explicit database clocks, and child-process lifecycle controls. Add no
  in-memory process backend, admin endpoint, direct process constructor, or
  live provider call.

- [ ] **Step 7: Run GREEN in deterministic and parallel modes**

  Run:

  ~~~bash
  cargo build -p donat-server --bin donat
  cargo test -p donat-conformance --test processes -- --test-threads=1
  cargo test -p donat-conformance --test processes -- --test-threads=2
  ~~~

  Expected: exact fixtures pass with database isolation and two-engine cases.

- [ ] **Step 8: Commit**

  ~~~bash
  git add crates/conformance
  git commit -m "test(processes): add native durable conformance"
  ~~~

### Task 15: Run final full-system acceptance

**Files:**

- None. A discovered regression returns the implementation to the earlier task
  that owns its exact file list and RED/GREEN cycle.

**Interfaces:**

- Consumes: all prior implementation commits.
- Produces: a fully verified declarative process range and one independent
  review result.

- [ ] **Step 1: Run formatting and lint**

  ~~~bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  ~~~

  Expected: both commands exit zero.

- [ ] **Step 2: Run non-conformance workspace tests**

  ~~~bash
  cargo test --workspace --exclude donat-conformance
  ~~~

  Expected: all unit, integration, Postgres, and reviewed snapshot tests pass.

- [ ] **Step 3: Rebuild the actual engine**

  ~~~bash
  cargo build -p donat-server --bin donat
  ~~~

  Expected: the binary used by conformance is fresh.

- [ ] **Step 4: Start Postgres**

  ~~~bash
  docker compose -f docker-compose.conformance.yml up -d --wait
  ~~~

  Expected: the Postgres 16/PostGIS service is healthy.

- [ ] **Step 5: Run focused cross-feature conformance**

  ~~~bash
  cargo test -p donat-conformance --test rules
  cargo test -p donat-conformance --test commands
  cargo test -p donat-conformance --test connectors
  cargo test -p donat-conformance --test processes -- --test-threads=1
  ~~~

  Expected: exact bodies and lifecycle behavior pass.

- [ ] **Step 6: Run full conformance**

  ~~~bash
  make conformance
  ~~~

  Expected: every native suite passes.

- [ ] **Step 7: Review snapshot state**

  ~~~bash
  cargo insta review
  git status --short
  ~~~

  Expected: every intended snapshot is individually reviewed and no
  unexplained `.snap.new` file remains.

- [ ] **Step 8: Verify operational invariants on clean sources**

  Run source-qualified migrate once per test source, start serve, and inspect
  SQL/log capture. Prove serve issued no DDL/reconciliation, no role bypass
  occurred, each worker used its source pool, no transaction crossed external
  I/O, no resolved secret appeared, replay preserved invocation UUID, expired
  execution changed it, and split inbound audit retained every delivery.

- [ ] **Step 9: Request one independent review**

  Request one review of the complete process commit range for spec
  compliance, source locality, error shapes, SQL safety, journal retention,
  and test completeness. Correct every material finding with a failing
  regression test and rerun Steps 1–8.

- [ ] **Step 10: Finish only from a clean verified implementation**

  If verification or review found a defect, return to the task that owns the
  failed interface, add its named regression test, commit the explicit files
  listed by that task, and rerun Steps 1–9. Declare completion only when
  `git status --short` contains no unexplained generated or uncommitted file.
