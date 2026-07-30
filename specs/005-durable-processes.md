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

The five state variants in Section 3 are the complete Process grammar. There
are no `If`, `Switch`, `Merge`, `Code`, or `Wait` logical nodes, workflow
loops, batching-as-workflow, item/paired-item model, subworkflows,
send-and-wait, or ambient workflow item. Decisions are typed Rule guards and
explicit transitions; waiting is only a typed signal or durable timer.
Connector pagination is bounded transport execution inside one committed
activity job and cannot create process states, items, branches, retries,
timers, commands, or database writes.

Commands contain only domain SQL plus closed durable `start_process` and
`signal_process` outbox effects. Connector/provider business logic is outside
the command planner and SQL generator. It can run only through a durable
process activity after the activity intent, lease, and applicable capacity
reservation are committed.

## 2. Cycle-free compilation and public contracts

### 2.1 Dependency ownership

The dependency direction is:

~~~text
donat-server -> donat-schema -> donat-ir
donat-server ----------------> donat-ir
donat-sqlgen ----------------> donat-ir
donat-ir --------------------> donat-metadata
donat-ir --------------------> donat-value-contract
~~~

`donat-schema` never depends on `donat-server`.

The implementation adds the unpublished lower crate
`crates/value-contract` (`donat-value-contract`) as the single owner of the
shared SQL-free value language. It is `#![no_std]`, uses `extern crate alloc`,
forbids unsafe code, has empty default features, has no `std` feature, build
script, procedural macro, or third-party runtime dependency, and exposes no
`serde_json::Value`. It owns `ValueType`, `TypeRef`, `TypedValue`, bounded
canonical constructors, deterministic object ordering, canonical-size
accounting, and the inline-byte representation. `donat-ir` depends on and
re-exports those types; it never defines a second value representation.
Task 1 of the implementation plan is the sole implementation unit and commit
for this shared crate. Connector work records and consumes that same commit;
it cannot create, reimplement, or recommit a second value-contract owner.

Commands, process metadata, declarative HTTP operations, fixed connector
modules, and Rule artifacts all normalize through this one version-1 type
language:

~~~text
type-ref    = primary [ "!" ]
primary     = named | "[" type-ref "]"
named       = identifier
identifier  = ASCII letter or "_" followed by ASCII letters, digits, or "_"
~~~

Whitespace is forbidden inside a type reference. `!` may occur once at each
node and means both present and non-null when that node is a named field;
without `!`, a field may be absent and its value may be null. Thus
`[uuid!]!` is a required non-null list of non-null UUID strings.

The closed canonical scalar vocabulary and aliases are:

| Canonical scalar | Accepted metadata spellings | JSON value |
| --- | --- | --- |
| `boolean` | `Boolean`, `bool`, `boolean` | boolean |
| `string` | `String`, `string`, `ID` | string |
| `int32` | `Int`, `int`, `int32` | integral number in signed 32-bit range |
| `int64` | `int64` | integral number in signed 64-bit range |
| `uint64` | `uint64` | integral number in unsigned 64-bit range |
| `decimal` | `Float`, `float`, `decimal` | canonical finite fixed-point JSON number |
| `uuid` | `uuid` | canonical UUID string |
| `date` | `date` | `YYYY-MM-DD` string |
| `timestamp` | `timestamp` | local ISO-8601 `YYYY-MM-DDTHH:MM:SS[.ffffff]`, with no offset |
| `timestamptz` | `timestamptz` | RFC 3339 `YYYY-MM-DDTHH:MM:SS[.ffffff]` followed by `Z` or `±HH:MM` |
| `json` | `json`, `jsonb` | any JSON value |

A local `timestamp` has exactly four year, two month/day/hour/minute/second
digits, uppercase `T`, a valid proleptic-Gregorian calendar date, hour
`00..23`, minute/second `00..59`, and either no fractional part or a dot
followed by one through six digits. It rejects a space separator, leap second,
trailing decimal point, `Z`, numeric offset, whitespace, and more than six
fractional digits. `timestamptz` requires uppercase `T` plus `Z` or a valid
numeric offset and uses the same zero-through-six fractional-digit and
no-leap-second rules. Validation does not infer a session time zone.

A name outside that table must be declared in
`metadata.custom_types.input_objects`, `metadata.custom_types.enums`, or
`metadata.custom_types.scalars`. A declared custom scalar retains nominal
identity and accepts any JSON scalar value—null, object, and array are not
scalar values. Process and HTTP type strings cannot name a `rules.yaml` type;
a compiled Rule artifact instead converts its already resolved `RuleType`
into the same normalized contract. Input-object recursion is allowed only
through a named reference; every reference must resolve, duplicate names are
rejected, and canonical maps use UTF-8 lexical key order. Enums are nominal
and preserve declared value order.

The lower crate exports these immutable types:

~~~rust
pub const VALUE_TYPE_LANGUAGE_VERSION: u32 = 1;

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
    Decimal(CanonicalDecimal),
}

pub struct CanonicalDecimal(String);

impl CanonicalDecimal {
    pub fn try_new(value: &str) -> Result<Self, ValueContractError>;
    pub fn as_str(&self) -> &str;
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
    media_type: BoundedMediaType,
    file_name: Option<BoundedFileName>,
}

struct BoundedMediaType(String);
struct BoundedFileName(String);

impl BoundedInlineBytes {
    pub fn try_new(
        bytes: Vec<u8>,
        media_type: &str,
        file_name: Option<&str>,
        maximum_decoded_bytes: usize,
    ) -> Result<Self, ValueContractError>;

    pub fn as_slice(&self) -> &[u8];
    pub fn media_type(&self) -> &str;
    pub fn file_name(&self) -> Option<&str>;
}

pub fn canonical_size(
    value: &TypedValue,
) -> Result<usize, ValueContractError>;
~~~

`roots` and every object field retain missing-value requiredness separately
from value nullability. `Ref` may name only an entry in `named_objects`, which
makes recursive input objects finite and self-contained. Map order is
canonical lexical order; enum value order is declared order. Contract
assignment, typed-value validation, and canonical size accounting are
implemented once in this lower crate and are reused by commands, processes,
and connectors. External JSON conversion stays in separately gated adapters.
The accepted connector catalog owns its separate tagged RFC 8785 projection
for persistent hashes. It does not add Serde/JCS to this crate or reuse
`canonical_size`: `I64`, full-width `U64`, exact `Decimal`, and every other
`TypedValue` variant receive distinct tags, object names use UTF-16 JCS order,
and exact integers/decimals are JSON strings. Process code consumes the
catalog's resulting `value_contract_sha256`; it does not implement another
projection or canonicalizer.
Assignability is exact after alias normalization except that any JSON-shaped
contract is assignable to `json`; nominal enum, object, and custom-scalar
names never widen to one another. Exact catalog assignment compares the
complete `named_objects` table in both directions, including declarations
unreachable from every root. An extra, missing, or field-incompatible
unreachable named object therefore rejects; reachability does not erase part
of the closed contract.

The declared identifier grammar has no implicit GraphQL-reserved-name rule.
`__bad` is a valid identifier because it matches the grammar; only an explicit
future metadata rule may reserve a prefix.

`CanonicalDecimal` has a private field, one checked constructor, and no
unchecked construction path. It accepts only an already minimal fixed-point
JSON number: no whitespace, leading plus, exponent, leading integer zero,
negative zero, trailing decimal point, or trailing fractional zero. Zero is
exactly `0`; a non-zero value is an optional `-`, either a non-zero integer
with an optional fraction or `0` with a required non-zero-ending fraction,
and any fraction ends in `1..=9`. The exact ASCII grammar is
`0|-?(?:[1-9][0-9]*(?:\.[0-9]*[1-9])?|0\.[0-9]*[1-9])`.
Thus `-12.5`, `0.01`, and `10` are valid,
while `-12.50e+2`, `-12.50`, `01`, `-0`, `0.0`, and `1.` are rejected.
`canonical_size` counts exactly `CanonicalDecimal::as_str()`, so arbitrary
caller-owned JSON-number spellings cannot change a canonical byte count.

`BoundedMediaType` accepts at most 255 ASCII bytes. `BoundedFileName` accepts
at most 255 UTF-8 bytes and is data, never a filesystem path. Construction
of both private newtypes occurs only inside `BoundedInlineBytes::try_new`;
there is no unchecked or exported constructor. Inline-byte construction
requires `bytes.len() <= maximum_decoded_bytes <= 131_072`. Complete-value
validation rejects more than 16 inline values, more than 131,072 aggregate
decoded bytes, or more than 262,144 canonical bytes. `canonical_size`
accounts for the exact future representation without exposing a JSON encoder.
Bytes use the RFC 4648 base64url alphabet without `=` padding. The root JCS
value is exactly:

~~~json
{"$binary":"<unpadded-base64url>","media_type":"<ASCII>"}
~~~

or, when a file name is present:

~~~json
{"$binary":"<unpadded-base64url>","file_name":"<UTF-8>","media_type":"<ASCII>"}
~~~

Those are the RFC 8785 JCS member orders (`$binary`, optional `file_name`,
`media_type`); there is no alternate spelling or custom ordering rule.
Production sizing uses private checked-arithmetic helpers. Unpadded base64url
length is
`4 * (decoded_len / 3) + { 0, 2, 3 }[decoded_len % 3]`, with every
multiplication and addition checked. JCS string sizing counts the surrounding
quotes, unchanged UTF-8 bytes, the two-byte escapes for quote, reverse
solidus, backspace, tab, line feed, form feed, and carriage return, and
lowercase `\u00xx` for the remaining U+0000..U+001F controls, also with
checked addition. The lower crate has no `serde_json` dependency and neither
helper allocates encoded JSON.

The exact canonical-size vectors are:

- 131,072 zero bytes with `application/octet-stream` and no file name:
  174,817 bytes;
