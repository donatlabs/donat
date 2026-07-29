# Spec 007 — Community connector factory

Status: proposed. This specification extends the compiled connector boundary
from Spec 006 into a repeatable, license-gated factory for provider
integrations. It does not add a workflow engine or a runtime plugin system.

## 1. Summary and confirmed product boundary

Donat will acquire ordinary provider integrations from two source-level donor
classes:

1. exact-version, permissively licensed n8n community packages admitted one
   version at a time; and
2. permissively licensed provider OpenAPI or Discovery artifacts admitted one
   immutable revision at a time.

A development-only importer inventories those sources and emits reviewable
Donat connector artifacts. An offline build compiler validates those artifacts
and embeds normalized connector IR plus statically registered Rust processors
in the existing Donat binary. Production executes only the embedded catalog.

The deployed product remains exactly one Rust binary plus Postgres:

- no connector UI, visual editor, dynamic options UI, or copied provider field
  descriptions;
- no Node.js, JavaScript execution, WASM, shared library, package manager,
  downloaded package, sidecar, or source-code loading in production;
- no n8n workflow or logical nodes, including `If`, `Switch`, `Merge`, `Code`,
  `Wait`, loops, the n8n item/paired-item model, subworkflows, send-and-wait,
  or AI orchestration;
- no arbitrary HTTP Request equivalent;
- no business workflow behavior in connector processors.

The only imported domain is provider integration: credential specifications
and instances, typed API operations, pagination, provider error mapping,
provider webhooks, polling triggers, bounded binary/multipart transport, and
narrow static Rust protocol processors. Donat Rules, Commands, and Processes
continue to own business decisions, database work, retries, waits, branching,
iteration, and orchestration.

Commands never perform connector I/O. A durable Process commits intent before
a worker calls a connector. Every connector instance used by a Process is
bound to the same Postgres source as that Process, and all activity capacity,
refresh serialization, polling checkpoints, and ingress journals are
source-local. There is no admin role, runtime metadata mutation API,
permission bypass, or connector-call GraphQL endpoint.

## 2. Background and governing evidence

Spec 006 established the first two implementation styles:

- `crates/server/src/connectors/http.rs` is a closed declarative HTTP executor;
- `crates/server/src/connectors/stripe.rs` is an independently implemented,
  provider-specific Rust module;
- `crates/server/src/connectors/mod.rs` builds an immutable startup registry;
- `crates/metadata/src/types.rs` accepts deploy-time connector instances,
  environment-backed `SecretRef` values, and operation capacity policy.

The current registry is intentionally small and hand-written. At
`crates/server/src/connectors/mod.rs:137-161`, an enum contains only HTTP and
Stripe variants. `ConnectorRegistry::execute` at
`crates/server/src/connectors/mod.rs:445-502` dispatches only a named instance
and enabled operation; it accepts no request URL, method, header name, or HTTP
client. `ExecutionContext` at `crates/server/src/connectors/mod.rs:99-109`
currently exposes only a deadline.

The existing transport invariants are retained:

```rust
// Existing code, crates/server/src/connectors/mod.rs:112-124.
pub struct ConnectorDefinition {
    pub module_name: &'static str,
    pub semantic_version: &'static str,
    pub runtime_abi: u32,
}

pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
}
```

`crates/server/src/connectors/http.rs:996-1060` validates the closed job input,
canonicalizes its fingerprint, checks the activity deadline, resolves the
fixed destination twice, enforces the 1 MiB request/response limit, rejects an
unvetted peer, and maps the response. Paths are static templates whose values
are percent-encoded (`crates/server/src/connectors/http.rs:1264-1337`).
Reqwest redirects, proxies, and transport retries are disabled
(`crates/server/src/connectors/http.rs:830-872`). These are factory
requirements, not implementation details that a generated connector may
override.

Stripe supplies the processor-backed reference. The actual operation ID is
`checkout.create_session`
(`crates/server/src/connectors/stripe.rs:32-34`); its input is closed and typed
(`crates/server/src/connectors/stripe.rs:41-150`), its provider origin is
fixed, and its webhook verifies the untouched raw bytes before parsing JSON
(`crates/server/src/connectors/stripe.rs:439-532`).

No existing Hasura/Donat-derived fixture governs this new community-connector
surface. The current Donat-owned cases in
`crates/conformance/tests/connectors.rs` and
`crates/conformance/fixtures/connectors/` cover startup rejection, secret
redaction, Stripe signature rejection, and the temporary verified-event
`503` boundary only. They are useful regression baselines but do not specify
source admission, normalized IR, credentials, generic pagination, processors,
polling, or binary values. New native conformance fixtures created for this
specification are the ground truth for those behaviors. They must pin exact
status, body, error class/code, redaction, request shape, and durable outcome;
there is no legacy pytest result to consult.

This specification follows:

- `knowledgebase/declarative-saas/decisions/001-declarative-saas-runtime-and-porting-policy.md`;
- `knowledgebase/declarative-saas/decisions/002-durable-process-operational-contracts.md`;
- `knowledgebase/declarative-saas/decisions/009-durable-process-source-local-compilation-and-journal-contracts.md`;
- `knowledgebase/declarative-saas/reference-porting-register.md`;
- `specs/006-in-binary-connectors.md`.

## 3. Sourcing and licensing policy

### 3.1 Hybrid source model

A source is eligible for source-level import only when its exact artifact is
MIT, Apache-2.0, or BSD-compatible and passes the admission pipeline in this
specification. A repository or npm metadata label is not sufficient.
Dependencies, generated files, embedded schemas, icons, fixtures, and test
payloads have their own origin and license decisions.

Permissive provider OpenAPI/Discovery is a co-equal input. A publicly readable
schema is not necessarily reusable. The exact artifact requires an immutable
revision, checksum, license evidence, notice, and per-file record before it can
generate checked-in Donat IR or Rust.

