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
Donat connector artifacts. An offline code generator validates the checked-in
normalized connector IR and emits checked-in static Rust registrations;
ordinary Cargo compilation embeds them with hand-written Rust processors in
the existing Donat binary. Production executes only that embedded catalog.

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

Commands never perform connector I/O. After the Spec 005 runtime prerequisites
in Section 13 exist, a durable Process commits intent before a worker calls a
connector. Every connector instance used by a Process is bound to the same
Postgres source as that Process, and all activity capacity, polling
checkpoints, and ingress journals are source-local. There is no admin role,
runtime metadata mutation API, permission bypass, or connector-call GraphQL
endpoint.

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

A source is eligible for Phase 1 source-level import only when its exact
artifact selects one of these SPDX licenses and passes the admission pipeline:
`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, or `0BSD`.
No other identifier, `LicenseRef`, exception, `WITH` expression, or ambiguous
text is accepted. A dual-licensed artifact is eligible only when the source
record selects one allowed branch and records every obligation of that
selection. A repository or npm metadata label is not sufficient. Generated
files, embedded schemas, icons, fixtures, test payloads, and every dependency
have their own origin and license decisions.

Each dependency and peer dependency has exactly one closed disposition:

- `Shipped`: its admitted bytes are linked into or distributed with Donat and
  its source, license, hashes, and notices satisfy the same gate;
- `BuildOnly`: an admitted Rust build/development dependency that is absent
  from release artifacts;
- `TypeOnlyReplaced`: donor types were inspected only to identify the boundary,
  while Donat supplies an independent neutral type/ABI and copies no bytes;
- `BehaviorOnly`: a documented observation supplies no compiler input, linked
  code, generated material, or fixture;
- `Rejected`: admission stops.

Every community donor in Section 3.2 must record its `n8n-workflow` peer as
`TypeOnlyReplaced`. The SUL dependency's bytes are not copied, parsed as
compiler input, executed, linked, generated from, or shipped. A source-policy
test scans acquisition inputs, committed manifests, generated Rust, Cargo
metadata, and release artifacts for that prohibition.

Permissive provider OpenAPI/Discovery is a co-equal input. A publicly readable
schema is not necessarily reusable. The exact artifact requires an immutable
revision, checksum, license evidence, notice, and per-file record before it can
generate checked-in Donat IR or Rust.

`n8n-nodes-base` version `2.33.0` at n8n revision
`b329d57767cb6ec046bb1ecf9293b450c831d720` is
`LicenseRef-n8n-sustainable-use`. Its source, tests, fixtures, credential
classes, descriptions, and generated metadata are not eligible for
source-level import, translation, fixture reuse, or generated-metadata
extraction under Donat's policy absent an express written grant. They remain
behavior-only. Such a grant would have to cover source copying, Rust
translation, derivative-work creation, fixture use, generated-metadata
extraction, commercial hosting, and redistribution. Enterprise or OEM
branding alone must not be interpreted as that grant.

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

- a pre-port `ConnectorSourceRecord` accepted through repository review;
- one `knowledgebase/declarative-saas/reference-porting-register.md` entry per
  imported or translated upstream file;
- the donor's required copyright/license text in root
  `THIRD_PARTY_NOTICES.md`;
- a provenance header or generated-record link on every derived destination;
- a Donat-owned failing test named in the record before the implementation;
- human review of both the checked-in normalized manifest and the checked-in
  generated Rust catalog.

Repository-level MIT licensing does not automatically clear fixtures. Each
fixture must be independently reviewed for third-party provider payloads,
personal data, secrets, trademarks, copied schemas, and generated proprietary
material. A fixture that fails that review is replaced with independently
authored Donat data, even when the donor source itself is eligible.

## 4. Source admission and compiler pipeline

### 4.1 `ConnectorSourceRecord`

Repository review is the admission authority. There is no cryptographic
signature, signer key, trust root, or self-referential commit claim in Phase 1.
Every exact donor version has one versioned pre-port record whose deserializer
denies unknown fields. Conceptually:

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
    proposed_manifest_path: RepoPath,
    proposed_destinations: Vec<RepoPath>,
    red_tests: Vec<TestId>,
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

`DependencyDecision` uses only the five dispositions in Section 3.1. The
safety record rejects dynamic code, `eval`, process or shell execution,
filesystem/environment access, arbitrary destinations, proxy or TLS controls,
long-lived sockets, local services, unadmitted SDK dependencies, unbounded
loops, and unbounded binary behavior. A mismatch in package version, tarball
integrity, provenance, git tree, entrypoint, license, dependency, or embedded
material fails closed.

Admission and port evidence are distinct:

1. the pre-port source record proves that exact source bytes and selected paths
   are eligible inputs and names proposed destinations/RED tests;
2. the port change checks in the normalized manifest, generated Rust catalog,
   notices, tests, and per-file reference-register entries;
3. repository review of that change is the post-port approval. The existing
   register records source path/hash, destination, adaptation, RED/GREEN
   evidence, and reviewer. Git history and the review system retain the
   resulting commit identity outside the record; no file contains its own
   commit hash.

### 4.2 Hostile acquisition and extraction

The network-capable acquisition tool treats every remote artifact as hostile.
Its Phase 1 policy is exact:

- acquisition uses HTTPS only;
- npm/repository bytes may come only from the checked-in allowlist
  `registry.npmjs.org`, `github.com`, and `codeload.github.com`; adding a
  provider-schema host is a reviewed source-policy change, not a command-line
  option;
- at most three redirects are followed, every redirect remains HTTPS and on
  the original host, and credentials/cookies are never forwarded;
- the streaming compressed artifact limit is 64 MiB;
- the complete tarball is written into an exclusive temporary directory with
  mode `0700`, hashed, and compared with the source record before any archive
  entry is inspected or extracted;
- extraction permits at most 256 MiB expanded total, 16 MiB per regular file,
  10,000 entries, and path depth 32;
- every entry path must be normalized relative UTF-8. Absolute paths, empty
  components, `.`/`..`, platform prefixes, NUL, duplicate normalized paths,
  and ASCII-case-folding collisions are rejected;
- only ordinary directories and regular files are accepted. Symlinks,
  hardlinks, devices, FIFOs, sockets, sparse entries, and unknown entry types
  are rejected;
- files are created exclusively beneath the quarantine root. Subsequent walks
  use no-follow metadata and verify that every visited path is still a regular
  file/directory beneath that root;
- an RAII cleanup guard removes the `0700` directory on success or error; a
  startup cleanup pass removes abandoned tool-owned directories after a crash;
- package scripts, executables, donor tests, npm hooks, Node, and JavaScript
  are never executed.

Synthetic adversarial tests cover absolute and `..` traversal, links and every
special type, sparse/unknown entries, duplicate/case-collision paths,
over-limit compressed/expanded/file/count/depth inputs, cross-host and excess
redirects, digest mismatch before extraction, no-follow walk replacement, and
a package-script sentinel that must remain untouched.

### 4.3 Admission and translation stages

The development pipeline performs these stages in order:

1. discover an exact `(package, version)` or provider artifact;
2. acquire and extract it under Section 4.2;
3. verify provenance/repository mapping, license, notices, dependencies,
   entrypoints, and embedded material;
4. inventory provider integrations only and classify each operation as Tier A,
   B, C, or rejected;
5. emit an unapproved pre-port review bundle containing the candidate source
   record, per-path hashes, approved-operation proposal, unsupported findings,
   proposed destinations/RED tests, and notice obligations, but no derivative
   manifest or Rust;
6. land and repository-review the pre-port record before any derivative;
7. start the recorded Donat RED tests, translate only the approved paths, and
   check in both the normalized manifest and deterministically generated Rust
   catalog with register/notices;
8. run offline `generate --check`, crate tests, and repository review over the
   complete derivative diff.

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

### 4.4 Development/build time versus production

The stages are deliberately separated:

| Stage | Inputs | May access donor source/network? | Output | Shipped in `donat`? |
| --- | --- | --- | --- | --- |
| Development acquisition | exact HTTPS identity | yes, only through Section 4.2 | ignored quarantine and candidate review bundle | no |
| Pre-port repository review | source record, hashes, license/dependency decisions | no | approved checked-in source record | no |
| Offline codegen | approved record plus checked-in normalized manifest | never | checked-in deterministic Rust catalog and digest report | Rust catalog only |
| Ordinary Cargo build | checked-in Rust catalog | never | `donat` | yes |
| Production runtime | embedded catalog, deploy-time metadata, Postgres, per-use credential capability | never | bounded provider result or verified ingress event | yes |

The normalized manifest and generated Rust catalog are both reviewed derivative
artifacts with stable repository paths. `donat-connector-codegen generate
--check` regenerates into a temporary directory, compares every byte and a
domain-separated digest with the checked-in Rust, and fails on drift,
unreferenced output, or unexpected files. There is no server `build.rs` and no
code generation during `cargo build`. Reproducible builds operate offline.
Production metadata cannot name a package URL, tarball, repository, source
path, code blob, processor path, or unregistered connector.

## 5. Normalized connector IR

The compiler emits immutable runtime descriptors. Source descriptions and UI
hints are discarded; stable field IDs, types, constraints, and protocol
mappings remain.

### 5.1 Canonical JSON v1 and domain-separated hashes

Every source record, normalized manifest, and descriptor object denies unknown
fields at every nesting level. Canonical JSON v1 is RFC 8785 JSON
Canonicalization Scheme (JCS): UTF-8 without BOM or insignificant whitespace,
lexicographically sorted object member names, preserved array order, JCS
number/string escaping, no duplicate member names, and no Unicode
normalization. Schema validation occurs before canonicalization. A format
change requires a new explicit canonical version; a v1 reader never guesses
how to read a later version.

Two lower-case SHA-256 hashes are mandatory and not interchangeable:

```text
semantic_sha256 =
  SHA256("donat.connector.semantic.v1\0" || JCS(semantic_material))

