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
  item/paired-item flow, subworkflows, send-and-wait, UI callbacks, or business
  orchestration. Pagination is bounded transport inside one operation.
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
- Acquisition and codegen are sibling crates over the catalog. Neither is a
  server dependency; codegen never acquires donor bytes.
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
- After Task 15, request one independent code review over the complete
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

Inline binary/multipart, provider continuation URLs, durable polling
checkpoints, and every process journal integration are outside this plan.
Inline bytes remain prohibited until `donat-value-contract` passes the Spec
007 Section 9 acceptance vectors and Spec 005 accepts that value in process
descriptors and journals.

---

### Task 1: Create the single canonical value-contract owner

**Files:**

- Create: `crates/value-contract/Cargo.toml`
- Create: `crates/value-contract/src/lib.rs`
- Create: `crates/value-contract/src/types.rs`
- Create: `crates/value-contract/src/value.rs`
- Create: `crates/value-contract/tests/value_contract.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/ir/Cargo.toml`
- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/ir/tests/ir_structure.rs`

**Interfaces:**

- Consumes: the exact version-1 type grammar and scalar aliases in Spec 005
  Section 2.1.
- Produces:

```rust
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

pub enum ValueType {
    Scalar { scalar: ValueScalar },
    Enum { name: String, values: Vec<String> },
    Object { fields: BTreeMap<String, ValueContractField> },
    List { element: Box<TypeRef> },
    Ref { name: String },
}

pub enum TypedValue {
    Null,
    Boolean(bool),
    String(String),
    Number(CanonicalNumber),
    List(Vec<TypedValue>),
    Object(BTreeMap<String, TypedValue>),
}

pub fn parse_type_ref(input: &str) -> Result<TypeRef, ValueContractError>;
pub fn validate_value(
    contract: &ValueContractCatalog,
    root: &str,
    value: &TypedValue,
) -> Result<(), ValueContractError>;
pub fn canonical_size(value: &TypedValue) -> Result<usize, ValueContractError>;
```

- `donat-ir` publicly re-exports these exact types and defines no duplicate.

- [ ] **Step 1: Add the failing canonical-language and ownership tests**

```rust
#[test]
fn value_type_language_is_closed_and_canonical() {
    assert_eq!(parse_type_ref("[uuid!]!").unwrap().to_string(), "[uuid!]!");
    assert!(parse_type_ref(" [uuid!]!").is_err());
    assert!(parse_type_ref("$unknown").is_err());
}

#[test]
fn value_contract_has_one_owner() {
    fn accepts_ir_type(_: donat_ir::TypeRef) {}
    accepts_ir_type(donat_value_contract::parse_type_ref("string!").unwrap());
}
```

Add structural assertions to `ir_structure.rs` that `donat-ir` depends on and
re-exports `donat-value-contract`, and that the old crate contains no second
`ValueType`, `TypeRef`, or `TypedValue` declaration.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-value-contract --no-default-features
cargo test -p donat-ir value_contract
```

Expected: Cargo reports that package `donat-value-contract` does not exist.

- [ ] **Step 3: Implement the lower crate and IR re-export**

Start `lib.rs` with:

```rust
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod types;
mod value;

pub use types::*;
pub use value::*;
```

Use `alloc::collections::BTreeMap`, checked constructors, exact alias
normalization, UTF-8 lexical object order, checked arithmetic, and no
`serde_json::Value`. Keep metadata/Rule adapters in `donat-ir`.

- [ ] **Step 4: Prove the mechanical no-OS boundary**

Run:

```bash
cargo check -p donat-value-contract --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-value-contract --target all \
  --edges normal,build,no-dev --no-default-features --offline --locked
```

Expected: the target check passes and the normal/build closure contains only
`donat-value-contract`.

- [ ] **Step 5: Run GREEN and regressions**

Run:

```bash
cargo test -p donat-value-contract --no-default-features
cargo test -p donat-ir
cargo test --workspace --no-run
```

Expected: all commands pass.

- [ ] **Step 6: Commit the value-owner slice**

```bash
git add Cargo.toml Cargo.lock crates/value-contract crates/ir
git commit -m "feat(value): add canonical value contract owner"
```


---

### Task 2: Add ABI-owned IDs, bounded envelopes, and host traits

**Files:**