`n8n-nodes-base` version `2.33.0` at n8n revision
`b329d57767cb6ec046bb1ecf9293b450c831d720` is
`LicenseRef-n8n-sustainable-use`. Its source, tests, fixtures, credential
classes, descriptions, and generated metadata are source-unavailable for
Donat (SUL) and behavior-only unless a signed agreement expressly grants
source copying, Rust translation, derivative-work creation, fixture use,
generated-metadata extraction, commercial hosting, and redistribution.
Enterprise or OEM branding alone must not be interpreted as that grant.

For SUL or otherwise non-permissive references, implementers may record a
path-level behavioral observation and then write independent Donat tests and
code from provider-owned public behavior. They must not preserve expressive
descriptions, field text, fixture bodies, code organization, or generated
catalogs.

### 3.2 Exact research cohort

The n8n verified-node API is discovery evidence only. Its 2026-07-29 snapshot
contained 1,337 node-type rows and 1,066 distinct package names; the
concatenated raw responses had SHA-256
`64a562c831328819b8fb9592f3b2989428f55a6060a1eddc72baa808a29eb9d7`.
Those counts are not connector or operation coverage claims.

The first donor cohort is pinned below. Every integrity is the npm-published
SHA-512 that was independently recomputed over the exact tarball in the
research report.

| Package | Exact repository identity | Exact npm integrity | Admission notes |
| --- | --- | --- | --- |
| `n8n-nodes-serpapi@0.1.10` | `serpapi/n8n-nodes-serpapi` at `e48b778878c043f30277b932c4c129804efee66d` (npm `gitHead`) | `sha512-E9tAU4c9mhNWr07s6RGeqzyrlQO8y42YvtMjPWuLf+tIEM8muU/RIgtp+ojhaoNVCP+jfrwmsSC75OIuoMVS9A==` | MIT; verified; no npm provenance statement; first Tier A compiler proof |
| `@brave/n8n-nodes-brave-search@1.1.2` | provenance source `brave/n8n-nodes-brave-search` at `5361456ed0103c46468d6fba107f735c6a15f4d3`; version-only tag commit `a8efa1adba006a8225b383882af97d370538c354` | `sha512-vmXh668+AmYXClW5D0Wf4HReSgZbhVHUG+NDqFpFNJVGLAZrPmSmoJHUk+GybzRbjkNCiXcTWIw29LONKt2vXA==` | MIT; verified and provenance-backed; retain both commits |
| `n8n-nodes-resend@2.6.4` | `resend/n8n-nodes-resend` at provenance/tag `b34f90f491e2f0c26b6105720437857028707397` | `sha512-84j71taNrjuUNGdk/vaOdFHe0JF06Qk+hJphh5phBwl6cHiZcmVeqNAIpyBtpN8C7TQIJIOJL4scHai1snqS1Q==` | MIT; verified and provenance-backed; send-and-wait excluded |
| `n8n-nodes-cloudinary@0.2.1` | `cloudinary/n8n-nodes-cloudinary` at provenance/npm `gitHead` `3aa104e40e2dd6a9d58ab65c8a1b14656935bef6` | `sha512-47KeOgPnVTn37EEceX+rRWSaqLO5XiMaL1sc19QnfWGIFU4JidoCTLFsSpLicXHD3XvpyEoSl+I/ih+jmw0whw==` | MIT; verified and provenance-backed; signing/cursor proof, multipart later |
| `@mercadopago/n8n-nodes-mercadopago@0.3.6` | `mercadopago/n8n-nodes-mercadopago` at provenance/tag/npm `gitHead` `739228119eeab96f955341e205a30cc9bcd6023b` | `sha512-NKMVqxBO+Rgo0Wsy5+1wTfN3GyaNVlSN70m7OCpeHD6rdV/EioisrYY6+VBtYYXgV8yHGW872o5m44+JAx08nA==` | MIT; verified and provenance-backed |
| `@apify/n8n-nodes-apify@0.6.10` | `apify/n8n-nodes-apify` at provenance/tag/npm `gitHead` `8edec8b87b811c0e85286b6f56e0124c021e26b5` | `sha512-IkZS1zKzIyt43ygBNeR/Ws5BIOQD6ap3KwWRdV/3IOXIaYidD7msuThT15/OiYs8xe/gXkF4JVnfpoUIzm9frg==` | MIT; verified and provenance-backed; source `package.json` said `0.6.9`, so source/tarball mismatch remains explicit |
| `@cloudconvert/n8n-nodes-cloudconvert@1.1.1` | `cloudconvert/n8n-nodes-cloudconvert` at npm `gitHead` `e81a9e8069050f1611e6192e04d6c93ca6a1edf7`; version-only tag commit `7a7297594ce513517a4d28ea7a3b0fbf713cef97` | `sha512-dHI4evF8wK2na7mibhKbZps8v5akmYcOxs+uJIphDKSl3maXIOxRlQTKL/tOddNZtHUy5a3c7hX7NaCtmemRXw==` | MIT; verified; no npm provenance statement; later binary/multi-step proof |

Other exact permissive inputs already identified are:

- `n8n-io/n8n-nodes-starter` at
  `3308a8eca314e388c40b29c9b6cefc49a8cf9115`, MIT. Only independently
  admitted starter boilerplate may be translated; it does not relicense n8n
  runtime packages.
- `stripe/openapi` at
  `6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69`, MIT. The current reference
  record gives `openapi/spec3.json` SHA-256
  `e24a26de4188fd64dec4c043d5d3726277fdcb07556a493ea481c305b0a223d8`.

The HubSpot public API spec collection at
`b43170f3a2e51ee9dd3933a261d2219485733f11` is proprietary according to its
immutable README and is behavior-only. Google Discovery documents and any
future provider schema require their own terms review before generation.

These rows are candidates, not completed port records. Missing license-file
hashes, per-source-file SHA-256 values, embedded-material decisions,
destinations, RED tests, notices, and reviewers must be supplied by admission
before any derived artifact lands.