provenance_sha256 =
  SHA256("donat.connector.provenance.v1\0" || JCS(provenance_material))
```

`semantic_material` contains connector/operation/credential/event versions,
types, every step effect and idempotency binding/scope/minimum-retention/clock
margin, fixed steps/origins, encodings, mappings, processors, and all bounds;
it excludes review names, source URLs, and notice text. `provenance_material`
contains the source-record identity, exact artifact/per-file/license hashes,
dependency/embedded-material decisions, notice IDs, normalized-manifest
semantic hash, and classifier/generator versions. Both hashes are stored in
the generated catalog and configuration fingerprint. A semantic change never
hides behind an unchanged provenance hash, and a source/license change never
hides behind unchanged behavior.

Golden vectors are normative:

| Canonical bytes | Domain | SHA-256 |
| --- | --- | --- |
| `{}` | `donat.connector.semantic.v1\0` | `799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19` |
| `{}` | `donat.connector.provenance.v1\0` | `a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.semantic.v1\0` | `2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.provenance.v1\0` | `4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23` |

Unit tests must construct those exact byte strings independently of the
production helper, then pin representative manifest and catalog vectors.

### 5.2 `CredentialSpec`

A `CredentialSpec` contains:

- stable connector-qualified ID and version;
- typed field IDs, optionality, secret classification, maximum size, and
  redaction behavior;
- one Phase 1 common auth plan: fixed-header API key, fixed-query API key,
  bearer, HTTP Basic, OAuth2 client credentials acquired once per activity, or
  a pre-provisioned non-refreshing OAuth access token;
- fixed allowed origins to which the capability may apply;
- declared scopes, a fixed token origin/response mapping for client
  credentials, and maximum credential/token sizes;
- an optional statically registered auth processor ID for a narrow provider
  signature scheme;
- an optional credential-test operation ID.

It contains no display widget, HTML, arbitrary expression, raw secret, runtime
URL, refresh token, refresh/writeback plan, or JavaScript hook.

### 5.3 `OperationSpec` and effect/idempotency

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
- one closed `OperationEffect`;
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

The only executable Phase 1 effect classes are:

```rust
pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: NonEmptyVec<ProviderIdempotentStep>,
    },
}

pub struct ProviderIdempotentStep {
    step: CompiledStepId,
    fixed_binding: FixedIdempotencyBinding,
    scope: ProviderIdempotencyScope,
    minimum_retention_ms: NonZeroU64,
    clock_safety_margin_ms: NonZeroU64,
}

pub enum FixedIdempotencyBinding {
    Header { name: StaticHeaderName },
    BodyField { pointer: StaticBodyPointer },
}
```

`ReadOnly` is headerless and is admitted only when the provider contract has
no external mutation. Worker-level automatic retry after a retryable failure
and lease takeover after ambiguous worker loss are safe for this class; the
HTTP client still performs no hidden transport retry.

`ProviderIdempotencyScope` is the canonical static ID for the
provider-documented uniqueness namespace, for example a connected provider
account plus API idempotency namespace. Each `ProviderIdempotentStep` is
sourced from immutable provider evidence that establishes the exact binding,
scope, and conservative minimum retention. The selected safety margin covers
provider/Donat clock uncertainty and is positive and strictly smaller than
`minimum_retention_ms`. These fields enter the source record, normalized IR,
semantic hash, public operation descriptor, configuration fingerprint, and
pinned process revision.

Every side-effecting compiled step has exactly one such entry; read-only steps
have none. Duplicate, missing, or unproven entries reject executable
admission. This permits a multi-step mutation only when every side-effecting
step is independently safe. For each entry the stable provider key is:

```text
base64url_no_pad(SHA256(
  "donat.connector.idempotency.step.v1\0" ||
  JCS({ "logical_activity_id": logical_activity_id,
        "scope": scope,
        "step": compiled_step_id })
))
```

The same key is applied to that step's one compiled header/body binding on
every retry and takeover. Neither another step nor another scope reuses it.

Before a Process revision can reference the operation, process compilation
calculates a conservative `maximum_send_horizon_ms` independently for each
side-effecting step. It is the maximum elapsed time from that step's first
provider attempt through the last instant any configured execution path may
send it again, including all remaining schedule/capacity/rate/serialization
delay, start-to-close time, lease expiry, takeover delay, maximum retry
backoff/jitter, and any other delay before the last possible send. Every
component uses its configured worst-case bound; a missing or unbounded
component in Spec 005 makes the operation non-executable until that contract
is added. It is never treated as infinity that somehow passes.

Compilation requires, with checked integer arithmetic:

```text
usable_window_ms = minimum_retention_ms - clock_safety_margin_ms
maximum_send_horizon_ms <= usable_window_ms
```

The source-local activity journal persists `first_provider_attempt_at` from
the database clock for each `(logical_activity_id, compiled_step_id)` in a
short transaction committed before that step's first network send. A retry or
takeover reuses it. Immediately before every later send the server checks the
database clock; equality with
`first_provider_attempt_at + usable_window_ms` is allowed, while a later time
permanently refuses I/O with
`connector_idempotency_window_exhausted`. This journal field and compiler
calculation are Spec 005 prerequisites; the factory must not create a parallel
lease/history store.

There is no executable non-idempotent side-effect class in Phase 1. A donor
side effect without provider idempotency may be inventoried and reported, but
cannot enter the generated executable catalog, be enabled in deployment
metadata, or be referenced by a Process. `max_attempts: 1` is not an escape:
lease loss can still leave an ambiguous provider outcome.

This contract normatively supersedes Spec 006's blanket rule that every
durable HTTP operation has an idempotency header. It also replaces the
currently proposed Spec 005 `ConnectorIdempotencySupport::{StableKeyHeader,
Unsupported}` before Spec 005 Task 2 publishes descriptors:

- `ReadOnly` projects as a first-class read-only effect with no binding;
- `ProviderIdempotent` projects every side-effect step's fixed binding, scope,
  retention, and clock margin;
- `Unsupported` is inventory-only and is rejected from the executable
  descriptor catalog.

There is no persisted implementation to migrate today. The compiler has
negative tests for inventory-only side effects, exact-boundary and
one-millisecond-over retention tests, and independent multi-step validation.
Spec 005-gated two-worker tests prove headerless read-only takeover,
stable-step-key provider mutation takeover, and late-takeover refusal before
I/O.

### 5.4 `PaginationPlan`

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

### 5.5 `ErrorMap`

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

### 5.6 `TriggerSpec`

`TriggerSpec` has two provider-integration forms:

- `Webhook`: fixed authenticator, server-codec, and pure normalizer IDs,
  selected headers, raw-body maximum, timestamp window, event
  ID/type/output contract, redaction, and optional explicit
  create/delete/check subscription operation IDs;
- `Poll`: versioned checkpoint type, static poll processor ID, event type,
  per-poll event limit, and operation bounds.

A webhook authenticator verifies bounded raw bytes and returns only an opaque
`AuthenticatedRawBody`. The server codec can parse the body only after that
token exists; a pure processor then normalizes the decoded typed value into a
verified provider event ID, event type, and typed payload. Authentication
failure is audit-only and never reaches the codec. Durable dedupe, delivery
audit, correlation, process start, and acknowledgement remain in the
source-local process ingress transaction defined by ADR 002/009.

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
- field-by-field read-only deploy-time `SecretRef` values;
- an immutable credential-spec version;
- no plaintext secret in metadata, a process revision, logs, errors, or
  conformance output.

Phase 1 supports static API keys, Basic secrets, bearer tokens, OAuth2 client
credentials whose access token is acquired fresh and never refreshed within
an activity, and pre-provisioned non-refreshing OAuth access tokens. It accepts
no refresh-token field and stores no token returned by a provider. A client
credentials token request uses a fixed compiled origin/step, consumes the same
call/byte/deadline budget as provider calls, and its capability is dropped
with the logical attempt.

Interactive authorization/consent, refresh-token persistence or rotation,
distributed refresh, external writable resolvers, tenant ownership/lifecycle,
a credential vault/API, and credential CRUD are a later ADR and
specification. They must preserve explicit roles without an admin role,
runtime metadata mutation, or data bypass.

### 6.2 Resolver and execution capability

The server reserves an internal read-only `CredentialResolver` boundary.
Resolver implementations are compiled into the binary; metadata cannot load
one. The Phase 1 production implementation is the environment resolver used
by the current `SecretRef`. Startup validates reference syntax and required
availability without retaining the value. `ConnectorRegistry` stores the
resolver binding/reference and resolves it again for every use, so no rotating
value is frozen into the immutable registry. A later read-only external
resolver remains possible, but no writable method exists in this ABI.

Resolution returns an opaque `CredentialCapability`, not a secret map. The
generic executor can apply only the auth action compiled for the selected
step. A named auth/verifier processor can request only its compiled primitive,
such as HMAC over bounded bytes. Operation processors cannot enumerate
credentials, read unrelated fields, expose raw values, select a new origin, or
log capability output.

The resolver has no enumerate, write, compare-and-swap, refresh, delete, or
administration method. Credential and token response errors are bounded,
typed, and fully redacted.

## 7. Static processor and I/O ABI

### 7.1 Crate graph and mechanical boundary

In this graph `A -> B` means crate `A` depends on crate `B`:

```text
donat-ir                    -> donat-value-contract
donat-connector-catalog     -> donat-value-contract
donat-connector-catalog     -> donat-connector-abi
donat-connector-abi         -> donat-value-contract
donat-connector-processors  -> donat-connector-abi
donat-connector-processors  -> donat-value-contract