- Create: `crates/connector-abi/Cargo.toml`
- Create: `crates/connector-abi/src/lib.rs`
- Create: `crates/connector-abi/src/ids.rs`
- Create: `crates/connector-abi/src/envelope.rs`
- Create: `crates/connector-abi/src/host.rs`
- Create: `crates/connector-abi/tests/abi_contract.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: `donat_value_contract::TypedValue`.
- Produces exact owned newtypes:

```rust
pub struct ConnectorId(String);
pub struct OperationId(String);
pub struct CompiledStepId(String);
pub struct ProcessorFamilyId(String);
pub struct AuthenticatorId(String);
pub struct CodecId(String);
pub struct NormalizerId(String);
pub struct TriggerId(String);
pub struct CredentialSpecId(String);
pub struct CredentialFieldId(String);
pub struct CapabilityId(String);
pub struct BindingSlotId(String);
pub struct OriginId(String);

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

The same crate owns checked constructors for bounded safe strings/bytes/maps
and these envelopes:

```rust
pub struct TypedBindings {
    slots: BTreeMap<BindingSlotId, TypedValue>,
}

pub struct BoundedTransportResponse {
    pub status: u16,
    pub selected_headers: BTreeMap<CapabilityId, BoundedString>,
    pub decoded: TypedValue,
    pub response_bytes: u32,
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

`ConnectorFailure` contains one class, a bounded Donat-owned code and safe
message, optional clamped retry delay, and allowlisted bounded correlation
IDs. It has no raw body, URL, header, credential, provider message, process
policy, or unbounded collection.

Each ID has `parse(&str) -> Result<Self, AbiError>` and `as_str(&self) ->
&str`; no blanket `From<String>` or public tuple field permits unchecked
identity construction. `ConnectorFailure` uses the existing eight error
classes and bounded safe fields.

- [ ] **Step 1: Write failing ID, bound, and object-safety tests**

```rust
#[test]
fn abi_ids_are_canonical_and_bounded() {
    assert!(ConnectorId::parse("serpapi").is_ok());
    assert!(ConnectorId::parse("").is_err());
    assert!(ConnectorId::parse("Serp API").is_err());
}

#[test]
fn host_traits_are_object_safe_send_and_sync() {
    fn connector_io(_: &dyn ConnectorIo) {}
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn ConnectorIo>();
    let _ = connector_io;
}
```

Add boundary tests for oversized strings, headers, binding maps, transport
bytes, retry delay, nesting, and canonical output size.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-abi --no-default-features
```

Expected: Cargo reports that package `donat-connector-abi` does not exist.

- [ ] **Step 3: Implement the neutral ABI**

Begin `lib.rs` with the same `no_std`, `forbid(unsafe_code)`, and `alloc`
directives as Task 1. Keep URL, reqwest, serde JSON, Tokio, database,
filesystem, environment, role, process, retry-policy, and raw credential types
out of every public field and signature.

- [ ] **Step 4: Run no-OS and closure checks**

```bash
cargo check -p donat-connector-abi --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-abi --target all \
  --edges normal,build,no-dev --no-default-features --offline --locked
```

Expected: only `donat-connector-abi` and the local value-contract crate appear.

- [ ] **Step 5: Run GREEN**

```bash
cargo test -p donat-connector-abi --no-default-features
cargo test -p donat-value-contract --no-default-features
```

- [ ] **Step 6: Commit the ABI foundation**

```bash
git add Cargo.toml Cargo.lock crates/connector-abi
git commit -m "feat(connectors): add neutral connector ABI"
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
- Create: `crates/connector-catalog/tests/canonical_hashes.rs`
- Create: `crates/connector-catalog/tests/type_identity.rs`
- Create: `crates/connector-catalog/tests/fixtures/missing-license-file-hash.yaml`
- Create: `crates/connector-catalog/tests/fixtures/missing-side-effect-step.yaml`
- Create: `crates/connector-catalog/tests/fixtures/unknown-effect.yaml`
- Create: `crates/connector-catalog/sources/records/donat-owned-http-v1.yaml`
- Create: `crates/connector-catalog/manifests/donat-owned-http-v1.yaml`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: all Task-2 ABI IDs and Task-1 value contracts.
- Produces strict `#[serde(deny_unknown_fields)]` source and normalized types:

```rust
pub struct ConnectorSourceRecord {
    pub record_version: u32,
    pub source_id: String,
    pub source_kind: SourceKind,
    pub repository: ImmutableRepository,
    pub artifact_hashes: Vec<ArtifactHash>,
    pub license: LicenseDecision,
    pub entrypoints: Vec<SourcePath>,
    pub dependencies: Vec<DependencyDecision>,
    pub embedded_material: Vec<EmbeddedMaterialDecision>,
    pub provider_contract: ProviderContractEvidence,
    pub safety_findings: SafetyFindings,
    pub approved_operations: Vec<OperationId>,
    pub reviewer: ReviewIdentity,
    pub approval_date: Date,
    pub proposed_destinations: Vec<RepoPath>,
    pub red_tests: Vec<TestId>,
}

pub struct OperationSpec {
    pub connector: ConnectorId,
    pub operation: OperationId,
    pub steps: NonEmptyVec<CompiledStepSpec>,
    pub effect: OperationEffect,
    pub input: ValueContractCatalog,
    pub output: ValueContractCatalog,
    pub bounds: OperationBounds,
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
```

Independently construct and assert all four Spec 007 Section 5.1
domain-separated SHA-256 vectors:

```text
semantic {}                         799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19
provenance {}                       a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4
semantic {"a":1,"b":[true,null,"x"]} 2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34
provenance {"a":1,"b":[true,null,"x"]} 4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23
```

Add
`catalog_descriptor_ids_match_connector_io` as a compile-only assignment from
normalized descriptor fields to `ConnectorIo` parameters.

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
and that read-only steps have none. Inventory-only records never enter the
executable catalog.

- [ ] **Step 4: Run GREEN and review snapshots**

```bash
cargo test -p donat-connector-catalog
cargo insta test -p donat-connector-catalog
cargo insta review
```

Expected: vectors and strict negative fixtures pass; reviewed snapshots contain
no source description or UI metadata.

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
- Create: `crates/connector-acquire/tests/hostile_archives.rs`
- Create: `crates/connector-acquire/tests/imperative_inventory.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Consumes: checked-in `ConnectorSourceRecord` schemas from
  `donat-connector-catalog`.
- Produces development-only commands:

```text
donat-connector-acquire inspect --record <candidate.yaml> --output <review-dir>
donat-connector-acquire verify --record <approved.yaml>
```

`inspect` writes only an ignored candidate review bundle. `verify` fails
closed and writes no source or generated Rust. Neither command executes donor
scripts, binaries, tests, Node, or JavaScript.

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

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-acquire --test source_admission
cargo test -p donat-connector-acquire --test hostile_archives
```

Expected: Cargo reports that package `donat-connector-acquire` does not exist.

- [ ] **Step 3: Implement the exact acquisition policy**

Enforce HTTPS, the checked-in host allowlist, at most three same-host HTTPS
redirects, 64 MiB compressed, 256 MiB expanded, 16 MiB per file, 10,000
entries, and depth 32. Hash the complete artifact before entry inspection.
Use an exclusive mode-`0700` temporary directory, exclusive file creation,
no-follow walks, normalized relative UTF-8 paths, and RAII cleanup.

- [ ] **Step 4: Add admission and imperative-source findings**

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

- [ ] **Step 5: Run GREEN**

```bash
cargo test -p donat-connector-acquire --test source_admission
cargo test -p donat-connector-acquire --test hostile_archives
cargo test -p donat-connector-acquire --test imperative_inventory
```

- [ ] **Step 6: Commit the acquisition tool**

```bash
git add Cargo.toml Cargo.lock crates/connector-acquire
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
pub struct GeneratedConnectorEntry {
    pub connector: ConnectorId,
    pub operations: &'static [GeneratedOperationEntry],
    pub semantic_sha256: Hash256,
    pub provenance_sha256: Hash256,
}

pub static CONNECTORS: &[GeneratedConnectorEntry] = &[/* sorted entries */];
```

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
unexpected path.

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

- [ ] **Step 4: Add the post-codegen ABI identity proof**

Add `generated_catalog_ids_match_abi` in the catalog crate. It assigns every
actual checked-in connector, operation, step, processor-family, credential,
and capability ID directly to the corresponding ABI type.

- [ ] **Step 5: Run GREEN and inspect output**

```bash
cargo test -p donat-connector-codegen --test deterministic_catalog
cargo run -p donat-connector-codegen -- generate --check
cargo test -p donat-connector-catalog generated_catalog_ids_match_abi
cargo insta test -p donat-connector-codegen
cargo insta review
```