### 3.3 Derivative work and fixture rule

Source-level translation of an MIT community donor into normalized YAML, Rust,
or generated static IR is a derivative port. It requires:

- a `ConnectorSourceRecord`;
- one `knowledgebase/declarative-saas/reference-porting-register.md` entry per
  imported or translated upstream file;
- the donor's required copyright/license text in root
  `THIRD_PARTY_NOTICES.md`;
- a provenance header or generated-record link on every derived destination;
- a Donat-owned failing test named in the record before the implementation;
- human review of the generated or translated diff.

Repository-level MIT licensing does not automatically clear fixtures. Each
fixture must be independently reviewed for third-party provider payloads,
personal data, secrets, trademarks, copied schemas, and generated proprietary
material. A fixture that fails that review is replaced with independently
authored Donat data, even when the donor source itself is eligible.

## 4. Source admission and compiler pipeline

### 4.1 `ConnectorSourceRecord`

Every exact donor version has one canonical, signed record. The serialized
schema is versioned and rejects unknown fields. Conceptually:

```rust
struct ConnectorSourceRecord {
    record_version: u32,
    source_id: String,
    source_kind: SourceKind, // NpmPackage | ProviderSchema | DonatOwned
    package: Option<ExactNpmPackage>,
    repository: ImmutableRepository,
    artifact_hashes: Vec<ArtifactHash>,
    license: LicenseDecision,
    entrypoints: Vec<SourcePath>,
    dependencies: Vec<DependencyDecision>,
    embedded_material: Vec<EmbeddedMaterialDecision>,
    provider_contract: ProviderContractEvidence,
    safety_findings: SafetyFindings,
    approved_operations: Vec<OperationId>,
    compatibility_tier: CompatibilityTier,
    reviewer: ReviewIdentity,
    approval_date: Date,
    approval_signature: Signature,
    port_commit: GitCommit,
}
```

The npm form records exact package name/version, tarball URL, SHA-512
integrity, npm signature and provenance when present, repository URL, full
commit, tag commit when different, source-tree hash, the provenance/`gitHead`
to-tarball mapping, package entrypoints, all dependency and peer-dependency
licenses, license file hash, maintainers, and repository-owner consistency.

The provider-schema form records the repository or immutable download URL,
full revision, exact schema paths, byte hashes, declared license, mandatory
notice, provider API version, and terms evidence. Mutable HTML documentation
can be a dated behavior reference but cannot be compiler input.

The safety record rejects or quarantines dynamic code, `eval`, process or shell
execution, filesystem/environment access, arbitrary destinations, proxy or TLS
controls, long-lived sockets, local services, SDK dependencies, unbounded
loops, and unbounded binary behavior. A mismatch in package version, tarball
integrity, provenance, git tree, entrypoint, license, dependency, or embedded
material fails closed.

### 4.2 Admission stages

The development importer performs these stages in order:

1. discover an exact `(package, version)` or provider artifact;
2. download it into a disposable quarantine directory;
3. verify hashes, signature/provenance, repository mapping, license, notices,
   dependencies, entrypoints, and embedded material;
4. inventory only provider integration nodes and operations;
5. classify each operation as Tier A, B, C, or rejected;
6. emit a review bundle containing the source record, per-path hashes,
   normalized candidate manifest, unsupported findings, and notice diff;
7. require an explicit reviewer approval before copying or generating any
   tracked artifact.

The importer treats TypeScript as source data. It does not execute package
scripts, node classes, JavaScript, npm hooks, or donor tests. It may parse an
AST to locate static object literals and imports. It may mechanically emit
Tier A fields only when each value maps to the closed Donat grammar.

AST conversion is not automatic translation of imperative code. Functions,
expressions, hooks, `execute`, `poll`, `trigger`, `webhook`, SDK calls,
workflow-item access, or ambiguous imports produce an explicit unsupported
finding or a reviewed processor work item. They never become generated Rust
behavior. Generated n8n metadata is neither executable truth nor a license
workaround because function bodies are absent.

### 4.3 Development/build time versus production

The stages are deliberately separated:

| Stage | Inputs | May access donor source/network? | Output | Shipped in `donat`? |
| --- | --- | --- | --- | --- |
| Development importer | quarantined exact tarball/repository/schema | yes, only for explicit acquisition | review bundle and proposed source record | no |
| Admission review | record, hashes, license/notices, diff, RED test | no network required | approved checked-in record/manifests/notices | no |
| Offline build compiler | approved checked-in records and Donat manifests | no | validated Rust constants/static catalog in `OUT_DIR` | generated catalog only |
| Production runtime | embedded catalog, deploy-time metadata, Postgres, resolved credential capabilities | never | bounded provider result or verified ingress event | yes |

`build.rs` must not fetch npm, GitHub, provider documents, or a mutable
registry. Reproducible builds operate offline. Production metadata cannot name
a package URL, tarball, repository, source path, code blob, processor path, or
unregistered connector.

## 5. Normalized connector IR

The compiler emits immutable, serde-independent runtime descriptors with
canonical encoding and hashes. Source descriptions and UI hints are discarded.
Stable field IDs, types, constraints, and protocol mappings remain.

### 5.1 `CredentialSpec`

A `CredentialSpec` contains:

- stable connector-qualified ID and version;
- typed field IDs, optionality, secret classification, maximum size, and
  redaction behavior;
- one common auth plan: fixed-header API key, fixed-query API key, bearer,
  HTTP Basic, OAuth2 client credentials, or pre-provisioned OAuth2
  authorization-code tokens;
- fixed allowed origins to which the capability may apply;
- OAuth scopes, fixed authorization/token origins, token response mapping,
  expiry/skew rules, and refresh support;
- an optional statically registered auth processor ID for a narrow provider
  signature scheme;
