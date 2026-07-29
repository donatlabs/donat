# Connector ABI Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the connector ABI enforce immutable bounded responses,
Donat-owned static failure text, host-authorized correlations, and aggregate
typed-binding limits while retaining the accepted static connector-factory
architecture.

**Architecture:** First update Spec 007, ADR 010, and the existing community
connector plan so their Task 2/6/8 ownership matches the approved remediation
design and removes the raw-public-field contradiction. Then make the existing
`donat-connector-abi` envelopes opaque, add the two restricted construction
namespaces plus their initial whole-workspace policy checker, and validate all
roots with shared counters. The host trait signatures and crate graph remain
unchanged; later catalog, processor, and fixed-origin executor tasks consume
this API without compatibility adapters.

**Tech Stack:** Rust 2024, `#![no_std] + alloc`,
`alloc::boxed::Box`, `BTreeMap`, rustdoc `compile_fail` tests, Python 3
standard library, GitHub Actions, native Postgres conformance harness.

## Global Constraints

- Baseline production behavior is commit `db289dc`
  (`feat(connectors): add neutral connector ABI`); the approved design is
  `docs/superpowers/specs/2026-07-29-connector-abi-remediation-design.md`.
- The deployed product remains exactly one Rust `donat` binary plus Postgres.
  This remediation adds no provider, catalog, processor, credential, server
  executor, database, or process runtime implementation.
- There is no admin role, permission bypass, runtime metadata mutation,
  connector execution route, credential administration surface, or
  caller-selected URL/method/header capability.
- No connector or ABI type adds `If`, `Switch`, `Merge`, `Code`, `Wait`,
  loops, workflow items, paired items, subworkflows, AI nodes,
  send-and-wait, business branching, retries, waits, database work, or
  orchestration.
- `donat-connector-abi` remains unpublished `#![no_std] + alloc`, with
  `#![forbid(unsafe_code)]`, empty default features, no `std` feature, build
  script, procedural macro, native dependency, third-party runtime
  dependency, FFI, or assembly.
- `donat-connector-abi` continues to depend only on
  `donat-value-contract`. The value crate remains the sole owner of
  `TypedValue`, `BoundedInlineBytes`, and `canonical_size`; this remediation
  creates no second value representation and changes no value-contract API.
- `ConnectorIo::call`, `ProcessorControl::check`, `ProcessorContext`,
  `BoxFuture`, all existing typed IDs, and the eight
  `ConnectorErrorClass` variants remain source-identical.
- `BoundedTransportResponse` fields become private. The only public reads are
  `status()`, `selected_headers()`, `decoded()`, `response_bytes()`, and
  `authorized_correlations()`. No mutable accessor, raw-parts constructor,
  public field, `Default`, conversion trait, or compatibility constructor
  remains.
- `StaticErrorCode`, `StaticSafeMessage`, `AuthorizedCorrelations`,
  `BoundedTransportResponse`, and `ConnectorFailure` carry their invariants
  through private fields. `AuthorizedCorrelations` does not implement
  `Clone`.
- `ConnectorFailure` stores one private
  `static_text: Box<StaticFailureText>`, where private
  `StaticFailureText { code: StaticErrorCode, safe_message:
  StaticSafeMessage }` has no independent constructor or accessor. The
  public `ConnectorFailure::try_new`, `code()`, and `safe_message()`
  signatures remain unchanged.
- The box comes from `alloc::boxed::Box`, adds no dependency, and keeps the
  `ConnectorFailure` error variant below Clippy's large-error threshold.
  `#[allow(clippy::result_large_err)]`,
  `#[expect(clippy::result_large_err)]`, crate lint overrides, command-line
  lint suppression, and equivalent spelling variants are forbidden.
- `StaticErrorCode` accepts exactly `1..=96` ASCII bytes matching
  `[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?`.
- `StaticSafeMessage` accepts `1..=1,024` UTF-8 bytes and rejects every
  Unicode scalar for which `char::is_control()` is true. Its fixed
  `[u8; 1024]` storage is zero-filled after its private `u16` length.
- `BoundedTransportResponse::try_new` creates an empty authorization set.
  Only `host_construction` may intersect captured selected headers with a
  catalog-derived capability allowlist.
- Status remains exactly `u16`: `0`, `u16::MAX`, and every value between them
  are valid. This remediation adds no HTTP semantic range.
- Response and authorization limits remain exact: 64 selected headers, 8,192
  UTF-8 bytes per value, 32,768 aggregate retained key/value bytes, and
  1,048,576 response bytes.
- Typed-value limits remain exact: depth 64 per root, 100,000 nodes across
  all roots, 16 inline values across all roots, 131,072 aggregate decoded
  inline bytes across all roots, 262,144 aggregate canonical bytes, and
  262,144 UTF-8 bytes in one typed string or object key.
- `TypedBindings` validates at most 64 slots, then performs one shape pass
  with shared node/inline/decoded-byte counters, then performs the existing
  checked aggregate canonical-size sum. Depth restarts at zero for each
  root.
- `ConnectorFailure::try_new` accepts only ABI-owned static code/message
  values and `Option<&AuthorizedCorrelations>`. It clamps retry delay to
  86,400 seconds and accepts no raw string, raw map, or caller-supplied
  allowlist.
- `catalog_construction` is reserved for
  `crates/connector-catalog/src/`; `host_construction` is reserved for
  `crates/server/src/connectors/`. The initial checker created here owns
  namespace, producer, trait-implementation, allocation-leak, and test-path
  policy.
- Community connector Task 6 must extend
  `scripts/check_connector_processor_boundary.py`; it must not create a
  second checker, wrapper checker, or parallel policy file. Task 6 adds the
  remaining processor dependency/source-boundary rules and keeps all initial
  deterministic fixtures.
- Community connector Task 8 is the only production caller of
  `host_construction`. Task 3 is the only production caller of
  `catalog_construction`; generated catalog code may use the two static
  literal constructors.
- There is no deprecated raw constructor or compatibility shim. Future Tasks
  3, 6, and 8 target only the remediated API.
- All Cargo commands in Task 2 run with this RAM-backed environment:

```bash
export PATH=/home/dev/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/dev/shm/donat-connector-abi-remediation-target
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
mkdir -p "$CARGO_TARGET_DIR"
: "${PG_URL:?PG_URL must be supplied by the invoking environment}"
: "${DONAT_BIN:?DONAT_BIN must be supplied by the invoking environment}"
test "$DONAT_BIN" = "$CARGO_TARGET_DIR/debug/donat"
```

  Verification commands never assign or hardcode `PG_URL` or `DONAT_BIN`.

## Authoritative Inputs

- `AGENTS.md`
- `knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md`
- `knowledgebase/declarative-saas/decisions/009-durable-process-source-local-compilation-and-journal-contracts.md`
- `specs/007-community-connector-factory.md`
- `docs/superpowers/plans/2026-07-29-community-connector-factory.md`
- `docs/superpowers/specs/2026-07-29-connector-abi-remediation-design.md`
- `.superpowers/sdd/2026-07-29-connector-abi-task2-review.md`
- `.superpowers/sdd/2026-07-29-community-connector-factory/task-2-brief.md`
- `crates/connector-abi/src/envelope.rs`
- `crates/connector-abi/src/host.rs`
- `crates/connector-abi/src/ids.rs`
- `crates/connector-abi/src/lib.rs`
- `crates/connector-abi/tests/abi_contract.rs`

