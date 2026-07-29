---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
  - "[[007-community-connector-factory]]"
---

# Community connectors use a licensed static factory and a compiled-step runtime

## Context

The first connector implementation established useful safety properties but
does not scale as a factory. `donat-server` currently contains a hand-written
HTTP/Stripe registry, module-specific credential fields, duplicated transport
code, and a broad direct execution function. It has no neutral connector
catalog, checked-in generator, reusable credential specification, pagination
plan, or mechanically constrained processor boundary.

Provider integrations are available from permissively licensed community
packages and provider schemas. n8n's built-in node package is not such a
source: at the reviewed revision it is distributed under the Sustainable Use
License. Executing or translating arbitrary node code would also pull in
workflow items, expressions, Node.js, ambient credentials, and broad operating
system capabilities that conflict with Donat's one-binary, deploy-time model.

The factory must fit the durable-process decisions without waiting for the
process journal to exist. Type contracts, admission tooling, generated static
catalogs, bounded transport, and direct server-side contract tests can land
first. Activity persistence, retries, lease takeover, capacity reservations,
idempotency-window enforcement, polling checkpoints, and webhook
acknowledgement remain source-local Process responsibilities.

## Decision

Build community connectors as a repository-reviewed, development-time source
pipeline whose only production output is checked-in Rust data and reviewed
native processor code. The production artifact remains one Rust serving
binary plus Postgres. It loads no npm package, Node.js or JavaScript runtime,
WASM module, shared library, downloaded plugin, package URL, donor source, or
generated metadata at runtime. Development acquisition and code generation
may be separate Rust tools, but they are never server dependencies, Cargo
build scripts, or release-image components.

The implementation order is:

1. the shared `donat-value-contract` owner from Spec 005 Task 1;
2. neutral connector IDs, envelopes, and host traits in
   `donat-connector-abi`;
3. source records, normalized IR, hashes, and checked-in entries in
   `donat-connector-catalog`;
4. independent `donat-connector-acquire` and
   `donat-connector-codegen` development tools; and
5. the sealed `donat-connector-processors` implementation boundary.

This decision supersedes ADR 009 only where that decision assigned the shared
value/type contract to `donat-ir`: `donat-value-contract` is now the owner and
`donat-ir` re-exports it. ADR 009's source-local compilation, journal,
transaction, and process-runtime decisions remain unchanged.

Spec 005 Task 1 and the connector factory's value-contract prerequisite are
one implementation unit and one commit. The connector plan may record and
verify that shared commit, but it must never create or implement a second
value crate. The shared type owns inert `InlineBytes(BoundedInlineBytes)` and
its canonical-size accounting immediately; external JSON encoding, multipart
transport, connector descriptor admission, and process persistence remain
disabled until the separate binary gate is accepted.

Spec 005's canonical task text is authoritative and must first declare the
shared superset. That contract includes the public `CanonicalDecimal` type
with a private tuple field, its sole checked `try_new` constructor and
`as_str` accessor, and the exact minimal fixed-point grammar
`0|-?(?:[1-9][0-9]*(?:\.[0-9]*[1-9])?|0\.[0-9]*[1-9])`. It accepts
`-12.5`, `0.01`, and `10`, rejects exponent, redundant fractional zero,
leading-zero, negative-zero, zero-fraction, trailing-point, whitespace, plus,
and non-finite spellings, and makes `canonical_size` count only the checked
`as_str` spelling. The identifier grammar has no implicit reserved-prefix
exception, so `__bad` remains valid unless a future explicit metadata rule
reserves it. The same contract includes private bounded media-type and
filename newtypes, `BoundedInlineBytes { bytes, media_type, file_name }`, the
four-argument constructor, accessors, and exact size/count vectors. If the
authoritative task differs, execution stops for cross-plan alignment; the
connector plan follows and re-exports that same shared unit rather than
silently redefining it or appending a second commit.

Dependency arrows below mean “depends on”:

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

`donat-value-contract` is the single owner of the closed SQL-free type/value
language, bounded canonical values, deterministic object ordering, canonical
size accounting, and inline bytes. `donat-ir` re-exports those types. Catalog
descriptors import ABI-owned connector, operation, step, processor-family,
credential, and capability IDs directly; they do not create string or wrapper
copies. Acquisition and code generation are siblings over the catalog.
Processors do not depend on catalog, acquisition, code generation, server, or
third-party runtime crates.

Every ABI identity has one const-constructible representation:
`InlineId { len: u8, bytes: [u8; 96] }`, wrapped by transparent typed IDs.
The accepted ASCII grammar is
`[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?`; length is `1..=96`. A const literal
constructor and the runtime parser call the same byte validator. The storage
and every typed wrapper are `Copy`, so checked-in generated statics,
`ConnectorIo`, and the private processor lookup pass the exact ABI type by
value without a `String`, clone, parse, serialization, or conversion bridge.