- 131,073 decoded bytes: rejected before encoding;
- the exact outer object
  `{"binary": <that accepted root binary>, "padding": "a" * 87,303}`:
  262,144 bytes;
- the same outer object with 87,304 `a` bytes: 262,145 bytes and rejected;
- 17 inline-byte values: rejected.

The outer descriptions denote `TypedValue::Object` values with the exact keys
`binary` and `padding`. Their serialized JCS bytes are
`{"binary":<root-binary-object>,"padding":"aaa..."}` with no spaces.
Tests construct zero bytes and filler independently. Their oracle does not
call `canonical_size` or any production JSON encoder: it independently
implements the checked RFC 4648 length/encoding and RFC 8785 escaping rules,
constructs the expected bytes, and asserts the root member order, base64url
alphabet, absence of padding, escaping, and exact lengths.

`InlineBytes` is inert at this stage. Metadata, external JSON/form encoding,
connector descriptor admission, multipart transport, commands, and process
journals reject it until their separate gates are implemented and accepted.
The connector ABI therefore consumes this owner and cannot invent a parallel
value, while Task 1 does not enable binary I/O or persistence.

`donat-ir` owns only metadata/Rule adapters such as
`compile_value_contract_catalog`, and re-exports every public value-contract
type. It also owns the unrelated closed process-start policy used by command
effects:

~~~rust
pub enum ProcessStartPolicy {
    Enabled,
    RejectRetired,
}
~~~

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
        BTreeMap<String, BTreeMap<String, TypeRef>>,
    pub definition_fingerprint: String,
}
~~~

The outer key of `required_session_variables` is an allowed explicit role; the
inner mapping contains every session-variable name and its single compatible
contract required by that role's effective table permissions, command
idempotency scope, and command-effect bindings. Incompatible uses of one name
are a deployment error.

Permission-filter inspection and runtime bool-exp planning use one shared,
closed predicate-operator classifier after the existing legacy-name
normalization and backend-capability checks. It assigns descriptor session
contracts as follows:

- scalar comparison, pattern, JSON value, and spatial predicate operands use
  the non-null contract of the predicate's column;
- `_in` and `_nin` use a non-null list of non-null column scalars;
- `_is_null` uses non-null `Boolean`, and a resolved permission-session text
  value accepts only case-insensitive `true` or `false`; all other text fails
  validation;
- `_has_key` uses non-null `String`, while `_has_keys_any` and
  `_has_keys_all` use non-null lists of non-null `String`;
- `_st_d_within` and `_st_3d_d_within` use non-null `Decimal` for `distance`
  and the nominal predicate-column contract for `from`; and
- relationship predicates and `_exists._where` resolve columns against the
  referenced remote table context.

Permission presets contribute a contract only when the preset value is
directly a session-variable string; descriptor compilation does not search
arbitrary preset JSON recursively. A predicate on a computed field whose
definition has `session_argument` is a deployment error because that
whole-session argument cannot publish a closed session-variable name set.
Unknown operators, unavailable capabilities, missing remote contexts, or
incompatible contracts for one role/name fail closed rather than falling back
to `String` or a local-table guess.

`definition_fingerprint` is lowercase SHA-256 over versioned canonical JSON
containing the source, name, recursive argument/result contracts, allowed
roles, required session-variable contracts, ordered raw guard declarations,
steps, Rule artifact hashes, idempotency declaration, and canonical raw effect
declarations. Canonicalization recursively sorts every JSON object key and
preserves every list's supplied order, including raw guard order; guard rule,
binding, and message changes therefore change the fingerprint. The raw effect
material contains process/signal names and raw binding shapes, but no resolved
process revision. Consequently a process revision may include a command
fingerprint without creating a fingerprint cycle when that command's effect is
later pinned to the process revision. Descriptors contain no request value,
resolved session value, database credential, or secret.

The existing command compiler remains the only command compiler.
`CompiledCommand` owns its `CommandDescriptor`; process compilation never
reconstructs command types from raw metadata.

### 2.3 Accepted connector ABI and catalog

Processes do not define connector descriptor types. The accepted
community-connector implementation owns the complete boundary, including the
persistent contracts accepted in
[[knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities]]:

- `donat-connector-abi` owns the const/static-safe `ConnectorId`,
  `OperationId`, `CompiledStepId`, `TriggerId`, processor/authenticator/codec/
  normalizer IDs, bounded envelopes, and host traits;
- `donat-connector-catalog` owns `OperationSpec`, `TriggerSpec`,
  `OperationEffect`, fixed idempotency bindings, capacity/bounds, type
  contracts, semantic/provenance hashes, and generated static entries; and
- the server `ConnectorRegistry` binds deploy-time connector instances to
  those generated entries and is the one catalog consumed by process
  compilation, reconciliation, workers, and ingress.

The Process Task-2 ledger records reviewed ancestor commits for the connector
ABI, normalized catalog, shared executor, static registry, and two-stage
webhook implementation, plus the single shared value-contract commit. Process
implementation stops if any recorded prerequisite is absent or fails its
owning tests. It never repairs the gap by adding server-local operation/event
descriptor structs, string-backed step/scope/header wrappers, or a second
effect enum.

The process compiler consumes these exact external owners:

~~~rust
pub use donat_connector_abi::{
    CompiledStepId, ConnectorId, OperationId, TriggerId,
};
pub use donat_connector_catalog::{
    FixedIdempotencyBinding, OperationEffect, OperationSpec,
    ProviderIdempotentStep, TriggerSpec,
};

pub fn compile_process_catalog(
    metadata: &Metadata,
    commands: &CompiledCommandCatalog,
    rules: &RuleCatalog,
    connectors: &ConnectorRegistry,
) -> Result<CompiledProcessCatalog, PlanError>;
~~~

Metadata source names and connector-instance names remain deployment
identities. Connector/module/operation/step/trigger identities are parsed once
into the exact ABI types and pass from generated statics through registry,
compiler, pinned dependency, and worker without a `String` copy, wrapper
conversion, serialization round-trip, or runtime reparse.

An immutable process revision pins the catalog-owned behavioral
`OperationSpec` or `TriggerSpec` plus a `CatalogIdentityEnvelopeV1` and the
compiler-calculated activity horizon. `OperationSpec` is the complete
non-circular behavioral snapshot: it
includes connector/operation stable SemVer, runtime/value epochs, complete
input/output contracts and recomputed hashes, optional versioned credential,
resolved origin closure, ordered compiled steps and transforms, optional
processor ID plus `implementation_revision`, effect, pagination, complete
error map,
capacity/rate/typed serialization defaults, every step/operation bound, stored
selected-header capabilities, and resolved fact values. It contains no source,
artifact, license, notice, review, or fact-origin identity. The envelope
separately pins canonical/source-record schema epochs, sorted source-record
IDs and record hashes, semantic/provenance hashes, every referenced
input/output/named value-contract hash, the complete operation/trigger
snapshot identity, and the source-bound non-secret deployment fingerprint
defined from `DeploymentMaterialV1` in ADR 012. The revision persists both
without restating a smaller process-owned shape.
Connector/credential/operation/trigger/event versions are SemVer cores without
Phase-1 prerelease/build metadata; schema/runtime and processor-like
implementation revisions are `u32` epochs named `implementation_revision`.
Resolved secret values and raw environment-derived URLs are never persisted.
There is one executable lookup path through `ConnectorRegistry`; inventory
records are absent from it.

The catalog-owned `OperationEffect` remains closed to headerless `ReadOnly`
or `ProviderIdempotent`. The latter contains exactly one evidence-backed fixed
header/body binding, provider uniqueness scope, conservative minimum
retention, and positive clock margin for every side-effecting compiled step.
Missing, duplicate, unproven, or unbounded fields keep an operation
inventory-only. There is no third executable effect and `max_attempts: 1` is
not an escape.

For each side-effecting step, workers derive the key exactly as:

~~~text
base64url_no_pad(SHA256(
  "donat.connector.idempotency.step.v1\0" ||
  JCS({ "logical_activity_id": logical_activity_id,
        "scope": scope,
        "step": compiled_step_id })
))
~~~

The same key is applied only to that step's fixed binding on every retry or
takeover. Another step or scope never reuses it.

`TriggerSpec::Webhook` owns the authenticator, server codec, pure normalizer,
verified event ID/type/output, selected headers, raw-body limit, redaction,
and exact ABI IDs. Raw authentication precedes parsing. Durable dedupe,
delivery audit, correlation, process transition, and acknowledgement remain
Process Task 12 responsibilities rather than connector persistence.

For operation responses, each selected header is the catalog-stored pair
`{canonical_lowercase_header_name, CapabilityId}`. Error correlations retain
that stored mapping or a validated step-local link. Process workers and the
server executor consume it unchanged; they never derive a capability ID or
accept a caller-supplied correlation allowlist.

Existing v2 declarative HTTP/idempotency metadata remains loadable under the
connector plan's compatibility rules. An incomplete legacy
`idempotency: { header: "Idempotency-Key" }` declaration has no proven scope,
retention, or margin and therefore cannot enter executable lookup or be
referenced by a Process.

Stripe mutation execution is likewise inventory-only unless the connector
plan's separate immutable provider-evidence task succeeds and its later
executable migration commit is recorded. A processor-only proof,
compatibility entry, mutable provider page, or negative evidence commit does
not make that operation executable. An independently accepted two-stage
Stripe webhook trigger does not establish mutation idempotency. Process
conformance uses accepted Donat-owned catalog fixtures when the optional
mutation gate has not passed.

For example, an accepted catalog-owned provider-idempotent test operation may
contain:

~~~yaml
effect:
  provider_idempotent:
    side_effect_steps:
      - step: request
        fixed_binding:
          header: Idempotency-Key
        scope: logistics-account-shipment-v1
        minimum_retention_ms: 86400000
        clock_safety_margin_ms: 300000