- an optional credential-test operation ID.

It contains no display widget, HTML, arbitrary expression, raw secret, runtime
URL, or JavaScript hook.

### 5.2 `OperationSpec`

An `OperationSpec` contains:

- stable connector/operation ID, semantic version, runtime ABI, and canonical
  input/output type hashes;
- a closed typed input and output using the shared SQL-free value-contract
  language;
- one or more named compiled steps, each with a fixed method, origin ID, path,
  query keys, header names, credential action, and encoding;
- explicit optional/default behavior for every binding;
- JSON, form-urlencoded, multipart, or raw-byte request shape;
- declared success statuses and response selection/normalization;
- one `ErrorMap`;
- idempotency header/body binding from the logical activity identity;
- one `PaginationPlan`;
- named pure pre-request/post-response transforms;
- an optional static `OperationProcessor` ID;
- capacity, rate, and typed serialization-key defaults;
- exact request, response, inline-binary, page, item, call, redirect, and
  deadline bounds.

No operation field can carry an arbitrary expression, dynamic method,
caller-owned header key, raw URL, proxy/TLS option, ambient workflow item,
database handle, role, filesystem path, process state, retry policy, or
business branch.

### 5.3 `PaginationPlan`

`PaginationPlan` is a closed enum:

- `None`;
- `Cursor`, extracting from a fixed JSON pointer or response header and
  binding to a fixed query/header slot;
- `OffsetLimit`;
- `PageNumber`;
- `LinkRelation`, accepting only a relative or same-compiled-origin `next`
  relation;
- `Processor`, naming a static driver for an exceptional provider.

Every non-`None` plan declares maximum pages, items, bytes, and calls. A
provider-returned absolute continuation is rejected in Phase 1 unless it
matches the already compiled origin exactly. A future continuation capability
may allow an explicit compiled origin set, but it remains HTTPS-only,
DNS/peer-validated, redirect-bounded, expiry-bounded, and size-bounded. It can
never be a caller-supplied arbitrary URL.

### 5.4 `ErrorMap`

`ErrorMap` orders rules over status ranges, a fixed provider-code pointer,
fixed header fields, and response shape. It maps every failure to the existing
closed `ConnectorErrorClass`:

`transport`, `timeout`, `http_429`, `http_5xx`, `authentication`,
`validation`, `permanent`, or `invariant`.

The compiler expands a complete deterministic fallback:

- transport/TLS/DNS failures → `transport`;
- activity deadline or HTTP 408 → `timeout`;
- HTTP 429 → `http_429`;
- declared 5xx → `http_5xx`;
- HTTP 401/403 → `authentication`;
- declared provider validation errors and malformed declared success bodies →
  `validation`;
- other supported non-success responses → `permanent`;
- a violated compiled contract → `invariant`.

Rules may extract `Retry-After` as integer seconds or an HTTP date and may
retain allowlisted provider correlation IDs. Safe messages are Donat-owned
constants. Raw provider bodies, authorization material, tokens, credential
fields, and unreviewed provider messages never cross the failure boundary.
Connectors classify outcomes; Process metadata alone owns retry and
`on_error` policy.

### 5.5 `TriggerSpec`

`TriggerSpec` has two provider-integration forms:

- `Webhook`: fixed verifier ID, selected headers, raw-body maximum, timestamp
  window, event ID/type/output contract, redaction, and optional explicit
  create/delete/check subscription operation IDs;
- `Poll`: versioned checkpoint type, static poll processor ID, event type,
  per-poll event limit, and operation bounds.

A webhook verifier receives raw bytes before parsing and returns only a
verified provider event ID, event type, and typed payload. Signature failure
is audit-only. Durable dedupe, delivery audit, correlation, process start, and
acknowledgement remain in the source-local process ingress transaction defined
by ADR 002/009.

A poll processor receives a typed checkpoint, database-derived `now`, deadline,
and event limit and returns ordered typed events plus the next checkpoint.
Postgres owns scheduling, locking, checkpoint persistence, retries, and the
transaction. A connector cannot maintain workflow static data.

Long-lived listener/socket `trigger()` implementations are excluded.

## 6. Credentials and capabilities

### 6.1 Specification versus instance

The compiled `CredentialSpec` defines what a connector can request. A
deploy-time `CredentialInstance` binds that spec to:

- a source name and non-secret `credential_identity`;
- field-by-field `SecretRef` values or one opaque external secret bundle;
- an immutable credential-spec version;
- optional token metadata that is safe to fingerprint;
- no plaintext secret in metadata, a process revision, logs, errors, or
  conformance output.

Phase 1 accepts deploy-time/external `SecretRef` values. Static API keys,
Basic secrets, bearer tokens, OAuth client credentials, and pre-provisioned
OAuth access/refresh tokens are supported. There is no interactive tenant
authorization, consent callback, credential CRUD route, or built-in vault API.
Those are a separately specified later product concern and must preserve
explicit roles without introducing an admin role or data bypass.

### 6.2 Resolver and execution capability

The server reserves an internal `CredentialResolver` boundary. Resolver
implementations are compiled into the binary; metadata cannot load one. An
environment resolver is read-only. A refreshable OAuth credential requires an
external resolver that supports versioned compare-and-swap writeback; a
read-only reference is rejected for refresh-enabled use.

Resolution returns an opaque `CredentialCapability`, not a secret map. The
generic executor can apply only the auth action compiled for the selected
step. A named auth/verifier processor can request only its compiled primitive,
such as HMAC over bounded bytes. Operation processors cannot enumerate
credentials, read unrelated fields, expose raw values, select a new origin, or
log capability output.

OAuth refresh uses this sequence:

1. acquire a durable Postgres refresh lease keyed by source,
   `credential_identity`, credential-spec version, and resolver version;
2. re-resolve the current token bundle after acquiring the lease;
3. call the fixed compiled token step only when expiry/skew requires it;
4. compare-and-swap the refreshed bundle through the resolver;
5. release the lease and give the operation an opaque access-token capability.