- [ ] **Step 6: Commit the codegen slice**

```bash
git add Cargo.toml Cargo.lock crates/connector-codegen \
  crates/connector-catalog/src/generated crates/connector-catalog/src/lib.rs
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
- Create: `scripts/check_connector_processor_boundary.py`
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
exist and Python reports that the policy file is absent.

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

- [ ] **Step 4: Implement the mechanical boundary checker**

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

The checker evaluates locked Cargo metadata for all supported targets and
features; rejects build scripts, native/git/patched dependencies, `links`,
unsafe/FFI/assembly, symlink or workspace escapes, generated/donor files in
the processor tree, and the forbidden source tokens listed in Spec 007
Section 7.1. It also rejects an `OperationProcessor` implementation outside
this crate. Add both positive and deliberately mutated negative fixtures
inside the Python script's temporary directory.

- [ ] **Step 5: Wire CI and run GREEN**

```bash
python3 scripts/check_connector_processor_boundary.py
cargo test -p donat-connector-processors --no-default-features
cargo check -p donat-connector-processors --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-processors --target all \
  --edges normal,build,no-dev --no-default-features --offline --locked
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
- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/src/loader.rs`
- Modify: `crates/metadata/tests/loader.rs`
- Modify: `crates/server/src/connectors/mod.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/Cargo.toml`
- Modify: `Cargo.lock`

**Metadata contract:**

```yaml
connectors:
  - name: search
    source: serpapi
    credential:
      spec: serpapi-api-key-v1
      fields:
        api_key:
          value_from_env: SERPAPI_API_KEY
```

Legacy `module`/`config` connector instances continue to deserialize until
their Task-13 migration. The new branch is strict and accepts no literal
credential value, runtime resolver kind, package URL, implementation path, or
credential operation.

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
- `credential_failure_never_contains_secret_value`;
- `credential_capability_has_no_debug_clone_or_serialize_surface`.

Use an in-memory fake resolver whose value changes between calls and whose
call log records only reference identities. Use compile-fail fixtures for the
forbidden trait implementations and raw getter.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-metadata community_credential
cargo test -p donat-server --test connector_credentials
```

Expected: the new YAML field `source` is unknown and the server test target is
absent.

- [ ] **Step 3: Implement strict metadata selection**

Represent legacy and catalog-backed instances as a strict untagged enum whose
branches cannot overlap. Validate the `CredentialSpecId` against the generated
catalog, every field against the selected spec, and every environment
variable name before the listener opens. Store only the source instance,
credential identity, and references in the immutable registry.

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
rg -n "CredentialCapability.*(Clone|Debug|Serialize)|fn (get|expose|value)" \
  crates/server/src/connectors
```

Expected: both test commands pass and the scan has no capability trait
derivation or raw-value API match.

- [ ] **Step 6: Commit the credential boundary**

```bash
git add crates/metadata crates/server/src/connectors/credentials.rs \
  crates/server/src/connectors/mod.rs crates/server/src/state.rs \
  crates/server/tests/connector_credentials.rs crates/server/Cargo.toml \
  Cargo.lock
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
- Create: `crates/connector-catalog/manifests/donat-owned-stripe-compat-v1.yaml`
- Create: `crates/connector-catalog/src/generated/donat_owned_stripe_compat.rs`
- Modify: `crates/connector-catalog/src/generated/mod.rs`
- Modify: `crates/connector-catalog/src/generated/catalog.digest`
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
    source: &'static GeneratedConnectorEntry,
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
- `unknown_source_fails_before_listen`;
- `inventory_only_source_fails_before_listen`;
- `generated_processor_id_resolves_without_conversion`;
- `registry_has_no_dynamic_registration_or_fallback`;
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

Compile instance source and operation lookup entirely at startup. Join exact
ABI-owned IDs without strings, parsing, serialization, or wrapper copying.
Unknown source, unknown operation, credential mismatch, unresolved processor,
or unsupported manifest feature is a startup error. Keep inventory-only
records outside generated executable entries.

- [ ] **Step 4: Delete the generic module dispatch shape**