## File and Responsibility Map

```text
specs/007-community-connector-factory.md
    normative opaque-envelope, static-failure, correlation-authority,
    aggregate-counter, status-domain, and acceptance contract

knowledgebase/declarative-saas/decisions/
  010-static-community-connector-factory-and-runtime-boundaries.md
    accepted reason for restricted constructors and mechanical producers

docs/superpowers/plans/2026-07-29-community-connector-factory.md
    Task 2 owns the initial checker and safe ABI; Task 6 extends that checker;
    Task 8 alone uses the host construction namespace

crates/connector-abi/src/ids.rs
    existing InlineId validator plus private-field StaticErrorCode

crates/connector-abi/src/envelope.rs
    StaticSafeMessage, AuthorizedCorrelations, opaque response/failure,
    response/correlation validation, shared TypedBindings counters

crates/connector-abi/src/lib.rs
    exact exports, root-level doc-hidden construction namespaces, and
    external compile-fail API invariants

crates/connector-abi/tests/abi_contract.rs
    public behavior and every independently observable exact boundary

scripts/check_connector_processor_boundary.py
    initial whole-workspace producer/test allowlists and deterministic
    positive/negative temporary fixtures

.github/workflows/ci.yml
    invokes the one checker before workspace Rust compilation
```

`crates/connector-abi/src/host.rs`, `crates/value-contract/`, every Cargo
manifest, `Cargo.lock`, catalog, processor, provider, server runtime, and
conformance fixture remain unchanged in the remediation code commit.

---

### Task 1: Align the authoritative ABI contract and downstream ownership

**Files:**

- Modify: `specs/007-community-connector-factory.md`
- Modify:
  `knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md`
- Modify:
  `docs/superpowers/plans/2026-07-29-community-connector-factory.md`

**Interfaces:**

- Consumes:
  `docs/superpowers/specs/2026-07-29-connector-abi-remediation-design.md`.
- Produces one authoritative contract in which Task 2 owns the safe ABI and
  initial checker, Task 3 consumes `catalog_construction`, Task 6 extends the
  same checker and uses response accessors/static literals, and Task 8 alone
  consumes `host_construction`.
- Changes documentation only. It does not revise the host trait signatures,
  crate graph, product boundary, or status representation.

- [ ] **Step 1: Record the current documentation contradiction as the RED**

Run:

```bash
rg -n \
  'pub status: u16|pub selected_headers:|pub decoded: TypedValue|pub response_bytes: u32' \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md
rg -n \
  'Create: `scripts/check_connector_processor_boundary.py`' \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md
```

Expected: the first command finds the raw public response fields in Task 2,
and the second finds Task 6 incorrectly claiming first ownership of the
checker.

- [ ] **Step 2: Align Spec 007 with the approved public ABI**

In Sections 5.5, 7.1, 7.2, 8, 13.2, and 14, state the following normative
contract and use these exact signatures:

```rust
use alloc::boxed::Box;

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
```

Add the exact statement that `status` accepts every `u16`, including `0` and
`u16::MAX`. State that the response and failure fields are immutable and
private, ordinary response construction produces empty authorization, and no
raw constructor or compatibility shim survives. Import
`alloc::boxed::Box`; state that failure construction performs exactly one
private box allocation only after every non-allocation validation succeeds,
that `code()`/`safe_message()` dereference the box, and that allocation
failure uses standard `alloc` behavior without a new `AbiError`.

Add the shared-counter order:

```text
1. reject more than 64 binding slots;
2. traverse every root with shared node, inline-value, and decoded-byte
   counters while restarting depth at zero per root;
3. sum canonical_size for every root with checked arithmetic;
4. construct TypedBindings only after all checks pass.
```

Add acceptance rows for external compile-fail privacy/conversion checks,
`0..=u16::MAX`, response/correlation exact boundaries, static text exact
boundaries, aggregate multi-root node/inline/decoded-byte accounting, and
deterministic checker mutations. Require four independent external
field-assignment compile failures for `status`, `selected_headers`, `decoded`,
and `response_bytes`, plus strict
`-D warnings -D clippy::result_large_err` evidence and negative lint-
suppression scans.

- [ ] **Step 3: Amend ADR 010 with the construction-authority decision**

Add a decision subsection containing these exact ownership rules:

```text
donat-connector-abi owns private invariant-carrying StaticErrorCode,
StaticSafeMessage, AuthorizedCorrelations, BoundedTransportResponse, and
ConnectorFailure values.

ConnectorFailure stores one private Box<StaticFailureText>; the private
StaticFailureText owns the StaticErrorCode and StaticSafeMessage. This
bounded indirection preserves the public constructor/accessors and satisfies
strict clippy::result_large_err without an allow, expectation, crate lint
override, or command-line suppression.

catalog_construction validates normalized Donat policy and is callable only
from crates/connector-catalog/src/.

host_construction intersects server-captured selected headers with the
catalog-derived CapabilityId allowlist and is callable only from
crates/server/src/connectors/.

Rust privacy cannot express friend crates, so Task 2 creates
scripts/check_connector_processor_boundary.py with deterministic producer and
test-path fixtures. Task 6 extends that exact checker with dependency and
processor-source rules.
```

Also record that `TypedBindings` shares shape counters across all roots,
depth remains per root, all `u16` statuses remain accepted, and these changes
refine the existing static native boundary without adding a sandbox or
runtime surface.

- [ ] **Step 4: Replace community-plan Task 2 with the remediated contract**

In Task 2:

- replace the raw public response-field sketch with the private fields and
  accessors from Step 2;
- add `StaticErrorCode`, `StaticSafeMessage`, `AuthorizedCorrelations`, both
  restricted namespace signatures, and the exact `ConnectorFailure::try_new`
  signature;
- add private `StaticFailureText { code, safe_message }`,
  `ConnectorFailure { class, static_text: Box<StaticFailureText>,
  retry_after_seconds, correlation_ids }`, the one post-validation
  `Box::new` allocation, dereferencing accessors, and strict
  `clippy::result_large_err` evidence without lint suppression;
- add shared `ValueCounters` semantics and the all-`u16` status domain;
- add `scripts/check_connector_processor_boundary.py` and
  `.github/workflows/ci.yml` to Task 2's files;
- state in Task 2's downstream contract that Task 3 imports the two static
  types from the ABI, calls `catalog_construction` only after strict
  normalized validation, derives each correlation header to exactly one
  `CapabilityId`, rejects missing/multiple/duplicate or more-than-64
  correlation capabilities, and includes static text plus derived
  capabilities in semantic hashing;
- replace Task 2's old broad boundary-test sentence with explicit
  compile-fail, exact-boundary, checker-self-test, no-OS, closure, lint,
  format, rebuilt-server, connector-conformance, and full-conformance gates;
