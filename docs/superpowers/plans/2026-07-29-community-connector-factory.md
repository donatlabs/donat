# Community Connector Factory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a license-gated, deterministic community connector factory
whose reviewed static catalog, bounded transport, and sealed native processors
ship in the single `donat` serving binary.

**Architecture:** A lower `no_std + alloc` value contract feeds a neutral
connector ABI and catalog. Independent acquisition and code-generation tools
validate exact donor records and emit checked-in Rust; the server consumes
that catalog through per-use credential capabilities, one fixed-origin
executor, and a sealed processor lookup. This plan lands process-independent
slices only: no activity journal, retry/lease state, capacity reservation,
poll checkpoint, webhook acknowledgement, or public connector execution
surface is invented.

**Tech Stack:** Rust 2024 workspace, `no_std + alloc` boundary crates, serde,
SHA-2, reqwest/rustls, Axum/Tokio local provider stubs, insta snapshots,
native conformance harness, Postgres conformance service.

## Global Constraints

- The serving product remains one Rust `donat` binary plus Postgres. Separate
  Rust acquisition/codegen binaries are development and CI tools only.
- Production and ordinary Cargo builds load no npm package, Node.js,
  JavaScript, WASM, shared library, package URL, donor source, or dynamic
  plugin.
- No connector exposes `If`, `Switch`, `Merge`, `Code`, `Wait`, loops,
  item/paired-item flow, subworkflows, AI nodes, send-and-wait, UI callbacks,
  or business orchestration. Pagination is bounded transport inside one
  operation.
- Port scope is integration plumbing and typed provider operations only;
  Rules, Commands, and Processes remain the sole owners of logical flow and
  orchestration.
- No GraphQL, REST, MCP, webhook, or administrative route executes an outbound
  connector operation. Direct provider calls in this plan exist only in
  internal Rust tests against Donat-owned local stubs.
- There is no admin role, permission bypass, runtime metadata mutation,
  credential CRUD, arbitrary request URL/method/header, or writable secret
  resolver.
- Commands never plan or invoke connector I/O. Process-owned persistence and
  provider mutation dispatch wait for the exact Spec 005 prerequisites.
- Operation effects are only headerless `ReadOnly` or per-compiled-step
  `ProviderIdempotent`. A side effect lacking immutable binding, scope,
  minimum-retention, and clock-margin evidence remains inventory-only.
- `donat-value-contract`, `donat-connector-abi`, and
  `donat-connector-processors` are unpublished `#![no_std] + alloc` crates
  with empty default features, no `std` feature, unsafe code, build script,
  procedural macro, native dependency, or Phase-1 third-party runtime
  dependency.
- `donat-connector-catalog` imports ABI-owned IDs directly. No string,
  wrapper, parser, serializer, `From`, or `Into` copy may bridge catalog,
  generated entries, `ConnectorIo`, or processor lookup.
- Every ABI ID is a transparent typed wrapper around the same
  const-constructible, `Copy`, 96-byte inline representation. Generated
  statics and host/processor calls pass those exact values by copy.
- Acquisition and codegen are sibling crates over the catalog. Neither is a
  server dependency; codegen never acquires donor bytes.
- Generated Rust and `catalog.digest` are generator-owned. A manifest task
  renders to a temporary directory, reviews the complete diff, updates
  checked-in output through the generator, and runs `generate --check`; no
  task hand-edits generated output or the digest.
- Phase-1 source licenses are exactly `MIT`, `Apache-2.0`, `BSD-2-Clause`,
  `BSD-3-Clause`, `ISC`, or `0BSD`. Every dependency and embedded artifact has
  a closed disposition.
- Every donor version has an approved pre-port record before derivative work.
  Every imported/translated file records its immutable revision, SHA-256,
  license/notice, destination, Donat-owned RED test, and reviewer in
  `knowledgebase/declarative-saas/reference-porting-register.md`.
- n8n SUL built-ins are behavior-only. `n8n-workflow` is always
  `TypeOnlyReplaced`; its bytes never enter parser inputs, manifests,
  generated Rust, fixtures, Cargo metadata, or release artifacts.
- Tests use synthetic archives and Donat-owned provider payloads unless a
  fixture has its own admitted per-file record. Tests never call a live
  provider.
- Review every insta diff with `cargo insta review`; never blanket-accept.
- Before every implementation commit, run the task's focused GREEN commands,
  `cargo fmt --all -- --check`, rebuild `donat`, run
  `cargo test -p donat-conformance --test connectors`, then run
  `cargo test -p donat-conformance`.
- After Task 17, request one independent code review over the complete
  implementation range and resolve its findings before integration. If the
  selected SDD execution workflow requires a fresh reviewer per task, follow
  that workflow instead; do not add another named post-commit gate.

## Authoritative Inputs

- `knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md`
- `specs/007-community-connector-factory.md`
- `specs/005-durable-processes.md` for dependency interfaces only
- `knowledgebase/declarative-saas/reference-porting-register.md`
- `.superpowers/sdd/2026-07-29-n8n-connector-sourcing/research-report.md`
- `.superpowers/sdd/2026-07-29-n8n-connector-sourcing/gap-audit.md`
- `.superpowers/sdd/2026-07-29-n8n-connector-sourcing/processor-boundary-feasibility.md`

## File and Dependency Map

```text
donat-ir                    -> donat-value-contract
donat-connector-abi         -> donat-value-contract
donat-connector-catalog     -> donat-value-contract
donat-connector-catalog     -> donat-connector-abi
donat-connector-acquire     -> donat-connector-catalog
donat-connector-codegen     -> donat-connector-catalog
donat-connector-processors  -> donat-connector-abi
donat-connector-processors  -> donat-value-contract

donat-server -> donat-ir
donat-server -> donat-connector-catalog
donat-server -> donat-connector-abi
donat-server -> donat-connector-processors
```

New files are grouped by responsibility:

```text
crates/value-contract/                  canonical SQL-free types and values
crates/connector-abi/                   exact IDs, envelopes, host traits
crates/connector-catalog/               source records, normalized IR, hashes
crates/connector-acquire/               hostile HTTPS/archive admission tool
crates/connector-codegen/               offline deterministic Rust generator
crates/connector-processors/            sealed no-OS processor implementations
policy/connector-processor-dependencies.toml
scripts/check_connector_processor_boundary.py
crates/server/src/connectors/credentials.rs
crates/server/src/connectors/transport.rs
crates/server/src/connectors/executor.rs
crates/server/src/connectors/catalog.rs
```

External inline-binary JSON, multipart transport, connector/process
descriptor admission, provider continuation URLs, durable polling
checkpoints, and every process journal integration are outside this plan.
The shared value crate owns the inert bounded inline-byte value and exact
canonical-size vectors in the single Spec 005 Task-1 implementation commit;
no external adapter may admit it until the later gates are accepted.

---

### Task 1: Reuse the single Spec 005 value-contract implementation unit

This is a cross-plan ledger alias, not a second implementation task. The
authoritative implementation unit is Task 1 in
`docs/superpowers/plans/2026-07-28-declarative-processes.md`. It creates
`donat-value-contract`, the `donat-ir` re-export, and command descriptors in
one commit. If that commit is already present, do not recreate, reimplement,
or recommit any file here. Record its hash and run the connector-specific
verification below. Task 2 cannot start until this shared commit is green.

Spec 005's canonical Task 1 is authoritative, and it must declare the exact
superset interface below before either plan executes. If it still declares
only `BoundedInlineBytes { bytes }` or `try_new(bytes, maximum_decoded_bytes)`,
stop: the process-plan owner must align that same canonical task first. This
connector plan does not silently redefine the shared unit, append a follow-up
commit, or create a connector-owned variant.

**Files owned by the one shared commit:**

- Create: `crates/value-contract/Cargo.toml`
- Create: `crates/value-contract/src/lib.rs`
- Create: `crates/value-contract/tests/value_contract.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/ir/Cargo.toml`
- Create: `crates/ir/src/value_contract.rs`
- Modify: `crates/ir/src/lib.rs`
- Create: `crates/ir/tests/value_contract_adapter.rs`
- Modify: `crates/schema/Cargo.toml`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/tests/commands.rs`

**Shared interface required by processes and connectors:**

```rust
pub const VALUE_TYPE_LANGUAGE_VERSION: u16 = 1;

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
```

The identifier grammar has no implicit GraphQL-reserved-name rule:
`__bad` is valid because it matches the declared grammar; only a future
explicit metadata rule may reserve a prefix. `CanonicalDecimal` has one
checked constructor and no public tuple field or unchecked constructor. It
accepts only an already minimal fixed-point JSON number: no whitespace,
leading plus, exponent, leading integer zero, negative zero, trailing decimal
point, or trailing fractional zero. Zero is exactly `0`; a non-zero value is
an optional `-`, either a non-zero integer with an optional fraction or `0`
with a required non-zero-ending fraction, and any fraction ends in `1..=9`.
The exact ASCII grammar is
`0|-?(?:[1-9][0-9]*(?:\.[0-9]*[1-9])?|0\.[0-9]*[1-9])`.
Thus `-12.5`, `0.01`, and `10` are valid, while `-12.50e+2`, `-12.50`,
`01`, `-0`, `0.0`, and `1.` are rejected. `canonical_size` counts exactly
`CanonicalDecimal::as_str()` and never accepts arbitrary caller-owned number
spelling.

`BoundedMediaType` accepts at most 255 ASCII bytes. `BoundedFileName` accepts
at most 255 UTF-8 bytes and is data, never a path. Construction rejects a
decoded-byte bound above 131,072. Complete-value validation rejects more than
16 inline values, more than 131,072 aggregate decoded bytes, or more than
262,144 canonical bytes. The lower crate accounts for the exact future
`$binary`/`file_name`/`media_type` JCS representation without exposing a JSON
encoder. Metadata, JSON/form connector descriptors, multipart, commands, and
process journals reject `InlineBytes` until the separate binary gate lands.

- [ ] **Step 1: Check whether the shared unit already has a commit**

```bash
shared_value_commit=$(git log -1 --format=%H -- crates/value-contract)
if test -n "$shared_value_commit"; then
  git show --stat --oneline "$shared_value_commit"
fi
```

If the crate is absent, execute the canonical Spec 005 Task 1. If it exists,
inspect that commit rather than creating another owner. In either case,
compare the authoritative task text and implementation with the exact
`CanonicalDecimal` representation, checked constructor, `as_str`, grammar,
examples, canonical-size rule, `__bad` identifier behavior,
`BoundedMediaType`, `BoundedFileName`, four-argument constructor, accessors,
limits, tests, and vectors above. Stop on any mismatch; do not repair it in a
second connector commit.

- [ ] **Step 2: Put connector prerequisites in the same shared RED test**

Before the canonical Task-1 commit, add these exact tests to
`crates/value-contract/tests/value_contract.rs`:

- `value_type_language_is_closed_and_canonical`;
- `value_contract_has_one_owner`;
- `value_type_identifier_grammar_has_no_implicit_reserved_prefix`;
- `canonical_decimal_spelling_is_exact`;
- `inline_bytes_have_one_inert_owner`;
- `inline_binary_canonical_size_vectors_are_exact`;
- `inline_binary_count_and_decoded_bounds_are_exact`;
- `inline_binary_external_adapters_remain_disabled`.

Assert that `__bad` parses because the declared identifier grammar has no
reserved-prefix exception. Construct decimals only through
`CanonicalDecimal::try_new`; accept `-12.5`, `0.01`, and `10`, and reject
`-12.50e+2`, `-12.50`, `01`, `-0`, `0.0`, `1.`, whitespace, plus, and
non-finite spellings. Prove that the private canonical spelling returned by
`as_str` is exactly what `canonical_size` counts.

The independent size helper must assert:

```text
131,072 zero bytes, application/octet-stream, no filename -> 174,817 bytes
131,073 decoded bytes                              -> rejected before encoding
accepted binary + 87,303-byte "padding" string     -> 262,144 bytes
accepted binary + 87,304-byte "padding" string     -> 262,145 and rejected
17 inline-byte values                              -> rejected
```

- [ ] **Step 3: Run the shared RED commands**

```bash
cargo test -p donat-value-contract canonical_decimal_spelling_is_exact
cargo test -p donat-value-contract \
  value_type_identifier_grammar_has_no_implicit_reserved_prefix
cargo test -p donat-value-contract inline_binary
cargo test -p donat-ir --test value_contract_adapter
cargo check -p donat-value-contract --no-default-features \
  --target thumbv7em-none-eabi
```

Expected when the shared unit is absent: Cargo reports that
`donat-value-contract` does not exist. Expected when an incomplete shared
implementation exists: the exact decimal spelling, identifier grammar,
inline-byte ownership, size, count, or adapter-gate assertion fails.

- [ ] **Step 4: Implement only through canonical Spec 005 Task 1**

Follow the canonical task's complete value, IR, schema, and command-descriptor
steps after its text contains the exact superset interface above. Implement
`CanonicalDecimal` with its private checked spelling in that same lower crate;
do not expose `Decimal(String)` or normalize an ambiguous caller spelling
implicitly. Keep `__bad` valid under the declared identifier grammar.
Implement inert bytes and checked size accounting in the same shared unit and
commit. Do not add a connector-local value type, serde adapter, multipart
encoder, process admission, or a second value-contract commit.

- [ ] **Step 5: Run shared GREEN and connector-specific verification**

```bash
cargo test -p donat-value-contract canonical_decimal_spelling_is_exact
cargo test -p donat-value-contract \
  value_type_identifier_grammar_has_no_implicit_reserved_prefix
cargo test -p donat-value-contract
cargo test -p donat-ir
cargo test -p donat-schema --test commands
cargo check -p donat-value-contract --no-default-features \
  --target thumbv7em-none-eabi
cargo tree -p donat-value-contract --target all \
  --edges normal,build --no-default-features --offline --locked
```

Expected: the exact decimal spelling and identifier grammar tests pass, the
inline-byte vectors pass, `donat-ir` re-exports `CanonicalDecimal` and the
other exact types, and the dependency closure contains only the local value
crate.

- [ ] **Step 6: Record the one shared commit; do not create another**

If the canonical task was executed now, use its one prescribed commit:

```bash
git add Cargo.toml Cargo.lock crates/value-contract crates/ir \
  crates/schema/Cargo.toml crates/schema/src crates/schema/tests
git commit -m "feat(processes): publish closed command contracts"
```

Then record and verify the same hash in the implementation task/PR notes:

```bash
shared_value_commit=$(git log -1 --format=%H -- crates/value-contract)
test -n "$shared_value_commit"
git show --name-only --format=fuller "$shared_value_commit"
```

Task 2 consumes this exact commit. There is no connector Task-1 commit.


---

### Task 2: Add ABI-owned IDs, bounded envelopes, and host traits

**Files:**

- Create: `crates/connector-abi/Cargo.toml`
- Create: `crates/connector-abi/src/lib.rs`
- Create: `crates/connector-abi/src/ids.rs`
- Create: `crates/connector-abi/src/envelope.rs`
- Create: `crates/connector-abi/src/host.rs`
- Create: `crates/connector-abi/tests/abi_contract.rs`
- Create: `scripts/check_connector_processor_boundary.py`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Starts only after the single shared Task-1 commit is recorded and green.
- Consumes: `donat_value_contract::TypedValue`.
- Produces one exact const/static-safe storage representation and transparent
  typed wrappers:

```rust
pub const ABI_ID_CAPACITY: usize = 96;

#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct InlineId {
    len: u8,
    bytes: [u8; ABI_ID_CAPACITY],
}

impl InlineId {
    pub const fn literal(value: &'static str) -> Self;
    pub fn parse(value: &str) -> Result<Self, AbiError>;
    pub fn as_str(&self) -> &str;
}

#[repr(transparent)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct ConnectorId(InlineId);

// The same transparent shape is used for:
// OperationId, CompiledStepId, ProcessorFamilyId, AuthenticatorId, CodecId,
// NormalizerId, TriggerId, CredentialSpecId, CredentialFieldId, CapabilityId,
// BindingSlotId, and OriginId.

impl ConnectorId {
    pub const fn literal(value: &'static str) -> Self;
    pub fn parse(value: &str) -> Result<Self, AbiError>;
    pub fn as_str(&self) -> &str;
}

pub struct NonEmptyVec<T> {
    head: T,
    tail: Vec<T>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ProcessorControl: Send + Sync {
    fn check(&self) -> Result<(), ConnectorFailure>;
}

pub trait ConnectorIo: Send + Sync {
    fn call<'a>(
        &'a self,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>>;
}
```

The single validator accepts `1..=96` ASCII bytes matching
`[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?`. Both `literal` and `parse` call that
validator; the const path panics at compile time for an invalid literal and
the runtime path returns `AbiError`. Neither path allocates. `InlineId` is 97
bytes with alignment 1; every wrapper has the same size/alignment and is
`Copy`. Generated entries use `ConnectorId::literal("serpapi")`; a static
step uses `CompiledStepId::literal("search")`. `ConnectorIo::call` and private
processor lookup continue taking the typed ID by value because the value is
copied directly from the static.

The same crate owns checked constructors for bounded safe strings/bytes/maps
and these envelopes:

```rust
pub struct TypedBindings {
    slots: BTreeMap<BindingSlotId, TypedValue>,
}

#[repr(transparent)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticErrorCode(InlineId);

#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticSafeMessage {
    len: u16,
    bytes: [u8; MAXIMUM_SAFE_MESSAGE_BYTES],
}

pub struct AuthorizedCorrelations {
    values: BTreeMap<CapabilityId, BoundedString>,
}

pub struct BoundedTransportResponse {
    status: u16,
    selected_headers: BTreeMap<CapabilityId, BoundedString>,
    decoded: TypedValue,
    response_bytes: u32,
    authorized_correlations: AuthorizedCorrelations,
}

impl BoundedTransportResponse {
    pub fn try_new(
        status: u16,
        selected_headers: BTreeMap<CapabilityId, BoundedString>,
        decoded: TypedValue,
        response_bytes: u32,
    ) -> Result<Self, AbiError>;