The value, ABI, and processor crates are unpublished `#![no_std] + alloc`
crates with empty default features, no `std` feature, build script, unsafe
code, procedural macro, native dependency, or third-party runtime dependency
in Phase 1. The processor crate owns its sealed traits, implementations,
private constructors, and private static lookup table. The server owns every
implementation of host I/O and control traits: transport, DNS/TLS/peer
validation, codecs, clocks, cancellation, credentials, cryptography, UUID and
time conversion, budgets, and compiled-step lookup. This is a mechanically
checked native-code API and dependency policy, not a sandbox; processor CPU
loops, panics, excessive allocation, and misuse of an overbroad admitted step
remain trusted review risks.

Connector operations are closed to headerless `ReadOnly` or
`ProviderIdempotent`. The latter records one fixed header/body binding,
provider scope, conservative minimum retention, and positive clock margin for
every side-effecting compiled step. A processor can request only a
`CompiledStepId` from its current operation. It cannot construct a URL,
method, header name, credential, retry, process transition, database write,
or raw HTTP request. Provider side effects without complete immutable
idempotency evidence remain inventory-only.

Catalog contract facts have a closed origin:
`ProviderEvidence { source_record_id, fact_id }` or
`DonatPolicy { policy_id, value }`. Provider facts resolve only to exact
locations in an immutable provider-artifact record; Donat-owned
normalization/safety values resolve only to reviewed policy IDs. Neither
variant can satisfy a requirement for the other. Normalized fact/policy
values enter semantic hashing, while provider record/artifact/fact identities
and locations plus Donat policy IDs enter provenance hashing.

Credentials are split into compiled `CredentialSpec` values and
source-bound deploy-time credential instances containing only read-only
`SecretRef` fields. The Phase 1 environment resolver probes availability at
startup, resolves again per use, and returns an opaque capability limited to
the selected compiled auth action. It has no enumerate, write, refresh,
compare-and-swap, delete, or administration method. Interactive OAuth,
refresh-token persistence, credential CRUD, and tenant onboarding require a
separate decision.

Deploy-time connector metadata retains the existing
`name`/`module`/`config`/`operations` separation. `module` selects the
compiled catalog connector; endpoint and credential identities remain under
`config`; enabled operations remain explicit. A future `source` field denotes
the distinct Spec 005 database/process source binding and never aliases a
catalog module ID.

The generated catalog and every derivative artifact are repository-visible
and deterministic. Code generation reads only checked-in source records and
normalized manifests, writes checked-in Rust, and supports an offline
`generate --check`. Generated Rust and its digest are generator-owned and are
never hand-edited: each manifest change renders to a temporary directory,
receives diff review, updates checked-in output through the generator, and
then passes `generate --check`. Ordinary Cargo builds never acquire, inspect,
or execute donor material.

The catalog crate owns the complete strict normalized model before any server
consumer: `CredentialSpec` and its closed auth plan, fixed origins and
compiled steps, `OperationSpec`, `ErrorMap`, pagination and finite bounds,
`TriggerSpec`, and typed provenance references. The const-safe generated
table carries credentials, operations, triggers, matching source-record
IDs/hashes, license/notice identities, semantic/provenance hashes, and every
exact ABI-owned ID. Credential validation and webhook lookup borrow those
generated catalog types directly; the server defines no replacement
descriptor.

