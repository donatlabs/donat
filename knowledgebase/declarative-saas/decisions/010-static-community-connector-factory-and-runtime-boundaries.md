---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
---

# Connectors use a static compiled catalog and a compiled-step runtime

> Partially superseded by
> [[037-connectors-are-written-by-hand-against-provider-documentation]].
> The source-admission pipeline, the acquisition tool, the offline code
> generator, and the sealed processor crate this decision introduced are not
> built. Connector declarations are authored by hand. Everything below about
> the value contract, the connector ABI, the catalog model, credentials,
> deploy-time metadata, effect classes, and the runtime boundaries remains in
> force.

## Context

The first connector implementation established useful safety properties but
does not scale. `donat-server` contains a hand-written HTTP/Stripe registry,
module-specific credential fields, duplicated transport code, and a broad
direct execution function. It has no neutral connector catalog, reusable
credential specification, pagination plan, or constrained processor boundary.

Executing provider integrations as downloaded packages would pull in a package
runtime, ambient credentials, and broad operating system capabilities that
conflict with Donat's one-binary, deploy-time model.

The design must fit the durable-process decisions without waiting for the
process journal to exist. Type contracts, static catalogs, bounded transport,
and direct server-side contract tests can land first. Activity persistence,
retries, lease takeover, capacity reservations, idempotency-window enforcement,
polling checkpoints, and webhook acknowledgement remain source-local Process
responsibilities.

## Decision

The production artifact is one Rust serving binary plus Postgres. It loads no
package, script runtime, WASM module, shared library, downloaded plugin,
package URL, or foreign source at runtime.

The implementation order is:

1. the shared `donat-value-contract` owner from Spec 005 Task 1;
2. neutral connector IDs, envelopes, and host traits in
   `donat-connector-abi`;
3. the normalized model, catalog entries, and canonical hashes in
   `donat-connector-catalog`.

This decision supersedes ADR 009 only where that decision assigned the shared
value/type contract to `donat-ir`: `donat-value-contract` is now the owner and
`donat-ir` re-exports it. ADR 009's source-local compilation, journal,
transaction, and process-runtime decisions remain unchanged.

Spec 005 Task 1 and the connector value-contract prerequisite are one
implementation unit and one commit. The connector plan may record and verify
that shared commit, but it must never create or implement a second value crate.
The shared type owns inert `InlineBytes(BoundedInlineBytes)` and its
canonical-size accounting immediately; external JSON encoding, multipart
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

