---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
  - "[[007-community-connector-factory]]"
  - "[[005-durable-processes]]"
---

# Canonical catalog projections and persisted response-header capabilities

## Context

ADR 010 fixed the static connector boundary, catalog ownership, fact-origin
split, and generated-catalog model, but it did not define exact persistent
schemas for source-record, semantic, provenance, and value-contract hashing.
It also left three related ambiguities: the planned `OperationSpec` was not a
complete self-contained snapshot, selected response headers had no persisted
`CapabilityId` derivation, and the shared `TypedValue` domain cannot be
losslessly represented by untagged I-JSON numbers.

These are persistent-contract questions, not runtime implementation details.
They must be fixed before the normalized catalog lands because generated Rust,
process revisions, and rolling binaries will retain the result.
This decision refines ADR 010; all of ADR 010's static runtime, licensing,
fixed-origin, and construction-authority boundaries remain in force.

## Decision

### Version and canonicalization taxonomy

The catalog owns these closed version forms:

```text
StableSemver = { "major": u32, "minor": u32, "patch": u32 }
Epoch        = u32
```

Connector, credential, operation, trigger, and event versions use
`StableSemver`. Phase 1 rejects prerelease and build metadata. Runtime ABI,
canonical schema, source-record schema, classifier, generator, and static
processor/authenticator/codec/normalizer implementation versions are integer
epochs. Every processor-like reference is exactly
`{ "id": <ABI-owned ID>, "implementation_revision": Epoch }`.
`ConnectorManifest.version` is split into `manifest_version: Epoch` and
`connector_version: StableSemver`.