Source admission is exact-version and fail-closed. Phase 1 accepts only
`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, or `0BSD`; an
allowed dual-license branch must be selected explicitly. Before a derivative
artifact lands, the reviewed record and
[[reference-porting-register]] identify the immutable source/revision,
artifact and per-file hashes, license evidence and notice, dependency and
embedded-material disposition, destination, Donat-owned failing test, and
reviewer. The first derivative port creates
`THIRD_PARTY_NOTICES.md`. Repository review is the admission authority; Phase
1 does not claim a cryptographic signer or trust-root protocol.

Source records are closed variants for exact npm packages, provider-contract
evidence, and Donat-owned source. They retain structured artifact integrity,
repository/tarball mapping, compatibility tier, inventory/admission state,
notices, proposed manifest and destination paths, and closed dependency and
embedded-material dispositions. A manifest can cite only the matching record;
an unrelated Donat-owned record cannot supply provenance.

An exact npm subject also retains closed verified-present,
verified-absent, or rejected decisions for registry signatures and signed
provenance, optional distinct tag/provenance commits, the reviewed maintainer
set, and a repository-owner consistency/mismatch/rejection decision. These
are retained provenance facts, not a cryptographic Donat admission root.
Every mismatch fails closed.

Acquisition uses disjoint networked schemas. Exact npm review requires
artifact URL, expected SHA-512 SRI, repository URL, and full commit.
Provider-only review requires exactly one immutable repository identity or
one versioned artifact plus expected SHA-256. Record-derived reacquisition
accepts no locator/hash override and makes accepted evidence reproducible in
a clean worktree; its closed reacquisition plan must match the source-subject
variant, while Donat-owned records are explicitly no-network. All forms share
the checked-in `registry.npmjs.org`/`github.com`/`codeload.github.com`
allowlist;
`docs.stripe.com` remains discovery-only and is represented by synthetic
rejection bytes unless a separate source-policy review admits that host.

The `n8n-workflow` peer dependency is always `TypeOnlyReplaced`: its bytes are
not compiler input, linked code, generated material, fixtures, or shipped
content. `n8n-nodes-base`, its tests, fixtures, credential classes,
descriptions, and generated metadata remain behavior-only under the reviewed
Sustainable Use License unless a written grant expressly permits source
copying, Rust translation, derivative works, fixture use, generated-metadata
extraction, commercial hosting, and redistribution.

The product boundary is provider integration only: credential
specifications, typed fixed-origin operations, errors, bounded pagination,
provider webhooks/polling, bounded binary transport, and narrow protocol
processors. It adds no connector UI, arbitrary HTTP request, generic
connector execution route, runtime metadata mutation, admin role, or
permission bypass. It does not import or emulate n8n `If`, `Switch`, `Merge`,
`Code`, `Wait`, loops, items/paired items, subworkflows, AI nodes,
send-and-wait, or other logical/workflow nodes. Rules, Commands, and Processes
continue to own business decisions, database work, branching, retries, waits,
and orchestration.

Process-independent factory slices may use direct internal server harnesses
only. No public GraphQL, REST, MCP, or administrative execution endpoint is
introduced. Provider mutation dispatch, activity persistence, lease/takeover
behavior, capacity reservations, per-step first-attempt timestamps, durable
webhook acknowledgement, polling checkpoint persistence, and revision
retirement are deferred until their exact Spec 005 source-local prerequisites
and tests are green. The temporary verified-webhook response remains an empty
`503` after successful verification.

For the first SerpAPI slice, the pinned donor record is the authority for
`/search.json`. The immutable official provider repository uses `/search`
and supports only compatible method, base-origin, API-key, JSON, result, and
error facts; the `.json` suffix is never attributed to that provider record.
Exact-200, top-level-error rejection, missing-as-empty, and generic status
normalization remain typed Donat policy.

Stripe work is split into three independently committed tasks. A
processor-only proof may exercise a narrow processor against fake
`ConnectorIo` while leaving the current adapter and inventory entry intact. A
separate evidence task must accept an immutable provider record proving the
exact idempotency binding, scope, and conservative retention, while recording
the positive clock margin as Donat-owned policy. Only the final executable
migration creates an `OperationSpec`, routes through shared transport, or
deletes the old Stripe transport; it does not start if the evidence task
cannot pass.

### Construction authority for the safe connector ABI

The following ownership rules are part of this decision:

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

[[011-version-independent-rust-boundary-lexer]] governs the checker's Rust
lexical policy. It uses a delimiter-based identifier over-approximation,
preserves raw-identifier state, and recursively resolves grouped `use` trees;
it does not couple construction authority to Python's Unicode database or a
version-pinned XID table.

`TypedBindings` shares shape counters across all roots while depth remains per
root. All `u16` statuses remain accepted. These refinements preserve the
existing static native boundary: they add neither a sandbox nor a runtime
surface. Task 3 alone consumes `catalog_construction`; Task 8 alone consumes
`host_construction`; Task 6 extends the one checker and consumes response data
only through its accessors and static literals.

## Alternatives

| Option | Why Not |
| --- | --- |
| Execute community packages or n8n built-ins through Node.js | adds an unbounded runtime/plugin trust boundary, violates the serving artifact model, and does not resolve source licensing |
| Translate all n8n built-ins directly to Rust | the reviewed built-in package is SUL, and its workflow/runtime coupling is broader than a connector ABI |
| Keep adding hand-written variants in `donat-server` | duplicates transport and credential policy and cannot mechanically constrain processor dependencies |
| Generate into `OUT_DIR` or fetch during `build.rs` | hides derivative diffs/notices from review and makes ordinary builds network/source dependent |
| Let processors own reqwest, crypto, codecs, or raw credentials | makes fixed-origin and non-exfiltration claims unenforceable at the crate boundary |
| Require one idempotency header on every operation | invents meaningless headers for reads and cannot make a multi-step mutation safe |
| Add a factory-owned activity or webhook journal | duplicates the source-local durable-process contract and creates inconsistent retry/dedupe semantics |
| Treat verified status or an npm license field as admission | neither proves the exact tarball, repository mapping, embedded material, file licenses, or notice obligations |
| Recreate n8n logical nodes as connectors | collapses the provider-integration boundary into a second workflow engine |

## Consequences

The design adds several small crates, strict source records, checked-in
generated output, boundary CI, and human provenance/snapshot review. A provider
may remain inventory-only even when its API is useful because licensing,
idempotency, binary, or process-runtime evidence is incomplete.

In return, ordinary REST coverage can grow from renewable permissive sources
without shipping donor runtimes. Every production operation remains
fixed-origin, typed, bounded, statically registered, and attributable. The
processor escape surface is substantially reduced, existing HTTP/Stripe
behavior remains a migration oracle, and the connector factory can progress
independently without inventing any process-owned persistence or public
execution surface.