Multiple Donat binaries therefore perform one serialized refresh. No database
transaction remains open during the token HTTP request: the lease is a
committed durable reservation with fencing, consistent with activity I/O.
Refresh failures are typed credential/connector failures and redact both the
old and returned token bundle.

## 7. Static processor and I/O ABI

The hand-written HTTP/Stripe registry becomes a generated static catalog plus
a small Donat-owned processor table. A processor orchestrates compiled step
IDs; it does not construct a reqwest request.

An object-safe interface equivalent to the following is sufficient:

```rust
pub trait OperationProcessor: Send + Sync {
    fn execute<'a>(
        &'a self,
        context: ProcessorContext<'a>,
        input: TypedValue,
        io: &'a dyn ConnectorIo,
    ) -> BoxFuture<'a, Result<TypedValue, ConnectorFailure>>;
}

pub trait ConnectorIo: Send + Sync {
    fn call<'a>(
        &'a self,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>>;
}
```

`ProcessorContext` exposes only connector/operation identity, deadline,
cancellation, stable logical activity/idempotency identity, canonical request
fingerprint, and the opaque credential capability. `ConnectorIo::call`
resolves a step within the current compiled operation. A foreign or undeclared
step is an `invariant` failure before network I/O.

A processor never receives a raw HTTP client, URL, method, header map, proxy,
TLS control, database pool, role, process instance/graph, Rule evaluator,
retry policy, environment, filesystem, workflow item, persistent static data,
thread/process spawning, or unbounded stream. It cannot recurse or call
another connector. Pure named transforms use a narrower synchronous interface.

The runtime checks cancellation and deadline before and after every transform,
credential action, page, and I/O step. It charges each call, page, item, and
byte against the compiled operation budget. Processor output is revalidated
against the compiled output type before it can reach a process journal.

The processor table maps stable `(processor_id, version)` pairs to Rust
implementations compiled into the binary. Metadata and generated manifests can
refer only to a present pair. There is no dynamic fallback.

## 8. Fixed-origin egress and runtime bounds

Provider connectors compile all outbound origin IDs. Deployment can select an
endpoint identity but cannot replace a provider origin. The existing generic
`http` module may retain a deploy-time fixed base URL for independently
authored private integrations, but neither process input nor an imported
provider operation may select its scheme, host, port, method, header names,
proxy, certificate policy, or redirect behavior.

Phase 1 provider origins are HTTPS and `public_only`. Every request:

- validates the initial origin and path;
- resolves under the activity deadline;
- denies non-global/special-use addresses according to the existing HTTP
  policy and IANA IPv6 decision;
- resolves again immediately before connection;
- pins the vetted addresses in the HTTP client;
- disables environment/system proxies and automatic retries;
- validates the connected peer;
- does not follow redirects.

A later compiled redirect/continuation step must repeat the same checks and
remain within an explicit origin allowlist. `private_allowed` is not available
to imported community/provider connectors.

Every operation declares stricter limits within these Phase 1 engine ceilings:

| Resource | Phase 1 engine ceiling |
| --- | ---: |
| Compiled outbound calls, including pages | 16 per logical attempt |
| Pages | 16 |
| Normalized output items | 10,000 |
| Request body | 1 MiB |
| Response body per call | 1 MiB |
| Aggregate decoded inline binary | 1 MiB |
| Raw webhook body | 1 MiB |
| Request/response headers retained for mapping | 64 |
| Redirects | 0 |

Build validation rejects a contradictory or larger declaration. The process
activity's start-to-close deadline is the authoritative time limit and may be
stricter. Increasing an engine ceiling requires a reviewed runtime-ABI change,
fresh adversarial tests, and a process dependency revision.

No connector stores unbounded response bodies or provider diagnostics. Item
normalization stops before exceeding the item/byte budget and returns a typed
failure; it never silently truncates a success.

## 9. Binary and multipart contract

Phase 1 supports only bounded inline bytes for small payloads. The typed value
is conceptually `{ bytes, media_type, file_name? }`; its decoded aggregate is
at most 1 MiB, and the enclosing request/response ceilings still apply.
Multipart field names, content disposition, and media-type policy are
compiled. Runtime input supplies only typed scalar or inline-byte values.

The runtime never accepts a filesystem path, file descriptor, stream handle,
bucket/key pair disguised as text, `file://` URI, or arbitrary HTTP(S) URL.
Temporary buffering, if required by reqwest, is memory-only and charged before
the request is sent.

A larger object-reference capability is later work. It must be an opaque,
resolver-qualified capability with tenant/source ownership, expiry, content
length/hash, and operation allowlist. It cannot be a URL and cannot weaken the
fixed-origin egress policy.

## 10. Static catalog, deployment metadata, and revision pinning

The build emits an immutable table containing:

- connector ID/version/runtime ABI and provider ID;
- `CredentialSpec`, `OperationSpec`, `PaginationPlan`, `ErrorMap`, and
  `TriggerSpec` entries;
- processor/auth/verifier IDs and versions;
- fixed origins and network policy;
- input/output and configuration hashes;
- source-record IDs, source file hashes, license class, and notice IDs;
- compiler/classifier versions.

`ConnectorRegistry::build` continues to run before the listener opens. It
validates deploy-time instances against this table, resolves only declared
credentials, and materializes an immutable registry. The generated table
replaces the `RegistryInstance::Http`/`Stripe` dispatch match without adding
runtime discovery. Existing `http` and `stripe` module IDs remain supported
through catalog entries during migration.

`ConnectorInstance` in `crates/metadata/src/types.rs` evolves rather than
creating a runtime API. A factory-backed instance selects:

- a source name;
- compiled connector/module ID;
- non-secret endpoint and credential identities;
- one compatible deploy-time credential instance;
- an enabled subset of compiled operations/triggers;
- worker-owned capacity/rate/serialization overrides within compiled bounds.