- change the Task 2 implementation commit message to
  `fix(connectors): enforce safe connector ABI`.

Use these exact restricted namespace signatures:

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

- [ ] **Step 5: Amend community-plan Task 6 to extend one checker**

Change Task 6's checker file action from `Create` to `Modify`. Its RED must
start from a green initial checker and a missing processor package/policy,
not from a missing checker. Its implementation step must preserve every
initial namespace/producer/test fixture and add the locked Cargo metadata,
dependency closure, build/procedural/native/git/patch, symlink/workspace
escape, generated/donor source, and processor-source rules to the same
script.

Add this downstream ABI rule:

```text
Processor production code reads BoundedTransportResponse only through
status(), selected_headers(), decoded(), response_bytes(), and
authorized_correlations(). Donat-owned failures use
StaticErrorCode::literal and StaticSafeMessage::literal. Processor production
code never refers to catalog_construction or host_construction and never uses
an allocation-leak API. It neither constructs nor names StaticFailureText;
boxing remains private to ConnectorFailure::try_new in the ABI.
```

Task 6 may create
`policy/connector-processor-dependencies.toml`; it may not create another
Python checker, wrapper, or parallel policy mechanism.

- [ ] **Step 6: Amend community-plan Task 8 to consume host authority**

Add these exact Task 8 requirements:

```text
Task 8 is the only production caller of host_construction. It captures only
compiled selected response headers, converts each compiled header to its
CapabilityId and BoundedString value, obtains the correlation-capability
allowlist derived by Task 3 from the selected ErrorAction, and calls
host_construction::transport_response.

Task 8 passes the selected catalog ErrorAction's StaticErrorCode and
StaticSafeMessage directly to ConnectorFailure::try_new. It passes
Some(response.authorized_correlations()) for an error response and None when
no host-authorized correlation set exists.

The server neither constructs nor inspects StaticFailureText and performs no
failure-text box allocation; ConnectorFailure::try_new performs the one
private allocation.

No server method accepts a raw failure code, raw safe message, raw
correlation map, or caller-supplied correlation allowlist.

An ABI construction failure caused by an admitted compiled contract maps to
the closed invariant class with one reviewed ABI-owned static code/message
pair and no provider text.
```

Keep Task 8's fixed-origin transport ownership and complete error fallback
unchanged.

- [ ] **Step 7: Run documentation GREEN checks**

Run:

```bash
if rg -n \
  'pub status: u16|pub selected_headers:|pub decoded: TypedValue|pub response_bytes: u32' \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md; then
  exit 1
fi
if awk \
  '/^### Task 6:/{inside=1} /^### Task 7:/{inside=0} inside' \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md |
  rg -n 'Create: `scripts/check_connector_processor_boundary.py`'; then
  exit 1
fi
rg -n \
  'StaticErrorCode|StaticSafeMessage|StaticFailureText|Box<StaticFailureText>|result_large_err|AuthorizedCorrelations|catalog_construction|host_construction|every `u16`|all `u16`' \
  specs/007-community-connector-factory.md \
  knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md
awk \
  '/^### Task 6:/{inside=1} /^### Task 7:/{inside=0} inside' \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md |
  rg -n \
    'extend|Modify: `scripts/check_connector_processor_boundary.py`'
git diff --check
```

Expected: both negative scans are empty; every positive term appears in the
normative spec, ADR, and plan; Task 6 explicitly modifies and extends the one
checker; `git diff --check` exits zero.

- [ ] **Step 8: Review and commit the docs-only alignment**

Run:

```bash
git diff -- \
  specs/007-community-connector-factory.md \
  knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md
git status --short
git add -- \
  specs/007-community-connector-factory.md \
  knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md \
  docs/superpowers/plans/2026-07-29-community-connector-factory.md
git diff --cached --name-only
git diff --cached --check
```

Expected staged paths, and no others:

```text
docs/superpowers/plans/2026-07-29-community-connector-factory.md
knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md
specs/007-community-connector-factory.md
```

Commit:

```bash
git commit -m "docs(connectors): align safe ABI ownership"
```

After the commit, the SDD controller generates a Task 1 review package from
the committed diff and dispatches an ordinary independent task reviewer. It
does not dispatch Judge. A rejection returns the findings to the Task 1
implementer; after corrections, the controller regenerates the package and
dispatches a fresh ordinary task review. Do not begin Task 2 until the
post-commit Task 1 review records acceptance and the worktree is clean.

---

### Task 2: Enforce the safe connector ABI and its producer boundary

**Files:**

- Modify: `crates/connector-abi/src/envelope.rs`
- Modify: `crates/connector-abi/src/ids.rs`
- Modify: `crates/connector-abi/src/lib.rs`
- Modify: `crates/connector-abi/tests/abi_contract.rs`
- Create: `scripts/check_connector_processor_boundary.py`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes: `donat_value_contract::{BoundedInlineBytes, TypedValue,
  canonical_size}` and the existing `InlineId` validator.
- Preserves exactly:

```rust
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

- Produces:

```rust
use alloc::boxed::Box;