donat-connector-acquire     -> donat-connector-catalog
donat-connector-codegen     -> donat-connector-catalog

donat-server -> donat-ir
donat-server -> donat-connector-catalog
donat-server -> donat-connector-abi
donat-server -> donat-connector-processors
donat-schema -> donat-ir
```

`donat-value-contract` is the single owner of the closed `ValueType`/`TypeRef`
language, `TypedValue`, bounded/canonical constructors, deterministic object
ordering, canonical-size accounting, and inline-byte representation. It
exposes no `serde_json::Value`. `donat-ir` re-exports these types rather than
defining a second representation. This supersedes ADR 009's accepted
`donat-ir` ownership statement; implementation must update ADR 009 or accept a
superseding ADR before the value-contract slice lands.

`donat-connector-catalog` explicitly owns `ConnectorSourceRecord`, the
normalized connector IR, canonical record/manifest/descriptor hashes, and the
checked-in generated catalog. It imports the ABI-owned connector, operation,
compiled-step, processor, authenticator, codec, normalizer, credential, and
capability ID/envelope types directly. It never defines a catalog-local string
copy or conversion type for them. Catalog may use serde and pure hashing but
does not depend on `donat-ir`, server, Tokio, reqwest, filesystem, or network
libraries. `donat-connector-acquire` and `donat-connector-codegen` are sibling
development/CI crates that each depend on the catalog; neither depends on the
other. Acquisition alone owns HTTPS/archive handling. Codegen reads checked-in
records/manifests and writes checked-in Rust. Neither is a server dependency
or runtime image component.

`donat-connector-abi` owns neutral IDs, bounded envelopes, errors, contexts,
and server-implemented host capability traits. The processor crate owns sealed
processor traits, implementations, and its private registry. The value,
ABI, and processor crates declare:

```rust
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
```

All three crates are unpublished, have no build script, have empty default
features, and expose no Phase 1 `std` feature. The Phase 1 processor runtime
closure contains only local path dependencies on `donat-connector-abi` and
`donat-value-contract`; the ABI depends only on the value contract. There are
no third-party runtime, build, or procedural-macro dependencies in that
closure. A future exception requires an exact version/checksum/features/target
and transitive-closure review; it is not part of Phase 1.

The catalog-to-ABI edge is one-way. It does not pull catalog serde/hashing,
generated entries, acquisition, or codegen into the ABI or processor
dependency closure.

The server exclusively owns and implements `ConnectorIo`,
`ProcessorControl`, credential and cryptographic primitives, JSON/form/URL and
UUID/text/time codecs, credential materialization, clocks, cancellation,
reqwest, TLS/DNS/peer checks, response header/status handling, compiled-step
lookup, and every other I/O. Processors receive only canonical values and
opaque host capabilities. An auth primitive returns a non-serializable,
non-debuggable opaque binding consumable only by the exact compiled step/slot;
it never returns a raw secret or authentication tag.

CI mechanically enforces the boundary by:

- evaluating locked metadata for normal, build, procedural-macro, optional,
  target-specific, and feature-unified edges for every supported target, then
  comparing package/version/checksum/feature/target data with the checked-in
  exact local-only allowlist;
- rejecting build scripts, build/procedural-macro/native/git/patched
  dependencies, `links`, FFI, inline/global assembly, and unsafe code in the
  boundary closure;
- compiling all three crates with default features disabled for a
  CI-installed no-OS target;
- canonicalizing path dependencies and scanning source plus expanded imports
  for symlink/workspace escapes, `include!`, `#[path]`, `env!`,
  `option_env!`, generated paths, `extern crate std`, `std::`,
  unsafe/FFI/assembly/link attributes, network/socket, filesystem,
  environment, process, database, reqwest, Tokio, and unapproved crate paths;
- forbidding generated or donor source inside the hand-written processor
  directory;
- compile-asserting object safety plus `Send + Sync` for every host and
  processor trait, and rejecting processor implementation blocks outside the
  processor crate; and
- scanning the separately packaged `donat` runtime closure as specified in
  Section 14, not the tooling-bearing workspace artifact directory.

This is an API/dependency policy for trusted in-tree native code, not a
sandbox. Mechanical checks do not preempt an infinite CPU loop, panic,
excessive allocation, static-state misuse, or exfiltration through a
semantically overbroad allowed step. The Rust toolchain, server adapters and
host capabilities, manifest admission, processor algorithms, and narrowness
of every compiled binding remain trusted review surfaces.

### 7.2 Core operation ABI

A processor orchestrates compiled step IDs; it does not construct a transport
request. `ConnectorIo` and `ProcessorControl` are ABI-owned host traits whose
only implementation is server-owned. `OperationProcessor` is a sealed
processor-crate trait:

```rust
pub trait ProcessorControl: Send + Sync {
    fn check(&self) -> Result<(), ConnectorFailure>;
}

// Defined in donat-connector-abi; implemented only by donat-server.
pub trait ConnectorIo: Send + Sync {
    fn call<'a>(
        &'a self,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>>;
}

// Defined and sealed in donat-connector-processors.
pub trait OperationProcessor: Send + Sync {
    fn execute<'a>(
        &'a self,
        context: ProcessorContext<'a>,
        input: TypedValue,
        io: &'a dyn ConnectorIo,
    ) -> BoxFuture<'a, Result<TypedValue, ConnectorFailure>>;
}
```