All projections below are validated I-JSON and are serialized with
[RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785.html). Object member
names are recursively ordered by unsigned UTF-16 code units, not Rust
`BTreeMap<String>`'s UTF-8 order. Arrays preserve the order stated by their
schema. Duplicate members, lone surrogates, noncharacters, and values outside
the closed schema reject before hashing. This follows
[RFC 7493](https://www.rfc-editor.org/rfc/rfc7493.html), including its
recommendation to encode exact 64-bit integers as strings when binary64 cannot
preserve them.

Safe-width schema and version integers remain JSON numbers. Every other
`u64` or `NonZeroU64` projection field is a minimal unsigned decimal JSON
string. Optional values are always present as their value or JSON `null`.
Enums use one form, `{"kind":"<snake_case-variant>","value":<payload-or-null>}`.
Set-like arrays are sorted by their stable typed ID and reject duplicate IDs.
Declared steps, pre-request transforms, post-response transforms, and error
rules retain declared order.

### Closed version-1 projection schemas

`SourceRecordMaterialV1` is the complete validated source record, with exactly
the following top-level members and their complete closed nested values from
Spec 007 Section 4.1:

```text
{
  "record_version", "record_id", "subject", "reacquisition",
  "artifact_hashes", "license", "notice", "entrypoints", "dependencies",
  "embedded_material", "provider_contracts", "compatibility", "admission",
  "safety_findings", "reviewer", "approval_date", "proposed_manifest",
  "proposed_destinations", "red_tests"
}
```

Nothing is dropped as “review-only” from this source-record projection.
Artifact hashes, entrypoints, dependencies, embedded decisions, provider
contracts, destinations, and RED tests are sorted by their stable identity
when they are set-like. A record cannot contain its own `record_sha256`.

`SemanticMaterialV1` has exactly these top-level members:

```text
{
  "canonical_schema_epoch",
  "value_language_epoch",
  "connector": {
    "id", "version", "provider_id", "api_identity"
  },
  "credentials",
  "origins",
  "operations",
  "triggers",
  "resolved_fact_values"
}
```

It contains behavior only. Credentials, origins, operations, and triggers are
complete normalized catalog values, not summaries. Consequently it retains
every schema and recomputed contract hash; compiled step; declared transform;
processor-like ID and implementation revision; static error code and safe
message; selected-header name and stored capability; mapping and typed
default; effect/idempotency fact value; pagination plan; capacity, rate,
serialization, request, response, binary, item, page, call, redirect, and
deadline bound. `resolved_fact_values` records the normalized value at each
stable use-site ID. Review identities, source locations, licenses, notices,
and policy IDs do not enter this projection. It cannot contain its own
`semantic_sha256`.

`ProvenanceMaterialV1` has exactly these top-level members:

```text
{
  "canonical_schema_epoch",
  "connector": {
    "id", "version", "semantic_sha256"
  },
  "sources",
  "artifacts",
  "files",
  "licenses",
  "dependencies",
  "embedded_material",
  "notices",
  "resolved_fact_origins",
  "donat_policy_ids",
  "classifier_epoch",
  "generator_epoch"
}
```

It contains attribution only. `sources` is sorted by source-record ID and
stores each source identity and `record_sha256`. Artifact, file, license,
dependency, embedded-material, and notice entries retain the immutable
decisions and hashes defined by the accepted source record. For every stable
fact use-site, `resolved_fact_origins` stores the exact provider
record/artifact/fact location; `donat_policy_ids` stores each Donat-owned
policy use. It cannot contain normalized fact values or its own
`provenance_sha256`.

Resolved evidence is structurally `(value, origin)`. Catalog validation
resolves both halves once: `SemanticMaterialV1` receives the value at the
stable use site and `ProvenanceMaterialV1` receives that same use site's exact
origin. Neither projection may reconstruct or omit the other half.

`ValueContractMaterialV1` is the exact catalog-owned projection of the shared
value-contract language:

```text
{
  "value_language_epoch",
  "roots": {
    <name>: { "required": boolean, "type_ref": TypeRefMaterialV1 }
  },
  "named_objects": {
    <name>: {
      "fields": {
        <name>: { "required": boolean, "type_ref": TypeRefMaterialV1 }
      }
    }
  }
}

TypeRefMaterialV1 = {
  "nullable": boolean,
  "value_type": ValueTypeMaterialV1
}

ValueTypeMaterialV1 =
  {"kind":"scalar","value":{"scalar":<closed tagged ValueScalar>}} |
  {"kind":"enum","value":{"name":string,"values":[string...]}} |
  {"kind":"object","value":{"fields":{<name>:ValueContractField...}}} |
  {"kind":"list","value":{"element":TypeRefMaterialV1}} |
  {"kind":"ref","value":{"name":string}}
```

The scalar enum is tagged by its existing canonical snake-case name; the
custom scalar payload is `{"kind":"custom","value":{"name":string}}`.
Roots, named objects, and fields use JCS UTF-16 member ordering. Enum values
retain declared order. The full named-object closure, including unreachable
declarations, is present. A value-contract material cannot contain its own
hash.

The four hashes and their calculation order are normative:

```text
record_sha256 =
  SHA256("donat.connector.source-record.v1\0" || JCS(SourceRecordMaterialV1))

semantic_sha256 =
  SHA256("donat.connector.semantic.v1\0" || JCS(SemanticMaterialV1))

provenance_sha256 =
  SHA256("donat.connector.provenance.v1\0" || JCS(ProvenanceMaterialV1))

value_contract_sha256 =
  SHA256("donat.connector.value-contract.v1\0" ||
         JCS(ValueContractMaterialV1))
```

Catalog construction computes record hashes first, then resolved manifest and
value-contract hashes, then the semantic hash, then the provenance hash.
Code generation runs only after those values exist and computes the generated
Rust/tree digest last. No material contains the digest that it produces.

### Lossless `TypedValue` projection

The catalog owns a JCS adapter; `donat-value-contract` remains `no_std +
alloc`, gains no Serde/JCS dependency, and keeps `canonical_size` as the
separate already accepted wire-size contract. Every `TypedValue` variant is
tagged:

```text
Null          {"kind":"null","value":null}
Boolean       {"kind":"boolean","value":true|false}
String        {"kind":"string","value":"..."}
I64           {"kind":"i64","value":"<minimal signed decimal>"}
U64           {"kind":"u64","value":"<minimal unsigned decimal>"}
Decimal       {"kind":"decimal","value":"<accepted exact spelling>"}
List          {"kind":"list","value":[TypedValueMaterialV1...]}
Object        {"kind":"object","value":{"key":TypedValueMaterialV1...}}
InlineBytes   {"kind":"inline_bytes","value":{
                 "$binary":"<RFC 4648 base64url without padding>",
                 "file_name":<string-or-null>,
                 "media_type":<string-or-null>
               }}
```

Object keys use RFC 8785 UTF-16 ordering. The tags prevent `I64(1)`, `U64(1)`,
`Decimal("1")`, and `String("1")` from colliding. Full-width integer strings
preserve `u64::MAX`; the decimal string preserves its already validated exact
spelling.

### Complete operation snapshot

The catalog-owned `OperationSpec` is self-contained and has exactly these
fields:

```text
{
  "connector", "connector_version",
  "operation", "operation_version",
  "runtime_abi_epoch", "value_language_epoch",
  "input", "input_contract_sha256",
  "output", "output_contract_sha256",
  "credential",
  "origins", "steps",
  "pre_request_transforms", "post_response_transforms",
  "operation_processor",
  "effect", "pagination", "error_map",
  "capacity", "rate", "serialization_key_default",
  "bounds",
  "provenance", "fact_bindings"
}
```

`credential` is null or
`{"id": CredentialSpecId, "version": StableSemver}`. `origins` is the exact
resolved origin closure used by the steps. `steps` contains every complete
compiled step in declared order. Transform entries and the optional operation
processor use processor-like `(ABI-owned ID, implementation_revision)`
references. Input and output hashes are recomputed from
`ValueContractMaterialV1` and must match. Effects, pagination, complete error
map, capacity/rate defaults, typed serialization-key default, complete
step/operation bounds, exact provenance references, and stable fact-use-site
bindings are present even when their value is null or empty.

A declarative operation without an operation processor has exactly one
compiled step. Multiple steps require a versioned static operation processor.
Generated entries are exact const-safe projections of this shape rather than
a second partial model. Every later process compiler, revision, worker, and
upgrade diff pins or compares all versions, hashes, defaults, origins,
processor revisions, selected-header capabilities, provenance, and fact
bindings in this snapshot.

### Persisted selected-response-header capabilities

Every compiled step stores each selected response header as:

```text
{
  "canonical_lowercase_header_name": StaticHeaderName,
  "capability": CapabilityId
}
```

The catalog ASCII-case-folds and validates the header name, then derives the
ID once during normalization:

```text
digest = SHA256(
  "donat.connector.response-header-capability.v1\0" ||
  JCS({
    "connector": connector_id,
    "header": canonical_ascii_lowercase_header,
    "operation": operation_id,
    "operation_version": StableSemver,
    "step": compiled_step_id
  })
)

CapabilityId = "response-header." || lowercase_hex(digest)
```

The result is exactly 80 ASCII bytes and fits the ABI's 96-byte ID bound.
Scope includes connector, operation, operation version, and step, so an equal
header name in another scope receives another ID.

Each error-correlation binding stores both the canonical header and capability
or a validated link to the same step-local stored mapping. Catalog
normalization rejects missing, ambiguous, duplicate, or more than 64
resolutions. Header names and IDs enter semantic material. Task 5 emits the
stored mappings exactly. Task 8 matches response header names, stores bounded
values under the stored IDs, and passes only the selected error action's
stored capability allowlist to `host_construction`. Runtime code never derives
a capability ID and never accepts a caller-provided allowlist.

### Normative vectors and gates

The independent domain-hash oracle pins:

| Canonical bytes | Domain | SHA-256 |
| --- | --- | --- |
| `{}` | `donat.connector.source-record.v1\0` | `210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4` |
| `{}` | `donat.connector.semantic.v1\0` | `799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19` |
| `{}` | `donat.connector.provenance.v1\0` | `a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4` |
| `{}` | `donat.connector.value-contract.v1\0` | `6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.source-record.v1\0` | `d6c4fc943d8ed980d248ffa25f2d8d16be65953603705d5afc29e5e8a045269f` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.semantic.v1\0` | `2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.provenance.v1\0` | `4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23` |
| `{"a":1,"b":[true,null,"x"]}` | `donat.connector.value-contract.v1\0` | `e74426ca8fb7b23e99f1f14f4a6d281575489c33312e27df9e9005f37158d4ab` |

For connector `donat.http`, operation `get` version `1.0.0`, step `request`,
and input header `X-Request-ID`, the canonical derivation bytes are:

```json
{"connector":"donat.http","header":"x-request-id","operation":"get","operation_version":{"major":1,"minor":0,"patch":0},"step":"request"}
```

The digest is
`fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`,
and the exact capability is
`response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`.

The tagged-value oracle pins:

| Value | Exact canonical bytes |
| --- | --- |
| `I64(1)` | `{"kind":"i64","value":"1"}` |
| `U64(1)` | `{"kind":"u64","value":"1"}` |
| `Decimal("1")` | `{"kind":"decimal","value":"1"}` |
| `String("1")` | `{"kind":"string","value":"1"}` |
| `U64(u64::MAX)` | `{"kind":"u64","value":"18446744073709551615"}` |
| `String("{\"a\":1}")` | `{"kind":"string","value":"{\"a\":1}"}` |
| `Object({"a": I64(1)})` | `{"kind":"object","value":{"a":{"kind":"i64","value":"1"}}}` |

Bytes `[0xff, 0x00]` encode as `_wA`, producing:

```json
{"kind":"inline_bytes","value":{"$binary":"_wA","file_name":null,"media_type":"application/octet-stream"}}
```

An object with keys U+10000 and U+FFFD must emit U+10000 first because its
first UTF-16 code unit is `0xd800`, even though a UTF-8 byte-order map would
place U+FFFD first.

Tests independently prove one-field mutation of each complete material changes
only the applicable domain, value/origin separation is total, generated
operation projections equal normalized operations field-for-field, and every
pinned consumer retains the complete operation. Header tests cover ASCII case
folding, scope separation, missing/ambiguous/duplicate/more-than-64 rejection,
the exact 80-byte result, and the ABI's 96-byte bound. A source-policy test
asserts Task 8 contains no capability-domain string, SHA-256 derivation, or
other capability-ID construction path.

## Alternatives

| Option | Why Not |
| --- | --- |
| Hash serde output from the implementation structs | field omission, enum representation, map order, and future serde changes would silently redefine persisted identities |
| Encode `TypedValue` as ordinary JSON | binary64 cannot preserve full `u64` or exact decimal spelling, and untagged numeric variants collide |
| Derive header capabilities in the runtime | rolling binaries could disagree, the generated catalog would not be self-contained, and callers could influence authorization |
| Persist only operation IDs and reload current catalog values | active process revisions would silently adopt new origins, processors, defaults, or error behavior |
| Add JCS to `donat-value-contract` | violates the accepted low-level dependency boundary and conflates catalog hashing with canonical wire-size accounting |

## Consequences

Task 3 must implement more explicit projection and validation code, and every
schema change requires a new epoch/domain rather than an in-place serde edit.
Generated entries are larger because operations and selected-header mappings
are complete.

In return, record, behavior, attribution, value contracts, generated Rust, and
process revisions have deterministic non-circular identities. Exact integers
and decimals survive hashing, response-header authorization is catalog-owned,
and old revisions remain executable without runtime derivation or a dynamic
plugin/admin surface. ADR 010's one-binary, static-catalog, fixed-origin,
no-admin, no-runtime-registration, and no-workflow-node boundaries remain
unchanged.