impl StaticErrorCode {
    pub const fn literal(value: &'static str) -> Self;
    pub fn as_str(&self) -> &str;
}

impl StaticSafeMessage {
    pub const fn literal(value: &'static str) -> Self;
    pub fn as_str(&self) -> &str;
}

impl AuthorizedCorrelations {
    pub fn get(&self, id: &CapabilityId) -> Option<&BoundedString>;
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&CapabilityId, &BoundedString)>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
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
```

- `catalog_construction` and `host_construction` have exactly the signatures
  recorded in Task 1. They are `#[doc(hidden)]`, not feature-gated.
- `AuthorizedCorrelations` has no public constructor, insertion method,
  mutable accessor, `Clone`, `Default`, `From`, `Into`, or raw-parts API.
- No Cargo manifest or lockfile changes are permitted in this task.

- [ ] **Step 1: Add public behavior tests before changing the implementation**

Update the `donat_connector_abi` import in
`crates/connector-abi/tests/abi_contract.rs` to include
`StaticErrorCode`, `StaticSafeMessage`, `AuthorizedCorrelations`,
`catalog_construction`, `host_construction`, and
`MAXIMUM_SAFE_STRING_BYTES`. Update the `donat_value_contract` import to
`use donat_value_contract::{BoundedInlineBytes, TypedValue};`. Define
source-static test values:

```rust
const FAILURE_CODE: StaticErrorCode =
    StaticErrorCode::literal("connector_rate_limited");
static FAILURE_MESSAGE: StaticSafeMessage =
    StaticSafeMessage::literal("provider rate limit reached");
```

Replace every direct response field assertion with:

```rust
assert_eq!(accepted.status(), 200);
assert_eq!(accepted.selected_headers().len(), 64);
assert_eq!(accepted.decoded(), &nested_list(64));
assert_eq!(accepted.response_bytes(), 1_048_576);
assert!(accepted.authorized_correlations().is_empty());
```

Replace the current fake `impl ProcessorControl` in the ABI integration test
with a compile-only consumer so the checker does not allow host trait
implementations in ABI tests:

```rust
fn inspect_context(context: ProcessorContext<'_>) {
    let _: &ConnectorId = context.connector;
    let _: &OperationId = context.operation;
    let _: &BoundedString = context.logical_activity_id;
    let _: &BoundedString = context.idempotency_identity;
    let _: &Hash256 = context.request_fingerprint;
    let _: &[CapabilityId] = context.capabilities;
    let _: &dyn ProcessorControl = context.control;
}

let _ = inspect_context;
```

Add named tests with these exact assertions:

```rust
#[test]
fn every_u16_status_is_accepted_and_public_construction_has_no_authority() {
    for status in [0, 1, 199, 200, 599, 600, u16::MAX] {
        let response = BoundedTransportResponse::try_new(
            status,
            BTreeMap::new(),
            TypedValue::Null,
            0,
        )
        .expect("the ABI does not impose HTTP status semantics");
        assert_eq!(response.status(), status);
        assert!(response.authorized_correlations().is_empty());
    }
}

#[test]
fn host_authority_is_intersection_only() {
    let allowed_present = CapabilityId::literal("request-id");
    let allowed_absent = CapabilityId::literal("trace-id");
    let selected_unallowed = CapabilityId::literal("server-id");
    let mut selected = BTreeMap::new();
    selected.insert(
        allowed_present,
        BoundedString::try_new("req-123", 8_192).unwrap(),
    );
    selected.insert(
        selected_unallowed,
        BoundedString::try_new("srv-456", 8_192).unwrap(),
    );

    let response = host_construction::transport_response(
        200,
        selected,
        TypedValue::Null,
        0,
        &[allowed_present, allowed_absent],
    )
    .unwrap();

    let authority: &AuthorizedCorrelations =
        response.authorized_correlations();
    assert_eq!(
        authority
            .get(&allowed_present)
            .map(BoundedString::as_str),
        Some("req-123"),
    );
    assert!(authority.get(&allowed_absent).is_none());
    assert!(authority.get(&selected_unallowed).is_none());
    assert_eq!(authority.len(), 1);
    assert_eq!(authority.iter().count(), 1);
    assert_eq!(response.selected_headers().len(), 2);
}
```

Add boundary helpers whose key-plus-value sums are exact:

```rust
fn aggregate_header_boundary(over_by_one: bool)
    -> BTreeMap<CapabilityId, BoundedString>
{
    ["a", "b", "c", "d"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let value_bytes =
                if over_by_one && index == 0 { 8_192 } else { 8_191 };
            (
                CapabilityId::parse(id).unwrap(),
                BoundedString::try_new(
                    &"a".repeat(value_bytes),
                    MAXIMUM_SAFE_STRING_BYTES,
                )
                .unwrap(),
            )
        })
        .collect()
}
```

Four keys plus four 8,191-byte values total 32,768 bytes. Raising one value
to 8,192 totals 32,769. Exercise this helper through both response
construction and `host_construction::authorized_correlations`.

Add exact/one-over assertions for:

- 64/65 selected headers;
- 8,192/8,193 bytes in one selected value;
- 32,768/32,769 aggregate retained header bytes;
- 1,048,576/1,048,577 response bytes;
- depth 64/65;
- 262,144/262,145 canonical bytes;
- 64/65 allowed correlation IDs;
- duplicate allowed IDs;
- allowed-but-absent and selected-but-unallowed IDs;
- 1/0 safe-message bytes and 1,024/1,025 safe-message bytes;
- ASCII `\n` and Unicode `\u{0085}` control characters;
- valid/invalid runtime catalog error codes;
- retry values 86,400/86,401;
- 16/17 inline values split across binding slots;
- 131,072/131,073 decoded inline bytes split as 65,536 plus
  65,536/65,537 bytes;
- depth 64 in two distinct roots;
- aggregate canonical bytes split across roots.

Construct split inline roots with:

```rust
fn inline_value(bytes: usize) -> TypedValue {
    TypedValue::InlineBytes(
        BoundedInlineBytes::try_new(
            vec![0; bytes],
            "application/octet-stream",
            None,
            bytes,
        )
        .unwrap(),
    )
}
```

Use `catalog_construction::static_error_code` and
`catalog_construction::static_safe_message` only for runtime catalog
validation cases. Failure behavior uses `FAILURE_CODE` and
`FAILURE_MESSAGE`, passes either `Some(response.authorized_correlations())`
or `None`, and verifies that `ConnectorFailure` copied the checked map. Add:

```rust
#[test]
fn failure_accessors_return_the_private_static_text_values() {
    let failure = ConnectorFailure::try_new(
        ConnectorErrorClass::Permanent,
        FAILURE_CODE,
        FAILURE_MESSAGE,
        None,
        None,
    )
    .unwrap();

    assert_eq!(failure.code(), "connector_rate_limited");
    assert_eq!(failure.safe_message(), "provider rate limit reached");
}
```

- [ ] **Step 2: Add private traversal tests for boundaries hidden by canonical size**

Add `#[cfg(test)] mod tests` at the bottom of
`crates/connector-abi/src/envelope.rs`. Call the private aggregate traversal
directly:

```rust
#[test]
fn aggregate_node_counter_accepts_100_000_and_rejects_100_001() {
    let exact = TypedValue::List(vec![TypedValue::Null; 99_999]);
    assert!(validate_typed_value_roots(core::iter::once(&exact)).is_ok());

    let over = TypedValue::List(vec![TypedValue::Null; 100_000]);
    assert_eq!(
        validate_typed_value_roots(core::iter::once(&over)),
        Err(AbiError::LimitExceeded("typed value node count")),
    );
}

#[test]
fn aggregate_node_counter_is_shared_while_depth_restarts() {
    let first = TypedValue::List(vec![TypedValue::Null; 49_999]);
    let second = TypedValue::List(vec![TypedValue::Null; 49_999]);
    assert!(
        validate_typed_value_roots([&first, &second].into_iter()).is_ok()
    );

    let one_over = TypedValue::List(vec![TypedValue::Null; 50_000]);
    assert_eq!(
        validate_typed_value_roots([&first, &one_over].into_iter()),
        Err(AbiError::LimitExceeded("typed value node count")),
    );
}
```

Also call the helper with two depth-64 roots and one depth-65 root, and assert
the exact inline-count and decoded-byte error variants. Add this private
representation check so both safe-message entry points prove the same
zero-filled value:

```rust
#[test]
fn static_safe_message_zero_fills_its_private_suffix() {
    const LITERAL: StaticSafeMessage =
        StaticSafeMessage::literal("connector failed");
    let runtime =
        StaticSafeMessage::try_catalog("connector failed").unwrap();

    assert!(LITERAL == runtime);
    assert_eq!(LITERAL.as_str(), "connector failed");
    assert!(
        LITERAL.bytes[usize::from(LITERAL.len)..]
            .iter()
            .all(|byte| *byte == 0)
    );
}
```

Import `core::mem::size_of` in that private test module and add:

```rust
#[test]
fn connector_failure_boxes_one_complete_static_text_bundle() {
    let failure = ConnectorFailure::try_new(
        ConnectorErrorClass::Permanent,
        StaticErrorCode::literal("connector_failed"),
        StaticSafeMessage::literal("connector failed"),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        failure.static_text.code.as_str(),
        "connector_failed",
    );
    assert_eq!(
        failure.static_text.safe_message.as_str(),
        "connector failed",
    );
    assert!(
        size_of::<ConnectorFailure>() < size_of::<StaticFailureText>()
    );
}
```

- [ ] **Step 3: Add external compile-fail API invariants**

Add crate-level rustdoc `compile_fail` examples in
`crates/connector-abi/src/lib.rs`. Each example imports
`donat_connector_abi` as an external crate and proves one invariant:

```rust
// A response struct literal cannot bypass validation.
let _ = BoundedTransportResponse {
    status: 200,
    selected_headers: BTreeMap::new(),
    decoded: TypedValue::Null,
    response_bytes: 0,
};
```

```rust
// Status mutation is independently inaccessible.
let mut response = BoundedTransportResponse::try_new(
    200,
    BTreeMap::new(),
    TypedValue::Null,
    0,
).unwrap();
response.status = 500;
```

```rust
// Selected-header mutation is independently inaccessible.
let mut response = BoundedTransportResponse::try_new(
    200,
    BTreeMap::new(),
    TypedValue::Null,
    0,
).unwrap();
response.selected_headers = BTreeMap::new();
```

```rust
// Decoded-value mutation is independently inaccessible.
let mut response = BoundedTransportResponse::try_new(
    200,
    BTreeMap::new(),
    TypedValue::Null,
    0,
).unwrap();
response.decoded = TypedValue::Null;
```

```rust
// Response-byte mutation is independently inaccessible.
let mut response = BoundedTransportResponse::try_new(
    200,
    BTreeMap::new(),
    TypedValue::Null,
    0,
).unwrap();
response.response_bytes = 1;
```

Each of the four mutation examples is a separate rustdoc fence with complete
imports and setup. Do not combine them: one private field must not make the
compile failure for another accidentally public field pass.

```rust
// Correlation authority cannot be constructed or inserted by a caller.
let _ = AuthorizedCorrelations { values: BTreeMap::new() };
response
    .authorized_correlations()
    .insert(CapabilityId::literal("request-id"), value);
```

```rust
// Runtime allocations cannot satisfy the literal-only API.
let dynamic = String::from("connector_failed");
let _ = StaticErrorCode::literal(dynamic.as_str());
let _ = StaticSafeMessage::literal(dynamic.as_str());
```

```rust
// Runtime conversion traits do not exist.
let _ = StaticErrorCode::try_from(String::from("connector_failed"));
let _ = StaticSafeMessage::try_from(String::from("connector failed"));
```

```rust
// Public runtime constructors do not exist.
let _ = StaticErrorCode::parse("connector_failed");
let _ = StaticSafeMessage::try_new("connector failed");
```

```rust
// Invariant-carrying values do not gain convenience construction traits.
fn require_clone<T: Clone>() {}
fn require_default<T: Default>() {}
require_clone::<AuthorizedCorrelations>();
require_default::<AuthorizedCorrelations>();
require_default::<StaticErrorCode>();
require_default::<StaticSafeMessage>();
```

Give every example all imports and local values it needs so the expected
privacy/lifetime/trait error is the only reason it fails.

- [ ] **Step 4: Create the checker self-test RED**

Create `scripts/check_connector_processor_boundary.py` with:

```python
#!/usr/bin/env python3

from dataclasses import dataclass
from pathlib import Path
import tempfile


@dataclass(frozen=True)
class Fixture:
    path: str
    source: str
    expected_rule: str | None


def scan_fixture(path: Path, source: str) -> list[str]:
    return []
```

Define deterministic positive and negative fixture rows for every initial
rule:

| Rule ID | Positive path | Negative mutation |
| --- | --- | --- |
| `host-construction-producer` | `crates/server/src/connectors/executor.rs` | same call under `crates/connector-processors/src/bad.rs` |
| `catalog-construction-producer` | `crates/connector-catalog/src/loader.rs` | same call under `crates/server/src/connectors/bad.rs` |
| `static-literal-producer` | ABI source, processor source, catalog generated source | direct literal call under ordinary server source |
| `static-literal-indirection` | exact type names and direct calls inside `STATIC_LITERAL_ROOTS` | alias, re-export, type alias, function, macro, or trait that reaches either literal constructor outside `STATIC_LITERAL_ROOTS` |
| `restricted-namespace-reexport` | unaliased private use in an allowed producer | `pub use` of either namespace |
| `restricted-namespace-alias` | exact namespace spelling | `use donat_connector_abi::host_construction as host_api` |
| `restricted-namespace-wrapper` | direct call in an allowed producer | forwarding function, macro, type alias, and trait under a disallowed path |
| `host-trait-implementation` | server connector source, processor test, server test | implementation under ordinary catalog/ABI/processor production source |
| `processor-allocation-leak` | ordinary owned allocation without a leak | `::leak`, `Box::leak`, `String::leak`, and `Vec::leak` under processor production source |
| `test-path-allowlist` | ABI namespace tests, processor fake-host tests, server integration fakes | the same test helper under any other crate test path |
| `exported-test-helper` | private `#[cfg(test)]` helper | `pub` test helper in a production library module |
| `large-error-lint-suppression` | strict `-D warnings -D clippy::result_large_err` | source `allow`/`expect`, Cargo lint override, or command-line lint suppression |

Use these exact negative fixture paths and diagnostics for static-literal
indirection. Each fixture lives outside `STATIC_LITERAL_ROOTS`, reaches the
named constructor through the form in its filename, and emits exactly one
most-specific diagnostic:

| Fixture path | Exact diagnostic after the fixture-path prefix |
| --- | --- |
| `crates/server/src/connectors/forbidden_static_error_alias.rs` | `static-literal-alias: StaticErrorCode::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_alias.rs` | `static-literal-alias: StaticSafeMessage::literal cannot be reached through an alias outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_error_reexport.rs` | `static-literal-reexport: StaticErrorCode::literal cannot be re-exported outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_reexport.rs` | `static-literal-reexport: StaticSafeMessage::literal cannot be re-exported outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_error_type_alias.rs` | `static-literal-type-alias: StaticErrorCode::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_type_alias.rs` | `static-literal-type-alias: StaticSafeMessage::literal cannot be reached through a type alias outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_error_function_wrapper.rs` | `static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_function_wrapper.rs` | `static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a function outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_error_macro_wrapper.rs` | `static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_macro_wrapper.rs` | `static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a macro outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_error_trait_wrapper.rs` | `static-literal-wrapper: StaticErrorCode::literal cannot be forwarded by a trait outside STATIC_LITERAL_ROOTS` |
| `crates/server/src/connectors/forbidden_static_message_trait_wrapper.rs` | `static-literal-wrapper: StaticSafeMessage::literal cannot be forwarded by a trait outside STATIC_LITERAL_ROOTS` |

Add exact negative fixtures for
`#[allow(clippy::result_large_err)]`,
`#[expect(clippy::result_large_err)]`, a Cargo
`result-large-err = "allow"` override, and a workflow command containing
`-A clippy::result_large_err`. Every one emits
`large-error-lint-suppression: clippy::result_large_err must remain denied without suppression`
with its fixture path.

The self-test writes each fixture beneath one
`tempfile.TemporaryDirectory`, scans paths in sorted order, requires no
diagnostic for positive fixtures, and requires exactly one stable diagnostic
containing both the rule ID and fixture path for each negative mutation.
`scan_fixture` returning an empty list makes the negative fixtures fail.

- [ ] **Step 5: Run the strict RED gates**

Run:

```bash
cargo test -p donat-connector-abi --test abi_contract \
  --no-default-features --offline --locked
cargo test -p donat-connector-abi --doc \
  --no-default-features --offline --locked
python3 scripts/check_connector_processor_boundary.py --self-test
```

Expected:

- the integration target fails to compile because the new static types,
  namespaces, accessors, and failure signature do not exist;
- rustdoc reports that the response literal or field-assignment example
  compiled even though it is marked `compile_fail`;
- the checker reports that a named negative fixture produced no expected
  diagnostic.

Do not change production code before recording all three RED results.

- [ ] **Step 6: Implement `StaticErrorCode` through the one ID validator**

In `crates/connector-abi/src/ids.rs`, add:

```rust
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticErrorCode(InlineId);

impl StaticErrorCode {
    pub const fn literal(value: &'static str) -> Self {
        Self(InlineId::literal(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn parse_catalog(value: &str) -> Result<Self, AbiError> {
        InlineId::parse(value).map(Self).map_err(|_| {
            AbiError::InvalidValue(
                "connector failure code must be a canonical ABI identifier",
            )
        })
    }
}
```

Do not add `parse`, `TryFrom`, `From`, `Into`, `Default`, a public tuple
field, or another ID validator to the public API.

- [ ] **Step 7: Implement the one const-capable static-message validator**

In `crates/connector-abi/src/envelope.rs`, add the private-field
`StaticSafeMessage`. Use one private const byte validator and one private
const copier for both entry paths:

```rust
#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticSafeMessage {
    len: u16,
    bytes: [u8; MAXIMUM_SAFE_MESSAGE_BYTES],
}

impl StaticSafeMessage {
    pub const fn literal(value: &'static str) -> Self {
        match Self::try_copy(value) {
            Ok(message) => message,
            Err(_) => panic!("invalid connector failure safe message literal"),
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("validated connector failure messages are UTF-8")
    }

    pub(crate) fn try_catalog(value: &str) -> Result<Self, AbiError> {
        Self::try_copy(value)
    }
}
```

Because each input is already valid UTF-8, the shared const byte scan rejects
ASCII control bytes `0x00..=0x1f`, `0x7f`, and the valid UTF-8 sequence
`0xc2 0x80..=0x9f`; these are exactly the Unicode scalar values for which
`char::is_control()` is true. Return the approved errors:

```rust
AbiError::InvalidValue(
    "connector failure safe message must not be empty",
)
AbiError::LimitExceeded("connector failure safe message bytes")
AbiError::InvalidValue(
    "connector failure safe message must not contain control characters",
)
```

Start from `[0_u8; MAXIMUM_SAFE_MESSAGE_BYTES]`, copy exactly `value.len()`
bytes, and leave the suffix zero-filled. Do not use unsafe code or a second
runtime grammar.

- [ ] **Step 8: Make responses and correlations opaque**

In `crates/connector-abi/src/envelope.rs`, define:

```rust
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
```

Implement only the public shared accessors specified in this plan. Keep
`AuthorizedCorrelations` non-`Clone`.

Make `BoundedTransportResponse::try_new` call the common private response
validator and store:

```rust
AuthorizedCorrelations {
    values: BTreeMap::new(),
}
```

Add a crate-private host constructor that validates the same response,
calls the one authorization helper, and stores the resulting opaque value.
The authorization helper:

1. validates captured selected headers with the existing 64/8,192/32,768
   limits;
2. rejects more than 64 allowed IDs with
   `AbiError::LimitExceeded("correlation authorization entries")`;
3. rejects the first duplicate allowed ID with
   `AbiError::InvalidValue("correlation authorization contains a duplicate capability")`;
4. copies only allowlisted IDs present in selected headers;
5. omits allowlisted IDs that are absent;
6. leaves selected-but-unallowed values available only through
   `selected_headers()`.

Every aggregate uses `checked_add` and returns `AbiError::SizeOverflow` on
overflow.

- [ ] **Step 9: Replace the raw failure constructor without a shim**

Add `use alloc::boxed::Box;` to
`crates/connector-abi/src/envelope.rs`. Change the private failure layout to:

```rust
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
```

Implement the exact new constructor signature. For `Some(authority)`, clone
the already checked private `BTreeMap` internally; for `None`, store an empty
map. Clamp the retry value before allocation:

```rust
let retry_after_seconds = retry_after_seconds
    .map(|seconds| seconds.min(u64::from(MAXIMUM_RETRY_AFTER_SECONDS)) as u32);
```

Only after the correlation copy and retry clamp succeed, allocate and publish
the complete value atomically:

```rust
let static_text = Box::new(StaticFailureText {
    code,
    safe_message,
});

Ok(Self {
    class,
    static_text,
    retry_after_seconds,
    correlation_ids,
})
```

Return code/message accessors as `&str` by dereferencing the private bundle:

```rust
pub fn code(&self) -> &str {
    self.static_text.code.as_str()
}

pub fn safe_message(&self) -> &str {
    self.static_text.safe_message.as_str()
}
```

`StaticFailureText` has no independent constructor or accessor. Standard
`alloc` allocation-failure behavior applies; do not add an `AbiError`
variant. The box replaces roughly 1,123 inline bytes with one pointer in the
`ConnectorFailure` error layout, allowing strict
`clippy::result_large_err` to pass for every existing
`Result<_, ConnectorFailure>` without lint suppression.

Delete the old raw `&str`, raw correlation map, and caller allowlist
parameters. Do not retain an overload, deprecated function, conversion, or
adapter. Do not add `#[allow(clippy::result_large_err)]`,
`#[expect(clippy::result_large_err)]`, a crate lint override, a Cargo lint
override, or a command-line `-A`/`--allow`/`--cap-lints` suppression.

- [ ] **Step 10: Share typed-value counters across every binding root**

Replace `validate_typed_value_shape` with:

```rust
#[derive(Default)]
struct ValueCounters {
    nodes: usize,
    inline_values: usize,
    decoded_inline_bytes: usize,
}

fn validate_typed_value_roots<'a>(
    roots: impl Iterator<Item = &'a TypedValue>,
) -> Result<(), AbiError> {
    let mut counters = ValueCounters::default();
    for root in roots {
        validate_typed_value_root(root, &mut counters)?;
    }
    Ok(())
}
```

Each `validate_typed_value_root` creates a fresh stack containing
`(root, 0)`, but receives the same `ValueCounters`. Count every root and
child as a node with checked arithmetic. For every
`TypedValue::InlineBytes(value)`, increment `inline_values` and add
`value.as_slice().len()` to `decoded_inline_bytes` with checked arithmetic.
Return exactly:

```rust
AbiError::LimitExceeded("typed value node count")
AbiError::LimitExceeded("inline value count")
AbiError::LimitExceeded("aggregate decoded inline bytes")
AbiError::LimitExceeded("typed value nesting depth")
AbiError::SizeOverflow
```

In `TypedBindings::try_new`, call
`validate_typed_value_roots(slots.values())` before the canonical-size loop.
Keep the canonical sum checked and return
`AbiError::LimitExceeded("binding canonical bytes")` above 262,144 bytes.
Use the same root helper for the response's single decoded root. Perform no
status validation.

- [ ] **Step 11: Define only the approved root construction namespaces**

In `crates/connector-abi/src/lib.rs`, define the two exact public namespaces
from Task 1 directly at crate root as thin delegates to the private
constructors:

```rust
#[doc(hidden)]
pub mod catalog_construction {
    use crate::{AbiError, StaticErrorCode, StaticSafeMessage};

    pub fn static_error_code(
        value: &str,
    ) -> Result<StaticErrorCode, AbiError> {
        StaticErrorCode::parse_catalog(value)
    }

    pub fn static_safe_message(
        value: &str,
    ) -> Result<StaticSafeMessage, AbiError> {
        StaticSafeMessage::try_catalog(value)
    }
}
```

`host_construction::transport_response` delegates to the crate-private host
response constructor.
`host_construction::authorized_correlations` delegates to the same
authorization helper. Neither module exposes a type alias, trait, macro,
mutable handle, or raw-parts constructor.

Do not place either namespace behind another module and do not re-export it.
Export `StaticErrorCode`, `StaticSafeMessage`, and
`AuthorizedCorrelations`. Preserve all current constants and exports.

- [ ] **Step 12: Implement the initial deterministic boundary checker**

Replace the RED `scan_fixture` body with the same scanner used for the real
workspace. Resolve the repository root from
`Path(__file__).resolve().parents[1]`, traverse candidate Rust files in
sorted relative-path order, never traverse `target`, `.git`, or another
worktree, and emit:

```python
f"connector-boundary: {relative_path}: {rule_id}: {message}"
```

Use one Python-standard-library lexical pass that blanks Rust line comments,
nested block comments, ordinary/raw byte and text strings, and character
literals while preserving byte offsets and newlines. Apply producer/callsite
rules to the resulting tokens so documentation examples and string decoys
cannot create false positives. Track braces plus the immediately preceding
`#[cfg(test)]` attribute to distinguish a private test module from production
code. Include comment/string decoys among the positive self-test fixtures.

Enforce these exact production allowlists:

```python
HOST_CONSTRUCTION_ROOTS = ("crates/server/src/connectors/",)
CATALOG_CONSTRUCTION_ROOTS = ("crates/connector-catalog/src/",)
STATIC_LITERAL_ROOTS = (
    "crates/connector-abi/src/",
    "crates/connector-processors/src/",
    "crates/connector-catalog/src/generated/",
)
HOST_IMPL_ROOTS = ("crates/server/src/connectors/",)
```

Enforce these exact test roots:

```python
ABI_TEST_ROOTS = ("crates/connector-abi/tests/",)
PROCESSOR_TEST_ROOTS = ("crates/connector-processors/tests/",)
SERVER_TEST_ROOTS = ("crates/server/tests/",)
```

ABI tests may call both construction namespaces. Processor and server tests
may define fake `ConnectorIo`/`ProcessorControl` implementations. ABI tests
may not define those implementations. Test-only implementations in
production files are accepted only inside a private `#[cfg(test)] mod` under
the corresponding allowed ABI/processor/server crate; exporting a test
helper is rejected.

Reject every `pub use` or alias of a restricted namespace. Outside its
producer path, reject direct calls and forwarding functions, macros, type
aliases, and traits. Under `crates/connector-processors/src/`, reject
`::leak`, `Box::leak`, `String::leak`, and `Vec::leak`.

Apply the same anti-indirection policy to both static literal constructors.
Track alias imports such as
`use donat_connector_abi::StaticErrorCode as ErrorCode`, corresponding
`StaticSafeMessage` aliases, `pub use`, `type` aliases, and
function/macro/trait definitions that name or invoke
`StaticErrorCode::literal` or `StaticSafeMessage::literal`. Resolve their
local alias at call sites and reject the call, export, or forwarding
definition when its path is outside `STATIC_LITERAL_ROOTS`. Emit the exact
fixture diagnostics from Step 4 and emit only the most-specific indirection
diagnostic when a direct-call rule would otherwise duplicate it.

Separately scan `crates/connector-abi/src/`,
`crates/connector-abi/Cargo.toml`, root `Cargo.toml`, an existing
`.cargo/config.toml`, and `.github/workflows/*.yml` for
`clippy::result_large_err` suppression. Reject `allow`, `expect`,
`result-large-err = "allow"`, `-A`, `--allow`, `--cap-lints`, or an
equivalent underscore/hyphen spelling with the exact
`large-error-lint-suppression` diagnostic from Step 4. A strict
`-D clippy::result_large_err` occurrence is permitted.

Run deterministic self-tests on every invocation before scanning the real
workspace; `--self-test` runs only the fixtures. Sort diagnostics by path,
rule ID, and source offset. Require exactly the expected rule and path for
every negative fixture from Step 4.

Task 6 later adds locked dependency, manifest, symlink, generated/donor, and
remaining source-token policy to this file. Do not implement a second checker
or emit a future policy file in this task.

- [ ] **Step 13: Wire the one checker before Rust compilation**

In `.github/workflows/ci.yml`, add this step immediately before the current
`Unit and snapshot tests` step in the `test` job:

```yaml
      - name: Connector processor boundary policy
        run: python3 scripts/check_connector_processor_boundary.py
```

Do not add a Python dependency, action, wrapper script, second job, or
duplicate invocation.

- [ ] **Step 14: Run focused GREEN and boundary verification**

Run:

```bash
python3 scripts/check_connector_processor_boundary.py --self-test
python3 scripts/check_connector_processor_boundary.py
cargo test -p donat-connector-abi --test abi_contract \
  --no-default-features --offline --locked
cargo test -p donat-connector-abi --doc \
  --no-default-features --offline --locked
cargo test -p donat-connector-abi \
  --no-default-features --offline --locked
cargo test -p donat-value-contract \
  --no-default-features --offline --locked
cargo check -p donat-connector-abi --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-abi --target all \
  --edges normal,build --no-default-features --offline --locked
cargo clippy -p donat-connector-abi --all-targets \
  --no-default-features --offline --locked -- \
  -D warnings -D clippy::result_large_err
cargo fmt --all -- --check
git diff --check
```

Expected:

- all ABI behavior, unit, and external compile-fail doctests pass;
- all 16 existing value-contract integration tests still pass;
- the no-OS build succeeds;
- the dependency tree is exactly:

```text
donat-connector-abi v0.1.0
└── donat-value-contract v0.1.0
```

- strict Clippy proves the private boxed layout stays below the large-error
  threshold without a lint escape;
- format and whitespace checks exit zero.

- [ ] **Step 15: Run source-breaking and scope scans**

Run:

```bash
if rg -n \
  'pub (status|selected_headers|decoded|response_bytes):' \
  crates/connector-abi/src; then
  exit 1
fi
if rg -n \
  'allowed_correlation_ids|code: &str|safe_message: &str' \
  crates/connector-abi/src; then
  exit 1
fi
if rg -n \
  'impl([^[:alnum:]]|<[^>]+> )*(From|TryFrom)<' \
  crates/connector-abi/src/envelope.rs \
  crates/connector-abi/src/ids.rs; then
  exit 1
fi
if rg -n \
  'result[_-]large[_-]err' \
  crates/connector-abi; then
  exit 1
fi
if rg -n \
  '(-A|--allow|--cap-lints|allow|expect).*clippy::result[_-]large[_-]err|result-large-err[[:space:]]*=[[:space:]]*"allow"' \
  Cargo.toml crates/connector-abi/Cargo.toml .github/workflows; then
  exit 1
fi
rg -n \
  'pub struct (StaticErrorCode|StaticSafeMessage|AuthorizedCorrelations|BoundedTransportResponse|ConnectorFailure)|struct StaticFailureText|static_text: Box<StaticFailureText>|pub mod (catalog_construction|host_construction)' \
  crates/connector-abi/src
git diff --name-only HEAD
```

Expected: all negative scans are empty; the positive scan finds the five
public invariant-carrying types, private boxed static-text bundle, and two
unique namespaces; changed paths are exactly the six paths listed for Task 2.

- [ ] **Step 16: Rebuild `donat` and run native conformance**

Run:

```bash
cargo build -p donat-server --bin donat --offline --locked
test -x "$DONAT_BIN"
cargo test -p donat-conformance --test connectors --offline --locked
cargo test -p donat-conformance --offline --locked
```

Expected:

- the binary at the caller-supplied `DONAT_BIN` is the freshly rebuilt
  `CARGO_TARGET_DIR` binary;
- connector conformance reports at least the existing 4 passing cases and
  zero failures;
- the complete conformance crate reports at least the existing 261 passing
  tests and zero failures.

The focused connector target must run before the full conformance crate.

- [ ] **Step 17: Perform the implementer self-review**

Review:

```bash
git diff -- \
  .github/workflows/ci.yml \
  crates/connector-abi/src/envelope.rs \
  crates/connector-abi/src/ids.rs \
  crates/connector-abi/src/lib.rs \
  crates/connector-abi/tests/abi_contract.rs \
  scripts/check_connector_processor_boundary.py
```

Check every approved-design section against the diff:

- response fields are private and every read is shared-only;
- `ConnectorFailure` owns exactly one private boxed `StaticFailureText`, its
  accessors dereference that box, and no lint suppression hides the layout;
- ordinary responses carry no authority;
- host authorization is an intersection and the failure caller supplies no
  allowlist;
- static code/message runtime construction exists only in the catalog
  namespace;
- all `u16` statuses are accepted;
- counters are shared across roots while depth restarts;
- every exact/one-over limit and compile-fail invariant has a named test;
- every checker rule has a positive and deliberately mutated negative
  fixture with a stable path-specific diagnostic;
- no host signature, value-contract API, Cargo dependency, provider, catalog,
  processor, server runtime, admin/bypass, or logical/workflow behavior
  changed.

This is the implementer's self-review only. Do not dispatch an independent
reviewer or Judge before the commit. A material self-review finding first
gains a focused regression test, then repeats Steps 14 through 16.

- [ ] **Step 18: Stage exact paths and commit the remediation**

Run:

```bash
git status --short
git add -- \
  .github/workflows/ci.yml \
  crates/connector-abi/src/envelope.rs \
  crates/connector-abi/src/ids.rs \
  crates/connector-abi/src/lib.rs \
  crates/connector-abi/tests/abi_contract.rs \
  scripts/check_connector_processor_boundary.py
git diff --cached --name-only
git diff --cached --check
```

Expected staged paths, and no others:

```text
.github/workflows/ci.yml
crates/connector-abi/src/envelope.rs
crates/connector-abi/src/ids.rs
crates/connector-abi/src/lib.rs
crates/connector-abi/tests/abi_contract.rs
scripts/check_connector_processor_boundary.py
```

Commit:

```bash
git commit -m "fix(connectors): enforce safe connector ABI"
```

After the commit, run:

```bash
git status --short
git show --stat --oneline --decorate HEAD
```

Expected: the worktree is clean and the commit contains only the six staged
paths above.

After the commit, the SDD controller generates a Task 2 review package from
the committed diff and dispatches an ordinary independent task reviewer. It
does not dispatch Judge. A rejection returns each material finding to the
Task 2 implementer; the implementer adds a focused regression test, repeats
Steps 14 through 16, commits the correction, and the controller generates a
new package for a fresh ordinary task review. The community connector
catalog task must not start until this post-commit Task 2 review records
acceptance.

## Plan Self-Review Checklist

- [ ] Every approved-design requirement maps to Task 1 documentation or Task
  2 implementation/test evidence.
- [ ] The public signatures in the global constraints, Task 1, and Task 2 are
  identical.
- [ ] Spec 007, ADR 010, and community-plan Task 2/6/8 ownership are aligned
  before production code changes.
- [ ] The raw-public-field contradiction is removed; no compatibility shim is
  planned.
- [ ] Private `StaticFailureText` is boxed exactly once after validation;
  public failure signatures stay unchanged and strict large-error Clippy runs
  without suppression.
- [ ] Status, selected headers, decoded value, and response-byte mutation
  each have an independent external compile-fail check.
- [ ] Task 2 creates the one checker and CI call; Task 6 modifies and extends
  that checker; Task 8 alone uses host construction.
- [ ] Static literal aliases, re-exports, type aliases, function/macro/trait
  wrappers, and lint suppressions each have exact deterministic negative
  fixtures.
- [ ] Task 1 and Task 2 each receive an ordinary independent post-commit SDD
  review; no Judge or pre-commit independent reviewer is planned, and their
  downstream task is gated on acceptance.
- [ ] Every created/modified path exists now or is explicitly marked
  `Create`, and no task claims another task's provider/catalog/processor/
  server-runtime ownership.
- [ ] RED precedes production implementation, and GREEN includes ABI, value,
  no-OS, dependency tree, clippy, format, diff, rebuilt binary, focused
  connector conformance, and full 261-or-more conformance.
- [ ] Cargo commands use the `/dev/shm` target environment and never assign
  `PG_URL` or `DONAT_BIN`.
- [ ] Staging commands enumerate exact paths and both task commit messages are
  exact.
- [ ] The plan contains no unfinished marker, vague error-handling step,
  unnamed test request, or undefined interface.