`BoxFuture` uses only `core::future`, `core::pin`, and `alloc::boxed`.
`ProcessorContext` exposes only connector/operation identity, stable logical
activity/idempotency identity, canonical request fingerprint, opaque
capability IDs, and a borrowed `ProcessorControl`. The control capability
checks the server's monotonic deadline and cancellation state without exposing
Tokio types. `ConnectorIo::call` resolves a step within the current compiled
operation. A foreign or undeclared step is an `invariant` failure before
network I/O.

The ABI contains no raw HTTP client, URL, method, header map, proxy, TLS
control, database pool, role, process instance/graph, Rule evaluator, retry
policy, environment, filesystem path, workflow item, persistent static data,
thread/process handle, or unbounded stream. A processor cannot recurse or call
another connector through the ABI.

The runtime invokes `ProcessorControl::check` before and after every transform,
credential/crypto action, page, codec, and I/O step; `ConnectorIo` checks
again on entry and completion. It charges each call, page, item, and byte
against the compiled operation budget. Processor output is revalidated
against the compiled output type before it can reach a process journal. This
cannot interrupt a processor stuck in native CPU work, which remains a trusted
review risk.

The processor crate's private table maps stable `(processor_id, version)` pairs
to implementations compiled into the binary. Constructors and registration
are private. Its public lookup accepts only generated admitted IDs and returns
an opaque processor handle; the server cannot implement/register an alternate
processor or provide a dynamic fallback. CI rejects implementation blocks
outside this crate.

Three compile-only checkpoints assign the exact same ABI-owned Rust ID types
without `String`, `&str`, parse, serialization round-trip, `From`/`Into`,
wrapper copy, or other conversion:

1. phase 2 `catalog_descriptor_ids_match_connector_io` assigns normalized
   descriptor IDs directly to `ConnectorIo` parameters;
2. phase 3b `generated_catalog_ids_match_abi`, run only after codegen creates
   the actual checked-in generated entries, assigns every generated ID to its
   ABI type; and
3. phase 4 `processor_lookup_ids_match_abi` assigns generated processor-family
   IDs directly to private-table lookup.

### 7.3 Auxiliary object-safe ABIs

The processor crate defines and seals its pure/orchestration traits. Their
contexts, values, results, and ABI-owned host capabilities remain neutral:

```rust
pub trait PureTransform {
    fn apply(
        &self,
        context: TransformContext<'_>,
        input: TypedValue,
    ) -> Result<TypedValue, ConnectorFailure>;
}

pub trait AuthProcessor {
    fn apply(
        &self,
        context: AuthContext<'_>,
        input: BoundedAuthInput,
        crypto: &dyn CredentialPrimitives,
    ) -> Result<TypedBindings, ConnectorFailure>;
}

pub trait WebhookAuthenticator {
    fn authenticate(
        &self,
        context: WebhookContext<'_>,
        input: BoundedWebhookInput,
        crypto: &dyn CredentialPrimitives,
    ) -> Result<AuthenticatedRawBody, WebhookRejection>;
}

pub trait WebhookNormalizer {
    fn normalize(
        &self,
        context: WebhookNormalizeContext<'_>,
        decoded: TypedValue,
    ) -> Result<VerifiedInboundEvent, WebhookRejection>;
}

pub trait PaginationProcessor {
    fn next(
        &self,
        context: PaginationContext<'_>,
        response: BoundedTransportResponse,
        state: PaginationState,
    ) -> Result<PaginationDecision, ConnectorFailure>;
}

pub trait PollProcessor {
    fn poll<'a>(
        &'a self,
        context: PollContext<'a>,
        checkpoint: TypedCheckpoint,
        io: &'a dyn ConnectorIo,
    ) -> BoxFuture<'a, Result<PollBatch, ConnectorFailure>>;
}
```

`CredentialPrimitives` exposes only catalog-declared operations such as
constant-time HMAC verification or opaque auth binding with an opaque field
ID; it never returns raw credential/tag bytes. `PaginationDecision` is only
`Done` or
`Continue { compiled_step, bindings }`; it cannot contain a URL or method.
Webhook authentication receives bounded raw bytes, selected copied headers, a
server-supplied receipt timestamp value, and credential capability. Success
returns an opaque `AuthenticatedRawBody` whose fields cannot be inspected,
serialized, formatted, or converted by processor code. Only the server-owned
codec consumes it and parses the bounded raw bytes into `TypedValue`; only
then does the pure `WebhookNormalizer` produce a typed event. Authentication
failure never invokes the codec, so raw-byte verification still precedes
parsing. Poll inputs contain a typed versioned checkpoint, server-supplied
database-time value, control capability, and event limit. Every output is
type- and budget-validated by server code.

Phase 1 implements common auth without an `AuthProcessor`, generic declarative
pagination, pure transforms needed by the first donors, the
`OperationProcessor` for the Stripe migration, and the existing Stripe
webhook as `WebhookAuthenticator` plus server codec and
`WebhookNormalizer`, while retaining `503`. Provider-specific auth and
pagination processors require a selected admitted donor. `PollProcessor` may
be added with a real donor, but scheduling/checkpoint persistence is Spec
005-gated. Binary and continuation ABIs remain gated by Section 9.

## 8. Fixed-origin egress and runtime bounds

Provider connectors compile all outbound origin IDs. Deployment can select an
endpoint identity but cannot replace a provider origin. The existing generic
`http` module may retain a deploy-time fixed base URL for independently
authored integrations, but it remains `public_only` in the current
implementation. Neither process input nor an imported provider operation may
select its scheme, host, port, method, header names, proxy, certificate policy,
or redirect behavior.

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
| Outbound calls, including provider, credential, auth, pagination, and continuation calls | 16 per logical attempt |
| Pages | 16 |
| Normalized output items | 10,000 |
| Request headers | 64 |
| Header name | 256 bytes |
| Header value | 8 KiB |
| Aggregate request/response headers retained or mapped | 32 KiB each |
| Rendered URL | 8 KiB |
| Rendered path | 2 KiB |
| Rendered query | 6 KiB |
| Parsed JSON depth | 64 |
| Parsed JSON nodes | 100,000 |
| Individual JSON string | 256 KiB |
| Request body per call | 1 MiB |
| Response body per call | 1 MiB |
| Aggregate request bodies | 4 MiB per logical attempt |
| Aggregate response bodies | 4 MiB per logical attempt |
| Normalized process-bound output | 256 KiB canonical JSON |
| Inline-binary values | 16 per complete typed value |
| Aggregate decoded inline binary | 128 KiB (131,072 bytes) and still within the 256 KiB canonical result |
| Raw webhook body | 1 MiB |
| Redirects | 0 |
| `Retry-After` delay | 86,400 seconds |

All auxiliary, credential/token, pagination, and future continuation calls
consume the same call, request-byte, response-byte, and activity deadline
budgets. No nested processor receives a fresh budget. `Retry-After` accepts
integer seconds or an HTTP date, clamps a later delay to 86,400 seconds, treats
a past date as zero, and ignores malformed values; it never expands the
activity deadline.

Build/deployment validation rejects a zero, contradictory, or larger static
declaration with `ConnectorConfigError`, outside activity retry routing.
Runtime overflow uses this exact closed policy:

| Overflow source | Class | Code |
| --- | --- | --- |
| operation input renders too many/large headers, URL/path/query, JSON structure/string, request body, or inline bytes | `validation` | `connector_input_limit_exceeded` |
| resolved credential/auth material exceeds its declared or engine limit | `authentication` | `connector_credential_limit_exceeded` |
| one provider response exceeds header/body/JSON structural limits | `validation` | `connector_response_limit_exceeded` |
| provider-driven pages/items or aggregate request/response bytes exceed the logical-attempt budget | `validation` | `connector_execution_limit_exceeded` |
| normalized typed output exceeds 256 KiB canonical JSON | `validation` | `connector_output_limit_exceeded` |
| a processor requests an undeclared step or exceeds its compiled step/call budget | `invariant` | `connector_processor_budget_exceeded` |