The connector catalog publishes public, secret-free typed operation/event
descriptors for the two-stage process compiler in ADR 009. A process revision
pins source name, connector instance, connector and operation versions,
runtime ABI, processor/version, credential-spec/identity, endpoint identity,
non-secret configuration fingerprint, origin policy, and input/output hashes.
Live-retired catalog entries remain executable until all pinned non-terminal
instances finish. A binary that lacks any pinned dependency cannot claim the
activity.

Credential rotation does not serialize a secret into the revision. A change
to credential class/spec, endpoint identity, origin, scope set, API version,
processor, operation schema, pagination/error plan, or runtime bound creates a
new dependency fingerprint.

## 11. Donat-owned examples

These examples are independently authored for this specification. They
illustrate the proposed Phase 1 shape and do not copy donor descriptions,
icons, display metadata, or executable source.

### 11.1 SerpAPI declarative connector instance

An admitted, compiled catalog entry fixes `https://serpapi.com`, the API-key
query slot, `GET /search.json`, the allowed inputs, and the normalized output.
Deployment metadata selects it and supplies only a secret reference and worker
policy:

```yaml
- name: public_web_search
  source: app
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
      capacity:
        max_in_flight: 8
        rate_limit: { permits: 20, per: 1s, burst: 4 }
```

The corresponding Donat-owned normalized operation is conceptually:

```yaml
id: search.google
version: 1
input:
  query: { type: string, min_length: 1, max_length: 512 }
steps:
  - id: search
    method: GET
    origin: serpapi_public
    path: /search.json
    query:
      engine: { literal: google }
      q: { input: query }
      api_key: { credential: api_key }
    success_statuses: [200]
output:
  results: { from_json_pointer: /organic_results, type: json_array }
bounds:
  calls: 1
  response_bytes: 1048576
  items: 100
```

Neither the caller nor a processor can add a query key, change `engine`,
select an origin, or request a raw response. The first compiler test compares
this canonical IR to a reviewed `insta` snapshot and separately proves the
prepared HTTP request through a local Donat-owned server.

### 11.2 Stripe processor-backed operation

The existing `checkout.create_session` behavior moves behind the static
processor table without changing its public connector contract:

```yaml
id: checkout.create_session
version: 1
processor: stripe.checkout.create_session.v1
steps:
  - id: create_session
    method: POST
    origin: stripe_api
    path: /v1/checkout/sessions
    encoding: form_urlencoded
    auth: stripe.secret_key.v1
    idempotency_header: Idempotency-Key
    success_statuses: [200]
bounds: { calls: 1, request_bytes: 1048576, response_bytes: 1048576 }
```

The Rust processor validates the existing typed input, creates only bindings
for `CompiledStepId("create_session")`, calls `ConnectorIo`, and normalizes the
existing `id`, `url`, `status`, and `expires_at` output. It cannot construct
`https://api.stripe.com` or an Authorization header itself. The existing raw
Stripe webhook verifier becomes a static `TriggerSpec::Webhook` registration.
This is a registry/ABI migration of independent Donat Rust, not a claim that
n8n's SUL Stripe implementation was ported.

## 12. First implementation cohort

Implement slices in this order, keeping only named provider operations:

| Order | Connector slice | Target style | Explicit exclusions/deferments |
| ---: | --- | --- | --- |
| 0 | Existing Stripe Checkout creation and completion webhook | static processor plus webhook verifier | no broad Stripe/n8n port |
| 1 | SerpAPI Google search, then selected search engines | Tier A generic HTTP; pagination only when proven | no UI metadata or arbitrary engine/request fields |
| 2 | Brave web/news/image search | generic HTTP plus one named query transform | no ambient expressions |
| 3 | Resend email send, contact list, verified inbound events | generic/error/pagination IR plus Rust verifier | send-and-wait excluded; attachments deferred to bounded binary slice |
| 4 | Cloudinary asset metadata, admin search, and signed delivery transforms | named signing/URL transform and cursor plan | upload-by-URL excluded; multipart upload waits for Phase 1 inline-byte ABI |
| 5 | Mercado Pago payment link and report list | generic HTTP plus pagination | report download/SFTP deferred |
| 6 | Apify run actor, inspect run, dataset items, webhook lifecycle | bounded processor, pagination, explicit webhook operations | no workflow wait node or unbounded poll loop |
| 7 | CloudConvert job creation/result retrieval | bounded multi-step processor | begins only after binary/continuation policy; no arbitrary download URL |
| Parallel | Donat-owned fixed-origin OAuth2 REST example from a permissive provider schema | common auth plus generic HTTP | no generic n8n HTTP Request behavior |

Slack, HubSpot, Google Sheets, Gmail, and Drive may follow from permissive
provider artifacts or independently authored provider-doc behavior. The
inspected n8n built-ins remain SUL behavior-only. Breadth never justifies
copying a built-in description or implementing n8n's workflow model.

## 13. Implementation decomposition

All paths below are current repository paths unless marked **Create**.