    pub const fn status(&self) -> u16;
    pub fn selected_headers(
        &self,
    ) -> &BTreeMap<CapabilityId, BoundedString>;
    pub fn decoded(&self) -> &TypedValue;
    pub const fn response_bytes(&self) -> u32;
    pub fn authorized_correlations(&self) -> &AuthorizedCorrelations;
}

struct StaticFailureText {
    code: StaticErrorCode,
    safe_message: StaticSafeMessage,
}

pub struct ConnectorFailure {
    class: ConnectorErrorClass,
    static_text: Box<StaticFailureText>,
    retry_after_seconds: Option<u32>,
    correlation_ids: BTreeMap<CapabilityId, BoundedString>,
}

impl ConnectorFailure {
    pub fn try_new(
        class: ConnectorErrorClass,
        code: StaticErrorCode,
        safe_message: StaticSafeMessage,
        retry_after_seconds: Option<u64>,
        correlations: Option<&AuthorizedCorrelations>,
    ) -> Result<Self, AbiError>;

    pub const fn class(&self) -> ConnectorErrorClass;
    pub fn code(&self) -> &str;
    pub fn safe_message(&self) -> &str;
    pub const fn retry_after_seconds(&self) -> Option<u32>;
    pub fn correlation_ids(
        &self,
    ) -> &BTreeMap<CapabilityId, BoundedString>;
}

pub struct ProcessorContext<'a> {
    pub connector: &'a ConnectorId,
    pub operation: &'a OperationId,
    pub logical_activity_id: &'a BoundedString,
    pub idempotency_identity: &'a BoundedString,
    pub request_fingerprint: &'a Hash256,
    pub capabilities: &'a [CapabilityId],
    pub control: &'a dyn ProcessorControl,
}

pub enum ConnectorErrorClass {
    Transport,
    Timeout,
    Http429,
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
}
```

`ConnectorFailure` contains one class, private boxed Donat-owned static text,
optional clamped retry delay, and host-authorized bounded correlation IDs. It
has no raw body, URL, header, credential, provider message, process policy, or
unbounded collection. `use alloc::boxed::Box;` keeps the ABI `no_std + alloc`.
`ConnectorFailure::try_new` validates every non-allocation input, then makes
exactly one `Box::new(StaticFailureText { code, safe_message })` allocation;
its text accessors dereference that private box. It requires strict
`clippy::result_large_err` evidence without an allow, expectation, crate lint
override, or command-line lint suppression.

`status` accepts all `u16` values, including `0` and `u16::MAX`. Ordinary
response construction produces empty authorization, and `TypedBindings`
shares `ValueCounters` across every root while depth restarts at zero per root.
The validation order is: reject more than 64 slots; traverse roots with shared
node, inline-value, and decoded-byte counters; sum `canonical_size` with
checked arithmetic; then construct `TypedBindings`.

The ABI exports these restricted construction namespaces:

```rust
#[doc(hidden)]
pub mod catalog_construction {
    pub fn static_error_code(
        value: &str,
    ) -> Result<StaticErrorCode, AbiError>;

    pub fn static_safe_message(
        value: &str,
    ) -> Result<StaticSafeMessage, AbiError>;
}

#[doc(hidden)]
pub mod host_construction {
    pub fn transport_response(
        status: u16,
        selected_headers: BTreeMap<CapabilityId, BoundedString>,
        decoded: TypedValue,
        response_bytes: u32,
        allowed_correlations: &[CapabilityId],
    ) -> Result<BoundedTransportResponse, AbiError>;

