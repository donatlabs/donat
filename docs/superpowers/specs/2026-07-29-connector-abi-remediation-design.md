# Connector ABI remediation design

Date: 2026-07-29

Baseline: `db289dc` (`feat(connectors): add neutral connector ABI`)

Decision: approved Option 1 — opaque envelopes with restricted construction
namespaces and mechanically enforced producer allowlists

## Context

The Task 2 ABI establishes the correct `no_std + alloc`, local-dependency,
typed-ID, object-safe host boundary, but three exported contracts do not yet
enforce their stated guarantees.

First, `BoundedTransportResponse` exposes raw public fields. External code can
construct the struct without `try_new` or mutate a checked instance, bypassing
the header, decoded-value, and response-byte limits.

Second, `ConnectorFailure::try_new` accepts runtime `&str` values for a
Donat-owned error code and safe message. The same caller supplies both
correlation values and the allowlist that purports to authorize them. A
processor can therefore copy provider text into a safe field and
self-authorize a secret-bearing correlation value.

Third, `TypedBindings::try_new` resets node and inline-binary counters for
every binding slot. The exact 100,000-node, 16-inline-value, and
131,072-decoded-inline-byte limits apply to the complete bindings envelope,
not independently to each root.

Fourth, storing the 97-byte `StaticErrorCode` and approximately 1,026-byte
`StaticSafeMessage` inline would make `ConnectorFailure` a large `Err` variant
for every `Result<_, ConnectorFailure>`. The ABI's mandatory
`cargo clippy -- -D warnings` gate would reject that layout through
`clippy::result_large_err`.

This design closes those gaps without changing the accepted connector crate
graph, host trait signatures, error classes, value-contract ownership, or
static-factory boundary.

## Decision

The ABI owns five public invariant-carrying types:

1. `StaticErrorCode`, a private-field wrapper over the existing checked
   `InlineId`;
2. `StaticSafeMessage`, a private fixed-capacity UTF-8 value;
3. `AuthorizedCorrelations`, a private bounded map with no public insertion
   path;
4. `BoundedTransportResponse`, an immutable private-field response envelope;
5. `ConnectorFailure`, an immutable failure containing one private boxed
   static-text bundle and host/catalog-authorized correlations.

Ordinary checked response construction creates no correlation authority.
The server attaches authority through the uniquely named
`host_construction` namespace after intersecting captured selected headers
with the catalog-derived allowlist. Catalog loading creates static code and
message values through the uniquely named `catalog_construction` namespace.
Whole-workspace CI restricts both namespaces to exact production paths.

`TypedBindings` performs one aggregate shape pass over all roots with shared
node, inline-value, and decoded-inline-byte counters. Depth starts at zero for
each root. Canonical bytes remain a checked aggregate sum of the shared value
owner's `canonical_size` result for each root.

The status contract remains exactly `u16`. Every value from `0` through
`u16::MAX` is accepted. This remediation adds no HTTP semantic range.

## Public ABI

The following types and methods are public from `donat-connector-abi`.
Every field shown without `pub` is private to the ABI crate.