~~~

The positive numeric fields use checked `u64`; configuration admission
requires `clock_safety_margin_ms < minimum_retention_ms`. The source evidence,
normalized behavioral effect, semantic hash, and pinned process revision
retain the exact values. Their source origins contribute to provenance; the
deployment selection contributes through the exact `DeploymentMaterialV1`
fingerprint in `CatalogIdentityEnvelopeV1`.

No Stripe mutation operation is assumed by this specification. If its
evidence/migration gates later pass, Processes consume the resulting
catalog-owned operation through the same registry path. Any independently
accepted webhook trigger is consumed through `TriggerSpec` and grants no
operation capability.

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
    pub current_revision: String,
    pub start_policy: ProcessStartPolicy,
    pub start_input: ValueContractCatalog,
    pub process_key: Option<TypeRef>,
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

`ProcessStartPolicy` is the closed `donat-ir` enum from Section 2.1;
`donat-schema` imports it rather than defining a schema-owned copy.
For each declared process it contains the current revision, explicit start
policy, exact start input, and optional typed process-key contract. A retired
declaration remains resolvable but
finalizes to `RejectRetired`; Task-owned SQL lowering turns that policy into a
materialized pre-DML `validation-failed` gate with path
`$.selectionSet.<command-field>` and message
`process '<source>.<process>' does not accept new starts`. For each command
signal the catalog contains the signal name, correlation/payload contracts,
the revision whose signal contract was checked, and the set of retained
revisions with an identical signal-contract fingerprint. It contains no
runtime handle or journal access.

`crates/server/src/processes/definition.rs` owns the only process compiler and
its immutable `CompiledProcessCatalog`. Candidate construction in
`crates/server/src/state.rs` performs exactly this order:

1. `compile_rule_catalog` compiles Rules.
2. `compile_command_catalog` compiles source-qualified commands and their
   pre-process `CommandDescriptor` values without resolving process revisions.
3. The accepted `ConnectorRegistry` supplies exact catalog-owned
   `OperationSpec` and `TriggerSpec` values with ABI-owned identities.
4. `processes::definition::compile_process_catalog` compiles source-qualified
   process definitions against those command, Rule, and connector catalog specs
   and derives immutable revisions.
5. The server-owned free function
   `processes::definition::build_process_effect_contract_catalog` consumes
   `&CompiledProcessCatalog` and returns the schema-owned
   `ProcessEffectContractCatalog`.
6. `finalize_command_effects` validates and pins raw command effects without
   changing the pre-process command fingerprint.
7. `CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects`
   compiles the serving schema.
Stages 1-7 are pure candidate compilation. Before publication,
`processes::reconcile::validate_serving_catalogs` is stage 8: it performs only
catalog reads, hash-verifies and recompiles all declared active plus
non-terminal live-retired revisions, checks deploy-time helper compatibility,
and returns an immutable `DeployedProcessCatalog`. A failure discards the
candidate.

Each published `Engine` snapshot uses these exact field names:

~~~rust
pub command_catalog: Arc<CompiledCommandCatalog>,
pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
pub process_catalog: Arc<CompiledProcessCatalog>,
pub deployed_process_catalog: Arc<DeployedProcessCatalog>,
pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
~~~

`process_catalog` is the metadata candidate used to finalize effects;
workers and history verification consume only `deployed_process_catalog`.
Process command transitions resolve executable commands only through the
co-published `finalized_command_catalog`; the pre-process command catalog is
not an executable fallback. All five values come from one candidate snapshot.
No compiler is duplicated and no dependency points from `donat-schema` to
`donat-server`.

The stage-5 function is free rather than an inherent implementation:

~~~rust
pub fn build_process_effect_contract_catalog(
    processes: &CompiledProcessCatalog,
) -> Result<ProcessEffectContractCatalog, PlanError>;
~~~

It is defined in `donat-server`, where `CompiledProcessCatalog` is local, and
constructs the public schema-owned data type through its fields/checked
schema constructors. Rust never receives an inherent implementation of a
foreign `donat-schema` type, and schema never imports the server catalog.

## 3. Canonical process metadata

`processes.yaml` is a top-level metadata section. These are the exact serde
interfaces. Every struct and untagged enum is
`#[serde(deny_unknown_fields)]`; enum spellings are `snake_case`. `lifecycle`
defaults to `active` so existing declarations remain source compatible.

~~~rust
pub struct ProcessDefinition {
    pub name: String,
    pub source: String,
    #[serde(default)]
    pub lifecycle: ProcessLifecycle,
    pub start: ProcessStart,
    #[serde(default)]
    pub input: BTreeMap<String, String>,
    #[serde(default)]
    pub state: BTreeMap<String, String>,
    #[serde(default)]
    pub initial_state: BTreeMap<String, ProcessValueBinding>,
    pub initial: String,
    #[serde(default)]
    pub cancellation: Option<ProcessCancellation>,
    pub states: BTreeMap<String, ProcessStateDefinition>,
}

pub enum ProcessLifecycle {
    Active,
    Retired,
}

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
    #[serde(rename = "type")]
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

pub struct ProcessTerminalState {
    pub terminal: bool,
}

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

pub enum ProcessRetryJitter {
    DeterministicFull,
}

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

pub struct ProcessActivityErrorFallback {
    pub next: String,
}

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
    #[serde(default)]
    pub timeout: Option<ProcessTimeout>,
}

pub struct ProcessVerifiedSignalWait {
    pub connector: String,
    pub event: String,
    pub provider_event_id: ProcessVerifiedEventField,
    pub correlate: BTreeMap<String, ProcessVerifiedEventCorrelation>,
}

pub struct ProcessVerifiedEventField {
    pub event: String,
}

pub struct ProcessVerifiedEventCorrelation {
    pub from: ProcessVerifiedEventField,
    pub equals: ProcessValueBinding,
}

pub struct ProcessCommandSignalState {
    pub wait_for_command: ProcessCommandSignalWait,
    pub on_signal: ProcessTransition,
    #[serde(default)]
    pub timeout: Option<ProcessTimeout>,
}

pub struct ProcessCommandSignalWait {
    pub signal: String,
    pub correlate: BTreeMap<String, ProcessCommandCorrelation>,
    pub payload: BTreeMap<String, String>,
}

pub struct ProcessTimerState {
    pub timer: ProcessTimer,
}

pub struct ProcessTimer {
    pub after: String,
    pub on_timeout: ProcessTransition,
}

pub struct ProcessTimeout {
    pub after: String,
    pub on_timeout: ProcessTransition,
}

pub struct ProcessTransition {
    #[serde(default)]
    pub guard: Option<ProcessRuleCall>,
    #[serde(default)]
    pub command: Option<ProcessCommandInvocation>,
    #[serde(default)]
    pub set: BTreeMap<String, ProcessValueBinding>,
    pub next: String,
    #[serde(default)]
    pub on_rejection: Option<String>,
}

pub struct ProcessRuleCall {
    pub rule: String,
    #[serde(rename = "with", default)]
    pub bindings: BTreeMap<String, ProcessValueBinding>,
}

pub struct ProcessCommandInvocation {
    pub name: String,
    pub run_as_role: String,
    #[serde(default)]
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
        #[serde(rename = "with", default)]
        bindings: BTreeMap<String, ProcessValueBinding>,
    },
}
~~~

The exact complete YAML form is illustrated below. It includes every state
variant and every binding spelling. `donat_checkout_fixture` is an accepted
Donat-owned catalog fixture, not Stripe or another provider admission claim:

~~~yaml
- name: checkout_order
  source: default
  lifecycle: active
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
      order_id:
        type: uuid!
        equals: { state: order_id }
    payload:
      reason: string!
    on_cancel: cancelled
  states:
    create_checkout:
      activity:
        connector: donat_checkout_fixture
        operation: checkout.create
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
          checkout_url: { activity_result: url }
        next: awaiting_payment
      on_error:
        routes:
          - kinds: [authentication, validation]
            next: manual_review
          - kinds: [permanent, invariant, timeout, retry_exhausted]
            next: failed
        fallback:
          next: cooldown
    awaiting_payment:
      wait_for_signal:
        connector: donat_checkout_fixture
        event: checkout.completed
        provider_event_id: { event: provider_event_id }
        correlate:
          order_id:
            from: { event: client_reference_id }
            equals: { state: order_id }
      on_signal:
        guard:
          rule: payment_matches_order
          with:
            order_id: { state: order_id }
            checkout_order_id: { verified_signal: client_reference_id }
        command:
          name: mark_order_paid
          run_as_role: payment_worker
          input:
            order_id: { state: order_id }
          session_variables: {}
        set:
          checkout_url:
            rule: normalize_checkout_url
            with:
              url: { state: checkout_url }
        next: awaiting_receipt
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
    awaiting_receipt:
      wait_for_command:
        signal: receipt_recorded
        correlate:
          order_id:
            type: uuid!
            equals: { state: order_id }
        payload:
          receipt_id: string!
      on_signal:
        command:
          name: attach_receipt
          run_as_role: payment_worker
          input:
            order_id: { state: order_id }
            receipt_id: { verified_signal: receipt_id }
          session_variables: {}
        set:
          checkout_url: { command_result: archived_url }
        next: completed
        on_rejection: failed
    cooldown:
      timer:
        after: 5m
        on_timeout:
          next: manual_review
    completed: { terminal: true }
    manual_review: { terminal: true }
    failed: { terminal: true }
    expired: { terminal: true }
    cancelled: { terminal: true }
~~~

`start` is required and has exactly one command in Phase 1.
That same-source command must contain exactly one raw `start_process` effect
targeting this process, and its canonical input/idempotency bindings must equal
the process `start` mapping. The compiler rejects a missing, duplicate, or
disagreeing declaration; neither side silently overrides the other.