The safe message names only the limit category, never a value, URL, header, or
body. Overflow is never partial success or silent truncation. The process
activity's start-to-close deadline is authoritative and may be stricter.
Increasing an engine ceiling requires a reviewed runtime-ABI change, fresh
adversarial tests, and a process dependency revision.

No connector stores unbounded response bodies or provider diagnostics. Item
normalization stops before exceeding the item/byte budget and returns a typed
failure; it never silently truncates a success.

## 9. Binary and multipart contract

Phase 1 supports only bounded inline bytes for small payloads after the shared
`donat-value-contract` crate gains an explicit bytes form. Its exact external
and canonical JSON representation is:

```json
{"$binary":"<unpadded-base64url>","media_type":"<ASCII>"}
```

or, when a filename is present:

```json
{"$binary":"<unpadded-base64url>","file_name":"<UTF-8>","media_type":"<ASCII>"}
```

Those member orders are the RFC 8785 JCS orders (`$binary`, optional
`file_name`, `media_type`) regardless of input member order; there is no
second custom ordering rule. `$binary` is RFC 4648 base64url without `=`
padding. A canonical decoder rejects padding, standard-base64 `+`/`/`,
non-zero discarded bits, duplicate/unknown members, or any other noncanonical
spelling. `media_type` is at most 255 ASCII bytes. `file_name`, when present,
is at most 255 UTF-8 bytes and is data only, never a path.

A complete typed value contains at most 16 inline-binary values and at most
131,072 aggregate decoded bytes. The complete normalized process-bound JCS
encoding remains at most 262,144 bytes. A binary-capable operation declares
finite maxima for every other string and every list/object cardinality. The
compiler calculates the worst-case complete canonical size, including object
keys, escaping, base64url expansion, media types, filenames, and enclosing
output structure, and rejects a declared contract whose maximum exceeds
262,144 bytes. An operation may therefore need a decoded-byte limit below the
engine ceiling.

These normative vectors use the exact representation above:

| Typed value | JCS byte length | Result |
| --- | ---: | --- |
| root binary of 131,072 zero bytes, media type `application/octet-stream`, no filename | 174,817 | accepted |
| root binary of 131,073 zero bytes, same media type, no filename | not encoded as a valid value | rejected by the decoded aggregate cap |
| `{"binary": <the accepted root binary>, "padding": "a" repeated 87,303 times}` | 262,144 | accepted |
| `{"binary": <the accepted root binary>, "padding": "a" repeated 87,304 times}` | 262,145 | rejected by the complete canonical cap |

The vector descriptions denote typed inputs; their serialized bytes contain
no spaces. Tests construct the zero bytes and filler independently, encode
with the production-independent RFC 4648/JCS helper, and assert the exact byte
counts. The 1 MiB per-call and 4 MiB logical-attempt raw transport ceilings
still apply. Multipart field names, content disposition, and media-type policy
are compiled. Runtime input supplies only typed scalar or inline-byte values.

The runtime never accepts a filesystem path, file descriptor, stream handle,
bucket/key pair disguised as text, `file://` URI, or arbitrary HTTP(S) URL.
Temporary buffering, if required by reqwest, is memory-only and charged before
the request is sent.

A larger object-reference capability is later work. It must be an opaque,
resolver-qualified capability with tenant/source ownership, expiry, content
length/hash, and operation allowlist. It cannot be a URL and cannot weaken the
fixed-origin egress policy.

## 10. Static catalog, deployment metadata, and revision pinning

`donat-connector-codegen` emits an immutable, checked-in Rust table under
`crates/connector-catalog/src/generated/` containing:

- connector ID/version/runtime ABI and provider ID;
- `CredentialSpec`, `OperationSpec`, `PaginationPlan`, `ErrorMap`, and
  `TriggerSpec` entries;
- processor/authenticator/codec/normalizer IDs and versions;
- fixed origins and network policy;
- every side-effect step's provider idempotency binding, scope,
  minimum-retention evidence, and clock margin;
- canonical semantic/provenance/input/output and configuration hashes;
- source-record IDs, source file hashes, license class, and notice IDs;
- compiler/classifier versions.

Generated files are sorted by stable connector/operation ID and contain a
header naming their manifest, source record, generator version, and both
hashes. The generated-artifact digest is
`SHA256("donat.connector.generated-rust.v1\0" || entries)`, where `entries` is
the concatenation, in UTF-8 repository-path byte order, of each path length as
an eight-byte big-endian integer, path bytes, file length in the same form, and
file bytes. `generate --check` reproduces those bytes in a fresh temporary
directory and rejects any digest/file/path difference. Cargo never invokes the
generator.

`ConnectorRegistry::build` continues to run before the listener opens. It
validates deploy-time instances against this table, probes declared read-only
secret references without retaining values, and materializes immutable
resolver bindings plus operation contracts. Credentials are re-resolved per
use. The generated table replaces the `RegistryInstance::Http`/`Stripe`
dispatch match without adding runtime discovery. Existing `http` and `stripe`
module IDs remain supported through catalog entries during migration.

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
runtime ABI, every step's effect/idempotency binding/scope/minimum
retention/clock margin, the compiler-calculated maximum send horizon,
processor/version, credential-spec/identity, endpoint identity, non-secret
configuration fingerprint, origin policy, semantic/provenance hashes, bounds,
and input/output hashes. This publication, live-retired execution, and worker
claim behavior do not exist until the prerequisites in Section 13 are met.

Rotating a read-only secret value does not serialize it into or change the
revision. A change to resolver identity, credential class/spec, endpoint
identity, origin, scope set, API version, effect/idempotency, processor,
operation schema, idempotency scope/retention/margin, pagination/error plan,
or runtime bound creates a new dependency fingerprint.

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
effect: read_only
input:
  query: { type: "string!", min_length: 1, max_length: 512 }
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
effect:
  provider_idempotent:
    side_effect_steps:
      - step: create_session
        fixed_binding: { header: Idempotency-Key }
        scope: stripe.account.v1
        minimum_retention_ms: 86400000
        clock_safety_margin_ms: 300000
processor: stripe.checkout.create_session.v1
steps:
  - id: create_session
    method: POST
    origin: stripe_api
    path: /v1/checkout/sessions
    encoding: form_urlencoded
    auth: stripe.secret_key.v1
    success_statuses: [200]
bounds:
  calls: 1
  pages: 1
  request_bytes: 1048576
  response_bytes: 1048576
  output_canonical_bytes: 262144
```

The scope and timing values are executable only when the admitted immutable
provider evidence records those exact conservative values; the example does
not authorize them by assertion.

The Rust processor validates the existing typed input, creates only bindings
for `CompiledStepId("create_session")`, calls `ConnectorIo`, and normalizes the
existing `id`, `url`, `status`, and `expires_at` output. It cannot construct
`https://api.stripe.com` or an Authorization header itself. The existing raw
Stripe webhook authentication becomes a static `TriggerSpec::Webhook`
authenticator/codec/normalizer registration. This is a registry/ABI migration
of independent Donat Rust, not a claim that n8n's SUL Stripe implementation
was ported.

## 12. First implementation cohort

Implement slices in this order, keeping only named provider operations:

| Order | Connector slice | Target style | Explicit exclusions/deferments |
| ---: | --- | --- | --- |
| 0 | Existing Stripe Checkout creation and completion webhook | static processor plus two-stage webhook authentication/normalization | no broad Stripe/n8n port |
| 1 | SerpAPI Google search, then selected search engines | Tier A generic HTTP; pagination only when proven | no UI metadata or arbitrary engine/request fields |
| 2 | Brave web/news/image search | generic HTTP plus one named query transform | no ambient expressions |
| 3 | Resend contact list and verified inbound events; inventory email send | generic/error/pagination IR plus Rust verifier | email send is executable only with admitted provider idempotency; send-and-wait excluded; attachments deferred |
| 4 | Cloudinary asset metadata, admin search, and signed delivery transforms | named signing/URL transform and cursor plan | upload-by-URL excluded; multipart upload waits for Phase 1 inline-byte ABI |
| 5 | Mercado Pago report list; inventory payment-link creation | generic HTTP plus pagination | payment-link creation needs admitted provider idempotency; report download/SFTP deferred |
| 6 | Apify inspect run and dataset items; inventory run/webhook mutation operations | bounded processor, pagination, explicit webhook operations | mutations need admitted provider idempotency; no workflow wait node or unbounded poll loop |
| 7 | CloudConvert job/result inventory | bounded multi-step processor | job creation needs admitted provider idempotency and begins only after binary/continuation policy |
| Parallel | Donat-owned fixed-origin OAuth2 REST example from a permissive provider schema | common auth plus generic HTTP | access token is pre-provisioned/non-refreshing or client-credentials-per-activity; no generic HTTP Request behavior |