Delete `ConnectorModule`, `ConnectorDefinition`, `RegistryInstance`,
`built_in_module_names`, and the `"http"`/`"stripe"` module match only after
equivalent static entries are present. Until Task 13 migrates Stripe, its
generated Donat-owned compatibility entry contains configuration and trigger
identity but no executable `OperationSpec`; a private fixed server function
pointer reaches the existing adapter for its direct Rust behavior oracle.
There is no string dispatch or process/public exposure. Preserve every current
inbound webhook result and the exact empty-body 404.

- [ ] **Step 5: Run GREEN and public-surface proof**

```bash
cargo test -p donat-server --test connector_catalog
cargo test -p donat-server --test connector_webhook
cargo test -p donat-conformance --test connectors
cargo test -p donat-conformance --test connector_public_surfaces
rg -n "/execute|execute_connector|RegistryInstance|register_connector" \
  crates/server/src crates/schema/src crates/conformance
```

Expected: tests pass; the scan has no public connector-execution route or
dynamic registry implementation.

- [ ] **Step 6: Commit the static registry**

```bash
git add crates/connector-catalog crates/server crates/conformance Cargo.lock
git commit -m "refactor(connectors): consume the static connector catalog"
```


---

### Task 10: Admit the exact SerpAPI source before derivative work

**Files:**

- Create: `crates/connector-catalog/sources/records/serpapi-0.1.10.yaml`
- Create: `crates/connector-acquire/tests/serpapi_admission.rs`
- Modify: `knowledgebase/declarative-saas/reference-porting-register.md`

**Pinned source identity:**

```text
repository: https://github.com/serpapi/n8n-nodes-serpapi
commit: e48b778878c043f30277b932c4c129804efee66d
git_tree: 6916ca97c870b5045200a207dfaf9cb40341f24d
package: n8n-nodes-serpapi@0.1.10
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

- [ ] **Step 1: Add the Donat-owned RED admission tests**

Add exact tests:

- `serpapi_record_matches_pinned_commit_tree_and_package`;
- `serpapi_record_matches_every_admitted_file_hash`;
- `serpapi_license_and_notice_are_complete`;
- `serpapi_dependency_dispositions_are_closed`;
- `serpapi_n8n_workflow_is_type_only_replaced`;
- `serpapi_record_is_not_executable_without_manifest`.

The test owns expected identities and hashes in Rust; it does not trust values
parsed from the record itself.

- [ ] **Step 2: Run RED before creating the record**

```bash
cargo test -p donat-connector-acquire --test serpapi_admission
```

Expected: the test fails because
`sources/records/serpapi-0.1.10.yaml` is absent.

- [ ] **Step 3: Create the complete pre-port record**

Record `n8n-workflow: "*"` as `TypeOnlyReplaced`; record all other package,
embedded, and tool dependencies with a closed disposition. Approve only
Google search descriptor inventory, the fixed `https://serpapi.com` provider
origin, `GET /search.json`, query encoding, JSON decoding, and static API-key
credential shape. Classify any imperative/function-valued source as a
reviewed work item. Mark the source record inventory-only because no
normalized manifest or Donat-owned behavior test exists yet.

- [ ] **Step 4: Add the register pre-port row**

Add an admission row with the exact commit, git tree, npm integrity, five
per-file hashes, MIT license hash and notice, proposed destinations, RED test
IDs, and reviewer. Do not add a derivative destination, generated file,
runtime adapter, notice bundle, or executable manifest in this task.

- [ ] **Step 5: Run GREEN and verify offline**

```bash
cargo test -p donat-connector-acquire --test serpapi_admission
cargo run -p donat-connector-acquire -- verify \
  --record crates/connector-catalog/sources/records/serpapi-0.1.10.yaml
cargo test -p donat-connector-catalog source_record
```

Expected: the record verifies from pinned local test material, remains
inventory-only, and no donor script or JavaScript executes.

- [ ] **Step 6: Commit admission separately**

```bash
git add crates/connector-catalog/sources/records/serpapi-0.1.10.yaml \
  crates/connector-acquire/tests/serpapi_admission.rs \
  knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "docs(connectors): admit pinned SerpAPI source"
```


---

### Task 11: Port the admitted SerpAPI search slice

**Files:**