A state deserializes as exactly one of the five untagged variants. `terminal`
must be `true`. Activity `on_success`/`on_error` and wait-state
`on_signal`/nested `timeout` are fields of that variant, not sibling kinds.
The compiler rejects an empty state map, duplicate state name, unknown target,
and any state unreachable from `initial` or an error/cancellation route.
Durations are a non-zero base-10 integer followed immediately by one of
`ms`, `s`, `m`, `h`, or `d`; they normalize to checked `u64` milliseconds.

Process runtime ABI 1 fixes
`MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS = 5000`. A claimed lease expires at its
non-renewing `start_to_close_deadline`. Rotating a lease token during takeover
does not move that deadline or increment the configured attempt ordinal. A
takeover of that same attempt may send again only through
`start_to_close_deadline + MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS`; a worker
arriving later records the typed timeout/window failure without provider I/O.
Capacity, rate, and serialization waiting for each new configured retry
attempt is bounded by that attempt's `schedule_to_start` deadline.
`Retry-After` may postpone a retry only up to that retry's declared
`retry_delay_upper_bound`; a larger value produces the typed timeout path
without another provider send. A runtime that cannot enforce these refusal
bounds cannot claim ABI-1 work.

For each activity and each provider-idempotent compiled step, compilation
calculates with checked integer arithmetic:

~~~text
retry_delay_upper_bound(i) =
  min(max_interval, initial_interval * 2^(i - 1))

maximum_send_horizon_ms =
  max_attempts * (
    start_to_close
    + MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS
  )
  + sum i=1..(max_attempts - 1) of (
    retry_delay_upper_bound(i)
    + schedule_to_start
  )

usable_window_ms =
  minimum_retention_ms - clock_safety_margin_ms
~~~

Deterministic full jitter is bounded by the corresponding
`retry_delay_upper_bound`; capacity/rate/serialization delay is inside
`schedule_to_start`; each attempt term bounds all compiled
step/page/call/redirect work plus its terminal takeover grace, including a
repeated send of the same step. The terminal grace is included even when
`max_attempts = 1`, so a single configured attempt is not an idempotency
escape. The connector's smaller operation deadline is validated against
`start_to_close`, and the HTTP client performs no hidden transport retry.
Compilation requires
`maximum_send_horizon_ms <= usable_window_ms`. Equality is accepted.
Overflow, a missing bound, or a one-millisecond excess rejects the Process as
non-executable. Every side-effecting step is checked independently, and the
result is pinned under that activity state in the executable dependency
closure. `ReadOnly` needs no retention calculation, remains headerless, and
is retry/takeover-safe under the same bounded worker policy.

Every activity uses the one total ordered error form:

~~~yaml
on_error:
  routes:
    - kinds: [authentication, validation]
      next: manual_review
  fallback:
    next: failed
~~~

`routes` is non-empty. Each `kinds` list is non-empty, a kind may occur in only
one route, and `fallback` is mandatory. `retry_on` accepts only
`transport`, `timeout`, `http_429`, and `http_5xx`; an exhausted retried
failure becomes `retry_exhausted` before ordered error routing.

Every transition that invokes a command requires `name`, fixed
`run_as_role`, exact `input`, explicit `session_variables`, `next`, and
`on_rejection`; a transition without `command` must omit `on_rejection`.
`session_variables` is closed: it contains every name required for that role
by `CommandDescriptor` and no other name.

The binding context is exact:

| Destination | Allowed `ProcessValueBinding` variants |
| --- | --- |
| `initial_state` | `literal`, `input`, `rule` over those values |
| activity input | `literal`, `input`, `state`, `rule` over those values |
| wait correlation `equals` | `literal`, `input`, `state`, `rule` over those values |
| transition guard, command input, session variables | the preceding variants plus `verified_signal` only in an `on_signal` transition and `activity_result` only in activity `on_success` |
| transition `set` | all values available before the command plus `command_result` when that same transition has a command |
| timer/timeout transition | no `verified_signal`, `activity_result`, or `command_result` unless the enclosing activity success transition supplies it |

`command_result` always names a field in the result of the command in that
same transition; it is unavailable to that command's own input/session
bindings and is not durable ambient state unless copied by `set`.
`verified_signal` names a field in the selected catalog-owned `TriggerSpec` or
the `wait_for_command.payload` contract. Rule bindings recursively obey the
row's context. No binding can read caller headers, ambient GraphQL session
data, an unverified payload field, a connector-selected role, arbitrary JSON
path, SQL, or a prior transition's ephemeral result.

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

Deployment has two deliberately separate migrate modes:

~~~text
donat --database-url <url> migrate --migrations-dir <dir>
donat migrate --migrations-dir migrations --metadata-dir <dir> --source <name>
donat validate --metadata-dir <dir> --source <name>
~~~

In refinery-only mode, no metadata directory is supplied. An explicit global
`--database-url`, `--metadata-database-url`, `DONAT_DATABASE_URL`, or existing
`DONAT_GRAPHQL_DATABASE_URL` alias is required. `--source` is rejected because
there is no metadata namespace to select. The command applies every pending
migration and exits; it does not load metadata, introspect a catalog, compile
Rules/commands/processes, or reconcile event triggers or process definitions.
This preserves the existing metadata-free
`donat migrate --migrations-dir <dir>` deployment path.

In metadata-aware mode, `--source` selects one metadata source, requires it to
be Postgres, resolves that source's own
`configuration.connection_info.database_url`, and uses the explicit global
database URL only as the existing fallback when the selected source has no
connection URL. A missing named environment variable is an error; it is never
silently replaced by another source's URL. Migration runs against the selected
URL—not against a global URL before selection—then selected-source
consistency, event-trigger reconciliation, and process reconciliation run in
that order.

When metadata is supplied, omitting `--source` is accepted only when it
contains exactly one Postgres source and its URL is unambiguous. Zero or
multiple Postgres sources is a usage error. `migrate` applies V1-V6 and
reconciles event triggers and process definitions only in the selected source.
It never changes another source. `validate` always requires a metadata
directory; it introspects only the selected source and reports that source's
catalog, command, process, deployed-revision, and compatibility problems plus
global metadata-only Rule/connector/source-binding problems. It does not claim
that unselected database catalogs were checked.

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
- referenced connector operation/trigger catalog specs and configuration
  fingerprints;
- the process runtime ABI.

The persisted `dependency_descriptors` JSON is executable, not only an audit
summary. Its exact logical types are:

~~~rust
pub struct ProcessDependencyDescriptors {
    pub commands: BTreeMap<(String, String), CommandDescriptor>,
    pub rules: BTreeMap<String, PinnedRuleBundle>,
    pub connector_operations:
        BTreeMap<(String, OperationId), PinnedConnectorOperation>,
    pub connector_inbound_events:
        BTreeMap<(String, TriggerId), PinnedConnectorInboundEvent>,
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
    pub connector_instance: String,
    pub spec: OperationSpec,
    pub catalog_identity: CatalogIdentityEnvelopeV1,
    pub activity_send_horizons_ms:
        BTreeMap<String, BTreeMap<CompiledStepId, u64>>,
}

pub struct PinnedConnectorInboundEvent {
    pub source_name: String,
    pub connector_instance: String,
    pub trigger: TriggerSpec,
    pub catalog_identity: CatalogIdentityEnvelopeV1,
}

pub struct CatalogIdentityEnvelopeV1 {
    pub canonical_schema_epoch: u32,
    pub source_record_schema_epoch: u32,
    pub source_records: Vec<SourceIdentityMaterialV1>,
    pub semantic_sha256: Hash256,
    pub provenance_sha256: Hash256,
    pub value_contracts: Vec<ValueContractIdentityV1>,
    pub snapshot_identity: CatalogSnapshotIdentityV1,
    pub deployment_fingerprint: Hash256,
}

pub struct ValueContractIdentityV1 {
    pub use_site: ValueContractUseSiteId,
    pub value_contract_sha256: Hash256,
}

pub enum CatalogSnapshotIdentityV1 {
    Operation {
        connector: ConnectorId,
        connector_version: StableSemver,
        operation: OperationId,
        operation_version: StableSemver,
        snapshot_sha256: Hash256,
    },
    Trigger {
        connector: ConnectorId,
        connector_version: StableSemver,
        trigger: TriggerId,
        trigger_version: StableSemver,
        snapshot_sha256: Hash256,
    },
}
~~~

The Rule bundle contains only the referenced definitions and their transitive
named-type closure. Loading it calls
`donat_rules::compile_catalog_with_declared_types` and compares both stored
hash maps before a worker may claim the revision. A pinned connector entry
stores its owning metadata source beside the exact complete behavioral
catalog `OperationSpec`; it is not a projection and cannot omit a behavioral
field. `catalog_identity` is the exact ADR-012
`CatalogIdentityEnvelopeV1`, not a process-owned summary. It pins canonical
and source-record schema epochs; sorted `(SourceRecordId, record_sha256)`
identities; semantic and provenance hashes; every referenced input, output,
event, named, and transitive value-contract hash; the complete operation or
trigger snapshot identity; and the separately computed
`deployment_fingerprint`. The deployment fingerprint is
`SHA256("donat.connector.deployment.v1\0" ||
JCS(DeploymentMaterialV1))`; that closed material stores source/instance,
connector, endpoint-reference, credential-reference, resolver, non-secret
configuration, enabled operation/trigger, and override identities, never
secret values or resolved URLs. For every activity state using
that operation it also stores the compiler's
per-side-effect-step `maximum_send_horizon_ms`; the `OperationSpec` retains the
matching fixed binding, scope, retention, and margin. A fresh binary
recomputes every hash, snapshot identity, deployment material, and horizon and
compares the behavioral snapshot and every envelope field before accepting
the persisted revision. Current
metadata may not bind that connector instance to another source while an
active or non-terminal live-retired revision retains the old binding.