Slack, HubSpot, Google Sheets, Gmail, and Drive may follow from permissive
provider artifacts or independently authored provider-doc behavior. The
inspected n8n built-ins remain SUL behavior-only. Breadth never justifies
copying a built-in description or implementing n8n's workflow model.
Any cohort side effect that lacks provider idempotency remains inventory-only;
cohort membership is not executable admission.

## 13. Prerequisites and implementation decomposition

Spec 005 is proposed and its process runtime, V6 journal, shared value
contract, descriptor publication, and worker do not exist in the current
tree. A connector-factory task must not invent a parallel form of any of those
contracts. Because Section 7 places that shared contract in
`donat-value-contract` rather than the `donat-ir` owner accepted by ADR 009,
an ADR 009 update or accepted superseding ADR is a hard prerequisite to the
first implementation slice.

### 13.1 Hard prerequisite matrix

| Spec 007 capability | May land before the process runtime? | Exact prerequisite | Boundary until the prerequisite is green |
| --- | --- | --- | --- |
| shared canonical value/type contract | yes | accepted ADR ownership update, then Spec 005 Task 1 implemented in `donat-value-contract` with `donat-ir` re-export and `value_type_language_is_closed_and_canonical` | no parallel value type in catalog, ABI, or IR |
| neutral connector ABI ID/envelope foundation | yes | shared value contract above | IDs, envelopes, and host traits only; no catalog descriptors, processor implementation, or registry |
| normalized catalog model, source records, canonical hashes, and checked-in generated-table shape | yes | shared value contract plus compiling ABI ID/envelope foundation | model/tests only; all connector IDs import the exact ABI types; no acquisition, generation, or executable operation yet |
| hostile acquisition, licensing/notices, and update inventory | yes | compiling `donat-connector-catalog` source-record model | development tooling only; no donor source enters a runtime dependency |
| deterministic checked-in Rust generation | yes | compiling catalog model and manifest validation | codegen uses checked-in inputs only and cannot acquire sources |
| static catalog, read-only credential capability, fixed-origin transport, error mapping, and a direct server-side SerpAPI harness | yes | value contract, catalog, and process-independent ABI/runtime slices | no Process descriptor or public execution route |
| public connector operation/inbound descriptors | yes, after the named contract | Spec 005 Task 2, updated to the Section 5.3 per-step effect/retention model, and `connector_descriptor_is_typed_and_non_secret` | registry remains server-internal |
| process connector source binding and candidate compilation | no | Spec 005 Task 3 plus `process_connector_instance_has_one_source` | no process YAML may reference a factory operation |
| durable connector activity invocation and committed intent | no | Spec 005 Sections 7 and 9, including its V6 journal/migrations, worker, per-step `first_provider_attempt_at`, and `process_activity_does_not_hold_tx` | tests may call the registry directly only |
| idempotency-window compilation and runtime refusal | no | Spec 005 supplies finite upper bounds for every Section 5.3 horizon component plus the per-step timestamp/deadline contract | a provider mutation stays inventory-only/non-executable; no missing component gets an assumed safe value |
| retry, lease takeover, capacity, rate, and serialization behavior | no | Spec 005 Section 9 and `process_lease_takeover_is_safe`, `process_activity_capacity_is_global`, `process_capacity_bucket_serializes_two_claimers`, and the Section 14 retention-boundary/late-takeover proofs | no factory-local lease, reservation, or first-attempt table |
| generic two-stage webhook dispatch | yes | static event descriptor plus authenticator/codec/normalizer ABI | preserve every current route outcome, including empty-body `503` after successful verification |
| durable webhook 2xx acknowledgement, audit, dedupe, correlation, and process start | no | Spec 005 Section 10 and `process_inbound_audit_is_split`, `process_accepted_delivery_links_instance_history`, and `process_invalid_signature_is_audit_only` | no factory persistence; a verified event remains unacknowledged with `503` |
| pure poll processor ABI and donor-local tests | yes, with an admitted donor | processor ABI and typed checkpoint value | no scheduler or durable checkpoint |
| poll scheduling/checkpoint persistence | no | a future explicit Spec 005 source-local checkpoint schema, transaction, restart, and DB-clock contract; Spec 005 currently has none | do not infer a persistence model from this specification |
| revision fingerprint pinning, retirement, and reload | no | Spec 005 Section 5 and `process_live_connector_rebind_is_rejected` plus `process_retired_revision_reloads_and_completes` | catalog fingerprints are inspectable only, not process lifecycle state |
| bounded inline bytes and multipart | no | `donat-value-contract` implements the Section 9 bytes value and exact canonical-size vectors, then Spec 005 accepts that shared value in descriptors/journals | JSON/form operations only |
| rolling-binary and two-worker/takeover proofs | no | the corresponding Spec 005 runtime row above and `process_revision_runtime_abi_is_fenced` where ABI compatibility is involved | single-process direct registry tests only |

The provider-observation proof for durable invocation is stronger than an
in-process sequence assertion: the provider stub blocks while a separate
Postgres connection observes the committed activity job and any applicable
capacity reservation. Only then may it accept the request. This proof belongs
to the Spec 005-gated activity integration, not the process-independent
factory.

### 13.2 Process-independent factory phases

All paths below are current repository paths unless marked **Create**. These
phases may land without the process migrations/worker. Each row starts from a
green predecessor, adds its own failing test, and must compile/test as a
standalone slice before the next row begins:

| Order | Files | Deliverable and RED gate |
| ---: | --- | --- |
| 0 | update/supersede ADR 009; **Create** `crates/value-contract/`; modify `crates/ir/` to re-export | single `no_std + alloc` type/value owner, canonical sizing, and Spec 005 Task 1 tests; `donat-ir` retains no duplicate |
| 1 | **Create** `crates/connector-abi/` and its foundation policy/tests | local-only no-OS canonical connector/operation/step/processor-family IDs, bounded envelopes, and host traits over the value contract; no processor implementation |
| 2 | **Create** `crates/connector-catalog/`, `connector-catalog/sources/records/`, and `connector-catalog/manifests/` | catalog-owned strict source record and neutral normalized IR importing exact ABI IDs, canonical hashes, per-step effect validation, and normalized-descriptor-to-`ConnectorIo` ID identity proof |
| 3a | **Create** `crates/connector-acquire/`; modify workspace `Cargo.toml` | sibling tool depending only on catalog for hostile HTTPS/archive acquisition; synthetic extraction/license/dependency tests |
| 3b | **Create** `crates/connector-codegen/` and `crates/connector-catalog/src/generated/` | sibling tool depending only on catalog for deterministic checked-in Rust, `generate --check`, and actual-generated-entry-to-ABI ID identity proof; no acquisition dependency, `build.rs`, or Cargo-time generation |
| 4 | **Create** `crates/connector-processors/` and complete the boundary checker | local-only no-OS processor implementation closure, sealed private registry, private-lookup-to-ABI ID identity proof, opaque host capabilities, and independent Cloudinary-shaped proof |
| 5 | modify `crates/metadata/src/types.rs` and loader/type fixtures; modify `crates/server/src/state.rs`; **Create** `crates/server/src/connectors/credentials.rs` | source/credential instance validation, per-use read-only resolution, capabilities, and redaction |
| 6 | modify `crates/server/src/connectors/http.rs`; **Create** focused transport/executor tests | sole fixed-origin `ConnectorIo`, server-owned codecs/crypto/control, typed JSON/query/form encoding, complete errors, bounds, then bounded pagination |
| 7 | modify `crates/server/src/connectors/mod.rs`; **Create** `crates/server/src/connectors/catalog.rs` | immutable generated catalog and registry dispatch through processor-crate lookup with no runtime registration/discovery |
| 8 | admit SerpAPI records/manifests/notices; extend server tests | first Tier A exact request/result/error/fingerprint proof through a Donat-owned local provider stub; no public execution route |
| 9 | modify Stripe connector/tests | compiled-step processor migration without moving transport, codec, crypto, clock, UUID/text/time, or credentials into the processor crate |
| 10 | generalize two-stage webhook authentication/codec/normalization and Stripe adapter/tests | bounded raw authentication before server parsing, existing route matrix, and verified-event `503` unchanged |
| 11 | add a selected real donor to pure pagination/auth/verifier/poll ABI as needed | processor-specific tests only; no process scheduling or ingress persistence |
| 12 | extend sibling acquisition/codegen update commands | exact-version re-admission and semantic/provenance diff; never automatic admission |