    pub fn authorized_correlations(
        selected_headers: &BTreeMap<CapabilityId, BoundedString>,
        allowed_correlations: &[CapabilityId],
    ) -> Result<AuthorizedCorrelations, AbiError>;
}
```

Task 2 creates `scripts/check_connector_processor_boundary.py` with
deterministic producer/test-path fixtures. Task 3 imports the two static types,
calls `catalog_construction` only after strict normalized validation, derives
each correlation header to exactly one `CapabilityId`, rejects missing,
multiple, duplicate, or more-than-64 correlation capabilities, and includes
static text plus derived capabilities in semantic hashing.

No ID contains `String`, `Vec`, `Box`, `&'static str`, or a public tuple
field. There is no blanket `From`/`Into`, clone-to-call, runtime reparsing of
generated IDs, serialization round-trip, lazy static, or startup ID table.
`ConnectorFailure` uses the existing eight error classes and bounded safe
fields.

- [ ] **Step 1: Write failing ID, bound, and object-safety tests**

```rust
#[test]
fn abi_ids_are_canonical_and_bounded() {
    assert!(ConnectorId::parse("serpapi").is_ok());
    assert!(ConnectorId::parse("").is_err());
    assert!(ConnectorId::parse("Serp API").is_err());
    assert!(ConnectorId::parse(&"a".repeat(96)).is_ok());
    assert!(ConnectorId::parse(&"a".repeat(97)).is_err());
}

const SERPAPI: ConnectorId = ConnectorId::literal("serpapi");
static STEPS: [CompiledStepId; 1] = [CompiledStepId::literal("search")];

#[test]
fn abi_ids_are_const_constructible_and_copy_from_statics() {
    fn takes_step(_: CompiledStepId) {}
    let step = STEPS[0];
    takes_step(step);
    takes_step(step);
    assert_eq!(SERPAPI.as_str(), "serpapi");
    assert_eq!(core::mem::size_of::<InlineId>(), 97);
}

#[test]
fn host_traits_are_object_safe_send_and_sync() {
    fn connector_io(_: &dyn ConnectorIo) {}
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn ConnectorIo>();
    let _ = connector_io;
}
```

Also reject leading/trailing separators, punctuation outside the grammar
(including adjacent invalid characters), non-ASCII, uppercase, embedded NUL,
and all boundary lengths. Add compile assignments proving every typed wrapper
is `Copy`, const-constructible, and the exact type consumed by generated
descriptors, `ConnectorIo`, and private lookup without `.clone()`, `.parse()`,
`String`, or wrapper conversion. Add boundary tests for oversized safe
strings, headers, binding maps, transport bytes, retry delay, nesting, and
canonical output size. Add four independent external compile-fail field
assignments for `status`, `selected_headers`, `decoded`, and `response_bytes`,
plus compile-fail response/correlation privacy and runtime conversion checks.
Exercise `0..=u16::MAX`, response/correlation exact boundaries, static-text
exact boundaries, aggregate multi-root node/inline/decoded-byte accounting,
and deterministic checker mutations.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-abi --no-default-features
```

Expected: Cargo reports that package `donat-connector-abi` does not exist.

- [ ] **Step 3: Implement the neutral ABI**

Begin `lib.rs` with the same `no_std`, `forbid(unsafe_code)`, and `alloc`
directives as the shared value crate. Implement the typed wrappers with one
private macro over `InlineId`; the macro must not create a second storage
shape or validator. Keep URL, reqwest, serde JSON, Tokio, database,
filesystem, environment, role, process, retry-policy, and raw credential types
out of every public field and signature. Keep all response/failure fields
private and immutable; no raw constructor or compatibility shim survives.
`catalog_construction` is restricted to `crates/connector-catalog/src/` and
`host_construction` to `crates/server/src/connectors/`; initial checker
fixtures prove their producer and test-path allowlists.

- [ ] **Step 4: Run no-OS and closure checks**

```bash
cargo check -p donat-connector-abi --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-abi --target all \
  --edges normal,build --no-default-features --offline --locked
```

Expected: only `donat-connector-abi` and the local value-contract crate appear.

- [ ] **Step 5: Run GREEN**

```bash
cargo test -p donat-connector-abi --no-default-features
cargo test -p donat-value-contract --no-default-features
cargo clippy -p donat-connector-abi --no-default-features -- \
  -D warnings -D clippy::result_large_err
python3 scripts/check_connector_processor_boundary.py
if rg -n 'String|Box<str>|Vec<u8>|OnceLock|LazyLock|\\.parse\\(' \
  crates/connector-abi/src/ids.rs; then
  exit 1
fi
lint_policy_inputs=(
  crates/connector-abi/src
  crates/connector-abi/Cargo.toml
  Cargo.toml
  .github/workflows
)
for cargo_config in .cargo/config.toml .cargo/config; do
  if test -f "$cargo_config"; then
    lint_policy_inputs+=("$cargo_config")
  fi
done
if rg -n \
  -e '(?:allow|expect|warn|force_warn)\s*\([^)]*clippy::result_large_err' \
  -e 'result-large-err\s*=\s*(?:\{\s*level\s*=\s*)?"(?:allow|warn)"' \
  -e '(?:-A|-W|--allow|--warn|--force-warn)(?:=|\s*)clippy::result[_-]large[_-]err' \
  -e '--cap-lints(?:=|\s+)(?:allow|warn)' \
  "${lint_policy_inputs[@]}"; then
  exit 1
fi
cargo fmt --all -- --check
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors
cargo test -p donat-conformance
```

- [ ] **Step 6: Commit the ABI foundation**

```bash
git add Cargo.toml Cargo.lock crates/connector-abi \
  scripts/check_connector_processor_boundary.py .github/workflows/ci.yml
git commit -m "fix(connectors): enforce safe connector ABI"
```


---

### Task 3: Create the strict normalized catalog and source-record model

**Files:**

- Create: `crates/connector-catalog/Cargo.toml`
- Create: `crates/connector-catalog/src/lib.rs`
- Create: `crates/connector-catalog/src/source.rs`
- Create: `crates/connector-catalog/src/model.rs`
- Create: `crates/connector-catalog/src/canonical.rs`
- Create: `crates/connector-catalog/tests/source_record.rs`
- Create: `crates/connector-catalog/tests/operation_spec.rs`
- Create: `crates/connector-catalog/tests/catalog_contracts.rs`
- Create: `crates/connector-catalog/tests/contract_facts.rs`
- Create: `crates/connector-catalog/tests/canonical_hashes.rs`
- Create: `crates/connector-catalog/tests/type_identity.rs`
- Create: `crates/connector-catalog/tests/fixtures/missing-license-file-hash.yaml`
- Create: `crates/connector-catalog/tests/fixtures/missing-side-effect-step.yaml`
- Create: `crates/connector-catalog/tests/fixtures/unknown-effect.yaml`
- Create: `crates/connector-catalog/tests/fixtures/unknown-auth-plan.yaml`
- Create: `crates/connector-catalog/tests/fixtures/incomplete-error-map.yaml`
- Create: `crates/connector-catalog/tests/fixtures/unbounded-trigger.yaml`
- Create: `crates/connector-catalog/tests/fixtures/policy-as-provider-fact.yaml`
- Create: `crates/connector-catalog/tests/fixtures/serpapi-npm-record.yaml`
- Create: `crates/connector-catalog/tests/fixtures/provider-contract-record.yaml`
- Create: `crates/connector-catalog/tests/fixtures/donat-owned-record.yaml`
- Create: `crates/connector-catalog/tests/fixtures/npm-repository-mismatch.yaml`
- Create: `crates/connector-catalog/tests/fixtures/npm-provenance-mismatch.yaml`
- Create: `crates/connector-catalog/tests/fixtures/open-dependency-disposition.yaml`
- Create: `crates/connector-catalog/sources/records/donat-owned-http-v1.yaml`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: all Task-2 ABI IDs and Task-1 value contracts.
- Produces strict `#[serde(deny_unknown_fields)]` source and normalized types:

```rust
pub struct ConnectorSourceRecord {
    pub record_version: u32,
    pub record_id: SourceRecordId,
    pub subject: SourceSubject,
    pub reacquisition: ReacquisitionPlan,
    pub artifact_hashes: Vec<ArtifactHash>,
    pub license: LicenseDecision,
    pub notice: NoticeIdentity,
    pub entrypoints: Vec<SourcePath>,
    pub dependencies: Vec<DependencyDecision>,
    pub embedded_material: Vec<EmbeddedMaterialDecision>,
    pub provider_contracts: Vec<ProviderContractReference>,
    pub compatibility: CompatibilityDecision,
    pub admission: AdmissionState,
    pub safety_findings: SafetyFindings,
    pub reviewer: ReviewIdentity,
    pub approval_date: Date,
    pub proposed_manifest: Option<RepoPath>,
    pub proposed_destinations: NonEmptyVec<RepoPath>,
    pub red_tests: NonEmptyVec<TestId>,
}

#[repr(transparent)]
pub struct SourceRecordId(InlineId);
#[repr(transparent)]
pub struct ProviderContractId(InlineId);
#[repr(transparent)]
pub struct ProviderFactId(InlineId);
#[repr(transparent)]
pub struct DonatPolicyId(InlineId);
#[repr(transparent)]
pub struct NoticeId(InlineId);

pub enum SourceSubject {
    ExactNpm(ExactNpmPackage),
    ProviderArtifact(ExactProviderArtifact),
    DonatOwned(DonatOwnedSource),
}

pub enum ReacquisitionPlan {
    ExactNpmReview,
    ProviderRepositoryReview,
    ProviderVersionedArtifactReview,
    DonatOwnedNoNetwork,
}

pub struct ExactNpmPackage {
    pub name: String,
    pub version: ExactSemver,
    pub tarball_url: ExactHttpsUrl,
    pub integrity: NpmIntegrity,
    pub repository: ImmutableRepository,
    pub npm_git_head: GitCommit,
    pub package_repository: RepositoryUrl,
    pub signature: NpmSignatureDecision,
    pub provenance: NpmProvenanceDecision,
    pub tag_commit: Option<GitCommit>,
    pub provenance_commit: Option<GitCommit>,
    pub maintainers: Vec<NpmMaintainerIdentity>,
    pub repository_owner: RepositoryOwnerDecision,
}

pub struct NpmIntegrity {
    pub algorithm: Sha512,
    pub digest: [u8; 64],
}

pub struct ImmutableRepository {
    pub url: RepositoryUrl,
    pub commit: GitCommit,
    pub tree: GitTree,
}

pub enum NpmSignatureDecision {
    Verified {
        signatures: NonEmptyVec<VerifiedNpmSignature>,
        registry_metadata_sha256: Hash256,
    },
    VerifiedAbsent {
        registry_metadata_sha256: Hash256,
    },
    Rejected {
        finding: FindingId,
    },
}

pub enum NpmProvenanceDecision {
    Verified {
        statement_sha256: Hash256,
        source_commit: GitCommit,
    },
    VerifiedAbsent {
        registry_metadata_sha256: Hash256,
    },
    Rejected {
        finding: FindingId,
    },
}

pub enum RepositoryOwnerDecision {
    Consistent {
        package_owner: NpmOwnerIdentity,
        repository_owner: RepositoryOwnerIdentity,
    },
    ReviewedMismatch {
        decision_id: ReviewDecisionId,
    },
    Rejected {
        finding: FindingId,
    },
}

pub struct ExactProviderArtifact {
    pub provider: String,
    pub evidence: NonEmptyVec<ProviderEvidenceArtifact>,
}

pub struct ProviderEvidenceArtifact {
    pub source: ImmutableProviderEvidenceSource,
    pub accessed_on: Date,
    pub content_sha256: Hash256,
    pub terms: EvidenceTermsDisposition,
    pub facts: NonEmptyVec<ProviderFact>,
}

pub enum ImmutableProviderEvidenceSource {
    RepositoryFile {
        repository: RepositoryUrl,
        commit: GitCommit,
        path: SourcePath,
    },
    VersionedArtifact {
        url: ExactHttpsUrl,
        provider_revision: NonEmptyString,
    },
}

pub struct ProviderFact {
    pub fact_id: ProviderFactId,
    pub location: ExactFactLocation,
    pub normalized_value: CanonicalProviderValue,
}

pub enum ContractFact {
    ProviderEvidence {
        source_record_id: SourceRecordId,
        fact_id: ProviderFactId,
    },
    DonatPolicy {
        policy_id: DonatPolicyId,
        value: TypedValue,
    },
}

pub struct ProviderContractReference {
    pub contract_id: ProviderContractId,
    pub facts: NonEmptyVec<ContractFact>,
}

pub struct DonatOwnedSource {
    pub repository_commit: GitCommit,
    pub files: NonEmptyVec<RepoFileHash>,
}

pub enum CompatibilityDecision {
    TierA,
    TierB,
    TierC,
    Rejected,
}

pub enum AdmissionState {
    InventoryOnly {
        findings: NonEmptyVec<FindingId>,
    },
    ApprovedForPort {
        operations: NonEmptyVec<OperationId>,
    },
    EvidenceAccepted {
        contracts: NonEmptyVec<ProviderContractId>,
    },
}

pub struct ConnectorManifest {
    pub connector: ConnectorId,
    pub version: u32,
    pub credentials: Vec<CredentialSpec>,
    pub origins: NonEmptyVec<FixedOrigin>,
    pub operations: NonEmptyVec<OperationSpec>,
    pub triggers: Vec<TriggerSpec>,
    pub provenance: NonEmptyVec<ManifestProvenanceReference>,
}

pub struct CredentialSpec {
    pub credential: CredentialSpecId,
    pub version: u32,
    pub fields: NonEmptyVec<CredentialFieldSpec>,
    pub auth_plan: AuthPlan,
    pub allowed_origins: NonEmptyVec<OriginId>,
    pub scopes: Vec<StaticScope>,
    pub auth_processor: Option<AuthenticatorId>,
    pub credential_test_operation: Option<OperationId>,
    pub bounds: CredentialBounds,
}

pub struct CredentialFieldSpec {
    pub field: CredentialFieldId,
    pub required: bool,
    pub secret: SecretClassification,
    pub maximum_bytes: NonZeroU32,
    pub redaction: RedactionPlan,
}

pub struct CredentialBounds {
    pub maximum_field_bytes: NonZeroU32,
    pub maximum_aggregate_bytes: NonZeroU32,
    pub maximum_token_bytes: NonZeroU32,
}

pub enum AuthPlan {
    FixedHeaderApiKey {
        field: CredentialFieldId,
        header: StaticHeaderName,
    },
    FixedQueryApiKey {
        field: CredentialFieldId,
        query: StaticQueryKey,
    },
    Bearer {
        token: CredentialFieldId,
    },
    HttpBasic {
        username: CredentialFieldId,
        password: CredentialFieldId,
    },
    OAuth2ClientCredentials {
        client_id: CredentialFieldId,
        client_secret: CredentialFieldId,
        token_origin: OriginId,
        token_step: CompiledStepId,
        scopes: Vec<StaticScope>,
        token_pointer: StaticJsonPointer,
    },
    PreprovisionedOAuthAccessToken {
        token: CredentialFieldId,
    },
}

pub struct FixedOrigin {
    pub origin: OriginId,
    pub scheme: HttpsOnly,
    pub host: StaticDnsName,
    pub port: NonZeroU16,
    pub network_policy: NetworkPolicy,
}

pub struct CompiledStepSpec {
    pub step: CompiledStepId,
    pub method: StaticHttpMethod,
    pub origin: OriginId,
    pub path: StaticPathTemplate,
    pub query: Vec<CompiledQueryBinding>,
    pub headers: Vec<CompiledHeaderBinding>,
    pub credential_action: Option<CompiledCredentialAction>,
    pub request: CompiledRequestShape,
    pub success_statuses: NonEmptyVec<StatusRange>,
    pub response: CompiledResponseShape,
    pub bounds: StepBounds,
}

pub struct StepBounds {
    pub maximum_headers: NonZeroU32,
    pub maximum_header_bytes: NonZeroU32,
    pub maximum_url_bytes: NonZeroU32,
    pub maximum_request_bytes: NonZeroU32,
    pub maximum_response_bytes: NonZeroU32,
    pub maximum_json_depth: NonZeroU32,
    pub maximum_json_nodes: NonZeroU32,
    pub maximum_inline_binary_bytes: NonZeroU32,
    pub deadline_ms: NonZeroU64,
}

pub struct OperationBounds {
    pub maximum_calls: NonZeroU32,
    pub maximum_pages: NonZeroU32,
    pub maximum_items: NonZeroU32,
    pub maximum_aggregate_request_bytes: NonZeroU32,
    pub maximum_aggregate_response_bytes: NonZeroU32,
    pub maximum_output_canonical_bytes: NonZeroU32,
    pub maximum_redirects: u8,
    pub deadline_ms: NonZeroU64,
}

pub struct ErrorMap {
    pub rules: Vec<ErrorRule>,
    pub fallback: CompleteErrorFallback,
}

pub struct ErrorRule {
    pub matcher: ErrorMatcher,
    pub action: ErrorAction,
}

pub struct ErrorAction {
    pub class: ConnectorErrorClass,
    pub code: StaticErrorCode,
    pub safe_message: StaticSafeMessage,
    pub retry_after: RetryAfterPolicy,
    pub correlation_headers: Vec<StaticHeaderName>,
}

pub struct CompleteErrorFallback {
    pub transport: ErrorAction,
    pub timeout: ErrorAction,
    pub http_429: ErrorAction,
    pub http_5xx: ErrorAction,
    pub authentication: ErrorAction,
    pub validation: ErrorAction,
    pub permanent: ErrorAction,
    pub invariant: ErrorAction,
}

pub enum ErrorMatcher {
    Status(StatusRange),
    ProviderCode {
        pointer: StaticJsonPointer,
        codes: NonEmptyVec<StaticProviderCode>,
    },
    Header {
        name: StaticHeaderName,
        values: NonEmptyVec<StaticHeaderValue>,
    },
    MalformedDeclaredSuccess,
}

pub enum TriggerSpec {
    Webhook {
        trigger: TriggerId,
        authenticator: AuthenticatorId,
        codec: CodecId,
        normalizer: NormalizerId,
        selected_headers: Vec<StaticHeaderName>,
        raw_body_max_bytes: NonZeroU32,
        timestamp_window_ms: NonZeroU64,
        event_id: ValueContractCatalog,
        event_type: ValueContractCatalog,
        output: ValueContractCatalog,
        redaction: RedactionPlan,
        subscription_operations: Option<SubscriptionOperationIds>,
    },
    Poll {
        trigger: TriggerId,
        checkpoint: ValueContractCatalog,
        processor: ProcessorFamilyId,
        event_type: ValueContractCatalog,
        per_poll_event_limit: NonZeroU32,
        bounds: OperationBounds,
    },
}

pub struct ManifestProvenanceReference {
    pub source_record_id: SourceRecordId,
    pub artifact_hashes: NonEmptyVec<ArtifactHash>,
    pub license_id: LicenseIdentity,
    pub notice_id: NoticeId,
    pub contract_facts: Vec<ContractFact>,
}

pub struct OperationSpec {
    pub connector: ConnectorId,
    pub operation: OperationId,
    pub version: u32,
    pub credential: Option<CredentialSpecId>,
    pub steps: NonEmptyVec<CompiledStepSpec>,
    pub effect: OperationEffect,
    pub input: ValueContractCatalog,
    pub output: ValueContractCatalog,
    pub pagination: PaginationPlan,
    pub error_map: ErrorMap,
    pub bounds: OperationBounds,
    pub provenance: NonEmptyVec<ManifestProvenanceReference>,
}

pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: NonEmptyVec<ProviderIdempotentStep>,
    },
}
```

`LicenseDecision` accepts only the six Phase-1 identifiers and one selected
allowed branch for dual licensing. `DependencyDisposition` is exactly
`Shipped`, `BuildOnly`, `TypeOnlyReplaced`, `BehaviorOnly`, or `Rejected`.
`NoticeIdentity` contains a stable notice ID, license-file path/hash, required
copyright lines, and the planned notice-bundle destination. Exact npm loading
decodes one canonical `sha512-...` SRI value into the structured 64-byte
digest and requires `npm_git_head == repository.commit`, exact package
name/version, tarball URL, package repository mapping, and tree identity.
The exact npm variant also retains a closed verified/present,
verified-absent, or rejected decision for registry signatures and signed
provenance; optional distinct tag/provenance commits; the reviewed maintainer
set; and a closed repository-owner consistency decision. A verified
provenance commit must match `provenance_commit`, and neither a tag nor
provenance commit may be silently substituted for `npm_git_head`.
Every dependency and embedded artifact has one explicit closed disposition.
Every proposed manifest/destination is a normalized repository-relative path.
`ConnectorManifest`, every nested credential/auth, fixed-origin/step,
operation/error/pagination/bounds, trigger, and provenance type denies unknown
fields. Credential and trigger bounds are finite and no auth, error, or
trigger variant accepts an arbitrary method, URL, header name, expression,
provider message, secret, or processor path.

- [ ] **Step 1: Write failing strictness, effect, and hash-vector tests**

```rust
#[test]
fn source_record_requires_exact_artifacts() {
    let error = load_record("tests/fixtures/missing-license-file-hash.yaml")
        .expect_err("an incomplete record fails closed");
    assert_eq!(error.code(), "source_record_incomplete");
}

#[test]
fn operation_effect_is_closed() {
    assert!(compile(read_only_fixture()).is_ok());
    assert!(compile(missing_side_effect_step_fixture()).is_err());
    assert!(compile(unknown_effect_fixture()).is_err());
}

#[test]
fn serpapi_npm_record_round_trips_without_information_loss() {
    let record = load_record("tests/fixtures/serpapi-npm-record.yaml").unwrap();
    let encoded = canonical_yaml(&record).unwrap();
    assert_eq!(load_record_bytes(&encoded).unwrap(), record);
    assert_eq!(record.compatibility, CompatibilityDecision::TierA);
    assert!(matches!(record.admission, AdmissionState::InventoryOnly { .. }));
}
```

The SerpAPI-shaped fixture asserts the exact `NpmSignatureDecision`,
`NpmProvenanceDecision::VerifiedAbsent`, optional tag/provenance commits,
maintainer set, and repository-owner decision. Add negative assertions for a
signature/provenance state that disagrees with the recorded registry metadata,
a provenance commit that differs from its signed statement, an unexplained
tag commit, a changed maintainer set, and an unreviewed owner mismatch.

Independently construct and assert all four Spec 007 Section 5.1
domain-separated SHA-256 vectors:

```text
semantic {}                         799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19
provenance {}                       a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4
semantic {"a":1,"b":[true,null,"x"]} 2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34
provenance {"a":1,"b":[true,null,"x"]} 4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23
```

Add `source_record_variants_are_closed`,
`npm_integrity_and_repository_mapping_are_exact`,
`npm_signature_provenance_tag_maintainer_and_owner_state_is_exact`,
`reacquisition_plan_matches_source_subject`,
`provider_contract_reference_requires_matching_record_and_facts`,
`contract_fact_origins_are_closed_and_non_substitutable`,
`donat_policy_cannot_satisfy_required_provider_evidence`,
`contract_fact_semantic_and_provenance_hashes_are_separate`,
`provider_evidence_acceptance_is_closed_and_non_executable`,
`dependency_and_embedded_dispositions_are_closed`,
`notice_and_destination_fields_are_required`, and
`catalog_descriptor_ids_match_connector_io`. In
`catalog_contracts.rs`, add
`credential_auth_plan_is_closed_and_bounded`,
`fixed_origin_step_and_operation_bounds_are_required`,
`error_map_is_complete_closed_and_redacted`,
`webhook_and_poll_trigger_specs_are_closed_and_bounded`, and
`manifest_provenance_references_match_exact_records`. Round-trip one complete
manifest containing a credential, fixed origin, compiled step, error map,
operation, webhook trigger, poll trigger, bounds, and provenance references;
reject every unknown enum tag, unknown nested field, unbounded declaration,
missing reference, dynamic destination, and raw provider message. The final
identity test assigns normalized descriptor IDs directly to `ConnectorIo`
parameters.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-catalog
```

Expected: Cargo reports that package `donat-connector-catalog` does not exist.

- [ ] **Step 3: Implement strict loading and canonical material**

Use `BTreeMap`, reject duplicate keys and unknown fields before
canonicalization, and keep semantic/provenance material separate:

```rust
pub fn semantic_sha256(value: &SemanticMaterial) -> Result<Hash256, CatalogError>;
pub fn provenance_sha256(
    value: &ProvenanceMaterial,
) -> Result<Hash256, CatalogError>;
```

Validate that every side-effecting step has exactly one evidence-backed entry
and that read-only steps have none. Validate all three source variants before
canonicalization. A provider artifact has no npm fields; a Donat-owned record
has no external package claim; an npm record cannot omit or flatten SRI and
repository, signature, provenance, tag, maintainer, or owner mapping. A
`ReacquisitionPlan` must match its exact source-subject variant, and
`DonatOwnedNoNetwork` can never reach a network command. A
mutable provider URL plus access date/hash is not an immutable source identity
and is rejected. Every provider fact names its exact location and normalized
value. Every `ContractFact::ProviderEvidence` resolves to a distinct matching
`ProviderArtifact` record, normalized contract, and complete fact set; every
`ContractFact::DonatPolicy` resolves to a matching reviewed Donat policy ID.
The two variants are not substitutable. Provider normalized values and Donat
policy values enter semantic material; provider record/artifact/fact
identities and locations plus Donat policy IDs enter provenance material.
Changing either origin or value changes the corresponding domain-separated
hash. `EvidenceAccepted` is valid only for a
`ProviderArtifact`; `ApprovedForPort` is valid only for a donor or Donat-owned
operation source. The five catalog-local provenance IDs use one strict
`1..=96`-byte ASCII grammar
`[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?` and reject unknown, empty, duplicate, or
mismatched references. They are transparent wrappers over Task 2's exact
const/copy `InlineId` storage and expose the same checked runtime parser plus
const literal constructor, so generated source, fact, policy, and notice
identities need no `String` or lazy initialization. The provider-contract
round-trip fixture exercises `EvidenceAccepted`; that state can never
generate an operation entry.
Inventory-only records never enter the executable catalog.

- [ ] **Step 4: Run GREEN and review snapshots**

```bash
cargo test -p donat-connector-catalog
cargo insta test -p donat-connector-catalog
cargo insta review
```

Expected: the exact SerpAPI-shaped npm fixture round-trips,
provider-evidence and Donat-policy origins remain distinct, the complete
credential/operation/trigger catalog round-trips, every
mismatch/open/unbounded disposition fails, and reviewed snapshots contain no
source description or UI metadata.

- [ ] **Step 5: Commit the neutral catalog**

```bash
git add Cargo.toml Cargo.lock crates/connector-catalog
git commit -m "feat(connectors): add normalized connector catalog"
```


---

### Task 4: Add hostile acquisition and offline admission checking

**Files:**

- Create: `crates/connector-acquire/Cargo.toml`
- Create: `crates/connector-acquire/src/lib.rs`
- Create: `crates/connector-acquire/src/main.rs`
- Create: `crates/connector-acquire/src/download.rs`
- Create: `crates/connector-acquire/src/archive.rs`
- Create: `crates/connector-acquire/src/admission.rs`
- Create: `crates/connector-acquire/src/inventory.rs`
- Create: `crates/connector-acquire/tests/source_admission.rs`
- Create: `crates/connector-acquire/tests/cli_contract.rs`
- Create: `crates/connector-acquire/tests/hostile_archives.rs`
- Create: `crates/connector-acquire/tests/imperative_inventory.rs`
- Modify: `.gitignore`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: checked-in `ConnectorSourceRecord` schemas from
  `donat-connector-catalog`.
- Produces development-only commands:

```text
donat-connector-acquire acquire-npm-review \
  --artifact-url <exact-https-url> \
  --expected-integrity <canonical-sha512-sri> \
  --repository-url <exact-https-git-url> \
  --commit <full-commit> \
  --output <ignored-quarantine-directory>

donat-connector-acquire acquire-provider-review \
  --repository-url <exact-https-git-url> \
  --commit <full-commit> \
  --output <ignored-quarantine-directory>

donat-connector-acquire acquire-provider-review \
  --artifact-url <exact-https-url> \
  --provider-revision <non-empty-immutable-revision> \
  --expected-sha256 <lower-case-64-hex> \
  --output <ignored-quarantine-directory>

donat-connector-acquire reacquire-reviewed \
  --record <approved.yaml> \
  --output <absent-ignored-quarantine-directory>

donat-connector-acquire check-record --record <approved.yaml>
donat-connector-acquire verify \
  --record <approved.yaml> \
  --artifact <local-artifact> \
  [--source-tree <local-source-tree>]
```

The two `acquire-*-review` commands are disjoint tagged schemas. The npm
command requires SRI before any record exists and rejects provider-only
flags. The provider command requires exactly one of repository
`{repository_url, commit}` or versioned-artifact
`{artifact_url, provider_revision, expected_sha256}` and rejects npm
integrity/package fields. `reacquire-reviewed` accepts no locator or expected
hash on the command line; it reads those exact identities only from a
schema-valid `ApprovedForPort` or `EvidenceAccepted` source record and refuses
an inventory/candidate record. These three are the only networked commands and
write an ignored mode-`0700` quarantine plus candidate review bundle.
`.donat/connector-quarantine/` is added to `.gitignore`. `check-record` is
offline and checks only schema/internal consistency; it never claims byte
verification. `verify` is offline, requires explicit local artifact and
source-tree paths for npm/repository subjects, forbids `--source-tree` for a
versioned single artifact, and recomputes artifact integrity, repository tree,
every admitted file hash, and the license hash from those bytes. Neither
offline command writes source or generated Rust. No command executes donor
scripts, binaries, tests, Node, or JavaScript.

An npm acquisition emits `package.tgz` and `source/`. A repository provider
acquisition emits `provider-source.tar` and `source/`; a versioned provider
artifact emits `provider-artifact.bin`. Mutable provider HTML with only an
access date is not an acquisition schema and cannot become immutable evidence.
Tests may use checked-in synthetic bytes to prove that rejection without
network access. All networked forms use the same checked-in host policy;
`docs.stripe.com` is not allowed by this plan.

- [ ] **Step 1: Add synthetic hostile-archive RED tests**

Generate archives inside tests for absolute paths, `..`, duplicate and
ASCII-case-colliding names, symlink, hardlink, device, FIFO, sparse/unknown
entry, count/depth/file/expanded limits, digest mismatch, no-follow
replacement, redirect violations, and a package-script sentinel.

```rust
#[test]
fn package_script_sentinel_is_never_executed() {
    let sentinel = temp.path().join("script-ran");
    verify_archive(archive_with_postinstall(&sentinel)).unwrap();
    assert!(!sentinel.exists());
}
```

- [ ] **Step 2: Add exact disjoint-CLI RED tests**

Add `npm_review_requires_expected_integrity`,
`npm_review_rejects_provider_only_flags`,
`provider_review_requires_one_exact_source_identity`,
`provider_review_rejects_npm_flags`,
`provider_versioned_artifact_requires_expected_sha256`,
`reacquire_uses_only_reviewed_record_identity`,
`reacquire_rejects_locator_overrides`, and
`unallowlisted_provider_host_is_rejected`. Parse the same clap command enum
used by `main`; do not duplicate a test-only grammar.

- [ ] **Step 3: Run RED**

```bash
cargo test -p donat-connector-acquire --test cli_contract
cargo test -p donat-connector-acquire --test source_admission
cargo test -p donat-connector-acquire --test hostile_archives
```

Expected: Cargo reports that package `donat-connector-acquire` does not exist.

- [ ] **Step 4: Implement the exact acquisition policy**

Enforce HTTPS, the checked-in host allowlist, at most three same-host HTTPS
redirects, 64 MiB compressed, 256 MiB expanded, 16 MiB per file, 10,000
entries, and depth 32. Hash the complete artifact before entry inspection and
require npm/versioned-provider bytes to match expected integrity first.
Safely extract repository bytes, then require the exact commit/tree identity;
reacquisition matches every corresponding record value before admission. Use
an exclusive mode-`0700` temporary directory, exclusive file creation,
no-follow walks, normalized relative UTF-8 paths, and RAII cleanup.

- [ ] **Step 5: Add admission and imperative-source findings**

Prove the six-license allowlist, closed dependency dispositions,
`n8n-workflow: TypeOnlyReplaced`, exact tree/file/license hashes, and
embedded-material decisions. A synthetic function-valued routing or
`execute`/`poll`/`webhook` source emits a reviewed work item or unsupported
finding and never generated behavior:

```rust
assert_eq!(
    inspect_source(imperative_fixture()).decision,
    InventoryDecision::ProcessorWorkItem
);
```

- [ ] **Step 6: Prove record-only and byte-verification modes differ**

Add tests `check_record_does_not_claim_byte_verification`,
`verify_requires_subject_specific_local_inputs`,
`verify_recomputes_every_recorded_hash`, and
`network_is_available_only_to_review_and_reacquire_commands`. Add
`provider_repository_emits_offline_artifact_and_tree` and
`mutable_provider_url_cannot_become_immutable_evidence`. A record that repeats
expected hash strings passes `check-record` but fails `verify` against one
changed artifact byte or source file.

- [ ] **Step 7: Run GREEN**

```bash
cargo test -p donat-connector-acquire --test cli_contract
cargo test -p donat-connector-acquire --test source_admission
cargo test -p donat-connector-acquire --test hostile_archives
cargo test -p donat-connector-acquire --test imperative_inventory
```

- [ ] **Step 8: Commit the acquisition tool**

```bash
git add .gitignore Cargo.toml Cargo.lock crates/connector-acquire
git commit -m "feat(connectors): add hostile source acquisition gate"
```


---

### Task 5: Generate deterministic checked-in Rust without acquisition

**Files:**

- Create: `crates/connector-codegen/Cargo.toml`
- Create: `crates/connector-codegen/src/lib.rs`
- Create: `crates/connector-codegen/src/main.rs`
- Create: `crates/connector-codegen/src/render.rs`
- Create: `crates/connector-codegen/src/check.rs`
- Create: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Create:
  `crates/connector-codegen/tests/snapshots/deterministic_catalog__donat_owned_http.snap`
- Create: `crates/connector-catalog/tests/generated_consumers.rs`
- Create: `crates/connector-catalog/manifests/donat-owned-http-v1.yaml`
- Create: `crates/connector-catalog/src/generated/mod.rs`
- Create: `crates/connector-catalog/src/generated/donat_owned_http.rs`
- Create: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/connector-catalog/src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: checked-in catalog records/manifests only.
- Produces:

```text
donat-connector-codegen generate --check
donat-connector-codegen generate --output <temporary-directory>
```

and immutable generated entries:

```rust
pub struct GeneratedCredentialSpec {
    pub credential: CredentialSpecId,
    pub version: u32,
    pub fields: &'static [GeneratedCredentialField],
    pub auth_plan: GeneratedAuthPlan,
    pub allowed_origins: &'static [OriginId],
    pub scopes: &'static [GeneratedStaticScope],
    pub auth_processor: Option<AuthenticatorId>,
    pub credential_test_operation: Option<OperationId>,
    pub bounds: GeneratedCredentialBounds,
}

pub struct GeneratedOperationEntry {
    pub operation: OperationId,
    pub version: u32,
    pub credential: Option<CredentialSpecId>,
    pub origins: &'static [GeneratedFixedOrigin],
    pub steps: &'static [GeneratedCompiledStep],
    pub effect: GeneratedOperationEffect,
    pub input: GeneratedValueContract,
    pub output: GeneratedValueContract,
    pub pagination: GeneratedPaginationPlan,
    pub error_map: GeneratedErrorMap,
    pub bounds: GeneratedOperationBounds,
    pub provenance: &'static [GeneratedProvenanceReference],
}

pub struct GeneratedTriggerSpec {
    pub trigger: TriggerId,
    pub kind: GeneratedTriggerKind,
    pub provenance: &'static [GeneratedProvenanceReference],
}

pub struct GeneratedSourceIdentity {
    pub record_id: SourceRecordId,
    pub record_sha256: Hash256,
    pub artifact_hashes: &'static [GeneratedArtifactHash],
}

pub struct GeneratedLegalIdentity {
    pub source_record_id: SourceRecordId,
    pub license_id: LicenseIdentity,
    pub notice_id: NoticeId,
    pub license_file_sha256: Hash256,
}

pub struct GeneratedConnectorEntry {
    pub connector: ConnectorId,
    pub credentials: &'static [GeneratedCredentialSpec],
    pub operations: &'static [GeneratedOperationEntry],
    pub triggers: &'static [GeneratedTriggerSpec],
    pub source_records: &'static [GeneratedSourceIdentity],
    pub legal: &'static [GeneratedLegalIdentity],
    pub semantic_sha256: Hash256,
    pub provenance_sha256: Hash256,
}

pub static CONNECTORS: &[GeneratedConnectorEntry] = &[/* sorted entries */];
```

Every rendered identity is emitted as an ABI const literal, for example
`ConnectorId::literal("donat.http")` and
`CompiledStepId::literal("request")`. Generated code contains no owned
`String`, lazy initialization, parse call, deserialization, clone, or
conversion bridge for an identity. The generated credential/auth, fixed
origin/step, operation/effect/error/pagination/bounds, trigger, contract-fact,
source-record, and legal shapes are const-safe projections of the strict Task
3 model; they do not omit a normalized field or introduce a server-owned
descriptor.

- [ ] **Step 1: Write the failing determinism and drift tests**

```rust
#[test]
fn generated_catalog_is_checked_in_and_deterministic() {
    let first = generate_to_temp();
    let second = generate_to_temp();
    assert_tree_bytes_equal(&first, &second);
    assert_tree_bytes_equal(&first, checked_in_generated_dir());
}
```

Also test deleted output, extra output, changed byte, unsorted IDs, duplicate
ID, manifest/source-record mismatch, semantic/provenance mismatch, and an
unexpected path. Add a renderer assertion that rejects output containing
identity `.parse()`, `String::`, `.to_owned()`, `.to_string()`, `.clone()`,
`OnceLock`, or `LazyLock`, and compile the emitted credentials, operations,
triggers, source identities, legal identities, and all ABI IDs in a const
context. Reject a generated entry that omits any matching source-record hash,
license/notice identity, contract-fact origin, fixed step/origin, bound, auth
plan, error map, or trigger field.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-codegen --test deterministic_catalog
```

Expected: Cargo reports that package `donat-connector-codegen` does not exist.

- [ ] **Step 3: Implement byte-for-byte generation**

Sort by stable connector/operation ID. Render a header containing manifest,
record, generator version, semantic hash, and provenance hash. Calculate the
generated-tree digest exactly as Spec 007 Section 10 specifies. Never use
`OUT_DIR`, `build.rs`, the network, or `donat-connector-acquire`.

- [ ] **Step 4: Add ABI identity and exact-consumer compile proofs**

Add `generated_catalog_ids_match_abi` in the catalog crate. It assigns every
actual checked-in connector, operation, step, processor-family, credential,
capability, trigger, authenticator, codec, normalizer, and origin ID directly
to the corresponding ABI type.

In `generated_consumers.rs`, compile these exact catalog-facing consumers:

```rust
fn task7_credential(
    spec: &'static GeneratedCredentialSpec,
) -> CredentialSpecId {
    spec.credential
}

fn task16_trigger(spec: &'static GeneratedTriggerSpec) -> TriggerId {
    spec.trigger
}
```

Add
`generated_credentials_compile_for_task7_without_server_descriptor` and
`generated_triggers_compile_for_task16_without_server_descriptor`. The test
imports both argument types from `donat_connector_catalog::generated`; it
must not define a server-owned credential, operation, trigger, source, or
legal descriptor.

- [ ] **Step 5: Render the first manifest to a temporary directory**

Create `donat-owned-http-v1.yaml` only after its matching Donat-owned source
record from Task 3 is green. Render and review before updating checked-in
output:

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

Review the manifest, renderer, renderer tests, snapshot, every generated Rust
byte, and the digest. Then update only through the generator:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
```

Never hand-edit `donat_owned_http.rs`, `mod.rs`, or `catalog.digest`.

- [ ] **Step 6: Run GREEN and inspect output**

```bash
cargo test -p donat-connector-codegen --test deterministic_catalog
cargo run -p donat-connector-codegen -- generate --check
cargo test -p donat-connector-catalog generated_catalog_ids_match_abi
cargo test -p donat-connector-catalog --test generated_consumers
cargo insta test -p donat-connector-codegen
cargo insta review
```

- [ ] **Step 7: Commit the codegen slice**

```bash
git add Cargo.toml Cargo.lock crates/connector-codegen \
  crates/connector-catalog/manifests/donat-owned-http-v1.yaml \
  crates/connector-catalog/src/generated crates/connector-catalog/src/lib.rs \
  crates/connector-catalog/tests/generated_consumers.rs
git commit -m "feat(connectors): generate checked-in static catalog"
```


---

### Task 6: Seal the local-only processor boundary

**Files:**

- Create: `crates/connector-processors/Cargo.toml`
- Create: `crates/connector-processors/src/lib.rs`
- Create: `crates/connector-processors/src/registry.rs`
- Create: `crates/connector-processors/src/proofs.rs`
- Create: `crates/connector-processors/src/cloudinary_shape.rs`
- Create: `crates/connector-processors/tests/registry.rs`
- Create: `crates/connector-processors/tests/cloudinary_shape.rs`
- Create: `policy/connector-processor-dependencies.toml`
- Modify: `scripts/check_connector_processor_boundary.py`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes only ABI-owned IDs, contexts, envelopes, host capabilities, and
  value-contract types.
- Produces sealed `OperationProcessor`, `PureTransform`, `AuthProcessor`,
  `WebhookAuthenticator`, `WebhookNormalizer`, `PaginationProcessor`, and
  `PollProcessor` traits. Only traits needed by admitted slices receive
  implementations; declaring the closed ABI does not admit a donor.
- Exposes:

```rust
pub struct ProcessorHandle {
    inner: &'static dyn OperationProcessor,
}

pub fn lookup_operation_processor(
    family: ProcessorFamilyId,
    version: u16,
) -> Option<ProcessorHandle>;
```

`ProcessorHandle` exposes only `execute`; table construction, registration,
and implementation types remain private.

Processor production code reads `BoundedTransportResponse` only through
`status()`, `selected_headers()`, `decoded()`, `response_bytes()`, and
`authorized_correlations()`. Donat-owned failures use
`StaticErrorCode::literal` and `StaticSafeMessage::literal`. Processor production
code never refers to `catalog_construction` or `host_construction` and never uses
an allocation-leak API. It neither constructs nor names `StaticFailureText`;
boxing remains private to `ConnectorFailure::try_new` in the ABI.

- [ ] **Step 1: Write the failing closure, registry, and shaped-proof tests**

Add these exact tests:

```rust
#[test]
fn processor_registry_is_closed() {
    assert!(lookup_operation_processor(
        ProcessorFamilyId::parse("not-admitted").unwrap(),
        1,
    )
    .is_none());
}

#[test]
fn processor_lookup_ids_match_abi() {
    let family: ProcessorFamilyId = generated_processor_family();
    let _ = lookup_operation_processor(family, 1);
}
```

`cloudinary_shape.rs` supplies Donat-owned values representing one transform,
one compiled call, and one normalized result. Assert that the processor can
select only its declared `CompiledStepId`, cannot construct a URL/header, and
observes `ProcessorControl::check` before and after the call.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-processors --no-default-features
python3 scripts/check_connector_processor_boundary.py
```

Expected: Cargo reports that package `donat-connector-processors` does not
exist. The initial checker is green; the RED is a missing processor package or
policy, never a missing checker.

- [ ] **Step 3: Implement the sealed ABI and private table**

Start the crate with:

```rust
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
```

Use a private sealing supertrait for every processor trait. Do not export a
constructor, registration method, mutable table, trait implementation type,
catalog type, generated entry, source bytes, donor constant, `std` feature,
panic-based normal control path, or global mutable state. The
Cloudinary-shaped processor remains Donat-owned proof code and is not an
admitted connector.

- [ ] **Step 4: Extend the mechanical boundary checker**

The checked-in policy must enumerate exactly the local path closure:

```toml
[package.donat-connector-processors]
normal = ["donat-connector-abi", "donat-value-contract"]
build = []
proc_macro = []

[package.donat-connector-abi]
normal = ["donat-value-contract"]
build = []
proc_macro = []

[package.donat-value-contract]
normal = []
build = []
proc_macro = []
```

Preserve every initial namespace/producer/test fixture and extend the same
checker with locked Cargo metadata, dependency closure,
build/procedural/native/git/patch, symlink/workspace escape, generated/donor
source, and processor-source rules. It evaluates all supported targets and
features; rejects build scripts, native/git/patched dependencies, `links`,
unsafe/FFI/assembly, symlink or workspace escapes, generated/donor files in
the processor tree, and the forbidden source tokens listed in Spec 007
Section 7.1. It also rejects an `OperationProcessor` implementation outside
this crate. Add both positive and deliberately mutated negative fixtures
inside the Python script's temporary directory. Task 6 may create
`policy/connector-processor-dependencies.toml`; it may not create another
Python checker, wrapper, or parallel policy mechanism.

- [ ] **Step 5: Wire CI and run GREEN**

```bash
python3 scripts/check_connector_processor_boundary.py
cargo test -p donat-connector-processors --no-default-features
cargo check -p donat-connector-processors --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-processors --target all \
  --edges normal,build --no-default-features --offline --locked
cargo test -p donat-connector-abi --no-default-features
cargo test -p donat-value-contract --no-default-features
```

Expected: the dependency tree is exactly the local-only policy closure and
all deliberate boundary mutations fail with the expected diagnostic.

- [ ] **Step 6: Commit the processor boundary**

```bash
git add Cargo.toml Cargo.lock crates/connector-processors \
  policy/connector-processor-dependencies.toml \
  scripts/check_connector_processor_boundary.py .github/workflows/ci.yml
git commit -m "feat(connectors): seal native processor boundary"
```


---

### Task 7: Resolve deploy-time credentials into per-use capabilities

**Files:**

- Create: `crates/metadata/tests/fixtures/connectors/community-credentials.yaml`
- Create: `crates/metadata/tests/fixtures/connectors/community-credentials-inline-secret.yaml`
- Create: `crates/server/src/connectors/credentials.rs`
- Create: `crates/server/tests/connector_credentials.rs`
- Create: `crates/server/tests/compile_fail/credential_capability_clone.rs`
- Create: `crates/server/tests/compile_fail/credential_capability_clone.stderr`
- Create: `crates/server/tests/compile_fail/credential_capability_debug.rs`
- Create: `crates/server/tests/compile_fail/credential_capability_debug.stderr`
- Create: `crates/server/tests/compile_fail/credential_capability_serialize.rs`
- Create: `crates/server/tests/compile_fail/credential_capability_serialize.stderr`
- Create: `crates/server/tests/compile_fail/credential_capability_expose.rs`
- Create: `crates/server/tests/compile_fail/credential_capability_expose.stderr`
- Create: `crates/server/tests/connector_credentials_compile_fail.rs`
- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/src/loader.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `Cargo.toml`
- Modify: `crates/server/Cargo.toml`
- Modify: `Cargo.lock`

**Metadata contract:**

```yaml
connectors:
  - name: search
    module: serpapi
    config:
      endpoint_identity: serpapi_public_v1
      credential_identity: serpapi_primary
      credential:
        spec: serpapi.api_key.v1
        fields:
          api_key:
            value_from_env: SERPAPI_API_KEY
    operations:
      - name: search.google
```

This extends the existing `ConnectorInstance { name, module, config,
operations }`; it does not add a second untagged branch. Existing HTTP and
Stripe fields continue to deserialize. `module` selects the compiled catalog
connector, endpoint/credential identities stay in `config`, and enabled
operations stay explicit. `source` remains absent here: it is reserved for
the distinct Spec 005 database/process source binding. The credential object
accepts no literal value, runtime resolver kind, package URL, implementation
path, or credential operation.

**Server interfaces:**

```rust
pub trait CredentialResolver: Send + Sync {
    fn validate_reference(
        &self,
        reference: &CredentialReference,
    ) -> Result<(), CredentialFailure>;

    fn resolve_for_use(
        &self,
        reference: &CredentialReference,
        use_scope: CredentialUseScope,
    ) -> Result<CredentialCapability, CredentialFailure>;
}

pub struct EnvironmentCredentialResolver;
pub struct CredentialCapability {
    /* private, zeroized-on-drop server fields */
}
```

`CredentialCapability` implements neither `Clone`, `Debug`, `Display`,
`Serialize`, nor any raw-value getter. It is consumed by one compiled
auth/verification action and one logical attempt.

- [ ] **Step 1: Write failing loader and capability tests**

Add exact tests:

- `community_credential_references_are_strict_and_non_secret`;
- `community_credential_literal_is_rejected`;
- `environment_reference_is_validated_before_listen`;
- `credential_is_resolved_again_for_every_use`;
- `capability_is_scoped_to_compiled_step_and_field`;
- `credential_validation_consumes_generated_spec_without_server_descriptor`;
- `credential_failure_never_contains_secret_value`;
- `credential_capability_has_no_debug_clone_or_serialize_surface`.

Use an in-memory fake resolver whose value changes between calls and whose
call log records only reference identities. Use compile-fail fixtures for the
forbidden trait implementations and raw getter.
`connector_credentials_compile_fail.rs` is the exact trybuild runner:

```rust
#[test]
fn credential_capability_surface_is_sealed() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/compile_fail/credential_capability_*.rs");
}
```

Add `trybuild = "1.0"` to workspace dependencies and
`trybuild = { workspace = true }` to `donat-server` dev-dependencies; commit
the resulting exact lockfile version.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-metadata community_credential
cargo test -p donat-server --test connector_credentials
cargo test -p donat-server --test connector_credentials_compile_fail
```

Expected: `config.credential` is unknown and both new server test targets are
absent.

- [ ] **Step 3: Implement strict metadata selection**

Add the strict optional credential object to existing `ConnectorConfig`.
Validate `module` against `GeneratedConnectorEntry`, select the exact
`&'static GeneratedCredentialSpec` emitted in Task 5, validate every
credential field against that value, validate every enabled
`GeneratedOperationEntry` against the same module, and validate every
environment variable name before the listener opens. Do not copy the
generated credential or operation into a server-owned descriptor. Store only
module/instance identity, endpoint identity, credential identity, enabled
operations, and secret references in the immutable registry.

- [ ] **Step 4: Implement read-only resolution and opaque consumption**

The environment resolver reads a named variable only within
`resolve_for_use`; it never enumerates, writes, refreshes, deletes, caches, or
logs variables. Bind the returned capability to connector, operation,
compiled step, credential spec, field, and attempt. Server-owned auth
primitives consume the capability and return an ABI opaque binding; processors
never receive raw credential bytes.

- [ ] **Step 5: Run GREEN and secret scans**

```bash
cargo test -p donat-metadata community_credential
cargo test -p donat-server --test connector_credentials
cargo test -p donat-server --test connector_credentials_compile_fail
if rg -n "CredentialCapability.*(Clone|Debug|Serialize)|fn (get|expose|value)" \
  crates/server/src/connectors/credentials.rs; then
  exit 1
fi
```

Expected: all tests pass and the inverted scan exits zero only when there is
no capability trait derivation or raw-value API match.

- [ ] **Step 6: Commit the credential boundary**

```bash
git add Cargo.toml Cargo.lock crates/metadata \
  crates/server/src/connectors/credentials.rs \
  crates/server/src/connectors/mod.rs crates/server/src/state.rs \
  crates/server/tests/connector_credentials.rs \
  crates/server/tests/connector_credentials_compile_fail.rs \
  crates/server/tests/compile_fail crates/server/Cargo.toml
git commit -m "feat(connectors): add per-use credential capabilities"
```


---

### Task 8: Centralize one fixed-origin executor and complete error mapping

**Files:**

- Create: `crates/server/src/connectors/transport.rs`
- Create: `crates/server/src/connectors/executor.rs`
- Create: `crates/server/src/connectors/codec.rs`
- Create: `crates/server/src/connectors/error_map.rs`
- Create: `crates/server/tests/connector_executor.rs`
- Modify: `crates/server/src/connectors/http.rs`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

```rust
pub struct FixedOriginExecutor {
    transport: FixedOriginTransport,
    credentials: Arc<dyn CredentialResolver>,
}

impl ConnectorIo for OperationAttempt<'_> {
    fn call<'a>(
        &'a self,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>>;
}
```

The executor resolves the step only from the current generated operation. It
owns URL joining, query/form/JSON encoding, auth application, deadline and
control checks, DNS resolution, address vetting, peer validation, reqwest,
status/header/body capture, decoding, bounds, redaction, and error mapping.
No public method accepts a raw URL, method, header name, auth value, or
arbitrary request object.

Task 8 is the only production caller of `host_construction`. It captures only
compiled selected response headers, converts each compiled header to its
`CapabilityId` and `BoundedString` value, obtains the correlation-capability
allowlist derived by Task 3 from the selected `ErrorAction`, and calls
`host_construction::transport_response`.

Task 8 passes the selected catalog `ErrorAction`'s `StaticErrorCode` and
`StaticSafeMessage` directly to `ConnectorFailure::try_new`. It passes
`Some(response.authorized_correlations())` for an error response and `None`
when no host-authorized correlation set exists.

The server neither constructs nor inspects `StaticFailureText` and performs no
failure-text box allocation; `ConnectorFailure::try_new` performs the one
private allocation.

No server method accepts a raw failure code, raw safe message, raw correlation
map, or caller-supplied correlation allowlist.

An ABI construction failure caused by an admitted compiled contract maps to
the closed invariant class with one reviewed ABI-owned static code/message
pair and no provider text.

- [ ] **Step 1: Add RED tests around the current transport contract**

Port the existing HTTP safety assertions into these exact tests:

- `fixed_origin_rejects_userinfo_fragments_and_path_escape`;
- `fixed_origin_rejects_private_and_special_use_dns_answers`;
- `fixed_origin_revalidates_dns_and_connected_peer`;
- `fixed_origin_ignores_environment_proxies`;
- `fixed_origin_never_redirects_or_retries`;
- `undeclared_step_fails_before_dns`;
- `typed_json_query_and_form_bindings_encode_canonically`;
- `operation_budget_counts_auth_calls_provider_calls_and_bytes`;
- `success_and_failure_bodies_are_bounded`;
- `error_map_covers_every_status_and_transport_failure`;
- `retry_after_supports_seconds_and_http_date`;
- `provider_body_and_credentials_never_enter_failure`.

Use only local Axum stubs and injected DNS/peer observations.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-server --test connector_executor
```

Expected: Cargo reports that test target `connector_executor` does not exist.

- [ ] **Step 3: Extract transport without weakening policy**

Move, do not duplicate, the current `http.rs` DNS/address/peer/proxy/redirect
logic into `transport.rs`. Preserve the independently authored `http`
connector's deploy-time fixed base URL and `public_only` behavior. Provider
catalog entries are fixed HTTPS origins and cannot select `private_allowed`.
Keep automatic retries disabled.

- [ ] **Step 4: Implement compiled-step execution and codecs**

Resolve `CompiledStepId` by direct ABI identity, reject foreign or undeclared
IDs as `invariant` before credential resolution or network I/O, charge the
shared call/page/item/byte budget, and check `ProcessorControl` before and
after auth, codec, and I/O. Implement only typed JSON, query, and form bodies;
multipart, inline binary, streaming, and continuation URLs stay rejected.

- [ ] **Step 5: Implement exhaustive safe errors**

Implement the Spec 007 Section 5.5 fallback exactly:

```text
transport/TLS/DNS -> transport
deadline or HTTP 408 -> timeout
HTTP 429 -> http_429
declared 5xx -> http_5xx
HTTP 401/403 -> authentication
declared provider validation or malformed success -> validation
other supported non-success -> permanent
compiled-contract violation -> invariant
```

Only allowlisted provider correlation IDs and a clamped retry delay survive.
Safe messages are Donat-owned constants.

- [ ] **Step 6: Run GREEN and existing connector regressions**

```bash
cargo test -p donat-server --test connector_executor
cargo test -p donat-server --test connectors_http
cargo test -p donat-server --test connectors_stripe
cargo test -p donat-server --test connector_webhook
```

- [ ] **Step 7: Commit the shared executor**

```bash
git add crates/server/src/connectors crates/server/tests/connector_executor.rs \
  crates/server/Cargo.toml Cargo.lock
git commit -m "refactor(connectors): centralize fixed-origin execution"
```


---

### Task 9: Replace hand-written module dispatch with the static catalog

**Files:**

- Create: `crates/server/src/connectors/catalog.rs`
- Create: `crates/server/tests/connector_catalog.rs`
- Create: `crates/conformance/fixtures/connectors/catalog_startup/metadata.yaml`
- Create: `crates/conformance/fixtures/connectors/catalog_startup/query.graphql`
- Create: `crates/conformance/fixtures/connectors/catalog_startup/expected.json`
- Create: `crates/conformance/tests/connector_public_surfaces.rs`
- Create: `crates/connector-catalog/sources/records/donat-owned-stripe-v1.yaml`
- Create: `crates/connector-catalog/manifests/donat-owned-stripe-compat-v1.yaml`
- Generate: `crates/connector-catalog/src/generated/donat_owned_stripe_compat.rs`
- Generate: `crates/connector-catalog/src/generated/mod.rs`
- Generate: `crates/connector-catalog/src/generated/catalog.digest`
- Create: `crates/connector-codegen/tests/donat_owned_stripe.rs`
- Create:
  `crates/connector-codegen/tests/snapshots/donat_owned_stripe__compat.snap`
- Modify: `crates/connector-codegen/src/render.rs`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Create: `scripts/check_connector_public_surfaces.py`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/src/main.rs`
- Modify: `crates/server/src/lib.rs`
- Modify: `crates/server/Cargo.toml`
- Modify: `crates/conformance/tests/connectors.rs`
- Modify: `Cargo.lock`

**Interfaces:**

```rust
pub struct ConnectorRegistry {
    instances: BTreeMap<String, CatalogInstance>,
}

struct CatalogInstance {
    module: &'static GeneratedConnectorEntry,
    credential: Option<CredentialInstanceBinding>,
}
```

The server joins generated operation descriptors with the private processor
lookup and its host implementations. It has no `RegistryInstance` enum,
module-name `match`, public registration method, dynamic fallback, or
catch-all executor.

- [ ] **Step 1: Write failing static-selection and public-surface tests**

Add exact tests:

- `catalog_instance_selects_only_generated_connector`;
- `unknown_module_fails_before_listen`;
- `inventory_only_module_cannot_enable_an_operation`;
- `generated_processor_id_resolves_without_conversion`;
- `registry_has_no_dynamic_registration_or_fallback`;
- `stripe_manifest_requires_matching_donat_owned_record`;
- `unrelated_donat_owned_record_cannot_satisfy_stripe_manifest`;
- `unknown_webhook_instance_is_exact_empty_404`;
- `graphql_rest_mcp_and_metadata_have_no_connector_execution_surface`.

The structural test scans router construction and OpenAPI/MCP/GraphQL
registries for a generic connector execution path. The only connector route
remains inbound:

```text
POST /v1/connectors/{instance}/webhooks
```

`POST /v1/connectors/{instance}/execute` must be an exact empty-body `404`.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-server --test connector_catalog
cargo test -p donat-conformance --test connector_public_surfaces
```

Expected: the catalog test target is absent and current dispatch still
contains the hand-written `RegistryInstance` branches.

- [ ] **Step 3: Implement immutable catalog selection**

Compile instance module and operation lookup entirely at startup. Join exact
ABI-owned IDs without strings, parsing, serialization, or wrapper copying.
Unknown module, unknown operation, credential mismatch, unresolved processor,
or unsupported manifest feature is a startup error. Keep inventory-only
records outside generated executable entries.

- [ ] **Step 4: Create the Donat-owned Stripe record before its manifest**

Pin the implementation-base Git commit and recomputed SHA-256 values for
`crates/server/src/connectors/stripe.rs`,
`crates/server/src/connector_webhook.rs`,
`crates/server/tests/connectors_stripe.rs`, and
`crates/server/tests/connector_webhook.rs`. The
`DonatOwnedSource` record names the exact compatibility RED tests and proposes
both manifests:

```text
crates/connector-catalog/manifests/donat-owned-stripe-compat-v1.yaml
crates/connector-catalog/manifests/donat-owned-stripe-webhook-v1.yaml
```

It also enumerates the processor/server/generated destinations used by Tasks
13, 15, and 16. Create and validate this record before either manifest.
`donat-owned-http-v1.yaml` must fail the Stripe manifest/source identity
check.

- [ ] **Step 5: Render and review the compatibility entry**

Create the manifest only after Step 4 passes. Never create or edit generated
Rust or `catalog.digest` by hand:

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

Review every path/byte in the diff. Then update through the generator and
prove reproducibility:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen donat_owned_stripe
cargo insta review
```

- [ ] **Step 6: Delete the generic module dispatch shape**

Delete `ConnectorModule`, `ConnectorDefinition`, `RegistryInstance`,
`built_in_module_names`, and the `"http"`/`"stripe"` module match only after
equivalent static entries are present. Until Task 15 migrates Stripe, its
generated Donat-owned compatibility entry contains configuration only: it has
neither an executable `GeneratedOperationEntry` nor a
`GeneratedTriggerSpec`. A private fixed server function pointer reaches the
existing adapter for its direct Rust behavior oracle. Task 16 is the sole
owner of the first generated Stripe webhook trigger descriptor. There is no
string dispatch or process/public exposure. Preserve every current inbound
webhook result and the exact empty-body 404.

- [ ] **Step 7: Run GREEN and scoped public-surface proof**

```bash
cargo test -p donat-server --test connector_catalog
cargo test -p donat-server --test connector_webhook
cargo test -p donat-conformance --test connectors
cargo test -p donat-conformance --test connector_public_surfaces
python3 scripts/check_connector_public_surfaces.py
if rg -n "RegistryInstance|register_connector" \
  crates/server/src/connectors crates/server/src/main.rs; then
  exit 1
fi
cargo run -p donat-connector-codegen -- generate --check
```

The structural checker examines only runtime registrations in
`crates/server/src/main.rs`, `crates/server/src/lib.rs`,
`crates/server/src/connector_webhook.rs`, `crates/server/src/rest.rs`,
`crates/server/src/mcp.rs`, and `crates/schema/src/lib.rs`. It does not scan
tests/fixtures, so the required negative `/execute` request remains legal.
Its self-test inserts a synthetic route/tool/field registration and requires
rejection.

- [ ] **Step 8: Commit the static registry**

```bash
git add Cargo.lock crates/connector-catalog \
  crates/connector-codegen/src/render.rs crates/connector-codegen/tests \
  crates/server crates/conformance scripts/check_connector_public_surfaces.py
git commit -m "refactor(connectors): consume the static connector catalog"
```


---

### Task 10: Admit the exact SerpAPI source before derivative work

**Files:**

- Create: `crates/connector-catalog/sources/records/serpapi-0.1.10.yaml`
- Create:
  `crates/connector-catalog/sources/records/serpapi-search-provider-v1.yaml`
- Create:
  `crates/connector-catalog/sources/provider-contracts/serpapi-search-v1.yaml`
- Create: `crates/connector-acquire/tests/serpapi_admission.rs`
- Modify: `knowledgebase/declarative-saas/reference-porting-register.md`

**Pinned source identity:**

```text
repository: https://github.com/serpapi/n8n-nodes-serpapi
commit: e48b778878c043f30277b932c4c129804efee66d
git_tree: 6916ca97c870b5045200a207dfaf9cb40341f24d
package: n8n-nodes-serpapi@0.1.10
tarball: https://registry.npmjs.org/n8n-nodes-serpapi/-/n8n-nodes-serpapi-0.1.10.tgz
npm_integrity: sha512-E9tAU4c9mhNWr07s6RGeqzyrlQO8y42YvtMjPWuLf+tIEM8muU/RIgtp+ojhaoNVCP+jfrwmsSC75OIuoMVS9A==
license: MIT
license_sha256: 053cb0df9afcf71ac340bdddccb3c25b280a8645e64ba93a709ebc0fbe0f4e35
notice: Copyright 2022 n8n
```

Required per-file SHA-256 values:

```text
LICENSE.md
  053cb0df9afcf71ac340bdddccb3c25b280a8645e64ba93a709ebc0fbe0f4e35
package.json
  f90fa3ff0d4c3b5353f26a6785d30119c3bcde890c0982006266b8947f158031
nodes/SerpApi/SerpApi.node.ts
  f2bfb8abbc3ea84d21da4d557a7553b34f7325236b8367902f9805b5f39cd58f
nodes/SerpApi/descriptions/GoogleSearchDescription.ts
  c286005eb9183bbd12b59cf6fcce07c9636fc1d68d52142d99bbc2aca3dbc87a
credentials/SerpApi.credentials.ts
  ab56494793bedf8636d5dc511ff678deaa76e2d1dabadca70412a3ac4f413ab9
```

**Pinned provider-contract evidence (immutable, behavior-only):**

```text
repository: https://github.com/serpapi/serpapi-python
commit: f0cc2fea09bab5884825cbb7bd74f845c8713ea6
git_tree: 8226e5b0a1a7500afc7876232447451c411aa59d
license: MIT
LICENSE:
  736f1be87b07e93f70a7d98e0428c250b0a18283b5de62f2b4da2b4778086293
README.md:
  8c9a18b072a22c641124945dc30307c799f2b34244b98b899f777cd22bc5e883
serpapi/core.py:
  f3f25be8053323dfc5ab58f841c626bac4eb1ecbf06b4ff2616481feee0aae6a
serpapi/http.py:
  d9fb4f908ce472d2a8e05bef41f7493dbfd710e37c78eef305c9dcd98399a845
tests/example_search_google_test.py:
  f63b1953fbd43552795f1545e298a495f3b2c08d15d584e01ffc570f302f6c8d
```

This official SerpApi repository is an immutable `ProviderArtifact` used
behavior-only; no provider wrapper code or fixture is copied or translated.
The record pins exact fact locations and normalized facts:

- `serpapi/core.py:34-77`: Google-capable provider path `/search`, `GET`,
  JSON default, and typed result construction;
- `serpapi/http.py:11-69`: fixed `https://serpapi.com`, API-key query binding,
  and generic HTTP-error propagation;
- `tests/example_search_google_test.py:6-14`: a successful Google query has
  `organic_results`;
- `README.md:48-70`: `400` missing-parameter, `401` authentication, and `429`
  throughput/account-limit examples.

The mutable pages at `https://serpapi.com/search-api`,
`https://serpapi.com/organic-results`, and
`https://serpapi.com/api-status-and-error-codes` are discovery links only.
Their dynamic HTML is neither hashed nor admitted. Missing
`/organic_results` becomes an empty list as a Donat-owned totalization, not a
provider promise. Exact `[200]` success, rejection of a `200` body containing
a top-level `error`, and generic `403`/`5xx` normalization are conservative
Donat-owned restrictions under Task 8; `requests.raise_for_status()` is not
misrepresented as an exact-200 check. No other provider-specific response or
error assumption is admitted.

The donor `nodes/SerpApi/SerpApi.node.ts` fact is the sole authority for the
literal `/search.json` path used by the normalized operation. The provider
`/search` fact proves compatible method/base-origin behavior only.

`serpapi-search-provider-v1.yaml` uses the strict `ProviderArtifact` subject
and owns these immutable provider facts. `serpapi-0.1.10.yaml` uses
`ExactNpm` and references that provider-contract record; one source record
never pretends to have two subjects.

- [ ] **Step 1: Acquire exact bytes into ignored quarantine**

This is the explicit networked review step:

```bash
quarantine=.donat/connector-quarantine/serpapi-0.1.10
test ! -e "$quarantine"
npm_review_dir="$quarantine/npm"
provider_review_dir="$quarantine/provider"
cargo run -p donat-connector-acquire -- acquire-npm-review \
  --artifact-url \
  https://registry.npmjs.org/n8n-nodes-serpapi/-/n8n-nodes-serpapi-0.1.10.tgz \
  --expected-integrity \
  sha512-E9tAU4c9mhNWr07s6RGeqzyrlQO8y42YvtMjPWuLf+tIEM8muU/RIgtp+ojhaoNVCP+jfrwmsSC75OIuoMVS9A== \
  --repository-url https://github.com/serpapi/n8n-nodes-serpapi.git \
  --commit e48b778878c043f30277b932c4c129804efee66d \
  --output "$npm_review_dir"
cargo run -p donat-connector-acquire -- acquire-provider-review \
  --repository-url https://github.com/serpapi/serpapi-python.git \
  --commit f0cc2fea09bab5884825cbb7bd74f845c8713ea6 \
  --output "$provider_review_dir"
```

Inspect the quarantine inventory, donor source tree, tarball, provider source
tree, both repository trees, every admitted file hash, registry signature and
provenance metadata, tag/provenance commits, maintainers, repository-owner
mapping, both licenses, scripts, dependencies, and embedded material. Nothing
under
`.donat/connector-quarantine/` is staged.

- [ ] **Step 2: Add the Donat-owned RED admission tests**

Add exact tests:

- `serpapi_record_matches_pinned_commit_tree_and_package`;
- `serpapi_record_matches_every_admitted_file_hash`;
- `serpapi_license_and_notice_are_complete`;
- `serpapi_dependency_dispositions_are_closed`;
- `serpapi_n8n_workflow_is_type_only_replaced`;
- `serpapi_npm_signature_provenance_and_owner_decisions_are_exact`;
- `serpapi_provider_repository_covers_only_pinned_protocol_facts`;
- `serpapi_success_body_and_unmapped_status_normalization_is_donat_owned`;
- `serpapi_npm_record_references_matching_provider_contract`;
- `serpapi_record_is_not_executable_without_manifest`.

The test owns expected identities and hashes in Rust; it does not trust values
parsed from the record itself.

- [ ] **Step 3: Run RED before creating the record**

```bash
cargo test -p donat-connector-acquire --test serpapi_admission
```

Expected: the test fails because
`sources/records/serpapi-0.1.10.yaml`,
`sources/records/serpapi-search-provider-v1.yaml`, and the normalized
provider contract are absent.

- [ ] **Step 4: Create the complete provider and donor pre-port records**

Record `n8n-workflow: "*"` as `TypeOnlyReplaced`; record all other package,
embedded, and tool dependencies with a closed disposition. Retain the exact
reviewed `NpmSignatureDecision`, the verified absence of an npm provenance
statement, optional tag/provenance commits, maintainer set, and
repository-owner decision. Approve only Google search descriptor inventory,
the donor-proven `/search.json` path, fixed `https://serpapi.com` provider
origin, compatible provider `GET /search` behavior, query encoding, JSON
decoding, static API-key credential shape, the `/organic_results` example,
and the exact `400`/`401`/`429` examples. Record exact `[200]` success,
top-level-error rejection on a `200` body, missing-as-empty, and generic
`403`/`5xx` normalization separately as typed
`ContractFact::DonatPolicy` values, never provider evidence. Classify any
imperative/function-valued source as a reviewed work item. Use the complete
Task-3 `ExactNpm` structure, Tier A compatibility, proposed
manifest/destinations, notice identity, repository mapping, and
`ApprovedForPort { operations: [search.google] }` state only after the exact
RED tests from Step 2 exist. Approval authorizes the narrow port; it does not
make anything executable without a matching generated manifest.

Create the strict `ProviderArtifact` record and normalized provider contract
first, then create the `ExactNpm` donor record that references them. The
provider record contains no npm fields; the donor record cannot inline or
rewrite the provider facts. Mark the provider record
`EvidenceAccepted { contracts: [serpapi.search.v1] }`; that state authorizes
only the fact contract, not executable catalog entry generation.

- [ ] **Step 5: Add the register pre-port row**

Add separate linked admission rows for the donor and provider records. Record
the exact donor commit/tree, npm integrity, five per-file hashes, MIT license
hash and notice, proposed destinations, RED test IDs, the provider repository
commit/tree, five provider file hashes and fact IDs, the Donat-owned
normalization facts, and reviewer. Record proposed destinations without
creating a derivative destination, generated file, runtime adapter, notice
bundle, or executable manifest in this task.

- [ ] **Step 6: Check the record, then verify it against quarantined bytes**

```bash
cargo test -p donat-connector-acquire --test serpapi_admission
cargo run -p donat-connector-acquire -- check-record \
  --record crates/connector-catalog/sources/records/serpapi-0.1.10.yaml
cargo run -p donat-connector-acquire -- check-record \
  --record \
  crates/connector-catalog/sources/records/serpapi-search-provider-v1.yaml
cargo run -p donat-connector-acquire -- verify \
  --record crates/connector-catalog/sources/records/serpapi-0.1.10.yaml \
  --artifact .donat/connector-quarantine/serpapi-0.1.10/npm/package.tgz \
  --source-tree .donat/connector-quarantine/serpapi-0.1.10/npm/source
cargo run -p donat-connector-acquire -- verify \
  --record \
  crates/connector-catalog/sources/records/serpapi-search-provider-v1.yaml \
  --artifact \
  .donat/connector-quarantine/serpapi-0.1.10/provider/provider-source.tar \
  --source-tree \
  .donat/connector-quarantine/serpapi-0.1.10/provider/source
cargo test -p donat-connector-catalog source_record
```

Expected: `check-record` proves only internal consistency; `verify` recomputes
the npm integrity, both Git trees, and every donor/provider source and license
hash from explicit local bytes. The donor record is approved only for the
narrow port, the provider record is accepted only for its fact contract, no
manifest is executable yet, and no donor or provider-wrapper script or
JavaScript executes.

- [ ] **Step 7: Commit admission separately**

```bash
git add crates/connector-catalog/sources/records/serpapi-0.1.10.yaml \
  crates/connector-catalog/sources/records/serpapi-search-provider-v1.yaml \
  crates/connector-catalog/sources/provider-contracts/serpapi-search-v1.yaml \
  crates/connector-acquire/tests/serpapi_admission.rs \
  knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "docs(connectors): admit pinned SerpAPI source"
```


---

### Task 11: Port the admitted SerpAPI search slice

**Files:**

- Create: `crates/connector-catalog/manifests/serpapi-0.1.10.yaml`
- Generate: `crates/connector-catalog/src/generated/serpapi.rs`
- Generate: `crates/connector-catalog/src/generated/mod.rs`
- Generate: `crates/connector-catalog/src/generated/catalog.digest`
- Create: `crates/connector-codegen/tests/serpapi_compile.rs`
- Create: `crates/connector-codegen/tests/snapshots/serpapi_compile__canonical_ir.snap`
- Modify: `crates/connector-codegen/src/render.rs`
- Create: `crates/metadata/tests/fixtures/connectors/serpapi.yaml`
- Create: `crates/server/tests/connectors_serpapi.rs`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/metadata.yaml`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/query.graphql`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/expected.json`
- Create: `THIRD_PARTY_NOTICES.md`
- Create: `policy/connector-legal-notices.toml`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/server/src/connectors/catalog.rs`
- Modify: `crates/conformance/tests/connectors.rs`
- Modify: `knowledgebase/declarative-saas/reference-porting-register.md`

**Immutable port pin (must byte-match the accepted Task-10 record):**

```text
commit/tree:
  e48b778878c043f30277b932c4c129804efee66d
  6916ca97c870b5045200a207dfaf9cb40341f24d
n8n-nodes-serpapi@0.1.10 integrity:
  sha512-E9tAU4c9mhNWr07s6RGeqzyrlQO8y42YvtMjPWuLf+tIEM8muU/RIgtp+ojhaoNVCP+jfrwmsSC75OIuoMVS9A==
LICENSE.md:
  053cb0df9afcf71ac340bdddccb3c25b280a8645e64ba93a709ebc0fbe0f4e35
package.json:
  f90fa3ff0d4c3b5353f26a6785d30119c3bcde890c0982006266b8947f158031
nodes/SerpApi/SerpApi.node.ts:
  f2bfb8abbc3ea84d21da4d557a7553b34f7325236b8367902f9805b5f39cd58f
nodes/SerpApi/descriptions/GoogleSearchDescription.ts:
  c286005eb9183bbd12b59cf6fcce07c9636fc1d68d52142d99bbc2aca3dbc87a
credentials/SerpApi.credentials.ts:
  ab56494793bedf8636d5dc511ff678deaa76e2d1dabadca70412a3ac4f413ab9
license/notice:
  MIT; Copyright 2022 n8n
```

**Normalized operation:**

```yaml
id: search.google
version: 1
effect: read_only
input:
  query: { type: "string!", min_length: 1, max_length: 512 }
steps:
  - id: search
    method: GET
    origin: { scheme: https, host: serpapi.com, port: 443 }
    path: /search.json
    query:
      engine: { literal: google }
      q: { input: query }
      api_key: { credential: api_key }
    success_statuses: [200]
output:
  results:
    type: "[json!]!"
    from_json_pointer: /organic_results
    missing: empty_list
bounds:
  calls: 1
  pages: 1
  response_bytes: 1048576
  items: 100
  output_canonical_bytes: 262144
```

It is headerless `ReadOnly`; caller input cannot select `engine`, origin,
path, query keys, raw output, or pagination.

- [ ] **Step 1: Add Donat-owned RED behavior tests before derivative files**

Create the test files first, without a manifest, generated entry, runtime
adapter, or notice. Add exact tests:

- `serpapi_exact_source_compiles`;
- `serpapi_manifest_requires_matching_provider_record_and_facts`;
- `serpapi_canonical_ir_matches_reviewed_contract`;
- `serpapi_prepared_request_is_fixed_and_headerless`;
- `serpapi_query_is_encoded_once`;
- `serpapi_result_selects_only_organic_results`;
- `serpapi_error_map_is_closed_and_redacted`;
- `serpapi_credential_is_resolved_per_use`;
- `serpapi_has_no_n8n_runtime_dependency`;
- `serpapi_startup_is_static_and_non_secret`.

The server test uses an injected Donat-owned transport stub while retaining
the compiled `https://serpapi.com:443` origin identity.

- [ ] **Step 2: Run RED and retain its diagnostic**

```bash
cargo test -p donat-connector-codegen --test serpapi_compile
cargo test -p donat-server --test connectors_serpapi
```

Expected: both fail because no admitted executable SerpAPI manifest/generated
entry exists. Record these exact test IDs in the source record and port
register before creating derivative files.

- [ ] **Step 3: Add one normalized manifest and generate Rust**

Translate only donor fields covered by Task 10's five source hashes and
provider facts covered by its exact immutable official-repository commit,
tree, file hashes, and fact locations. Cite the donor record for the literal
`/search.json` path: the pinned donor proves that suffix. Cite the provider
record only for compatible `GET` semantics, fixed
`https://serpapi.com`, its provider-owned `/search` location, API-key binding,
JSON behavior, the `/organic_results` example, and `400`/`401`/`429`
examples; never attribute `/search.json` to the provider record. Exact
`[200]` success, top-level-error rejection on a `200` body, missing-as-empty,
and generic `403`/`5xx` normalization use typed
`ContractFact::DonatPolicy` entries. Put the source-record ID,
provider-record ID, both full commits/trees, package integrity, contributing
source paths/hashes, typed provider-evidence and Donat-policy origins,
license/notice ID, RED tests, and destination paths in the manifest header.
Reject any display name, icon, UI condition, workflow item, expression,
function body, JavaScript, arbitrary engine, or arbitrary request field.

Render to a temporary directory and review before updating checked-in output:

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

After inspecting every changed path/byte, run:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
cargo run -p donat-connector-codegen -- generate --check
```

Never edit `serpapi.rs`, `mod.rs`, or `catalog.digest` manually.

- [ ] **Step 4: Complete notices and per-file port records**

Copy the admitted MIT license/notice text into
`THIRD_PARTY_NOTICES.md`. Add one port-register subsection for each translated
source file:

```text
nodes/SerpApi/SerpApi.node.ts
nodes/SerpApi/descriptions/GoogleSearchDescription.ts
credentials/SerpApi.credentials.ts
```

Each subsection repeats commit, source SHA-256, destination manifest/generated
path, adaptation boundary, exact RED test, reviewed generation command, and
reviewer/date. `package.json` and `LICENSE.md` remain provenance inputs rather
than translated code and retain their admission entries.

Compute the complete notice bundle's SHA-256 after review and record the exact
lower-case 64-hex digest with path `THIRD_PARTY_NOTICES.md` in
`policy/connector-legal-notices.toml`. The policy allows the whole file only
when that exact hash matches; it does not allow an `n8n` package, source path,
symbol, or dependency identity.

- [ ] **Step 5: Wire metadata and internal direct execution**

Accept the Task-7 `module: serpapi` instance, endpoint/credential identities,
enabled `search.google` operation, and API-key reference. Do not add a
connector metadata `source`; the future Spec 005 source binding is distinct.
Execution remains an internal registry method used only by Rust tests; add no
route, GraphQL field, REST endpoint, MCP tool, command syntax, or process
descriptor. The shared executor performs the fixed request and validates the
typed result.

- [ ] **Step 6: Run GREEN and review every snapshot**

```bash
cargo test -p donat-connector-codegen --test serpapi_compile
cargo test -p donat-connector-catalog
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen serpapi
cargo insta review
cargo test -p donat-metadata serpapi
cargo test -p donat-server --test connectors_serpapi
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors serpapi_startup
```

- [ ] **Step 7: Prove SUL absence**

```bash
if rg -n "n8n-workflow|IExecuteFunctions|INodeType|pairedItem|\\$node|\\$workflow" \
  crates/connector-catalog/manifests/serpapi-0.1.10.yaml \
  crates/connector-catalog/src/generated/serpapi.rs \
  crates/server/tests/connectors_serpapi.rs; then
  exit 1
fi
notice_sha256=$(sha256sum THIRD_PARTY_NOTICES.md | cut -d' ' -f1)
rg -q "sha256 = \"${notice_sha256}\"" policy/connector-legal-notices.toml
cargo tree -p donat-server --target all --edges normal \
  --offline --locked
```

Expected: the inverted source scan exits zero with no SUL API match, the exact
legal notice hash is allowlisted, and the runtime tree contains no npm, n8n
runtime dependency, Node, JavaScript, or donor package.

- [ ] **Step 8: Commit the derivative port**

```bash
git add crates/connector-catalog crates/connector-codegen \
  crates/metadata/tests crates/server/src/connectors/catalog.rs \
  crates/server/tests/connectors_serpapi.rs crates/conformance \
  THIRD_PARTY_NOTICES.md policy/connector-legal-notices.toml \
  knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "feat(connectors): add admitted SerpAPI search"
```


---

### Task 12: Add bounded generic pagination inside one operation

**Files:**

- Create: `crates/connector-catalog/tests/pagination_plan.rs`
- Create: `crates/connector-catalog/tests/fixtures/pagination-cross-origin.yaml`
- Create: `crates/connector-catalog/tests/fixtures/pagination-over-engine-limit.yaml`
- Create: `crates/server/src/connectors/pagination.rs`
- Create: `crates/server/tests/connector_pagination.rs`
- Modify: `crates/connector-catalog/src/model.rs`
- Modify: `crates/connector-catalog/src/lib.rs`
- Modify: `crates/connector-catalog/manifests/donat-owned-http-v1.yaml`
- Generate: `crates/connector-catalog/src/generated/donat_owned_http.rs`
- Generate: `crates/connector-catalog/src/generated/mod.rs`
- Generate: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/connector-codegen/src/render.rs`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Create:
  `crates/connector-codegen/tests/snapshots/deterministic_catalog__pagination.snap`
- Modify: `crates/server/src/connectors/executor.rs`
- Modify: `crates/server/src/connectors/mod.rs`

**Closed plan:**

```rust
pub enum PaginationPlan {
    None,
    Cursor(CursorPlan),
    OffsetLimit(OffsetLimitPlan),
    PageNumber(PageNumberPlan),
    LinkRelation(LinkRelationPlan),
    Processor {
        family: ProcessorFamilyId,
        version: u16,
    },
}
```

Each non-`None` branch declares smaller-or-equal maxima within 16 calls, 16
pages, 10,000 items, 1 MiB per response, 4 MiB aggregate response bodies, and
256 KiB canonical normalized output.

- [ ] **Step 1: Write RED catalog and local-provider tests**

Add exact tests:

- `pagination_plan_is_closed_and_strict`;
- `pagination_limits_cannot_exceed_engine_ceilings`;
- `cursor_offset_and_page_bind_only_compiled_slots`;
- `relative_and_same_origin_link_relation_are_accepted`;
- `cross_origin_next_is_rejected_before_dns`;
- `pagination_is_bounded_by_calls_pages_items_bytes_and_deadline`;
- `pagination_uses_one_shared_logical_attempt_budget`;
- `pagination_overflow_is_not_partial_success`;
- `pagination_processor_cannot_return_a_url_or_method`;
- `pagination_has_no_process_checkpoint_or_static_workflow_state`.

Use a Donat-owned manifest and Axum provider payloads. Do not claim SerpAPI
pagination behavior without another admitted immutable source record.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-catalog --test pagination_plan
cargo test -p donat-server --test connector_pagination
```

Expected: the catalog has no `PaginationPlan` and the server test target is
absent.

- [ ] **Step 3: Compile the closed plan fail-closed**

Reject zero, contradictory, over-ceiling, undeclared binding, dynamic pointer,
dynamic header, and URL-shaped processor results. A `LinkRelation` accepts
only a relative reference or the exact already compiled HTTPS origin; every
follow-up call repeats DNS/address/peer validation and redirects remain zero.

- [ ] **Step 4: Execute within the original attempt budget**

Charge every page, call, request/response byte, item, auth action, and codec
action to the same `OperationAttempt`. Check cancellation/deadline before and
after extraction and every call. On overflow return:

```text
class: validation
code: connector_execution_limit_exceeded
```

Discard accumulated output; never truncate or return partial success. Store
no cursor, checkpoint, lease, retry, or workflow static data.

- [ ] **Step 5: Regenerate and run GREEN**

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

Review the renderer, manifest, snapshot, generated Rust, and digest diff.
Then update only through the generator:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
cargo test -p donat-connector-catalog --test pagination_plan
cargo test -p donat-server --test connector_pagination
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen
cargo insta review
```

- [ ] **Step 6: Commit the bounded pagination slice**

```bash
git add crates/connector-catalog crates/connector-codegen/src/render.rs \
  crates/connector-codegen/tests crates/server/src/connectors \
  crates/server/tests/connector_pagination.rs
git commit -m "feat(connectors): add bounded compiled pagination"
```


---

### Task 13: Prove the Stripe processor boundary without changing runtime routing

**Files:**

- Create: `crates/connector-processors/src/stripe_checkout.rs`
- Create: `crates/connector-processors/tests/stripe_checkout.rs`
- Modify: `crates/connector-processors/src/registry.rs`

**Proof boundary:**

- This task is processor-only. It creates no `OperationSpec`, manifest,
  generated catalog entry, server route, credential mapping, or transport
  adapter.
- The current server-owned Stripe adapter, static inventory entry, and all
  existing request/result/error tests remain byte-for-byte unchanged.
- Independently authored Donat behavior is the oracle. No n8n Stripe source,
  fixture, description, helper, workflow mechanic, or runtime type is used.
- The processor accepts typed input, binds only the ABI-owned
  `CompiledStepId` for `create_session`, calls a fake `ConnectorIo` in tests,
  and normalizes `id`, `url`, `status`, and `expires_at`. It cannot construct
  transport, credentials, headers, retry state, or process state.

- [ ] **Step 1: Add the processor-only RED tests**

Add exact tests:

- `stripe_processor_preserves_donat_owned_input_and_result_contract`;
- `stripe_processor_calls_only_create_session_step`;
- `stripe_processor_passes_exact_copy_abi_ids_without_conversion`;
- `stripe_processor_cannot_construct_transport_or_credentials`;
- `stripe_processor_observes_control_checks`;
- `stripe_processor_registry_is_private_and_static`;
- `stripe_runtime_adapter_is_unchanged_by_processor_proof`.

The last test is a repository-structure assertion over the pre-existing
runtime registration files. The processor test uses only fake `ConnectorIo`
and Donat-owned values; it must not depend on `donat-server`.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-processors --test stripe_checkout
cargo test -p donat-connector-processors
```

Expected: the processor family and private registry entry do not exist.
Existing server tests remain green because this task does not touch their
path:

```bash
cargo test -p donat-server --test connectors_stripe
```

- [ ] **Step 3: Implement the narrow processor only**

Implement pure input validation, exact compiled-step bindings, and result
normalization. Register the sealed processor family/version in the private
processor lookup. Preserve the exact ABI-owned `Copy` typed IDs from the
static lookup through `ConnectorIo`; do not parse or allocate an ID bridge.
Do not edit `crates/server`, `crates/connector-catalog`, metadata, or
conformance fixtures.

- [ ] **Step 4: Run GREEN and boundary checks**

```bash
cargo test -p donat-connector-processors --test stripe_checkout
cargo test -p donat-connector-processors
cargo test -p donat-server --test connectors_stripe
python3 scripts/check_connector_processor_boundary.py
```

- [ ] **Step 5: Commit only the processor proof**

```bash
git add crates/connector-processors/src/stripe_checkout.rs \
  crates/connector-processors/src/registry.rs \
  crates/connector-processors/tests/stripe_checkout.rs
git commit -m "feat(connectors): prove Stripe processor boundary"
```


---

### Task 14: Admit immutable Stripe idempotency evidence or stop

**Files:**

- Create on acceptance:
  `crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml`
- Create on acceptance:
  `crates/connector-catalog/sources/provider-contracts/stripe-idempotency-v1.yaml`
- Create: `crates/connector-catalog/tests/stripe_provider_evidence.rs`
- Create:
  `crates/connector-catalog/tests/fixtures/stripe-mutable-evidence-rejected.yaml`
- Modify on acceptance:
  `knowledgebase/declarative-saas/reference-porting-register.md`

**Hard gate:**

This task must finish with an `EvidenceAccepted` provider-artifact source
record before Task 15 starts. It creates no executable operation or server
routing.
If immutable provider evidence cannot prove every required fact, the task
leaves Stripe inventory-only, records the rejected candidate in test
fixtures rather than the accepted register, and the implementation plan
stops before Task 15. The successful Task 13 processor proof remains inert.

The accepted record pins all four contract values:

```yaml
fixed_binding: { header: Idempotency-Key }
scope: stripe.account.v1
minimum_retention_ms: 86400000
clock_safety_margin_ms: 300000
```

The header, account scope, and minimum retention require Stripe-authored
evidence. The positive clock margin is a Donat-owned conservative policy
recorded as
`ContractFact::DonatPolicy { policy_id: stripe.clock_margin.v1, ... }`; it
must not be misattributed to Stripe or accepted in place of any required
Stripe-authored fact.

- [ ] **Step 1: Add strict RED evidence tests**

Add exact tests:

- `stripe_provider_contract_rejects_mutable_locator_only`;
- `stripe_provider_contract_requires_exact_header_scope_and_retention`;
- `stripe_provider_contract_records_positive_donat_clock_margin`;
- `stripe_donat_policy_cannot_satisfy_provider_fact_requirement`;
- `stripe_provider_contract_hashes_complete_evidence_bytes`;
- `stripe_provider_contract_is_offline_reverifiable`;
- `stripe_operation_spec_requires_accepted_provider_contract`;
- `stripe_unaccepted_provider_contract_remains_inventory_only`.

The tests use the closed `ProviderArtifact` source-record variant from Task
3. A test assertion, existing adapter behavior, blog post, mutable page
locator, or OpenAPI endpoint shape cannot substitute for retention evidence.

- [ ] **Step 2: Record candidate evidence identities without admitting them**

The acquisition review starts from these exact behavior-only identities:

```text
Stripe OpenAPI:
  repository: https://github.com/stripe/openapi
  commit: 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69
  source: openapi/spec3.json
  sha256: e24a26de4188fd64dec4c043d5d3726277fdcb07556a493ea481c305b0a223d8
  license: MIT

Stripe idempotent requests page:
  url: https://docs.stripe.com/api/idempotent_requests?lang=curl
  accessed_utc: 2026-07-29

Stripe low-level errors page:
  url: https://docs.stripe.com/error-low-level?locale=en-GB
  accessed_utc: 2026-07-29
```

The two documentation URLs are mutable candidate locators. Their current
bytes are deliberately not assigned a reproducible-hash or admission claim.
They are outside Task 4's checked-in network host policy and are not fetched
by this plan. The rejection fixture uses independently authored synthetic
bytes and those locator strings to prove that mutable HTML plus access date
cannot satisfy an immutable source identity.
An accepted record must add a Stripe-owned immutable revision or immutable
provider repository on an allowed host, complete-byte SHA-256, exact fact
locations, retrieval date, terms/redistribution disposition, and reviewer.
Task 14 accepts only the repository form so clean-worktree reacquisition has
one exact output layout. Do not check copied Stripe HTML into the repository.

- [ ] **Step 3: Acquire allowed immutable inputs; reject mutable pages offline**

Acquire the already pinned OpenAPI repository through the provider-only
schema:

```bash
stripe_openapi_dir=.donat/connector-quarantine/stripe-openapi-6dfda253
test ! -e "$stripe_openapi_dir"
cargo run -p donat-connector-acquire -- acquire-provider-review \
  --repository-url https://github.com/stripe/openapi.git \
  --commit 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69 \
  --output "$stripe_openapi_dir"
```

The command never contacts `docs.stripe.com`. Test the two mutable locator
strings only with
`stripe-mutable-evidence-rejected.yaml` and synthetic local bytes. Inspect the
OpenAPI bytes for the exact header/scope/retention facts; endpoint shape alone
is insufficient. If no allowed-host immutable Stripe repository proves all
three provider facts, keep the negative test green and stop.

If review does find a qualifying immutable repository, write its exact
identity, tree, file hashes, fact locations, and terms into the proposed
`EvidenceAccepted` record. Reacquire solely from that record into the
standard repository-provider layout:

```bash
stripe_review_dir=.donat/connector-quarantine/stripe-idempotency-v1
test ! -e "$stripe_review_dir"
cargo run -p donat-connector-acquire -- reacquire-reviewed \
  --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
  --output "$stripe_review_dir"
cargo run -p donat-connector-acquire -- verify \
  --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
  --artifact "$stripe_review_dir/provider-source.tar" \
  --source-tree "$stripe_review_dir/source"
```

`verify` must fail on any byte, fact location, source identity, selected
terms, or disposition mismatch. `check-record` is syntax-only and cannot
admit the contract. The exact `reacquire-reviewed --record ... --output ...`
command is recorded in the accepted source record and port-register entry.

- [ ] **Step 4: Run the acceptance gate**

```bash
cargo test -p donat-connector-catalog --test stripe_provider_evidence
cargo run -p donat-connector-acquire -- check-record \
  --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml
cargo run -p donat-connector-acquire -- verify \
  --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
  --artifact "$stripe_review_dir/provider-source.tar" \
  --source-tree "$stripe_review_dir/source"
```

Expected on acceptance: all facts and complete evidence bytes verify and the
record is `EvidenceAccepted`. Otherwise, keep the negative inventory test
green and stop; do not create an approximation of Task 15.

- [ ] **Step 5: Commit the evidence result without crossing the gate**

On acceptance, commit the exact record, normalized provider contract, tests,
rejected-candidate fixture, and register entry:

```bash
git add crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
  crates/connector-catalog/sources/provider-contracts/stripe-idempotency-v1.yaml \
  crates/connector-catalog/tests/stripe_provider_evidence.rs \
  crates/connector-catalog/tests/fixtures/stripe-mutable-evidence-rejected.yaml \
  knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "docs(connectors): admit Stripe idempotency contract"
```

Record this accepted commit hash as Task 15's non-negotiable prerequisite.
If acceptance fails, commit only the negative fixture/test that keeps Stripe
inventory-only:

```bash
git add crates/connector-catalog/tests/stripe_provider_evidence.rs \
  crates/connector-catalog/tests/fixtures/stripe-mutable-evidence-rejected.yaml
git commit -m "test(connectors): keep Stripe inventory-only without evidence"
```

Then stop this plan. That negative commit is not an accepted evidence commit
and can never satisfy Task 15's precondition.


---

### Task 15: Migrate Stripe transport after the evidence gate

**Files:**

- Create: `crates/connector-catalog/manifests/stripe-checkout-v1.yaml`
- Generate: `crates/connector-catalog/src/generated/stripe_checkout.rs`
- Create: `crates/connector-catalog/tests/stripe_admission.rs`
- Modify: `crates/connector-codegen/src/render.rs`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Create: `crates/connector-codegen/tests/stripe_checkout.rs`
- Create:
  `crates/connector-codegen/tests/snapshots/stripe_checkout__canonical_ir.snap`
- Generate: `crates/connector-catalog/src/generated/mod.rs`
- Generate: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/src/connectors/catalog.rs`
- Modify: `crates/server/tests/connectors_stripe.rs`

**Precondition:**

Task 14 has one reviewed `EvidenceAccepted` source record and commit proving
the exact four-value contract. Verify and record that commit before RED. If
the record is absent, inventory-only, mutable-locator-only, or fails offline
verification, do not start this task.

Only this task may create Stripe's executable `OperationSpec`, route
`create_session` through shared `ConnectorIo`, and delete the old duplicate
Stripe transport. The server retains fixed `https://api.stripe.com`, form
encoding, authorization and idempotency header materialization, credentials,
reqwest, DNS/peer checks, JSON codec, UUID/text/time, crypto, clock, deadline,
and control.

- [ ] **Step 1: Verify the evidence precondition**

```bash
accepted_stripe_evidence_commit=$(git log -1 --format=%H -- \
  crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml)
test -n "$accepted_stripe_evidence_commit"
git merge-base --is-ancestor "$accepted_stripe_evidence_commit" HEAD
stripe_review_dir=.donat/connector-quarantine/stripe-idempotency-v1
if ! test -d "$stripe_review_dir"; then
  cargo run -p donat-connector-acquire -- reacquire-reviewed \
    --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
    --output "$stripe_review_dir"
fi
test -f "$stripe_review_dir/provider-source.tar"
test -d "$stripe_review_dir/source"
cargo test -p donat-connector-catalog --test stripe_provider_evidence
cargo run -p donat-connector-acquire -- verify \
  --record crates/connector-catalog/sources/records/stripe-idempotency-v1.yaml \
  --artifact "$stripe_review_dir/provider-source.tar" \
  --source-tree "$stripe_review_dir/source"
```

- [ ] **Step 2: Add RED migration and unchanged-oracle tests**

Add exact tests:

- `stripe_operation_requires_accepted_matching_provider_contract`;
- `stripe_operation_compiles_exact_effect_values`;
- `stripe_generated_ids_are_const_copy_abi_values`;
- `stripe_routes_create_session_through_connector_io`;
- `stripe_legacy_transport_is_absent_after_migration`;
- `stripe_existing_form_header_result_and_error_tests_remain_unchanged`;
- `stripe_processor_calls_connector_io_once`;
- `stripe_migration_adds_no_process_dispatch_or_retry_state`.

Run:

```bash
cargo test -p donat-connector-catalog --test stripe_admission
cargo test -p donat-server --test connectors_stripe
```

Expected: the accepted evidence exists, but no executable Stripe operation or
shared-executor route exists and the old adapter still owns transport.

- [ ] **Step 3: Add the evidence-bound manifest**

Compile exactly:

```yaml
id: checkout.create_session
version: 1
effect:
  provider_idempotent:
    side_effect_steps:
      - step: create_session
        fixed_binding: { header: Idempotency-Key }
        scope: stripe.account.v1
        minimum_retention_ms: 86400000
        clock_safety_margin_ms: 300000
processor: stripe.checkout.create_session.v1
```

The manifest cites both the matching Donat-owned Stripe source record from
Task 9 and the accepted provider-contract record from Task 14. Codegen
rejects any value not exactly supported by those records.

- [ ] **Step 4: Render to a temporary directory and review**

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

Review the manifest, renderer, renderer tests, snapshots, generated Rust, and
digest. Update generated output only through the generator:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen
cargo insta review
```

Never hand-edit `stripe_checkout.rs`, `mod.rs`, or `catalog.digest`.

- [ ] **Step 5: Route through the shared executor and remove old transport**

Move only pure validation/binding/result normalization to the Task 13
processor. Route the accepted operation through shared `ConnectorIo`; then
delete Stripe's duplicate reqwest/DNS/peer implementation. Preserve exact
form ordering, header behavior, result fields, error classes/codes, one-call
bound, and redaction. Add no first-attempt timestamp, retry/takeover path,
idempotency-window assumption, process dispatch, or public execution route.

- [ ] **Step 6: Run GREEN and the unchanged behavior oracle**

```bash
cargo test -p donat-connector-catalog --test stripe_admission
cargo test -p donat-connector-processors --test stripe_checkout
cargo test -p donat-server --test connectors_stripe
cargo test -p donat-server --test connector_executor
cargo run -p donat-connector-codegen -- generate --check
python3 scripts/check_connector_processor_boundary.py
```

- [ ] **Step 7: Commit the executable migration**

```bash
git add crates/connector-catalog/manifests/stripe-checkout-v1.yaml \
  crates/connector-catalog/src/generated crates/connector-catalog/tests/stripe_admission.rs \
  crates/connector-codegen/src/render.rs crates/connector-codegen/tests \
  crates/server/src/connectors crates/server/tests/connectors_stripe.rs
git commit -m "refactor(connectors): migrate Stripe to compiled steps"
```


---

### Task 16: Generalize two-stage webhook dispatch and retain `503`

**Files:**

- Create: `crates/connector-processors/src/stripe_webhook.rs`
- Create: `crates/connector-processors/tests/stripe_webhook.rs`
- Create: `crates/connector-catalog/manifests/donat-owned-stripe-webhook-v1.yaml`
- Generate: `crates/connector-catalog/src/generated/donat_owned_stripe_webhook.rs`
- Create: `crates/server/src/connectors/webhooks.rs`
- Create: `crates/server/tests/connector_webhook_ordering.rs`
- Modify: `crates/connector-processors/src/registry.rs`
- Modify: `crates/connector-codegen/src/render.rs`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Create: `crates/connector-codegen/tests/stripe_webhook.rs`
- Create:
  `crates/connector-codegen/tests/snapshots/stripe_webhook__canonical_trigger.snap`
- Generate: `crates/connector-catalog/src/generated/mod.rs`
- Generate: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/src/connectors/catalog.rs`
- Modify: `crates/server/src/connectors/codec.rs`
- Modify: `crates/server/src/connector_webhook.rs`
- Modify: `crates/server/tests/connector_webhook.rs`
- Modify: `crates/conformance/tests/connectors.rs`

The manifest must cite the matching `DonatOwned` Stripe source record created
in Task 9. Codegen rejects an unrelated source record. Generated Rust and the
digest remain generator-owned.

**Two-stage boundary:**

```text
bounded raw bytes + selected headers + receipt time + credential capability
  -> sealed WebhookAuthenticator
  -> opaque AuthenticatedRawBody
  -> server-owned JSON codec
  -> sealed pure WebhookNormalizer
  -> VerifiedInboundEvent
  -> exact empty-body 503
```

Only the server codec can consume `AuthenticatedRawBody`. The authenticator
cannot parse JSON or reveal secret/tag bytes; the normalizer never sees
unauthenticated raw bytes.

- [ ] **Step 1: Add RED ordering, provenance, and route-matrix tests**

Add exact tests:

- `webhook_authentication_precedes_codec`;
- `invalid_signature_never_invokes_codec`;
- `authenticated_raw_body_is_opaque_to_processors`;
- `normalizer_receives_only_typed_decoded_value`;
- `webhook_manifest_requires_matching_donat_owned_record`;
- `webhook_lookup_consumes_generated_trigger_without_server_descriptor`;
- `webhook_registry_returns_only_declared_static_trigger`;
- `unknown_or_no_verifier_is_empty_404_before_body_read`;
- `oversized_declared_webhook_is_empty_413`;
- `invalid_or_malformed_webhook_is_empty_400`;
- `verified_webhook_remains_empty_503`;
- `webhook_path_has_no_queue_audit_dedupe_or_process_start`.

Use a codec spy that panics if invoked after failed authentication.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-processors --test stripe_webhook
cargo test -p donat-server --test connector_webhook_ordering
cargo test -p donat-server --test connector_webhook
```

Expected: current lookup returns a concrete Stripe connector and no opaque
authentication token or generic descriptor exists.

- [ ] **Step 3: Split Stripe without moving server capabilities**

Move constant-time signature comparison and pure event normalization behind
sealed processor implementations. Keep bounded body collection, selected
header copying, environment credential resolution, HMAC primitive,
receipt-time clock, timestamp-window enforcement primitive, JSON parsing, and
HTTP status construction server-owned. Register exact authenticator, codec,
and normalizer IDs in the Task-5 `GeneratedTriggerSpec`. The registry borrows
that exact generated value; it does not define or populate a server-owned
trigger descriptor.

- [ ] **Step 4: Generalize lookup and preserve the route**

Replace `stripe_webhook_instance` with descriptor lookup by instance and the
single compiled trigger. Keep the route exactly:

```text
POST /v1/connectors/{instance}/webhooks
```

Resolve the instance/verifier before reading the body. Add no trigger-name
wildcard, generic webhook registration, runtime code, subscription lifecycle,
queue, table, audit row, dedupe, correlation, process signal, or success
acknowledgement.

- [ ] **Step 5: Render to a temporary directory and review**

```bash
generated_review_dir=$(mktemp -d)
trap 'rm -rf "$generated_review_dir"' EXIT
cargo run -p donat-connector-codegen -- generate \
  --output "$generated_review_dir"
diff -ru -- crates/connector-catalog/src/generated "$generated_review_dir" || true
```

Review the manifest, renderer, tests, snapshots, generated Rust, and digest.
Then update only through the generator:

```bash
cargo run -p donat-connector-codegen -- generate \
  --output crates/connector-catalog/src/generated
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen
cargo insta review
```

- [ ] **Step 6: Run GREEN and native conformance**

```bash
cargo test -p donat-connector-processors --test stripe_webhook
cargo test -p donat-server --test connector_webhook_ordering
cargo test -p donat-server --test connector_webhook
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors webhook
cargo run -p donat-connector-codegen -- generate --check
python3 scripts/check_connector_processor_boundary.py
```

- [ ] **Step 7: Commit the generic webhook boundary**

```bash
git add crates/connector-processors \
  crates/connector-catalog/manifests/donat-owned-stripe-webhook-v1.yaml \
  crates/connector-catalog/src/generated crates/connector-codegen/src/render.rs \
  crates/connector-codegen/tests crates/server/src/connectors \
  crates/server/src/connector_webhook.rs crates/server/tests \
  crates/conformance/tests/connectors.rs
git commit -m "refactor(connectors): generalize webhook verification"
```


---

### Task 17: Prove the runtime package is static and source-free

**Files:**

- Create: `policy/connector-runtime-package.toml`
- Create: `scripts/check_connector_runtime_package.py`
- Modify: `policy/connector-legal-notices.toml`
- Modify: `crates/conformance/tests/connector_public_surfaces.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`
- Modify: `Dockerfile`
- Modify: `crates/conformance/tests/connectors.rs`

**Policy:**

- Workspace release builds may contain the separate acquisition/codegen
  development tools in `target`; runtime-package proof scans a newly created
  directory containing only the separately built `donat` binary, runtime
  metadata/SBOM, and required license notices.
- The runtime dependency closure contains server, static catalog, ABI,
  processors, and value contract; it excludes acquisition, codegen, donor
  packages/source trees, npm/Node/JavaScript/WASM, n8n/SUL, build scripts,
  dynamic libraries/plugins, runtime discovery/configuration, and every
  workflow/logical-node mechanic.
- `THIRD_PARTY_NOTICES.md` is the sole content exception: the scanner accepts
  that exact regular-file path only when its complete SHA-256 equals the
  lowercase digest in `policy/connector-legal-notices.toml`. It treats the
  matching notice as opaque legal text. The exception does not allow an n8n
  dependency, donor file, source path, package name, SUL text, or workflow
  API anywhere else.

- [ ] **Step 1: Establish the real RED condition**

First run the already-existing public-surface target unchanged:

```bash
cargo test -p donat-conformance --test connector_public_surfaces
```

Expected: the baseline target exists and is green. It is not the RED.

Add the new assertion
`runtime_package_allows_only_hashed_legal_notice` plus fixture cases for:

- the exact whole-file notice hash at exact path, which is accepted;
- a one-byte-tampered notice, renamed notice, second notice, symlink, or
  unlisted notice, which is rejected;
- donor `.ts`, `.js`, `.mjs`, `.cjs`, npm tarball, or `node_modules`;
- acquisition/codegen binary or dependency;
- `.wasm`, unexpected shared library, plugin directory, package URL, or
  runtime registration manifest;
- n8n/SUL code, dependency, source path, workflow-node API, `If`, `Switch`,
  `Merge`, `Code`, `Wait`, loop, item/paired-item, subworkflow, AI-node, or
  send-and-wait material outside the exact hashed notice;
- a binary dependency tree absent from the checked-in policy.

Run the new focused assertion:

```bash
cargo test -p donat-conformance --test connector_public_surfaces \
  runtime_package_allows_only_hashed_legal_notice
python3 scripts/check_connector_runtime_package.py --self-test
```

Expected RED: the new package script/policy and new assertion behavior are
missing. Do not claim that the existing conformance target is absent.

- [ ] **Step 2: Retain negative public-surface coverage**

Keep or add exact conformance tests:

- `public_surfaces_cannot_execute_connectors`;
- `post_connector_execute_is_empty_404`;
- `graphql_has_no_connector_execution_field`;
- `rest_has_no_connector_execution_endpoint`;
- `mcp_has_no_connector_execution_tool`;
- `commands_cannot_plan_connector_io`;
- `webhook_route_preserves_phase1_boundary`.

The route scan is limited to runtime registration sources. The negative
`POST /v1/connectors/{instance}/execute` test is valid evidence and must not
cause the structural scanner to report a public route.

- [ ] **Step 3: Implement separate locked package proof**

The script accepts only explicit paths, creates its own mode-`0700` temporary
directory, runs:

```bash
cargo build -p donat-server --bin donat --release --offline --locked
cargo tree -p donat-server --target all \
  --edges normal --offline --locked
```

It copies only `target/release/donat`, the exact-hash
`THIRD_PARTY_NOTICES.md`, and a canonicalized runtime-tree/SBOM record into
the package directory, then scans that directory and dependency closure
against the closed policy. It never scans or packages the tooling-bearing
workspace `target` directory. The Docker final stage copies the same release
binary and exact notice only.

- [ ] **Step 4: Wire clean offline CI**

Add separate CI jobs:

```bash
cargo build --workspace --release --offline --locked
python3 scripts/check_connector_processor_boundary.py
python3 scripts/check_connector_runtime_package.py --build
cargo run -p donat-connector-codegen -- generate --check
```

The workspace job proves all checked-in dependencies are available offline;
the package job proves the serving artifact is source-free. Release refuses
to upload an artifact or image unless both pass.

- [ ] **Step 5: Run GREEN and full regression**

```bash
python3 scripts/check_connector_runtime_package.py --self-test
python3 scripts/check_connector_runtime_package.py --build
cargo test -p donat-conformance --test connector_public_surfaces
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors
cargo test -p donat-conformance
make test
```

Inspect the package inventory emitted by the script; it must name exactly the
binary, exact-hash notice, and canonical runtime-tree/SBOM record.

- [ ] **Step 6: Commit the release proof**

```bash
git add policy/connector-runtime-package.toml \
  policy/connector-legal-notices.toml \
  scripts/check_connector_runtime_package.py .github/workflows \
  Dockerfile crates/conformance
git commit -m "ci(connectors): prove static source-free runtime package"
```


---

## Explicit Process-Gated Integration Handoff

This plan stops after Task 17. The following work starts only when the named
Spec 005 prerequisite is implemented and green; none belongs in a factory
crate, compatibility adapter, or server-local persistence table:

1. Publish secret-free typed operation/event descriptors and bind one
   connector instance to one process source after Spec 005 Tasks 2–3 and
   `connector_descriptor_is_typed_and_non_secret` plus
   `process_connector_instance_has_one_source`.
2. Compile every side-effect step's complete maximum send horizon after Spec
   005 provides finite schedule, capacity, rate, serialization,
   start-to-close, lease, takeover, backoff, and retry bounds. Equality with
   the usable provider window passes; one millisecond over and every missing
   bound fail.
3. Persist source-local per-step `first_provider_attempt_at`, derive the
   stable step key, and send only a committed job after the V6 journal,
   worker, claim, and source-local activity transaction exist. The provider
   stub must block until a separate Postgres connection observes the committed
   job and applicable capacity reservation.
4. Add retry, lease takeover, global capacity/rate/serialization, and
   idempotency-window refusal only through the Spec 005 journal and database
   clock. No factory-local retry loop, reservation, first-attempt table, or
   assumed retention window is permitted.
5. Change a verified webhook from empty `503` only through the Spec 005
   source-local audit/dedupe/correlation/process-start transaction and its
   separately specified exact success response.
6. Add poll scheduling/checkpoint persistence only after Spec 005 defines its
   missing schema, transaction, restart, locking, and database-clock
   semantics. A pure poll ABI test is not authorization to invent storage.
7. Pin semantic/provenance/configuration hashes into process revisions and
   retain live-retired operation versions only after Spec 005 revision
   reconcile/reload tests exist.
8. Add inline binary/multipart only after `donat-value-contract` implements
   the exact Spec 007 Section 9 base64url/JCS vectors and Spec 005 accepts that
   value in descriptors and journals.

Each integration item receives its own implementation plan, Donat-owned RED
crate/native conformance test, scoped commit, and independent review.

## Plan Self-Review Checklist

- [ ] Tasks execute in dependency order:
  `value -> ABI -> catalog -> acquisition/codegen siblings -> processors ->
  credentials -> executor -> registry -> SerpAPI -> pagination -> Stripe
  processor proof -> Stripe evidence admission -> Stripe executable migration
  -> webhooks -> package proof`.
- [ ] Every donor derivative starts only after an approved immutable
  source/version/tree/artifact/per-file hash, license/notice, destination, RED
  test, reviewer, and register entry.
- [ ] SerpAPI is the sole derivative port in this plan; Stripe is an
  independent Donat implementation plus behavior-only provider references.
- [ ] `n8n-workflow` remains `TypeOnlyReplaced`; inspected n8n built-ins remain
  behavior-only and no `If`/`Switch`/`Merge`/`Code`/`Wait`, loop,
  item/paired-item, subworkflow, AI-node, send-and-wait, or other
  workflow/logical/UI node enters catalog or runtime.
- [ ] Every operation is headerless `ReadOnly` or has evidence for each
  `ProviderIdempotent` side-effect step; unsupported mutation remains
  inventory-only.
- [ ] Only the server owns transport, codec, credential, crypto, time,
  cancellation, DNS/peer validation, and process control.
- [ ] No task adds a public outbound execution route, admin role, runtime
  plugin, arbitrary HTTP request, command effect, or process persistence.
- [ ] Existing exact error classes/codes, HTTP statuses/bodies, one-statement
  data-operation invariant, and no-admin boundary remain unchanged.
- [ ] Every task runs focused RED/GREEN tests, reviewed snapshots, full
  connector conformance after rebuilding `donat`, then creates its scoped
  commit; the completed implementation range receives independent review.