The deploy-time reconciliation entry point is:

~~~rust
pub async fn reconcile(
    source_name: &str,
    database_url: &str,
    source_catalog: &donat_catalog::Catalog,
    compiled_processes: &CompiledSourceProcessCatalog,
) -> anyhow::Result<()>;
~~~

It inserts a missing immutable revision, leaves an identical deployment
unchanged, activates a declaration whose lifecycle is `active`, and records a
declaration whose lifecycle is `retired` without accepting new starts.
Replacing A with active B retires A without deleting it. Omitting the entire
process while any non-terminal instance exists is rejected; explicit
`lifecycle: retired` is the only way to stop new starts while retaining its
runtime dependency closure. Once every revision is terminal, omission may
delete no history but may leave all definition rows retired.

Reconciliation rejects removal or an incompatible change of a command,
runtime ABI, wait/cancellation signal contract, connector operation, or
connector/source binding still referenced by a non-terminal instance. It also
queries active plus non-terminal live-retired dependency rows across every
metadata source mapped to the same physical database before accepting a
connector rebind. A process revision remains executable until no live instance
can use it.

Serving loads persisted executable revisions through:

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

pub async fn validate_serving_catalogs(
    runtimes: &HashMap<String, SourceRuntime>,
    process_catalog: &CompiledProcessCatalog,
    command_catalog: &CompiledCommandCatalog,
    connectors: &ConnectorRegistry,
) -> anyhow::Result<DeployedProcessCatalog>;
~~~

For each real source this function reads only `pg_catalog` and `donat`
catalog/journal rows. It verifies the deploy-time helper signature, selects
the declared current revision plus every distinct revision referenced by a
non-terminal instance, verifies canonical definition/dependency hashes,
recompiles the persisted Rule bundle and process definition, verifies current
command fingerprints and connector ABI/configuration support, and rejects a
cross-source connector binding. It issues no `CREATE`, `ALTER`, `DROP`,
`INSERT`, `UPDATE`, `DELETE`, or reconciliation call. The returned catalog is
published atomically with the candidate Engine.

A start request stores the enabled active revision selected when the command
statement executes. `RejectRetired` raises before command domain DML and writes
no request. The start worker never substitutes a newer revision. A signal
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

The migration and compatible command writer are one implementation unit and
one commit. In the same Task 6 that makes the column non-null, the existing
one-statement claim/result CTE explicitly supplies `gen_random_uuid()` on a
successful first or expired generation and selects the stored UUID on replay;
it does not rely on a column default. Task 6 runs command regressions and the
full native conformance crate, so there is no green checkpoint where V6 has
landed but an existing writer omits the column. Task 7 only adds the typed
internal `CommandExecutionResult` projection/decoder for that already-correct
stored identity.

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
~~~

`CurrentExecution` is a trusted closed reference, not metadata. SQLgen
resolves it to the concrete UUID returned by the command invocation CTE.
`effect_position` is the zero-based position in canonical command effect
order. `ProcessStartPolicy` is the `donat-ir` enum from Section 2.1.
All other fields are fully source- and type-resolved by the planner.

`plan_mutation.rs` lowers finalized effects into this IR. SQLgen inserts each
start or signal request from the same successful command CTE statement and
only from the `first` claim path. Every outbox row copies the concrete
`invocation_id` and is unique on
`(command_invocation_id, effect_position)`. Replay, changed-input rejection,
guard rejection, or any database error inserts no second outbox row.
`RejectRetired` is checked by a materialized effect gate before the
idempotency claim and every domain CTE; it raises the exact Section 2.4
envelope and therefore cannot leave domain DML, a command journal row, or an
outbox row.

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

V6 also owns the deploy-time definition of
`donat.check_violation(msg text) returns json`: it raises SQLSTATE `23514`
with the existing permission-error JSON payload. It uses `create or replace
function` in the migration. Serve deletes `ensure_check_violation_helper` and
checks the installed function's schema, argument type, return type, language,
and execute privilege through `pg_catalog` only; absence or incompatibility is
a startup error instructing the operator to run `donat migrate`.

The migration alters the command journal:

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
  source_name text not null,
  process_name text not null,
  revision text not null,
  canonical_definition jsonb not null,
  dependency_descriptors jsonb not null,
  runtime_abi integer not null check (runtime_abi > 0),
  status text not null check (status in ('active','retired')),
  deployed_at timestamptz not null default now(),
  retired_at timestamptz,
  primary key(source_name, process_name, revision)
)
unique(source_name, process_name) where status = 'active';

donat.process_start_requests(
  source_name text not null,
  id uuid not null,
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
  primary key(source_name, id),
  unique(source_name, command_invocation_id, effect_position),
  foreign key(source_name, process_name, revision)
    references donat.process_definition_versions(
      source_name, process_name, revision
    )
);

donat.process_instances(
  source_name text not null,
  id uuid not null,
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
  primary key(source_name, id),
  unique(source_name, process_name, start_idempotency_key),
  unique(source_name, source_request_id),
  foreign key(source_name, source_request_id)
    references donat.process_start_requests(source_name, id),
  foreign key(source_name, process_name, revision)
    references donat.process_definition_versions(
      source_name, process_name, revision
    )
);

donat.process_events(
  source_name text not null,
  id uuid not null,
  instance_id uuid not null,
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
  primary key(source_name, id),
  foreign key(source_name, instance_id)
    references donat.process_instances(source_name, id),
  foreign key(source_name, process_name, revision)
    references donat.process_definition_versions(
      source_name, process_name, revision
    )
)
unique(source_name, process_name, revision, kind, idempotency_key)
  where idempotency_key is not null;

donat.process_signal_requests(
  source_name text not null,
  id uuid not null,
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
  primary key(source_name, id),
  unique(source_name, command_invocation_id, effect_position),
  foreign key(source_name, process_name, process_revision)
    references donat.process_definition_versions(
      source_name, process_name, revision
    )
);

donat.process_activity_jobs(
  source_name text not null,
  id uuid not null,
  instance_id uuid not null,
  enqueued_from_event_id uuid not null,
  state_name text not null,
  logical_activity_id text not null,
  connector_instance text not null,
  operation text not null,
  serialization_key_hash bytea,
  input_json jsonb not null,
  result_json jsonb,
  request_fingerprint text not null,
  status text not null check (status in (
    'scheduled','running','succeeded','failed','cancelled'
  )),
  attempts integer not null default 0 check (attempts >= 0),
  lease_generation bigint not null default 0 check (lease_generation >= 0),
  available_at timestamptz not null default now(),
  schedule_to_start_deadline timestamptz not null,
  start_to_close_deadline timestamptz,
  lease_token uuid,
  lease_expires_at timestamptz,
  last_error_json jsonb,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  primary key(source_name, id),
  unique(source_name, instance_id, logical_activity_id),
  foreign key(source_name, instance_id)
    references donat.process_instances(source_name, id),
  foreign key(source_name, enqueued_from_event_id)
    references donat.process_events(source_name, id)
);

donat.process_activity_provider_steps(
  source_name text not null,
  activity_job_id uuid not null,
  logical_activity_id text not null,
  compiled_step_id text not null,
  idempotency_key text not null,
  first_provider_attempt_at timestamptz not null,
  maximum_send_deadline_at timestamptz not null,
  usable_window_expires_at timestamptz not null,
  created_at timestamptz not null default now(),
  primary key(source_name, logical_activity_id, compiled_step_id),
  unique(source_name, activity_job_id, compiled_step_id),
  foreign key(source_name, activity_job_id)
    references donat.process_activity_jobs(source_name, id)
);
index(source_name, usable_window_expires_at);

donat.process_transition_logs(
  source_name text not null,
  id uuid not null,
  instance_id uuid not null,
  event_id uuid,
  activity_job_id uuid,
  activity_attempt integer,
  activity_lease_generation bigint,
  from_state text,
  to_state text,
  outcome text not null,
  definition_revision text not null,
  command_result_json jsonb,
  before_state_hash bytea,
  after_state_hash bytea,
  redacted_context jsonb not null,
  created_at timestamptz not null default now(),
  primary key(source_name, id),
  foreign key(source_name, instance_id)
    references donat.process_instances(source_name, id),
  foreign key(source_name, event_id)
    references donat.process_events(source_name, id),
  foreign key(source_name, activity_job_id)
    references donat.process_activity_jobs(source_name, id)
)
unique(source_name, instance_id, event_id) where event_id is not null;
unique(
  source_name, activity_job_id, activity_attempt,
  activity_lease_generation, outcome
)
  where activity_job_id is not null
    and activity_attempt is not null
    and activity_lease_generation is not null;

donat.process_capacity_reservations(
  source_name text not null,
  id uuid not null,
  activity_job_id uuid not null,
  connector_instance text not null,
  operation text not null,
  serialization_key_hash bytea,
  lease_token uuid not null,
  reserved_at timestamptz not null,
  expires_at timestamptz not null,
  released_at timestamptz,
  primary key(source_name, id),
  unique(source_name, activity_job_id, lease_token),
  foreign key(source_name, activity_job_id)
    references donat.process_activity_jobs(source_name, id)
);
index(source_name, connector_instance, operation, expires_at);
index(
  source_name, connector_instance, operation,
  serialization_key_hash, expires_at
);

donat.process_capacity_buckets(
  source_name text not null,
  connector_instance text not null,
  operation text not null,
  available_tokens numeric(38,18) not null
    check (available_tokens >= 0),
  last_refill_at timestamptz not null,
  policy_fingerprint text not null,
  primary key(source_name, connector_instance, operation)
);