The first derivative port also creates root `THIRD_PARTY_NOTICES.md` and
updates `knowledgebase/declarative-saas/reference-porting-register.md` in the
same reviewed change. The catalog generator reads only checked-in source
records and manifests and writes only checked-in Rust. There is no
`OUT_DIR` artifact and no `crates/server/build.rs`.

### 13.3 Process-dependent integration phases

After the exact matrix rows are green, separately reviewable tasks may:

1. publish typed connector descriptors and bind them to the process source;
2. compile every provider side-effect step against its complete bounded send
   horizon and reject equality-plus-one-millisecond policies;
3. persist per-step `first_provider_attempt_at`, derive the stable step key,
   and dispatch only committed jobs while enforcing source-local capacity,
   retry, takeover, and the usable idempotency deadline;
4. replace the verified-webhook `503` only through the Spec 005-owned inbound
   transaction and its separately specified exact success response;
5. add polling persistence only after Spec 005 defines its missing checkpoint
   schema/transaction/restart contract;
6. pin catalog dependencies in revisions and retain live-retired operations;
7. add inline bytes only after the shared value contract owns them; and
8. run corresponding two-worker and rolling-binary proofs.

Each behavior slice starts with its Donat-owned failing crate/native
conformance test, lands as a separately reviewable commit, and retains old
operation versions needed by live Process revisions. `donat-schema` never
depends on `donat-server`; descriptor publication preserves ADR 009's
two-stage candidate build.

## 14. TDD acceptance matrix

No Donat-derived fixture can be copied to fill this matrix because this is a
new non-Hasura surface. Tests use independently authored records, hostile
archives, manifests, provider stubs, DNS/peer doubles, token endpoints,
webhook bytes, and payloads. Tests never call a live provider API.

| ID | Level | Required proof |
| --- | --- | --- |
| `value_contract_has_one_owner` | value/IR compile | the no-std value crate owns types/canonical sizing, `donat-ir` re-exports them, and catalog/ABI compile against the same type identity without a conversion copy |
| `catalog_descriptor_ids_match_connector_io` | phase 2 catalog compile | normalized catalog descriptors pass their exact ABI-owned IDs directly to `ConnectorIo`, with no string, wrapper, parse, serialization, or `From`/`Into` conversion copy |
| `generated_catalog_ids_match_abi` | phase 3b post-codegen catalog compile | after deterministic codegen creates the actual checked-in entries, every generated connector/operation/step/processor-family/credential/capability ID assigns directly to its ABI-owned type |
| `processor_lookup_ids_match_abi` | phase 4 processor compile | private static-table lookup accepts the exact generated ABI-owned processor-family IDs without a conversion copy |
| `source_record_requires_exact_artifacts` | catalog/acquisition unit | catalog-owned records reject missing or mismatched exact version, integrity, repository/tree/license/file hash, provenance mapping, entrypoint, closed dependency/embedded-material decision, reviewer, destination, RED test, or notice; unknown fields fail at every nesting level |
| `hostile_archive_is_never_trusted` | acquisition integration | HTTPS/host/three-redirect policy, hash-before-extract, every archive size/count/depth ceiling, normalized-path collisions, links/special/sparse/unknown entries, no-follow replacement, cleanup, and an unexecuted package-script sentinel are pinned |
| `license_and_dependency_disposition_is_closed` | admission unit | only the six Section 3.1 SPDX choices and an explicit allowed dual-license selection pass; every dependency has one closed disposition |
| `sul_source_cannot_generate_artifacts` | policy integration | every donor marks `n8n-workflow` `TypeOnlyReplaced`; SUL bytes are absent from parser input, manifests, generated Rust, fixtures, Cargo metadata, and release artifacts |
| `imperative_ast_emits_work_item_not_rust` | codegen snapshot | function-valued routing, `execute`, `poll`, webhook code, or an ambiguous expression produces a processor/unsupported inventory finding, never guessed behavior |
| `canonical_json_and_hash_vectors_match` | catalog unit | an implementation-independent helper produces every exact Section 5.1 vector; duplicate/unknown/noncanonical input rejects |
| `generated_catalog_is_checked_in_and_deterministic` | codegen/CI | `generate --check` reproduces every path/byte/digest in two clean worktrees; deletion, drift, extra output, or manifest/source-record mismatch fails |
| `serpapi_exact_source_compiles` | codegen insta + local HTTP | admitted `0.1.10` record produces the reviewed manifest/generated Rust and fixed prepared request; snapshots are individually reviewed |
| `operation_effect_is_closed` | catalog/compiler | headerless `ReadOnly` and per-side-effect-step `ProviderIdempotent` binding/scope/retention/margin compile; missing/duplicate/unproven step entries and non-idempotent side effects remain inventory-only |
| `provider_idempotency_horizon_is_bounded` | Spec 005-gated process compiler | a complete policy whose maximum send horizon equals the usable window compiles; the same policy one millisecond over rejects, and any missing/unbounded schedule/capacity/start-to-close/lease/takeover/backoff component rejects |
| `provider_idempotency_steps_are_independent` | catalog/process compiler | a multi-step mutation derives different stable keys and independently validates the evidence, scope, retention, margin, and horizon of every side-effect step |
| `late_takeover_exhausts_idempotency_window` | Spec 005-gated two binaries | the persisted per-step first-attempt timestamp and stable key survive takeover; a database clock one millisecond after the usable deadline performs no provider I/O and returns permanent `connector_idempotency_window_exhausted` |
| `credential_capability_is_read_only_and_per_use` | metadata/server | only SecretRef-backed Phase 1 fields pass; the registry retains a binding, resolves each use, exposes only compiled primitives, and cannot enumerate/write/refresh/CAS/delete secrets |
| `client_credentials_is_one_activity_step` | server/local token stub | token acquisition consumes the same call/byte/deadline budget, is not refreshed or persisted, and its opaque capability is dropped after the logical attempt |
| `processor_boundary_is_mechanical` | ABI/CI | value/ABI/processor crates compile without default features for the pinned no-OS target; locked checks cover normal/build/proc-macro/optional/target edges, feature unification, build scripts, native links, include/path/env indirection, unsafe/FFI, generated/donor source, and any nonlocal runtime dependency |
| `processor_registry_is_closed` | processor/server compile + source policy | only private processor-crate constructors/table can create handles for admitted IDs; server-side implementation or registration and dynamic fallback are impossible/rejected |
| `processor_calls_only_compiled_steps` | processor/server | every auxiliary ABI is object-safe and bounded; a foreign step, URL-shaped continuation, undeclared primitive, excess budget, deadline, or cancellation fails before further I/O |
| `webhook_authentication_precedes_codec` | processor/server/endpoint | invalid authentication never invokes the server codec; valid raw authentication creates one opaque token, then server decoding, then pure typed normalization, while the public route matrix remains exact |
| `fixed_origin_is_unescapable` | server integration | input, processor bindings, pagination, provider body, redirects, DNS rebinding, proxy environment, and peer mismatch cannot change the compiled destination |
| `runtime_limits_have_exact_failures` | server/local-provider matrix | every header/URL/path/query/JSON/body/aggregate/item/page/call/output/inline-byte ceiling reaches the exact Section 8 class/code without partial output or leakage; `Retry-After` clamps at 86,400 seconds |
| `error_map_is_closed_and_redacted` | server/local-provider matrix | every transport/status/provider-code/malformed-success case reaches one existing class and Donat-owned safe message with no secret/raw body leakage |
| `pagination_is_bounded` | server integration | cursor/offset/page/link plans share the logical-attempt budget and stop at calls/pages/items/aggregate bytes/deadline; cross-origin `next` is rejected |
| `public_surfaces_cannot_execute_connectors` | server/native conformance | GraphQL introspection, REST endpoint compilation, MCP tool listing/calls, and admin/unknown routes expose no connector execution surface; `POST /v1/connectors/{instance}/execute` returns HTTP `404` with an empty body |
| `commands_cannot_plan_connector_io` | command compiler/IR | every connector-effect syntax or descriptor is rejected and no command plan/IR variant can carry connector I/O |
| `webhook_route_preserves_phase1_boundary` | endpoint/native conformance | `POST /v1/connectors/{instance}/webhooks` returns empty `404` for unknown/no verifier, empty `413` for an oversized body, empty `400` for malformed/invalid verification, and empty `503` after valid verification |
| `provider_observes_only_committed_activity` | Spec 005-gated Postgres/provider integration | the provider stub accepts no request until a separate connection sees the committed job and applicable capacity reservation |
| `read_only_and_provider_idempotent_takeover_are_safe` | Spec 005-gated two binaries | headerless read-only takeover and within-window stable per-step-key mutation takeover create one durable transition with stale-worker audit |
| `durable_webhook_ack_is_source_local` | Spec 005-gated webhook integration | only the Spec 005 transaction may change valid verification from `503`; exact success status/body must be specified there before a fixture lands, after audit/dedupe/correlation commit |
| `poll_checkpoint_persistence_matches_process_contract` | future Spec 005-gated process integration | only after the missing explicit checkpoint contract exists, restart/DB-clock/source-local transaction tests prove persistence; the processor has no static workflow data |
| `inline_binary_is_bounded` | value-contract/server/native conformance | exact Section 9 JCS/base64url vectors prove 131,072/131,073 decoded-byte acceptance/rejection and 262,144/262,145 complete-value acceptance/rejection; 17 values, paths, URLs, and object references fail before I/O |
| `revision_fingerprint_is_complete` | Spec 005-gated compiler/reconcile | origin, every step's binding/scope/retention/margin/horizon, operation/processor/credential versions, schemas, bounds, pagination/error plan, source records, and configuration change the pinned dependency; live-retired/reload tests pass |
| `upgrade_diff_is_semantic` | codegen snapshot | upgrade reports record/integrity/license, operation/type/effect, scopes, origin, request, pagination, errors, bounds, processors, tests, and notices; no retag auto-accept |
| `workspace_build_is_offline` | clean network-disabled CI | `cargo build --workspace --release --offline --locked` succeeds without network; expected acquisition/codegen source and artifacts are separately inventoried and allowed in this proof |
| `runtime_package_is_source_free` | runtime package/image CI | a separate locked offline `donat-server --bin donat` release build has a runtime-package dependency tree/SBOM; only that binary, package/image, and runtime closure are scanned and contain no acquisition/codegen/donor/SUL, Node, JS, WASM, npm, n8n, dynamic plugin, or unapproved processor runtime material |

