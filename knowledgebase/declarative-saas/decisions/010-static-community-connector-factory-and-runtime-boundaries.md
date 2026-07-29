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

1. the shared `donat-value-contract` owner;
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

Credentials are split into compiled `CredentialSpec` values and
source-bound deploy-time credential instances containing only read-only
`SecretRef` fields. The Phase 1 environment resolver probes availability at
startup, resolves again per use, and returns an opaque capability limited to
the selected compiled auth action. It has no enumerate, write, refresh,
compare-and-swap, delete, or administration method. Interactive OAuth,
refresh-token persistence, credential CRUD, and tenant onboarding require a
separate decision.

The generated catalog and every derivative artifact are repository-visible
and deterministic. Code generation reads only checked-in source records and
normalized manifests, writes checked-in Rust, and supports an offline
`generate --check`. Ordinary Cargo builds never acquire, inspect, or execute
donor material.

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
`Code`, `Wait`, loops, items/paired items, subworkflows, send-and-wait, or
other logical/workflow nodes. Rules, Commands, and Processes continue to own
business decisions, database work, branching, retries, waits, and
orchestration.

Process-independent factory slices may use direct internal server harnesses
only. No public GraphQL, REST, MCP, or administrative execution endpoint is
introduced. Provider mutation dispatch, activity persistence, lease/takeover
behavior, capacity reservations, per-step first-attempt timestamps, durable
webhook acknowledgement, polling checkpoint persistence, and revision
retirement are deferred until their exact Spec 005 source-local prerequisites
and tests are green. The temporary verified-webhook response remains an empty
`503` after successful verification.

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