donat.process_inbound_deliveries(
  source_name text not null,
  id uuid not null,
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
  instance_id uuid,
  process_event_id uuid,
  redacted_metadata jsonb not null,
  received_at timestamptz not null default now(),
  primary key(source_name, id),
  foreign key(source_name, instance_id)
    references donat.process_instances(source_name, id),
  foreign key(source_name, process_event_id)
    references donat.process_events(source_name, id),
  check (
    (outcome = 'accepted'
      and instance_id is not null
      and process_event_id is not null)
    or
    (outcome <> 'accepted'
      and instance_id is null
      and process_event_id is null)
  )
);
index(source_name, instance_id, received_at)
  where instance_id is not null;

donat.process_inbound_events(
  source_name text not null,
  id uuid not null,
  connector_instance text not null,
  provider_event_id text not null,
  first_delivery_id uuid not null,
  payload_digest bytea not null,
  verified_at timestamptz not null,
  primary key(source_name, id),
  unique(source_name, connector_instance, provider_event_id),
  foreign key(source_name, first_delivery_id)
    references donat.process_inbound_deliveries(source_name, id)
    deferrable initially deferred
);
~~~

No process table has a foreign key to `donat.command_invocations` or
`donat.command_invocation_claims`. Indexes cover due event/start/signal/job
status and `available_at`, activity lease expiry, instance process/state
correlation, definition status, source-qualified inbound received/instance
time, and capacity expiry. Every primary, foreign, partial-unique, semantic
dedupe, due-work, and reconciliation predicate begins with `source_name`;
two metadata source names sharing one physical database therefore remain
disjoint even when process names, UUIDs, and semantic keys are identical.
`process_activity_provider_steps` has a row only for a
provider-idempotent side-effecting compiled step. Its database-clock
`first_provider_attempt_at`, derived stable key, compiled maximum-send
deadline, and immutable usable-window deadline are inserted and committed
before that step's first network send; retry and takeover observe the same
row.

## 8. Start consumption and command transitions

The start worker claims one pending request with
`FOR UPDATE SKIP LOCKED` in the request's source pool. In one short
transaction it resolves the exact `(source_name, process_name, revision)` from
the Engine's immutable `deployed_process_catalog`, validates the canonical input,
inserts or finds the instance by
`(source_name, process_name, start_idempotency_key)`, appends the start event
and transition log for a new instance, records duplicate outcome for an
existing semantic instance, and marks the request consumed. It never compiles
metadata or substitutes the current declaration. A crash before commit leaves
the request pending; a crash after commit cannot create a second instance.

Every ordinary transition locks one due event and its instance, verifies the
pinned deployed definition/runtime ABI, evaluates its closed guard context,
compares the optimistic instance version, writes the transition log and next
event/timer/activity, and marks the event consumed in one short outer
transaction. The worker refuses a revision absent from
`deployed_process_catalog`; it does not fall back to `process_catalog`.

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

The executable entry point accepts the finalized owner, never a bare
pre-process command:

~~~rust
pub async fn execute_process_command_in_savepoint(
    transaction: &tokio_postgres::Transaction<'_>,
    command: &FinalizedCompiledCommand,
    role: &str,
    arguments: TypedValue,
    session_variables: BTreeMap<String, TypedValue>,
) -> anyhow::Result<ProcessCommandOutcome>;
~~~

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

`crates/server/src/commands.rs` owns the strict shared decoder extracted from
the current private GraphQL helper:

~~~rust
pub(crate) struct CommandBusinessRejection {
    pub code: String,
    pub path: String,
    pub message: String,
}

pub(crate) fn decode_command_business_rejection(
    error: &tokio_postgres::Error,
) -> Option<CommandBusinessRejection>;
~~~

It returns `Some` only when the driver error has SQLSTATE `P0D01` and its
primary message is an object with exactly the four string fields `kind`,
`code`, `path`, and `message`, `kind == "donat.graphql-error.v1"`, a non-empty
code, and a path beginning with `$`. GraphQL rendering and the process
savepoint consume this typed value. Permission SQLSTATE `23514`, malformed
reserved payloads, and every non-`P0D01` error return `None`.

GraphQL and process execution share the same internal command planner,
single-statement renderer, result decoder, and strict rejection decoder.
Neither path accepts raw SQL or constructs a new error envelope.

Each Postgres `ProcessRuntime` owns a cloned
`deadpool_postgres::Pool` extracted from the matching
`SourceRuntime::Postgres { pool, .. }` during Engine worker construction.
Non-Postgres variants are rejected before a worker is created; no abstract
pool type or cross-backend process executor is introduced.

~~~rust
pub struct ProcessRuntime {
    pub source_name: String,
    pub pool: deadpool_postgres::Pool,
    pub deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    pub connector_registry: Arc<ConnectorRegistry>,
}
~~~

Both command catalogs come from the same published Engine snapshot.
Reconciliation and descriptor checks may inspect `command_catalog`; process
command execution resolves the source/name in
`finalized_command_catalog` and passes the resulting
`FinalizedCompiledCommand` to the shared planner. That planner lowers its
finalized start/signal effects exactly as GraphQL command execution does.
Applied transition commands therefore commit domain DML, the command
generation, and all effect outboxes in the command's one statement inside the
savepoint. A business rejection rolls all of those command writes back before
the outer `on_rejection` transition commits.

## 9. Activities, leases, retries, and capacity

A state transition gives each activity one deterministic
`logical_activity_id` from the process revision, instance, triggering event,
and state name. That logical ID is not itself an HTTP header. For each
side-effecting compiled step, Section 2.3 derives a distinct provider key from
the logical ID, fixed provider scope, and step ID. The per-step key and
database-clock first-attempt time are unchanged by retries, crash recovery,
or lease takeover. A `ReadOnly` operation is headerless.

`permits` and `burst` are positive integers and `per` is a positive duration.
They define a token bucket with refill rate `permits / per` and maximum
`burst`; one activity start consumes exactly one token. `max_in_flight` is a
separate positive upper bound on unexpired, unreleased reservations. A token
is never refunded because the rate policy limits starts, while releasing or
expiring a reservation restores only an in-flight slot.

An activity claim uses this exact transaction protocol:

1. Select one scheduled due new/retry attempt or one running job with an
   expired lease in the source using `FOR UPDATE SKIP LOCKED`; read
   `db_now = statement_timestamp()` once. A running job retains its configured
   `attempts` ordinal and non-renewing `start_to_close_deadline`.
2. For a scheduled attempt, if
   `schedule_to_start_deadline <= db_now`, append its timeout event and finish
   without touching capacity. For a takeover, if
   `db_now > start_to_close_deadline +
   MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS`, append the typed timeout/window
   failure and make no provider call.
3. Insert the `(source_name, connector_instance, operation)` bucket with
   `available_tokens = burst`, `last_refill_at = db_now`, and the pinned
   capacity-policy fingerprint using `ON CONFLICT DO NOTHING`.
4. Lock that one bucket row `FOR UPDATE`. Every claimant acquires locks in
   job-then-bucket order and an activity references exactly one bucket, so no
   transaction holds multiple bucket locks. A fingerprint mismatch is an
   invariant failure; reconciliation may replace/reset a bucket policy only
   after no active or live-retired revision references the prior catalog spec.
5. Mark expired unreleased reservations for this bucket released at `db_now`.
   Count only rows with `released_at IS NULL AND expires_at > db_now`.
   Reject capacity when that count is `>= max_in_flight`. If
   `serialize_by` is present, also reject when one such row has the same
   non-null `serialization_key_hash`.
6. Refill with exact numeric arithmetic:
   `tokens = min(burst, available_tokens + elapsed_ms * permits / per_ms)`,
   where `elapsed_ms = max(0, db_now - last_refill_at)`. Persist `tokens` and
   `last_refill_at = db_now`. If `tokens < 1`, leave the job scheduled and set
   `available_at` to
   `db_now + ceil((1 - tokens) * per_ms / permits)` milliseconds.
7. Otherwise subtract one token, insert one reservation, assign a random lease
   token, and increment `lease_generation`. Increment `attempts` and set a new
   `start_to_close_deadline` only when claiming a scheduled new/retry attempt.
   A takeover keeps both the attempt ordinal and its original deadline.
   Store the lease expiry in that same transaction.
8. Commit before calling the connector. Any database error rolls back the
   bucket, reservation, and job together and makes no provider call.

Unavailable max-in-flight or serialization capacity leaves the job scheduled
without consuming a token. Because every decision is made while holding the
durable bucket row lock, two connections cannot both observe and consume the
same last slot or token.
After the claim transaction commits, the activity runtime may invoke only the
pinned connector operation. For a provider-idempotent compiled step it opens
a separate short transaction immediately before the first network send,
inserts or reads
`(source_name, logical_activity_id, compiled_step_id)` in
`process_activity_provider_steps` using `statement_timestamp()`, derives the
fixed step key, stores
`maximum_send_deadline_at = first_provider_attempt_at +
maximum_send_horizon_ms` and
`usable_window_expires_at = first_provider_attempt_at +
(minimum_retention_ms - clock_safety_margin_ms)`, and commits. Before every
retry/takeover send it reads the same row using the database clock. Equality
does not by itself expire either bound because both comparisons are strict
`>`; authorization still evaluates both bounds in the following order.
Authorization uses this exact precedence:

1. If `db_now > usable_window_expires_at`, permanently refuse network I/O
   with `connector_idempotency_window_exhausted`.
2. Otherwise, if `db_now > maximum_send_deadline_at`, refuse network I/O
   through the typed timeout route.
3. Otherwise authorize the send with the persisted fixed key/binding.

Because compilation requires the maximum-send deadline to be no later than
the usable provider deadline, a time after the usable deadline satisfies both
late predicates; step 1 deliberately wins. A time strictly between unequal
deadlines takes step 2. Neither refusal path rotates the key.