### 14.1 RED/GREEN sequence

Representative focused commands for the implementation:

```bash
# Shared value owner, then ABI ID/envelope foundation, then catalog.
cargo test -p donat-value-contract --no-default-features
cargo test -p donat-ir value_contract
cargo check -p donat-value-contract --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo test -p donat-connector-abi --no-default-features
cargo check -p donat-connector-abi --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo test -p donat-connector-catalog
cargo test -p donat-connector-catalog catalog_descriptor_ids_match_connector_io

# Acquisition and codegen are independent siblings over that catalog.
cargo test -p donat-connector-acquire --test source_admission
cargo test -p donat-connector-acquire --test hostile_archives
cargo test -p donat-connector-codegen --test deterministic_catalog
cargo test -p donat-connector-codegen --test serpapi_compile
cargo insta test -p donat-connector-codegen
cargo insta review
cargo test -p donat-connector-catalog generated_catalog_ids_match_abi

# The later local-only processor implementation is checked on a no-OS target
# before any real processor is admitted.
cargo check -p donat-connector-processors --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
cargo tree -p donat-connector-processors --target all \
  --edges normal,build,no-dev --no-default-features --offline --locked

cargo test -p donat-connector-processors --no-default-features
cargo test -p donat-connector-processors processor_lookup_ids_match_abi

# Metadata, credential, egress, pagination, error, and adapter slices.
cargo test -p donat-metadata connectors
cargo test -p donat-server --test connectors_factory
cargo test -p donat-server --test connectors_http
cargo test -p donat-server --test connectors_stripe
cargo test -p donat-server --test connector_webhook

# Native ground truth after rebuilding the binary used by the harness. Process-
# gated targets run only after their Section 13 prerequisite is implemented.
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test connectors

# A clean CI image has the locked Rust cache/toolchain prepared first; the
# whole workspace, including expected development tools, must build offline.
podman run --rm --network=none <pinned-build-image> \
  cargo build --workspace --release --offline --locked

# Runtime absence is a separate package proof, never a workspace-artifact
# absence assertion.
cargo build -p donat-server --bin donat --release --offline --locked
cargo tree -p donat-server --target all --edges normal,no-dev \
  --offline --locked

# Required regression proof after each engine-behavior slice is green.
cargo test -p donat-conformance
```

`cargo insta review` is interactive evidence review, not blanket acceptance.
The implementation report records every changed snapshot and why it changed.
CI compares generated path/byte/digest output across two clean worktrees,
checks locked metadata plus the complete boundary closure/feature allowlist,
and inventories the expected development tools in workspace source/artifacts.
A repository-pinned SBOM generator then emits the `donat-server` runtime
package closure. The absence scan is restricted to that dependency tree/SBOM,
the packaged `donat` binary, production package/image, and runtime filesystem;
it rejects acquisition/codegen/donor/SUL, Node, JavaScript, WASM, npm, n8n,
dynamic-plugin, and unapproved processor runtime material. It does not require
the workspace source or workspace release artifact directory to omit its
development tooling.
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
- credential fields, auth class, OAuth scopes, non-refreshing token acquisition
  behavior, and redactions;
- operation/event IDs and versions, input/output hashes, request steps, and
  idempotency;
- pagination/error/trigger plans and every runtime bound;
- processor/authenticator/codec/normalizer IDs and Rust diffs;
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
- interactive tenant OAuth consent/onboarding, refresh-token
  persistence/rotation, distributed refresh, writable external resolvers, a
  built-in credential vault, tenant credential CRUD, or consent callback
  routing;
- filesystem or shell access, arbitrary environment access, browser
  automation, database-driver nodes, local service nodes, or long-lived socket
  triggers;
- unbounded streaming, resumable/multi-gigabyte transfers, external object
  storage, or URL-shaped binary references;
- complete provider catalog parity, copied upstream prose, or live-provider
  tests;
- claiming exactly-once external delivery. Once Spec 005 integration exists,
  durable activities remain at-least-once; Phase 1 executes only headerless
  read-only calls or side effects protected by the compiled provider
  idempotency binding.

## 17. Acceptance and estimated complexity

The factory is accepted only when:

- the exact cohort records pass immutable admission and the first derivative
  port carries per-file provenance and notices;
- SerpAPI proves the generic Tier A compiler without executing JavaScript;
- the existing Stripe slice proves the compiled-step processor ABI without a
  behavior regression;
- credentials resolve per use only as read-only capabilities, client
  credentials tokens are per-activity and non-refreshing, pre-provisioned
  access tokens are never refreshed or persisted, and no interactive/admin
  surface exists;
- fixed-origin egress, runtime budgets, pagination, errors, triggers, and
  inline binary behavior pass the matrix above;
- clean production builds and runtime operate offline with no donor
  source/package/runtime or acquisition/codegen dependency;
- every process-dependent behavior remains absent until its exact Section 13
  Spec 005 prerequisite and proof are green;
- new native connector conformance is exact and the full conformance crate is
  green after rebuilding `donat`.

Estimated complexity: XL. This is a new acquisition/admission subsystem,
offline checked-in code generator, cross-cutting runtime ABI, metadata
evolution, credential boundary, and native conformance surface. The ordered
slices keep the legal gate, generic executor, processor ABI, triggers, and
binary work independently reviewable.

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