donat-server -> donat-ir
donat-server -> donat-connector-catalog
donat-server -> donat-connector-abi
```

`donat-value-contract` is the single owner of the closed SQL-free type/value
language, bounded canonical values, deterministic object ordering, canonical
size accounting, and inline bytes. `donat-ir` re-exports those types. Catalog
descriptors import ABI-owned connector, operation, step, processor-family,
credential, and capability IDs directly; they do not create string or wrapper
copies.

Every ABI identity has one const-constructible representation:
`InlineId { len: u8, bytes: [u8; 96] }`, wrapped by transparent typed IDs.
The accepted ASCII grammar is
`[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?`; length is `1..=96`. A const literal
constructor and the runtime parser call the same byte validator. The storage
and every typed wrapper are `Copy`, so checked-in statics, `ConnectorIo`, and
the private processor lookup pass the exact ABI type by value without a
`String`, clone, parse, serialization, or conversion bridge.

The value and ABI crates are unpublished `#![no_std] + alloc` crates with empty
default features, no `std` feature, build script, unsafe code, procedural
macro, native dependency, or third-party runtime dependency in Phase 1. The
server owns every implementation of host I/O and control traits: transport,
DNS/TLS/peer validation, codecs, clocks, cancellation, credentials,
cryptography, UUID and time conversion, budgets, and compiled-step lookup. This
is a native-code API and dependency policy, not a sandbox; processor CPU loops,
panics, excessive allocation, and misuse of an overbroad admitted step remain
trusted review risks.

Connector operations are closed to headerless `ReadOnly` or
`ProviderIdempotent`. The latter records one fixed header/body binding,
provider scope, conservative minimum retention, and positive clock margin for
every side-effecting compiled step. A processor can request only a
`CompiledStepId` from its current operation. It cannot construct a URL,
method, header name, credential, retry, process transition, database write,
or raw HTTP request. Provider side effects without complete idempotency
evidence remain inventory-only.

Catalog contract facts have a closed origin: provider evidence, which resolves
to an exact cited location in the provider's own published contract, or Donat
policy, which resolves to a reviewed policy ID. Neither variant can satisfy a
requirement for the other. Normalized fact/policy values enter semantic
hashing, while fact identities and locations plus Donat policy IDs enter
provenance hashing.
[[012-canonical-catalog-projections-and-persisted-header-capabilities]]
defines the exact stable use-site pairing: semantic material takes the
resolved value, provenance material takes that same use site's exact origin,
and neither side may be reconstructed or omitted.

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

The persistent calculation order and closed materials are fixed by
[[012-canonical-catalog-projections-and-persisted-header-capabilities]]:
resolved manifest and value-contract hashes, semantic hash, then provenance
hash. No material contains its own result. JCS object names use RFC 8785
UTF-16 ordering, and the catalog uses the decision's tagged `TypedValue`
adapter rather than raw JSON numbers or the lower value crate's separate
`canonical_size`.

The catalog crate owns the complete strict normalized model before any server
consumer: `CredentialSpec` and its closed auth plan, fixed origins and
compiled steps, `OperationSpec`, `ErrorMap`, pagination and finite bounds,
`TriggerSpec`, and typed provenance references. The const-safe checked-in table
carries credentials, operations, triggers, notice identities,
semantic/provenance hashes, and every exact ABI-owned ID. Credential validation
and webhook lookup borrow those catalog types directly; the server defines no
replacement descriptor.

`OperationSpec` is the complete self-contained behavioral snapshot from
[[012-canonical-catalog-projections-and-persisted-header-capabilities]], not a
summary. It retains connector and operation SemVer, runtime/value epochs,
recomputed input/output contract hashes, optional versioned credential,
resolved origin closure, ordered compiled steps and transforms, optional
operation processor ID plus `implementation_revision`, effect, pagination,
complete error map, capacity/rate/typed serialization defaults, all
step/operation bounds, exact resolved fact values, and persisted
selected-header capabilities. It contains no license, notice, review, or
fact-origin identity. Provenance and deployment identity instead live in the
separate `CatalogIdentityEnvelopeV1` persisted beside a pinned operation or
trigger. That envelope carries schema epochs, all referenced value-contract
hashes, semantic/provenance hashes, the complete behavioral snapshot identity,
and the exact non-secret `DeploymentMaterialV1` fingerprint. Reload compares
both behavioral snapshot and envelope field-for-field.
Processorless declarative operations have exactly one step; multiple steps
require the static operation processor reference with
`implementation_revision`. Connector, credential, operation, trigger, and event
versions are SemVer cores without Phase-1 prerelease/build metadata; every
schema/runtime/implementation epoch is `u32`, and processor-like references
always call it `implementation_revision`.

The product boundary is provider integration only: credential specifications,
typed fixed-origin operations, errors, bounded pagination, provider
webhooks/polling, bounded binary transport, and narrow protocol processors. It
adds no connector UI, arbitrary HTTP request, generic connector execution
route, runtime metadata mutation, admin role, or permission bypass. It does not
acquire a second workflow engine: Rules, Commands, and Processes continue to
own business decisions, database work, branching, retries, waits, and
orchestration.

Process-independent slices may use direct internal server harnesses only. No
public GraphQL, REST, MCP, or administrative execution endpoint is introduced.
Provider mutation dispatch, activity persistence, lease/takeover behavior,
capacity reservations, per-step first-attempt timestamps, durable webhook
acknowledgement, polling checkpoint persistence, and revision retirement are
deferred until their exact Spec 005 source-local prerequisites and tests are
green. The temporary verified-webhook response remains an empty `503` after
successful verification.

Stripe work is split into independently committed tasks. A processor-only
proof may exercise a narrow processor against a fake `ConnectorIo` while
leaving the current adapter intact. A separate evidence task must establish the
exact idempotency binding, scope, and conservative retention from the
provider's published contract, while recording the positive clock margin as
Donat-owned policy. Only the final executable migration creates an
`OperationSpec`, routes through shared transport, or deletes the old Stripe
transport; it does not start if the evidence task cannot pass.

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
catalog-stored CapabilityId allowlist and is callable only from
crates/server/src/connectors/.

Rust privacy cannot express friend crates, so the boundary is enforced by
scripts/check_connector_processor_boundary.py with deterministic producer and
test-path fixtures.
```

[[011-version-independent-rust-boundary-lexer]] governs the checker's Rust
lexical policy. It uses a delimiter-based identifier over-approximation,
preserves raw-identifier state, and recursively resolves grouped `use` trees;
it does not couple construction authority to Python's Unicode database or a
version-pinned XID table.

`TypedBindings` shares shape counters across all roots while depth remains per
root. All `u16` statuses remain accepted. These refinements preserve the
existing static native boundary: they add neither a sandbox nor a runtime
surface.

Each selected response-header name is canonicalized and its scoped 80-byte
`CapabilityId` derived and stored exactly once under
[[012-canonical-catalog-projections-and-persisted-header-capabilities]].
Error correlations retain the stored header/capability link and reject
missing, ambiguous, duplicate, or more-than-64 resolutions. Runtime code only
matches captured header names, stores values under the persisted IDs, and
passes the selected action's stored allowlist; it has no capability derivation
or caller-provided allowlist path.

## Alternatives

| Option | Why Not |
| --- | --- |
| Execute integration packages through a script runtime | adds an unbounded runtime/plugin trust boundary and violates the serving artifact model |
| Keep adding hand-written variants in `donat-server` | duplicates transport and credential policy and cannot constrain processor dependencies |
| Generate into `OUT_DIR` or fetch during `build.rs` | hides diffs and notices from review and makes ordinary builds network dependent |
| Let processors own reqwest, crypto, codecs, or raw credentials | makes fixed-origin and non-exfiltration claims unenforceable |
| Require one idempotency header on every operation | invents meaningless headers for reads and cannot make a multi-step mutation safe |
| Add a connector-owned activity or webhook journal | duplicates the source-local durable-process contract and creates inconsistent retry/dedupe semantics |

## Consequences

The design adds small crates, a checked-in catalog, boundary CI, and human
snapshot review. A provider may remain inventory-only even when its API is
useful because idempotency, binary, or process-runtime evidence is incomplete.

In return, every production operation remains fixed-origin, typed, bounded,
statically registered, and attributable. The processor escape surface is
substantially reduced, existing HTTP/Stripe behavior remains a migration
oracle, and connector work can progress independently without inventing any
process-owned persistence or public execution surface.

---

## Per-tenant credentials: what it would take (2026-08-23)

[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] defers
per-tenant connector credentials and points here, because this is where the
question was left open. Having looked at what it would cost, the shape is
smaller than either decision implied, and worth writing down before somebody
budgets it as a rewrite.

**The dimension already exists.** `donat.connector_credential` is keyed by
`(source, connector, instance, subject)`, and `subject` is the provider account
the credential belongs to. Storing one credential per tenant needs no new
column, no change to the sealing envelope — the AAD already binds `subject` —
and no migration of anything sealed.

**The runtime already refuses the ambiguity.** `CredentialRuntime::subject`
lists the subjects for an instance and requires exactly one: none is
`NO_CREDENTIAL`, more than one is `AMBIGUOUS_CREDENTIAL`. Multiple accounts per
instance are therefore modelled and deliberately turned away, for want of
anything that says which to pick. **A tenant is exactly the thing that says
which to pick.**

**What is actually missing is one thread of plumbing.** `CredentialRuntime` is
built once per source and holds no per-request state, so the tenant has to
arrive per acquisition — from the activity attempt, whose process session
already carries it under the caller-session contract this branch added. Until
it does, `subject` has nothing to choose with, which is why the ambiguity is
refused rather than guessed.

**And one deliberate consequence to decide.** `examples/pethub`'s migration
records that provider-issued identifiers are globally unique *only while one
deployment holds one provider account*. Per-tenant credentials end that, and
the unique constraints over `provider_event_id` and its neighbours become
per-tenant on the same day. That is named in the migration already; it stops
being a note and becomes work at that point.

Not built here, and not started: threading a request-scoped value into a
credential path is where a subtle mistake becomes a token used for the wrong
account, and it deserves its own slice rather than the tail of another.