- Create: `crates/connector-catalog/manifests/serpapi-0.1.10.yaml`
- Create: `crates/connector-catalog/src/generated/serpapi.rs`
- Create: `crates/connector-codegen/tests/serpapi_compile.rs`
- Create: `crates/connector-codegen/tests/snapshots/serpapi_compile__canonical_ir.snap`
- Create: `crates/metadata/tests/fixtures/connectors/serpapi.yaml`
- Create: `crates/server/tests/connectors_serpapi.rs`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/metadata.yaml`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/query.graphql`
- Create: `crates/conformance/fixtures/connectors/serpapi_startup/expected.json`
- Create: `THIRD_PARTY_NOTICES.md`
- Modify: `crates/connector-catalog/src/generated/mod.rs`
- Modify: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/connector-codegen/tests/deterministic_catalog.rs`
- Modify: `crates/metadata/tests/loader.rs`
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

Translate only the admitted Google-search fields whose five source hashes are
fixed in Task 10. Put the source-record ID, full commit, git tree, package
integrity, each contributing source path/hash, license/notice ID, RED tests,
and destination paths in the manifest header. Run generation, inspect the
Rust and digest, and reject any imported display name, icon, UI condition,
workflow item, expression, function body, JavaScript, arbitrary engine, or
arbitrary request field.

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

- [ ] **Step 5: Wire metadata and internal direct execution**

Accept the Task-7 `source: serpapi` instance and its API-key reference.
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
rg -n "n8n-workflow|IExecuteFunctions|INodeType|pairedItem|\\$node|\\$workflow" \
  crates/connector-catalog/manifests/serpapi-0.1.10.yaml \
  crates/connector-catalog/src/generated/serpapi.rs \
  crates/server/tests/connectors_serpapi.rs THIRD_PARTY_NOTICES.md
cargo tree -p donat-server --target all --edges normal,build,no-dev \
  --offline --locked
```

Expected: the source scan has no SUL API match and the runtime tree contains
no npm, n8n, Node, JavaScript, or donor package.

- [ ] **Step 8: Commit the derivative port**

```bash
git add crates/connector-catalog crates/connector-codegen \
  crates/metadata/tests crates/server/src/connectors/catalog.rs \
  crates/server/tests/connectors_serpapi.rs crates/conformance \
  THIRD_PARTY_NOTICES.md \
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
- Modify: `crates/connector-catalog/src/generated/donat_owned_http.rs`
- Modify: `crates/connector-catalog/src/generated/catalog.digest`
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
cargo test -p donat-connector-catalog --test pagination_plan
cargo test -p donat-server --test connector_pagination
cargo run -p donat-connector-codegen -- generate --check
cargo insta test -p donat-connector-codegen
cargo insta review
```

- [ ] **Step 6: Commit the bounded pagination slice**

```bash
git add crates/connector-catalog crates/server/src/connectors \
  crates/server/tests/connector_pagination.rs
git commit -m "feat(connectors): add bounded compiled pagination"
```


---

### Task 13: Migrate Stripe through the compiled-step processor ABI

**Files:**

- Create: `crates/connector-catalog/sources/records/stripe-checkout-v1.yaml`
- Create: `crates/connector-catalog/manifests/stripe-checkout-v1.yaml`
- Create on the admitted branch only:
  `crates/connector-catalog/sources/provider-contracts/stripe-idempotency-v1.yaml`
- Create on the admitted branch only:
  `crates/connector-catalog/src/generated/stripe_checkout.rs`
- Create: `crates/connector-catalog/tests/stripe_admission.rs`
- Create: `crates/connector-processors/src/stripe_checkout.rs`
- Create: `crates/connector-processors/tests/stripe_checkout.rs`
- Modify: `crates/connector-processors/src/registry.rs`
- Modify: `crates/connector-catalog/src/generated/mod.rs`
- Modify: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/src/connectors/catalog.rs`
- Modify: `crates/server/tests/connectors_stripe.rs`
- Modify: `knowledgebase/declarative-saas/reference-porting-register.md`

**Migration boundary:**

- Existing independently authored Donat Rust and Donat-owned tests are the
  behavior oracle.
- The processor validates typed input, creates bindings only for
  `CompiledStepId("create_session")`, calls `ConnectorIo`, and normalizes
  `id`, `url`, `status`, and `expires_at`.
- The server retains fixed `https://api.stripe.com`, form encoding,
  authorization and idempotency headers, credential materialization, reqwest,
  DNS/peer checks, JSON codec, UUID/text/time, crypto, clock, deadline, and
  control.
- No n8n Stripe source, fixture, description, helper, or runtime type is used.

- [ ] **Step 1: Add the Donat-owned RED migration tests**