```rust
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticErrorCode(InlineId);

#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticSafeMessage {
    len: u16,
    bytes: [u8; MAXIMUM_SAFE_MESSAGE_BYTES],
}

impl StaticErrorCode {
    pub const fn literal(value: &'static str) -> Self;
    pub fn as_str(&self) -> &str;
}

impl StaticSafeMessage {
    pub const fn literal(value: &'static str) -> Self;
    pub fn as_str(&self) -> &str;
}

pub struct AuthorizedCorrelations {
    values: BTreeMap<CapabilityId, BoundedString>,
}

impl AuthorizedCorrelations {
    pub fn get(&self, id: &CapabilityId) -> Option<&BoundedString>;

    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&CapabilityId, &BoundedString)>;

    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
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

    pub fn authorized_correlations(
        &self,
    ) -> &AuthorizedCorrelations;
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

There is no public mutable accessor, `Default`, tuple field, map insertion
method, raw-parts constructor, `From`/`Into`, `TryFrom<&str>`, or
`TryFrom<String>` for the five invariant-carrying types.

`AuthorizedCorrelations` does not implement `Clone`. A failure copies the
already checked map internally when it receives
`Option<&AuthorizedCorrelations>`. A caller chooses either all host-authorized
correlations or none; it cannot add a key, replace a value, or declare a new
allowlist.

`BoundedTransportResponse::try_new` performs all ordinary response checks and
stores an empty `AuthorizedCorrelations`. It accepts every `u16` status.
Read-only collection access cannot invalidate a checked response.

`ConnectorFailure::try_new` clamps `retry_after_seconds` to
`MAXIMUM_RETRY_AFTER_SECONDS`. It never accepts a raw code, raw safe message,
raw correlation map, or caller-supplied allowlist.

`StaticFailureText` is private and has no independent constructor or accessor.
`ConnectorFailure::try_new` validates all remaining inputs and boxes the two
already validated static values together as
`Box::new(StaticFailureText { code, safe_message })`. It publishes the failure
only after the complete value exists. `code()` and `safe_message()` dereference
`static_text`. The public constructor and accessor signatures remain
unchanged.

The box uses `alloc::boxed::Box`, so the ABI remains `no_std + alloc` with no
new dependency. The `ConnectorFailure` `Err` layout contains one pointer
instead of both inline fixed-capacity values. No
`allow(clippy::result_large_err)`, `expect(clippy::result_large_err)`, crate
lint override, or command-line lint suppression is permitted.

## Static failure text

`StaticErrorCode` uses the existing `InlineId` representation and validator.
Its accepted language remains `1..=96` ASCII bytes matching
`[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?`. `literal` is const-constructible and
panics during const evaluation for invalid input. `as_str` returns only the
validated spelling.

`StaticSafeMessage` stores:

- a private `u16` UTF-8 byte length;
- a private `[u8; 1024]` buffer;
- zero bytes after the validated message length.

The shared const-capable validator enforces all of these conditions:

- the message contains at least one byte;
- the UTF-8 byte length is at most 1,024;
- the input is valid UTF-8 by construction because both entry points receive
  `&str`;
- no Unicode scalar in the message satisfies `char::is_control()`;
- bytes after `len` are zero-filled by the constructor.

`StaticSafeMessage::literal` and the runtime catalog constructor call the same
validator and byte copier. There is no second message grammar. `as_str` uses
checked UTF-8 conversion and no unsafe code.

`StaticErrorCode` and `StaticSafeMessage` retain their exact public
representations and const behavior. Boxing occurs only when
`ConnectorFailure::try_new` combines them into the private
`StaticFailureText`; catalog entries and processor constants remain inline,
const-constructible values.

Processor implementations use `literal` only with reviewed source literals.
They cannot convert response strings, provider bodies, headers, credentials,
or other runtime values into either static type through the ordinary API.
The processor boundary rejects allocation leaks, which are the safe-Rust
escape from a runtime allocation to `&'static str`.

## Restricted construction namespaces

The ABI exports two `#[doc(hidden)]` namespaces because the intended producer
crates are outside `donat-connector-abi`:

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

`catalog_construction` accepts normalized checked-in Donat policy, validates
it with the exact static validators, and copies it into the private
representations. It is not a provider-response parsing API.

`host_construction::transport_response` calls the same private response
constructor as the public unprivileged path. It then constructs
`AuthorizedCorrelations` from the intersection of:

1. bounded `selected_headers` captured by the server executor; and
2. the derived `CapabilityId` allowlist compiled from the selected catalog
   `ErrorAction`.

The host path rejects more than 64 allowlist entries and duplicate capability
IDs. An allowlisted ID absent from `selected_headers` contributes no value and
does not produce an error. A selected header absent from the allowlist remains
available through `selected_headers()` but never enters
`AuthorizedCorrelations`.

`host_construction::authorized_correlations` exists for transport failures
and error mapping paths that do not retain a complete success response. It
applies the same intersection, duplicate, entry-count, per-value, and
aggregate-byte checks as `transport_response`.

Neither namespace is a security sandbox. Both are mechanically restricted
cross-crate construction APIs.

## Why Rust privacy is insufficient

Rust has no friend-crate visibility. `pub(crate)` grants access only inside
`donat-connector-abi`; `pub(in path)` accepts only an ancestor module path in
the same crate. Neither form grants construction to `donat-server` or
`donat-connector-catalog` while denying it to
`donat-connector-processors`.

A sealed trait does not solve the problem because sealing
`ConnectorIo`/`ProcessorControl` would also prevent the required external
server implementations. The orphan rules permit every dependent crate to
implement these public traits for its own local type. A Cargo feature does
not form an authority boundary because feature unification exposes an enabled
API throughout the complete dependency graph.

Rust also does not prove that an `&'static str` originated as a source
literal. Safe allocation-leak APIs can manufacture that lifetime. The design
therefore combines private invariant-carrying fields with exact CI producer
and callsite rules.

## Exact CI producer and callsite policy

This remediation creates
`scripts/check_connector_processor_boundary.py` when the file is absent and
owns its initial restricted-namespace, trait-implementation, allocation-leak,
producer-path, and test-path policy. Later Task 6 extends this exact checker
with the remaining processor dependency and source-boundary rules. Task 6
must not create a second checker, wrapper checker, or parallel policy file.

The accepted follow-up
`docs/superpowers/specs/2026-07-29-connector-boundary-lexer-remediation-design.md`
and ADR 011 refine how that policy recognizes Rust. The checker must not use
Python Unicode identifier/whitespace classification or a version-pinned XID
table. It preserves `r#` state, over-approximates identifier atoms from Rust's
fixed whitespace/punctuation boundaries, and recursively resolves grouped
`use` trees so every compiler-valid alias remains visible.

`scripts/check_connector_processor_boundary.py` enforces the following
whole-workspace production allowlists:

| Construct or implementation | Allowed production paths |
| --- | --- |
| Any reference to `donat_connector_abi::host_construction` | `crates/server/src/connectors/` |
| Any reference to `donat_connector_abi::catalog_construction` | `crates/connector-catalog/src/` |
| `StaticErrorCode::literal` or `StaticSafeMessage::literal` | `crates/connector-abi/src/`, `crates/connector-processors/src/`, `crates/connector-catalog/src/generated/` |
| `impl ConnectorIo` or `impl ProcessorControl` | `crates/server/src/connectors/` |

Tests use a separate exact allowlist:

- `crates/connector-abi/tests/` and ABI `#[cfg(test)]` modules exercise both
  construction namespaces;
- `crates/connector-processors/tests/` and processor `#[cfg(test)]` modules
  contain fake `ConnectorIo`/`ProcessorControl` implementations;
- `crates/server/tests/` contains server integration fakes;
- no test helper is exported from a production library module.

The checker rejects all of the following throughout production source:

- a re-export of either restricted namespace;
- an alias import of either restricted namespace;
- a wrapper function, macro, type alias, or trait that forwards either
  namespace outside its allowlisted path;
- `impl ConnectorIo` or `impl ProcessorControl` outside the server path;
- `::leak`, `Box::leak`, `String::leak`, or `Vec::leak` under
  `crates/connector-processors/src/`;
- unsafe code, FFI, assembly, build scripts, native dependencies, and the
  existing forbidden processor-boundary tokens.

The checker includes deliberately mutated temporary negative fixtures for
every rule. Each fixture must fail with a stable diagnostic naming the
disallowed construct and source path. CI runs the checker before compiling
the processor crate. Review rejects any syntax or indirection added to evade
the unique namespace names or callsite scan.

## Response, correlation, and failure data flow

The complete flow is:

1. Task 3 retains `ErrorAction.correlation_headers` as normalized
   `StaticHeaderName` values. Catalog compilation resolves every selected
   correlation header to exactly one response-header `CapabilityId`.
   Compilation fails if a header has no capability, resolves more than once,
   duplicates another allowlist entry, or produces more than 64 entries. The
   generated descriptor exposes the derived `&[CapabilityId]`; the source
   metadata schema does not change.
2. Task 8 captures only compiled selected headers. Header names become their
   compiled `CapabilityId`; values become `BoundedString`. Existing 64-entry,
   8 KiB-value, and 32 KiB-aggregate checks run before the response exists.
3. The executor calls
   `host_construction::transport_response(status, selected_headers, decoded,
   response_bytes, allowed_correlations)`.
4. The ABI validates the response, intersects selected and allowed IDs, and
   returns an immutable `BoundedTransportResponse`.
5. A processor reads status, selected headers, decoded value, and byte count
   through shared accessors. It receives no mutable field or insertion API.
6. A processor or server constructs a failure with a static code, static safe
   message, optional retry delay, and either
   `Some(response.authorized_correlations())` or `None`.
7. `ConnectorFailure` clones the authorized bounded map, clamps the retry
   delay, boxes the code and safe message together as one private
   `StaticFailureText`, and exposes only shared read access by dereferencing
   that box.

Provider bodies, provider messages, arbitrary headers, authorization
material, credentials, URLs, and tokens never enter the failure. Task 8's
complete fallback remains:

```text
transport/TLS/DNS                         -> transport
activity deadline or HTTP 408             -> timeout
HTTP 429                                  -> http_429
declared 5xx                              -> http_5xx
HTTP 401/403                              -> authentication
declared validation or malformed success  -> validation
other supported non-success               -> permanent
compiled-contract violation               -> invariant
```

Only the catalog-selected Donat code/message pair, clamped retry delay, and
authorized correlations survive mapping.

## Response invariants and status

Both response construction paths enforce:

- at most 64 selected headers;
- at most 8,192 UTF-8 bytes per selected value;
- at most 32,768 aggregate retained key/value bytes;
- `response_bytes <= 1_048_576`;
- at most 64 levels below a typed-value root;
- at most 100,000 typed-value nodes;
- at most 16 inline-binary values;
- at most 131,072 aggregate decoded inline-binary bytes;
- at most 262,144 canonical output bytes;
- at most 262,144 UTF-8 bytes in an individual typed string or object key;
- checked arithmetic for every aggregate.

The `status` field has no semantic invariant beyond its `u16`
representation. Construction accepts `0`, `u16::MAX`, and every intermediate
value. Error classification remains the responsibility of the compiled
Task 8 error map. A narrower HTTP-status domain requires a separate accepted
specification or ADR and does not enter this remediation.

## Aggregate `TypedBindings` accounting

The ABI replaces the per-root shape counter with:

```rust
#[derive(Default)]
struct ValueCounters {
    nodes: usize,
    inline_values: usize,
    decoded_inline_bytes: usize,
}

fn validate_typed_value_roots<'a>(
    roots: impl Iterator<Item = &'a TypedValue>,
) -> Result<(), AbiError>;
```

`TypedBindings::try_new` executes these checks in this order:

1. reject more than 64 binding slots;
2. traverse all slot values with one shared `ValueCounters`;
3. call `canonical_size` for each root and add every result to one checked
   canonical-byte sum;
4. construct `TypedBindings` only after every check passes.

The traversal counts every `TypedValue`, including each root, as one node.
It increments `inline_values` for every `TypedValue::InlineBytes` and adds
`BoundedInlineBytes::as_slice().len()` to `decoded_inline_bytes`. All three
increments use checked arithmetic. It returns:

- `AbiError::LimitExceeded("typed value node count")` above 100,000 nodes;
- `AbiError::LimitExceeded("inline value count")` above 16 inline values;
- `AbiError::LimitExceeded("aggregate decoded inline bytes")` above 131,072
  decoded bytes;
- `AbiError::SizeOverflow` for arithmetic overflow.

Each root starts with depth zero and a fresh traversal stack, while the three
counters remain shared. A child increments depth by one. Depth 64 is accepted;
depth 65 returns
`AbiError::LimitExceeded("typed value nesting depth")`.

The shape pass runs before canonical summation so the aggregate node counter
is independently observable even when the canonical-byte ceiling is smaller
for a particular constructed value. The canonical sum retains the existing
`AbiError::LimitExceeded("binding canonical bytes")` result above 262,144
bytes.

The response validator calls the same helper with one decoded root. The
shared value crate remains the sole owner of `canonical_size` and
`BoundedInlineBytes`; this design changes no value-contract API.

## Error handling

Constructor errors remain `AbiError` and never become provider diagnostics.
Every validation path fails atomically; no constructor returns a truncated
header map, partial correlation set, partial typed value, or shortened safe
message.

Failure construction creates exactly one `Box<StaticFailureText>` after its
static values and bounded correlations have passed validation. No caller can
observe or recover a partially assembled static-text bundle. Allocation uses
the standard `alloc` allocation-failure behavior and does not introduce a new
ABI error variant.

The new static constructors return:

- `AbiError::InvalidValue("connector failure code must be a canonical ABI identifier")`
  for an invalid runtime catalog code;
- `AbiError::InvalidValue("connector failure safe message must not be empty")`
  for an empty message;
- `AbiError::LimitExceeded("connector failure safe message bytes")` above
  1,024 bytes;
- `AbiError::InvalidValue("connector failure safe message must not contain control characters")`
  for a control character.

The host authorization path returns:

- `AbiError::LimitExceeded("correlation authorization entries")` above 64
  allowed IDs;
- `AbiError::InvalidValue("correlation authorization contains a duplicate capability")`
  for a duplicate;
- the existing selected-header value and aggregate limit errors for captured
  values;
- `AbiError::SizeOverflow` for aggregate arithmetic overflow.

Runtime executor code maps ABI construction failure caused by an admitted
compiled contract to the closed `invariant` class. It uses a reviewed
ABI-owned static code/message pair and no provider text. Build/deployment
catalog validation catches invalid static policy before serving starts.

## Downstream contracts

### Task 3 — normalized catalog

Task 3 imports `StaticErrorCode` and `StaticSafeMessage` from
`donat-connector-abi`. It does not define replacement wrappers.

Its strict normalized loader calls `catalog_construction` only after unknown
fields, duplicate keys, and source provenance pass catalog validation.
`ErrorAction` stores the ABI-owned static types. The normalized
`correlation_headers` field remains unchanged. Catalog compilation derives
and validates the generated correlation capability allowlist described above.
Static text values and the derived correlation IDs enter semantic hashing.
The source schema gains no runtime expression, provider message, arbitrary
header value, or processor path.

### Task 6 — processor boundary

Task 6 reads response fields through accessors. Its Donat-owned processor
constants use `StaticErrorCode::literal` and
`StaticSafeMessage::literal`. Processor production code has no reference to
either restricted construction namespace and no allocation-leak API.

Processors propagate failures from `ConnectorIo` and `ProcessorControl`.
When a reviewed processor creates a failure, it supplies only source-static
code/message values and either host-supplied opaque correlations or none.
It neither constructs nor names `StaticFailureText`; boxing remains an ABI
implementation detail.
The `OperationProcessor` result type and `ConnectorIo::call` signature do not
change.

Test-only fake host implementations stay under the exact test allowlist. The
production dependency closure remains only `donat-connector-abi` and
`donat-value-contract`.

### Task 8 — fixed-origin executor

Task 8 is the only production caller of `host_construction`. The executor
resolves a step from the current generated operation, captures bounded
selected headers, obtains the derived catalog correlation capabilities, and
constructs the response through the host path.

The executor passes `StaticErrorCode` and `StaticSafeMessage` from the selected
catalog `ErrorAction` directly into `ConnectorFailure::try_new`. It never
converts raw provider text into either type. Transport failures use
`None` when no authorized correlations exist. Error responses use the opaque
authorized set produced from captured headers and the selected action.
The server does not allocate or inspect `StaticFailureText`; the ABI performs
the single private box allocation.

No server method accepts a raw failure code, raw safe message, raw correlation
allowlist, or arbitrary correlation map.

## TDD and acceptance

The remediation is accepted only when all of the following evidence is green.

### Compile-fail API invariants

External `compile_fail` doctests prove:

- a `BoundedTransportResponse` struct literal is inaccessible;
- response field assignment is inaccessible;
- `AuthorizedCorrelations` struct construction is inaccessible;
- correlation insertion and mutable map access are unavailable;
- a normal runtime `String` cannot satisfy either `literal` constructor;
- neither static type has a runtime conversion trait.

### ABI behavior

ABI tests prove:

- ordinary `BoundedTransportResponse::try_new` returns empty correlation
  authority;
- status `0` and `u16::MAX` both succeed;
- every response and failure limit accepts its exact boundary and rejects one
  over;
- 64 and 65 correlation entries, 8,192 and 8,193 value bytes, and 32,768 and
  32,769 aggregate bytes are independently exercised;
- an allowed-but-absent correlation is omitted;
- a selected-but-unallowed correlation is never authorized;
- duplicate and oversized allowlists fail;
- code/message literals work in `const` and `static` positions;
- `ConnectorFailure::code()` and `safe_message()` return the two values stored
  in the private boxed bundle;
- `ConnectorFailure` remains below Clippy's large-error threshold without a
  lint override or suppression;
- empty, oversized, invalid-code, and control-bearing text fails;
- retry delay clamps at 86,400 seconds;
- 16 and 17 inline values split across slots are exercised;
- 131,072 and 131,073 decoded inline bytes split across slots are exercised;
- aggregate node accounting spans multiple roots;
- depth remains per root;
- aggregate canonical bytes remain checked.

Crate-local tests call the private aggregate traversal directly for exact
counter boundaries dominated by the canonical-byte ceiling.

### Boundary policy

The processor-boundary checker has positive fixtures for every allowed
production/test path and negative fixtures for every forbidden namespace,
alias, re-export, wrapper, trait implementation, and leak call. The checker
runs in CI and emits stable path-specific diagnostics. Its fixtures include
post-Python-Unicode aliases for both namespaces, static types, and host traits;
raw-keyword decoys; raw alias destinations; and nested/grouped `self` and
descendant aliases with preceding siblings.

### Verification gates

```bash
python3 scripts/check_connector_processor_boundary.py
cargo test -p donat-connector-abi --no-default-features
cargo test -p donat-value-contract --no-default-features
cargo check -p donat-connector-abi --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-abi --target all \
  --edges normal,build --no-default-features --offline --locked
cargo clippy -p donat-connector-abi --all-targets --no-default-features \
  -- -D warnings -D clippy::result_large_err
if rg -n 'result[_-]large[_-]err' crates/connector-abi; then
  exit 1
fi
cargo fmt --all -- --check
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors
cargo test -p donat-conformance
```

The invoking environment supplies `PG_URL` and `DONAT_BIN` to both
conformance commands. The verification commands do not assign or hardcode
either value. The focused `connectors` target runs against the freshly rebuilt
`donat` binary before the full conformance crate.

The dependency tree contains only `donat-connector-abi` and
`donat-value-contract`. The no-OS build, object-safety assertions,
exact-call-signature assertions, server build, focused connector coverage,
and full conformance remain mandatory. The negative source scan proves that
the green strict-Clippy result comes from the bounded private layout rather
than a source attribute, lint expectation, lint override, or suppression.

## Migration from `db289dc`

The migration is source-breaking and contains no compatibility shim:

- make all `BoundedTransportResponse` fields private;
- replace direct response field reads with `status()`, `selected_headers()`,
  `decoded()`, and `response_bytes()`;
- add the private empty `authorized_correlations` field;
- add `AuthorizedCorrelations` and both restricted construction namespaces;
- replace `ConnectorFailure::try_new(class, &str, &str, retry, map,
  allowlist)` with
  `ConnectorFailure::try_new(class, StaticErrorCode, StaticSafeMessage,
  retry, Option<&AuthorizedCorrelations>)`;
- add private `StaticFailureText { code, safe_message }` and store it as
  `Box<StaticFailureText>` inside `ConnectorFailure`;
- retain the exact public `ConnectorFailure::try_new`, `code()`, and
  `safe_message()` signatures while changing only their private storage and
  dereference path;
- replace raw test strings with static literals or catalog-construction test
  calls;
- replace direct failure/response field assertions with accessors;
- replace per-slot shape validation with the shared aggregate traversal;
- retain `ConnectorIo::call`, `ProcessorControl::check`,
  `ProcessorContext`, all typed IDs, `ConnectorErrorClass`, and
  `BoxFuture` unchanged;
- retain `status: u16` without semantic validation;
- retain all current constants unless this design names a new error
  diagnostic;
- remove no persisted value, wire field, metadata field, or runtime endpoint
  because the baseline ABI has no deployed persistence or public execution
  surface.

No deprecated raw constructor remains after migration. Future Task 3, Task 6,
and Task 8 code targets only the remediated API.

## Explicit exclusions

This design does not add or change:

- an HTTP status range narrower than `u16`;
- retry or `on_error` process policy;
- process activity persistence, leases, takeover, capacity reservations,
  idempotency windows, polling checkpoints, or webhook acknowledgement;
- database access, source selection, roles, permissions, an admin role, or a
  permission bypass;
- URL, method, path, query, header-name, proxy, TLS, DNS, peer-validation,
  credential, crypto, clock, cancellation, filesystem, environment, thread,
  process, or raw HTTP request types in the ABI;
- provider bodies, provider messages, credentials, tokens, or arbitrary
  headers in failures;
- a public GraphQL, REST, MCP, admin, metadata-mutation, or connector execution
  endpoint;
- a dynamic plugin, JavaScript, Node.js, WASM, shared library, downloaded
  module, donor runtime, build script, native dependency, third-party runtime
  dependency, `std` feature, unsafe code, FFI, or assembly;
- a second value representation or any change to
  `donat-value-contract`;
- recursion, nested connectors, streams, unbounded collections, or fresh
  per-page budgets;
- n8n `If`, `Switch`, `Merge`, `Code`, `Wait`, loops, workflow items,
  paired items, subworkflows, AI nodes, send-and-wait, or any other
  logical/workflow node;
- business decisions, branching, database work, waits, retries, or
  orchestration owned by Rules, Commands, and Processes.

## Rejected alternatives

### Split normalized and static error representations

The rejected Option 2 stores generated messages as `&'static str` and adds
separate owned catalog source types. It reduces static message size but
requires dual `ErrorAction` representations and pulls Task 5 code-generation
mapping into the Task 2 remediation. The fixed-capacity representation keeps
one checked type for Task 3 loading and generated statics, preserves
`no_std + alloc`, and is smaller in implementation scope.

### Processor failure-action IDs with server materialization

The rejected Option 3 changes `OperationProcessor` to return failure-action
requests, adds action IDs to every catalog error rule and fallback, and makes
Task 8 resolve and materialize those requests. It creates a stronger
processor/server split but changes the Task 3/6/8 contracts and adds
substantial machinery that the current defects do not require. Opaque
host-authorized correlations plus static text remove the reported
self-authorization and provider-text paths without changing the host trait or
processor result signatures.