The connector receives immutable typed input, the logical activity identity,
canonical request fingerprint, deadline/control capability, and only the
per-step fixed idempotency binding when the pinned effect requires it. It
receives no pool, role, mutable process, arbitrary transport request, retry
policy, process transition, timer, command, or secret-bearing logger.
Provider-specific business logic executes only here after committed intent;
the command planner, command SQL, and command-transition savepoint never call
a connector.

Completion uses a new short transaction and updates only a job whose current
lease token still matches. It stores the result/failure, releases the
reservation, and appends one completion event atomically. A stale completion
cannot advance the instance and receives an append-only attempt audit.

`schedule_to_start` is measured from each attempt's scheduled
`available_at`—initially activity enqueue, then the bounded retry
`available_at`—and makes no provider call when expired. `start_to_close` is
measured from claim and makes a late completion stale. Phase 1 has no
heartbeat extension.

`max_attempts` includes the first attempt. Retry delay is
`min(max_interval, initial_interval * 2^(attempt-1))`, with deterministic full
jitter derived from logical activity ID and attempt. `Retry-After` may
postpone, never accelerate, but a value beyond the declared upper bound
terminates that retry path without another send. Exhaustion appends
`retry_exhausted`. No retry is implicit or infinite.

## 10. Timers, command signals, and verified inbound delivery

Timers are source-local journal events due by the owning Postgres clock.
Tokio polling is only a wake-up mechanism; no sleep or in-memory wheel is
business state.

The connector endpoint remains
`POST /v1/connectors/{connector_instance}/webhooks`. The compiled connector
binding selects exactly one process source. The connector verifies the raw
bytes before parsing and returns only its typed verified event contract.

The connector-owned raw-verification responses remain unchanged:

| Pre-persistence outcome | Exact response |
| --- | --- |
| unknown connector instance or no webhook verifier | empty `404 Not Found` |
| raw body exceeds the connector bound | empty `413 Payload Too Large` |
| missing, malformed, expired, unsupported, or invalid verification | empty `400 Bad Request` |
| successful verification before durable Process ingress exists | empty `503 Service Unavailable` |

Inbound persistence is deliberately split:

- `process_inbound_events` is only the verified provider-event dedupe ledger,
  unique on `(source_name, connector_instance, provider_event_id)`;
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

An `accepted` transaction creates the process event first and sets both
`process_inbound_deliveries.instance_id` and `process_event_id` before commit.
Every other outcome leaves both links null. Consequently instance inspection
uses the indexed relational predicate
`delivery.source_name = instance.source_name AND
delivery.instance_id = instance.id`; it never joins on redacted payloads or
provider text. A duplicate remains independently auditable but links to no new
event because it creates none; the original accepted delivery remains linked.

The route acknowledges a verified provider event only after the complete
transaction commits. Every successfully committed verified outcome has the
same exact response:

| Committed verified outcome | Exact response |
| --- | --- |
| `accepted` | empty `204 No Content` |
| `duplicate` | empty `204 No Content` |
| `unmatched` | empty `204 No Content` |
| `ambiguous` | empty `204 No Content` |
| `guard_false` | empty `204 No Content` |
| `unexpected_state` | empty `204 No Content` |

A post-verification persistence or transition database failure returns empty
`503 Service Unavailable`, so the provider can retry. It never returns a
success status or body before commit. The durable matrix does not change the
raw `404`/`413`/`400` verification matrix above. Payload digests and redacted
metadata are retained; raw bodies and secrets are not retained by this
Phase-1 schema.

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
| retryable selected connector error | job rescheduled with deterministic delay and the same fixed key for each re-sent side-effect step |
| non-retried connector error | typed completion event follows the first matching route or fallback |
| retry limit | `retry_exhausted` follows the declared route or fallback |
| capacity or serialization unavailable | job remains scheduled; no provider call |
| schedule-to-start timeout | typed timeout event; no provider call |
| start-to-close timeout or stale completion | current lease may be retried; stale completion is audit-only |
| compiled maximum-send horizon exhausted while provider window remains usable | typed timeout before network I/O |
| provider idempotency window exhausted | permanent typed failure before network I/O; the fixed step key is not rotated |
| duplicate start semantic key | request is duplicate; existing instance is unchanged |
| duplicate verified provider event | new delivery audit plus existing dedupe row; no second process event |
| invalid signature | delivery audit only |
| unmatched/ambiguous/unexpected signal | delivery/request audit only |
| cancellation | declared transition; only unclaimed jobs cancelled |
| missing pinned definition/ABI | invariant audit and claim stop; no invented transition |

A terminal instance treats later signals, timers, and completions as
audit-only. `lifecycle: retired` blocks new starts but keeps the complete
declaration and executable dependency closure until every instance is
terminal; omission while live is a deployment error.

## 12. Test-first acceptance matrix

Every behavior starts with a failing crate test and, where externally visible,
a native conformance fixture. The focused identifiers are normative:

| Test ID | Level | Required proof |
| --- | --- | --- |
| `value_type_language_is_closed_and_canonical` | value-contract unit | aliases, scalar JSON semantics, named refs, recursion, requiredness, and assignability use one parser in the `no_std + alloc` owner |
| `value_type_identifier_grammar_has_no_implicit_reserved_prefix` | value-contract unit | `__bad` is accepted because the declared identifier grammar contains no reserved-prefix exception |
| `canonical_decimal_spelling_is_exact` | value-contract unit | only the private minimal fixed-point spelling constructs; exponent, plus, leading/trailing zero ambiguity, negative zero, and arbitrary `String` construction reject |
| `value_contract_timestamp_grammar_is_exact` | value-contract unit | local timestamp accepts only valid `T`-separated no-offset values with zero-to-six fractional digits; timestamptz requires `Z`/offset |
| `value_contract_no_std_boundary_is_mechanical` | workspace policy | value contract compiles for the no-OS target, has no `std`/unsafe/build-script/third-party runtime edge, and IR only re-exports it |
| `value_contract_has_one_owner` | workspace policy | process and connector plans name the same Task-1 crate and commit; no connector-local value implementation exists |
| `catalog_value_projection_is_not_a_second_value_owner` | cross-plan boundary | the catalog alone adapts shared `TypedValue` to tagged JCS with UTF-16 object ordering and exact integer/decimal strings; the lower crate gains no Serde/JCS and process code implements no projection |
| `value_contract_assignability_compares_unreachable_named_objects` | value-contract unit | exact assignment compares the full named-object table, so extra, missing, or incompatible unreachable declarations reject |
| `inline_bytes_have_one_inert_owner` | value-contract/workspace | the lower crate alone owns bytes, bounded media type/file name, and canonical accounting while every external adapter remains gated |
| `inline_binary_canonical_size_vectors_are_exact` | value-contract unit | an independent checked oracle proves unpadded RFC 4648 base64url, root/outer JCS order and escaping, and the 174,817/262,144/262,145-byte vectors |
| `inline_binary_count_and_decoded_bounds_are_exact` | value-contract unit | 131,072 aggregate decoded bytes and 16 inline values are accepted boundaries; one-byte/one-value excess rejects |
| `inline_binary_external_adapters_remain_disabled` | boundary unit | metadata, JSON/form, connector admission, multipart, commands, and process journals reject `InlineBytes` |
| `adapter_normalizes_custom_types_and_recursive_refs` | IR unit | the sole metadata adapter emits normalized custom scalars, enums, input objects, and recursive references |
| `adapter_rejects_unknown_duplicate_and_invalid_refs` | IR unit | unknown, duplicate, and malformed declarations fail before a descriptor publishes |
| `adapter_accepts_the_declared_double_underscore_identifier_grammar` | IR unit | adapter normalization preserves the shared grammar in which `__bad` has no implicit reserved-prefix rejection |
| `missing_command_source_catalog_is_a_validation_error` | schema unit | an absent source catalog reports the exact command validation error instead of panicking |
| `command_descriptor_exposes_exact_contract` | schema unit | recursive argument/result contracts, roles, session variables, and deterministic pre-process fingerprint are public |
| `command_descriptor_session_contracts_follow_predicate_operators_and_tables` | schema unit | list/is-null operands and relationship/_exists recursion publish contracts from the shared operator classifier and remote table context |
| `command_descriptor_session_contracts_follow_operator_operand_types` | schema unit | JSON key operands and spatial distance/from operands publish their exact String/List<String>/Decimal/nominal-column contracts |
| `command_descriptor_rejects_computed_permission_session_argument` | schema unit | a whole-session computed-field argument cannot publish a closed session-variable name set and fails deployment |
| `command_descriptor_rejects_predicate_operator_contract_conflicts` | schema unit | one role/name used by operators with incompatible operand contracts fails deployment |
| `command_descriptor_fingerprint_is_pre_process_and_deterministic` | schema unit | recursive key sorting plus ordered raw guards/effects and Rule hashes are deterministic and exclude resolved process revisions |
| `command_descriptor_rejects_incompatible_session_variable_uses` | schema unit | incompatible permission-column uses of one role/name fail before serving |
| `is_null_permission_session_string_is_strict_boolean` | planner unit | resolved permission-session text accepts only case-insensitive true/false and rejects all other text |
| `process_connector_ledger_uses_catalog_specs_without_local_descriptors` | cross-plan boundary | Processes consume accepted `OperationSpec`/`TriggerSpec` and ABI IDs; no server-local string descriptor model exists |
| `process_pins_complete_operation_snapshot` | process compiler/restart | the persisted behavioral `OperationSpec` retains every connector/operation/credential version, epoch, contract/hash, default, origin, ordered step/transform, `implementation_revision`, effect, pagination/error plan, bound, selected-header capability, and resolved fact value; any omitted/changed field rejects reload |
| `process_pins_complete_catalog_identity_envelope` | process compiler/restart | operation and inbound-event pins persist and field-for-field compare canonical/source-record epochs, sorted source IDs/hashes, semantic/provenance hashes, every referenced value-contract hash, complete snapshot identity, and deployment fingerprint |
| `process_deployment_fingerprint_is_exact_and_secret_free` | process compiler/restart | independent JCS/domain vectors cover every `DeploymentMaterialV1` field; one-field mutation changes the fingerprint, while changing a resolved secret value does not enter persisted bytes |
| `process_runtime_never_derives_header_capabilities` | cross-plan source policy | executor/process source consumes only catalog-stored header/capability mappings and selected-action allowlists; no capability domain/hash derivation or caller-provided allowlist path exists |
| `process_connector_registry_is_the_only_task_3_to_14_catalog` | cross-plan boundary | compiler, reconcile, activity, ingress, and rolling tests use one accepted static registry |
| `process_connector_inventory_only_stripe_mutation_is_not_executable` | connector/process boundary | Stripe mutation is absent from process operation lookup until immutable evidence and executable migration commits both pass; a webhook trigger grants no operation |
| `connector_effect_model_is_closed_and_per_step` | connector catalog unit | headerless read-only and complete fixed provider-idempotent step records publish; inventory-only side effects reject |
| `connector_effect_retention_boundary_is_exact` | process compiler | every step accepts horizon equality, rejects one millisecond over, uses checked arithmetic, and includes final takeover grace for `max_attempts: 1` and the final attempt |
| `connector_effect_multistep_coverage_is_total` | process compiler | every side-effecting step has one independent binding/scope/window and read-only steps have none |
| `http_template_slots_require_declared_types` | metadata/server unit | path/query/header/body slots reject undeclared or incompatible input |
| `http_request_headers_preserve_static_yaml_and_add_typed_input` | metadata serde | legacy `{name,value}` round-trips and exactly one static/input value is accepted |
| `process_metadata_requires_start_and_cancellation_shape` | metadata unit | canonical grammar includes start and typed cancellation |
| `process_metadata_state_and_binding_unions_are_closed` | metadata serde | every Section 3 variant round-trips; mixed/unknown/context-invalid forms reject |
| `process_effect_catalog_free_constructor_is_cycle_free` | state unit | server-owned free construction, descriptors, process revisions, effect finalization, and schema compile in the required order without a foreign inherent impl |
| `process_rejects_cross_source_command` | process compiler | start, transition, start effect, and signal effect cannot cross source |
| `process_connector_instance_has_one_source` | process compiler | one connector instance cannot coordinate two process sources |
| `process_deployment_selects_one_real_source` | migrate integration | selected source alone is introspected/reconciled; omitted ambiguous source fails |
| `metadata_free_migrate_preserves_refinery_only_mode` | CLI integration | explicit URL plus no metadata applies migrations without source selection or reconciliation |
| `serve_with_readonly_role_issues_no_ddl` | state/Postgres | startup succeeds without schema-create privilege and SQL capture contains no DDL |
| `process_v6_schema_is_exact` | migrate integration | every Section 7 column/key/index exists and no command-journal FK exists |
| `process_sources_sharing_database_are_isolated` | Postgres integration | identical names, UUIDs, and semantic keys in two source namespaces do not collide |
| `process_live_connector_rebind_is_rejected` | reconcile integration | active/live-retired source-A catalog spec prevents binding that instance to source B |
| `process_retired_revision_reloads_and_is_available` | restart integration | fresh Engine hash-verifies and exposes non-terminal retired A without executing it |
| `process_retired_revision_reloads_and_completes` | restart integration | A starts, explicit retirement rejects new start, fresh Engine loads and completes/verifies A |
| `command_v6_writer_remains_compatible` | command conformance/Postgres | the V6 migration and all existing command writers are green together; first/expired claims populate UUID and replay preserves it |
| `command_invocation_id_replays_unchanged` | SQLgen/Postgres | exact replay preserves UUID |
| `command_invocation_id_changes_after_expiry` | SQLgen/Postgres | expired-key execution gets a new UUID |
| `command_effect_positions_share_generation` | SQLgen/Postgres | multiple positions copy one invocation UUID and remain individually unique |
| `process_start_outbox_row_pins_revision` | command/Postgres | atomic start row written under A retains A after B deployment, without requiring a consumer |
| `process_start_request_pins_revision` | command/process integration | request under A consumed after B deploy starts A |
| `process_start_semantic_dedupe_is_separate` | process integration | distinct invocation UUIDs with one semantic key create one instance |
| `process_session_variables_are_closed` | compiler/runtime | missing/extra/ambient mappings reject; worker uses only compiled values |
| `process_command_later_assert_commits_on_rejection` | Postgres integration | valid write then later assert rolls back domain DML while one rejection transition commits |
| `process_command_guard_prevents_first_write` | Postgres integration | a false guard reaches no domain CTE and follows the declared rejection behavior |
| `process_command_database_error_aborts_outer` | Postgres integration | non-P0D01 error commits no process writes |
| `process_transition_command_executes_finalized_effects_atomically` | command/process integration | transition commands with finalized start and signal effects commit domain DML, generation, and outboxes together; rejection rolls every command write back |
| `process_activity_does_not_hold_tx` | recording connector/Postgres observer | provider stub accepts only after another connection sees committed job and capacity intent |
| `process_provider_logic_is_activity_only` | planner/runtime | command planning/execution cannot invoke a connector; only a committed activity dispatch can |
| `process_lease_takeover_is_safe` | two binaries | read-only takeover is headerless; provider mutation reuses each stable step key; one transition and stale audit |
| `process_provider_step_deadline_precedence_is_exact` | worker/clock | maximum-deadline equality and equality of both deadlines authorize; between-deadline time is timeout; usable-window-plus-one millisecond is `connector_idempotency_window_exhausted` with zero I/O |
| `process_late_takeover_refuses_before_io` | two binaries/clock | final-attempt and `max_attempts: 1` takeover boundaries use the non-renewing deadline and zero provider calls after the permitted grace/window |
| `process_activity_capacity_is_global` | two binaries | max/rate/serialization policies hold in one source |
| `process_capacity_bucket_serializes_two_claimers` | two Postgres connections | a barrier race cannot oversubscribe the last slot/token or serialization key |
| `process_timer_survives_restart` | controlled DB clock | timer fires once without in-memory state |
| `process_inbound_audit_is_split` | webhook integration | accepted plus duplicate create two deliveries, one dedupe row, one transition |
| `process_accepted_delivery_links_instance_history` | webhook/inspect integration | accepted row links source/instance/event and appears in that instance timeline |
| `process_invalid_signature_is_audit_only` | webhook integration | invalid signature creates delivery only and permits absent provider ID |
| `process_raw_webhook_response_matrix_is_unchanged` | webhook integration | unknown/no verifier, oversized, and verification rejection remain exact empty 404/413/400 |
| `process_verified_inbound_ack_matrix_is_exact` | webhook integration | every committed accepted/duplicate/unmatched/ambiguous/guard_false/unexpected_state outcome is empty 204; post-verification DB failure is empty 503 |
| `process_signal_is_not_buffered` | command/webhook integration | early/late signal cannot advance a later wait |
| `process_revision_runtime_abi_is_fenced` | rolling deployment | incompatible worker cannot claim pinned work |
| `process_grammar_has_no_workflow_nodes_or_items` | metadata/runtime | the five state variants are closed; node/item/loop/subworkflow spellings reject |
| `connector_pagination_is_bounded_transport_only` | connector/runtime | pages stay inside one activity budget and cannot create process state, branch, retry, timer, command, or DB write |
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

Both are read-only. Inspect emits a redacted journal timeline and selects
deliveries only by indexed `(source_name, instance_id)`. Verify-history loads
the hash-verified revision from `deployed_process_catalog`, reapplies stored
events and stored command/activity results, and exits non-zero on a
before/after-state-hash mismatch. Neither subcommand invokes a command or
connector, changes a row or lease, accepts arbitrary SQL or a role, or
publishes an HTTP surface. They are not retry, replay, repair, cancellation,
or definition-mutation tools.

## 14. Provenance and no-copy treatment

Temporal, AWS Step Functions, Inngest, Stripe, and Airbyte remain
behavior/test-category references under
`knowledgebase/declarative-saas/reference-porting-register.md`.
This process contract copies no upstream source, fixture, generated artifact,
schema, or large text. Stripe mutation remains inventory-only for this Process
specification unless the connector plan's separate immutable-evidence and
executable-migration gates pass; an independently accepted webhook trigger
grants no operation and no idempotency fact is invented here. No n8n-derived
code, fixture, module, or reference is added. In
particular this design does not port logical/workflow nodes, item or
paired-item semantics, loops, batching-as-workflow, or subworkflows.

A future source-level import requires a separately admitted exact source file,
immutable revision, hash, license/notice, destination, failing-first Donat
test, and reviewer before the imported artifact lands.

## 15. Component ownership

| Area | Required owner | Prohibited shortcut |
| --- | --- | --- |
| normalized value contracts | `donat-value-contract`; `donat-ir` re-exports | second IR/command/process/connector value representation |
| command descriptors/effect catalog | `donat-schema` | schema dependency on server or post-plan raw metadata lookup |
| process compiler/runtime | `donat-server::processes` and one `Engine` snapshot catalog | separate workflow service or mutable global definition |
| connector contracts | existing compiled connector registry | duplicated protocol compiler, dynamic module, workflow node/item model, or connector-owned business branching |
| DDL/reconciliation | V6 plus source-qualified `donat migrate` | serve-time DDL or reconciliation |
| business writes | existing explicit-role command IR/SQLgen path | hand-written process business SQL or admin role |
| process journals | short source-local Postgres transactions | distributed transaction or external I/O under transaction |
| ingress | connector verifier plus split source-local ledger/audit | parse-before-verify, in-memory dedupe, or one-row audit overwrite |
| proof | crate tests, native conformance, two-binary Postgres integration | mocked single-worker proof of durability |