Before adding the processor or generated entry, add exact tests:

- `stripe_processor_preserves_existing_request_result_and_error_contract`;
- `stripe_processor_calls_only_create_session_step`;
- `stripe_processor_cannot_construct_transport_or_credentials`;
- `stripe_processor_observes_control_checks`;
- `stripe_catalog_effect_requires_immutable_provider_evidence`;
- `stripe_without_retention_evidence_is_inventory_only`;
- `stripe_existing_form_header_result_and_error_tests_remain_unchanged`.

The first test reuses independently authored values but does not copy current
implementation code into its expected result.

- [ ] **Step 2: Run RED**

```bash
cargo test -p donat-connector-processors --test stripe_checkout
cargo test -p donat-connector-catalog --test stripe_admission
cargo test -p donat-server --test connectors_stripe stripe_processor
```

Expected: no Stripe processor/manifest exists and the current server module
still performs transport directly.

- [ ] **Step 3: Close the provider-evidence gate before executable admission**

Start from the pinned behavior-only Stripe OpenAPI identity:

```text
repository: https://github.com/stripe/openapi
commit: 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69
source: openapi/spec3.json
sha256: e24a26de4188fd64dec4c043d5d3726277fdcb07556a493ea481c305b0a223d8
license: MIT
```

The provider-contract record must additionally pin immutable evidence for all
four executable facts: exact `Idempotency-Key` header binding, Stripe-account
API namespace, conservative minimum retention of 86,400,000 ms, and a
300,000 ms positive clock margin. Record the immutable URL/revision, complete
document SHA-256, extracted fact locations, retrieval date, license/terms
disposition, and reviewer.

If any of those facts lacks immutable admissible evidence, stop executable
admission: keep `stripe-checkout-v1.yaml` as inventory-only, make
`stripe_without_retention_evidence_is_inventory_only` pass, omit
`stripe_checkout.rs` from generated executable entries, and continue only
with the processor migration under Donat-owned direct tests. A current web
page, example assertion, or the existing request test cannot substitute for
retention evidence.

- [ ] **Step 4: Implement the narrow processor and server-owned adapter**

Move only pure validation/binding/result normalization into
`stripe_checkout.rs`. Delete Stripe's duplicate reqwest/DNS/peer transport and
route `create_session` through the shared `ConnectorIo`. Register the sealed
processor family/version privately. Preserve exact form field ordering,
header behavior, result fields, error classes/codes, one-call bound, and
redaction.

- [ ] **Step 5: Generate the catalog entry only if the gate passed**

When all immutable evidence is present, compile exactly:

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

If the gate did not pass, the codegen negative test proves there is no
executable Stripe entry. Neither branch adds process dispatch, a first-attempt
timestamp, retry/takeover behavior, or an idempotency-horizon assumption.

- [ ] **Step 6: Run GREEN and the unchanged behavior oracle**

```bash
cargo test -p donat-connector-processors --test stripe_checkout
cargo test -p donat-connector-catalog --test stripe_admission
cargo test -p donat-server --test connectors_stripe
cargo test -p donat-server --test connector_executor
cargo run -p donat-connector-codegen -- generate --check
python3 scripts/check_connector_processor_boundary.py
```

- [ ] **Step 7: Commit the Stripe migration**

```bash
git add crates/connector-catalog crates/connector-processors \
  crates/server/src/connectors crates/server/tests/connectors_stripe.rs \
  knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "refactor(connectors): migrate Stripe to compiled steps"
```


---

### Task 14: Generalize two-stage webhook dispatch and retain `503`

**Files:**

- Create: `crates/connector-processors/src/stripe_webhook.rs`
- Create: `crates/connector-processors/tests/stripe_webhook.rs`
- Create: `crates/connector-catalog/manifests/donat-owned-stripe-webhook-v1.yaml`
- Create: `crates/connector-catalog/src/generated/donat_owned_stripe_webhook.rs`
- Create: `crates/server/src/connectors/webhooks.rs`
- Create: `crates/server/tests/connector_webhook_ordering.rs`
- Modify: `crates/connector-processors/src/registry.rs`
- Modify: `crates/connector-catalog/src/generated/mod.rs`
- Modify: `crates/connector-catalog/src/generated/catalog.digest`
- Modify: `crates/server/src/connectors/stripe.rs`
- Modify: `crates/server/src/connectors/catalog.rs`
- Modify: `crates/server/src/connectors/codec.rs`
- Modify: `crates/server/src/connector_webhook.rs`
- Modify: `crates/server/tests/connector_webhook.rs`
- Modify: `crates/conformance/tests/connectors.rs`

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