| Slice | Files | Deliverable |
| --- | --- | --- |
| 0. Admission core | **Create** `crates/connector-compiler/`; **Create** `connector-sources/records/`; modify workspace `Cargo.toml` | strict `ConnectorSourceRecord`, hash/license/dependency/embedded-material admission, quarantine importer, no source execution |
| 1. Normalized IR and SerpAPI | **Create** `connector-sources/manifests/`; **Create** compiler snapshots/tests; **Create** `crates/server/build.rs` | closed IR compiler, offline generated catalog, first reviewed SerpAPI artifact |
| 2. Metadata and credentials | modify `crates/metadata/src/types.rs`, loader/type fixtures, `crates/server/src/state.rs`; **Create** `crates/server/src/connectors/credentials.rs` | source binding, credential instances/spec matching, SecretRef resolver/capability, serialized OAuth refresh |
| 3. Static runtime catalog | modify `crates/server/src/connectors/mod.rs`; **Create** `crates/server/src/connectors/catalog.rs` and `processor.rs` | generated connector and processor tables, descriptor publication, no enum/match breadth bottleneck |
| 4. Transport IR | modify `crates/server/src/connectors/http.rs`; add focused server tests | typed optional/default bindings, encodings, complete `ErrorMap`, pagination, fixed transforms, budgets |
| 5. Stripe migration | modify `crates/server/src/connectors/stripe.rs`, `crates/server/tests/connectors_stripe.rs`, `crates/server/tests/connector_webhook.rs` | compiled-step processor and generic webhook registration with unchanged tested behavior |
| 6. Triggers | **Create** generic poll/verifier modules under `crates/server/src/connectors/`; modify process ingress integration when its owning task exists | raw webhook ABI, versioned poll checkpoints, source-local durable handoff |
| 7. Inline binary/multipart | extend connector IR/HTTP executor and focused tests | bounded typed inline bytes only; no path/URL/object reference |
| 8. Provenance and native conformance | modify `knowledgebase/declarative-saas/reference-porting-register.md`; **Create on first derivative port** `THIRD_PARTY_NOTICES.md`; extend `crates/conformance/tests/connectors.rs` and `crates/conformance/fixtures/connectors/` | per-file records/notices and exact native ground-truth cases |
| 9. Update tooling | extend `crates/connector-compiler/` | exact-version re-admission and semantic catalog diff report |

The build compiler may share the SQL-free value-contract types used by process
descriptors, but `donat-schema` must not depend on `donat-server`. Server
publishes the compiled connector descriptors consumed by the server-owned
process compiler, preserving ADR 009's two-stage candidate build.

Each slice starts with its failing crate-local/native conformance test, lands
as a separately reviewable commit, and retains old operation versions needed
by live Process revisions.

## 14. TDD acceptance matrix

No Donat-derived fixture can be copied to fill this matrix because this is a
new non-Hasura surface. Tests use independently authored records, manifests,
provider stubs, DNS/peer doubles, token endpoints, webhook bytes, and payloads.
Tests never call a live provider API.

| ID | Level | Required proof |
| --- | --- | --- |
| `source_record_requires_exact_artifacts` | compiler unit | missing/mismatched npm version, integrity, commit, tree/license/file hash, provenance mapping, entrypoint, dependency, embedded-material decision, reviewer, or notice fails closed |
| `sul_source_cannot_generate_artifacts` | compiler unit | n8n `b329d...`/`n8n-nodes-base@2.33.0` can be behavior-only but cannot emit source-derived IR, Rust, fixture, or description |
| `importer_never_executes_source` | compiler integration | package scripts/functions are inventory findings; no Node/npm hook/subprocess executes |
| `imperative_ast_emits_work_item_not_rust` | compiler snapshot | function-valued routing, `execute`, `poll`, webhook code, or ambiguous expression produces a processor/unsupported finding, never guessed behavior |
| `serpapi_exact_source_compiles` | compiler insta + local HTTP | admitted `0.1.10` record produces the reviewed Donat IR and fixed prepared request; snapshots are manually reviewed |
| `openapi_requires_permissive_license` | compiler unit | exact Stripe MIT artifact can be admitted with notice; proprietary/unknown HubSpot artifact is rejected |
| `catalog_is_static_and_offline` | build/server unit | build uses checked-in records only; production registry accepts no package/source/processor path and performs no discovery |
| `credential_capability_is_narrow` | server unit | operation can apply only its compiled auth action; raw/enumerated/unrelated secrets are unavailable and redacted |
| `oauth_refresh_is_source_global` | two-binary integration | two workers sharing a source execute one fenced refresh and observe the CAS-persisted token; read-only resolver is rejected for refresh-enabled spec |
| `fixed_origin_is_unescapable` | server integration | input, processor bindings, pagination, provider body, redirects, DNS rebinding, proxy environment, and peer mismatch cannot change the compiled destination |
| `processor_calls_only_compiled_steps` | server unit | foreign step, excessive call/page/item/byte count, deadline expiry, and cancellation fail before further network I/O |
| `error_map_is_closed_and_redacted` | server/local-provider matrix | every transport/status/provider-code/malformed-success case reaches one existing class, safe code/message, and optional bounded Retry-After without secret/body leakage |
| `pagination_is_bounded` | server integration | cursor/offset/page/link plans stop at calls/pages/items/bytes/deadline; cross-origin `next` is rejected |
| `webhook_verification_precedes_parse` | endpoint/native conformance | exact raw-byte signature, timestamp, event identity, invalid-signature audit-only behavior, and durable handoff are pinned |
| `poll_checkpoint_is_typed_and_versioned` | process integration | scheduler persists a source-local checkpoint and events transactionally; processor has no static workflow data |
| `inline_binary_is_bounded` | server/native conformance | JSON/form/multipart accepts at most 1 MiB decoded inline bytes; filesystem paths, URLs, oversized bytes, and object references fail before I/O |
| `revision_fingerprint_is_complete` | process compiler integration | origin, operation/processor/credential versions, schemas, bounds, pagination/error plan, source records, and configuration change the pinned dependency |
| `live_retired_operation_remains_available` | rolling-binary process integration | old pinned work drains under a compatible binary; incompatible worker cannot claim it |
| `upgrade_diff_is_semantic` | compiler snapshot | upgrade reports record/integrity/license, operation/type, scopes, origin, request, pagination, errors, bounds, processor, tests, and notices; no retag auto-accept |

### 14.1 RED/GREEN sequence

Representative focused commands for the implementation:

```bash
# Slice 0 — initially RED because the compiler crate/admission schema is absent.
cargo test -p donat-connector-compiler --test source_admission
# GREEN after strict record/admission implementation.
cargo test -p donat-connector-compiler --test source_admission

# Slice 1 — RED before normalized IR/import support; GREEN with reviewed snapshots.
cargo test -p donat-connector-compiler --test serpapi_compile
cargo insta test -p donat-connector-compiler
cargo insta review

# Credential, processor, egress, pagination, error, and binary slices.
cargo test -p donat-metadata connectors
cargo test -p donat-server --test connectors_factory
cargo test -p donat-server --test connectors_http
cargo test -p donat-server --test connectors_stripe
cargo test -p donat-server --test connector_webhook

# Native ground truth after rebuilding the binary used by the harness.
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors

# Required regression proof after each engine-behavior slice is green.
cargo test -p donat-conformance
```

`cargo insta review` is interactive evidence review, not blanket acceptance.
The implementation report records every changed snapshot and why it changed.
Live provider credentials and provider network access are forbidden in tests.

## 15. Upgrade and semantic-diff policy

An update is a new exact source admission, even when the package name is
unchanged. The importer refetches the immutable tarball/repository/schema,
recomputes every hash, rechecks ownership/provenance/license/dependencies and
embedded material, and creates a new record. A mutable verified-registry row,
retag, semver range, or npm `latest` change never updates Donat.

The generated review report must diff:

- package/repository/schema identity, integrity, provenance, and license;
- source paths/file hashes and notices;
- provider API version and fixed origins;
- credential fields, auth class, OAuth scopes/token behavior, and redactions;
- operation/event IDs and versions, input/output hashes, request steps, and
  idempotency;
- pagination/error/trigger plans and every runtime bound;
- processor/auth/verifier IDs and Rust diffs;
- Donat tests, fixtures, and snapshots.

Any protocol-visible or type-semantic change receives a new operation or
credential version. Removal is rejected while a non-terminal Process revision
pins the dependency. Security response can retire new starts through
deploy-time metadata, but it does not reinterpret history or silently swap the
implementation under active work.

Every generated diff and snapshot receives human review. Source records,
reference register entries, and notices are updated in the same change. A
meaningful new trade-off requires an ADR before implementation.

## 16. Exact out of scope

The following are not part of this specification:

- any UI, editor, form generator, icon catalog, provider description catalog,
  dynamic options callback, or n8n-compatible node UX;
- Node.js, JavaScript execution, npm installation at runtime, WASM, dynamic
  libraries, downloaded/native plugins, package URLs, sidecars, or connector
  microservices;
- source-level use of n8n built-ins or SUL tests/fixtures/metadata without
  explicit derivative-work rights;
- automatic TypeScript-to-Rust translation of imperative code;
- `If`, `Switch`, `Merge`, `Code`, `Wait`, loops, batching as workflow logic,
  n8n items/paired items, subworkflows, AI agent/tool orchestration, or
  send-and-wait;
- arbitrary URL/method/header HTTP requests, caller-selected proxy/TLS policy,
  private-network access for imported providers, or provider-returned
  cross-origin URLs;
- business branching, compensation, retries, sleeps, durable timers, database
  writes, authorization decisions, or Process transitions inside a connector;
- a generic connector execution endpoint through GraphQL, REST, MCP, or an
  admin route;
- an admin role, data-permission bypass, runtime metadata mutation, credential
  management API, or mutable process operator API;
- interactive tenant OAuth consent/onboarding, a built-in credential vault,
  tenant credential CRUD, or consent callback routing;
- filesystem or shell access, arbitrary environment access, browser
  automation, database-driver nodes, local service nodes, or long-lived socket
  triggers;
- unbounded streaming, resumable/multi-gigabyte transfers, external object
  storage, or URL-shaped binary references;
- complete provider catalog parity, copied upstream prose, or live-provider
  tests;
- claiming exactly-once external delivery. Durable activities remain
  at-least-once with stable idempotency identities.

## 17. Acceptance and estimated complexity

The factory is accepted only when:

- the exact cohort records pass immutable admission and the first derivative
  port carries per-file provenance and notices;
- SerpAPI proves the generic Tier A compiler without executing JavaScript;
- the existing Stripe slice proves the compiled-step processor ABI without a
  behavior regression;
- credentials resolve only as capabilities, pre-provisioned OAuth refresh is
  serialized across binaries, and no interactive/admin surface exists;
- fixed-origin egress, runtime budgets, pagination, errors, triggers, and
  inline binary behavior pass the matrix above;
- production starts and operates offline with no donor source/package/runtime;
- new native connector conformance is exact and the full conformance crate is
  green after rebuilding `donat`.

Estimated complexity: XL. This is a new build-time compiler/admission subsystem
plus a cross-cutting runtime ABI, metadata evolution, credential boundary, and
native conformance surface. The ordered slices keep the legal gate, generic
executor, processor ABI, triggers, and binary work independently reviewable.

## 18. References

- `.superpowers/sdd/2026-07-29-n8n-connector-sourcing/research-report.md`
- `specs/006-in-binary-connectors.md`
- `docs/superpowers/plans/2026-07-28-in-binary-connectors.md`
- `knowledgebase/declarative-saas/reference-porting-register.md`
- `knowledgebase/declarative-saas/decisions/001-declarative-saas-runtime-and-porting-policy.md`
- `knowledgebase/declarative-saas/decisions/002-durable-process-operational-contracts.md`
- `knowledgebase/declarative-saas/decisions/009-durable-process-source-local-compilation-and-journal-contracts.md`
- `crates/metadata/src/types.rs`
- `crates/server/src/connectors/mod.rs`
- `crates/server/src/connectors/http.rs`
- `crates/server/src/connectors/stripe.rs`
- `crates/server/tests/connectors_http.rs`
- `crates/server/tests/connectors_stripe.rs`
- `crates/server/tests/connector_webhook.rs`
- `crates/conformance/tests/connectors.rs`
- `crates/conformance/fixtures/connectors/`