- [ ] **Step 1: Add RED ordering and route-matrix tests**

Add exact tests:

- `webhook_authentication_precedes_codec`;
- `invalid_signature_never_invokes_codec`;
- `authenticated_raw_body_is_opaque_to_processors`;
- `normalizer_receives_only_typed_decoded_value`;
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
and normalizer IDs in the static trigger descriptor.

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

- [ ] **Step 5: Run GREEN and native conformance**

```bash
cargo test -p donat-connector-processors --test stripe_webhook
cargo test -p donat-server --test connector_webhook_ordering
cargo test -p donat-server --test connector_webhook
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors webhook
python3 scripts/check_connector_processor_boundary.py
```

- [ ] **Step 6: Commit the generic webhook boundary**

```bash
git add crates/connector-processors crates/connector-catalog \
  crates/server/src/connectors crates/server/src/connector_webhook.rs \
  crates/server/tests crates/conformance/tests/connectors.rs
git commit -m "refactor(connectors): generalize webhook verification"
```


---

### Task 15: Prove the runtime package is static and source-free

**Files:**

- Create: `policy/connector-runtime-package.toml`
- Create: `scripts/check_connector_runtime_package.py`
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
  dynamic libraries/plugins, and runtime discovery/configuration.

- [ ] **Step 1: Write the failing package and public-surface checks**

The Python check builds fixture package trees and asserts exact rejection of:

- donor `.ts`, `.js`, `.mjs`, `.cjs`, npm tarball, or `node_modules`;
- acquisition/codegen binary or dependency;
- `.wasm`, unexpected shared library, plugin directory, package URL, or
  runtime registration manifest;
- `n8n`, `n8n-workflow`, workflow-node APIs, or donor source material;
- a binary dependency tree absent from the checked-in policy.

Add exact conformance tests:

- `public_surfaces_cannot_execute_connectors`;
- `post_connector_execute_is_empty_404`;
- `graphql_has_no_connector_execution_field`;
- `rest_has_no_connector_execution_endpoint`;
- `mcp_has_no_connector_execution_tool`;
- `commands_cannot_plan_connector_io`;
- `webhook_route_preserves_phase1_boundary`.

- [ ] **Step 2: Run RED**

```bash
python3 scripts/check_connector_runtime_package.py --self-test
cargo test -p donat-conformance --test connector_public_surfaces
```

Expected: the policy/script and conformance target do not exist.

- [ ] **Step 3: Implement separate locked package proof**

The script accepts only explicit paths, creates its own mode-`0700` temporary
directory, runs:

```bash
cargo build -p donat-server --bin donat --release --offline --locked
cargo tree -p donat-server --target all \
  --edges normal,build,no-dev --offline --locked
```

It copies only `target/release/donat`, `THIRD_PARTY_NOTICES.md`, and a
canonicalized runtime-tree/SBOM record into the package directory, then scans
that directory and dependency closure against the closed policy. It never
scans or packages the tooling-bearing workspace `target` directory. The
Docker final stage copies the same release binary and notice only.

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
binary, notice, and canonical runtime-tree/SBOM record.

- [ ] **Step 6: Commit the release proof**

```bash
git add policy/connector-runtime-package.toml \
  scripts/check_connector_runtime_package.py .github/workflows \
  Dockerfile crates/conformance
git commit -m "ci(connectors): prove static source-free runtime package"
```


---

## Explicit Process-Gated Integration Handoff

This plan stops after Task 15. The following work starts only when the named
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
  credentials -> executor -> registry -> SerpAPI -> pagination -> Stripe ->
  webhooks -> package proof`.
- [ ] Every donor derivative starts only after an approved immutable
  source/version/tree/artifact/per-file hash, license/notice, destination, RED
  test, reviewer, and register entry.
- [ ] SerpAPI is the sole derivative port in this plan; Stripe is an
  independent Donat implementation plus behavior-only provider references.
- [ ] `n8n-workflow` remains `TypeOnlyReplaced`; inspected n8n built-ins remain
  behavior-only and no workflow/logical/UI node enters catalog or runtime.
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
