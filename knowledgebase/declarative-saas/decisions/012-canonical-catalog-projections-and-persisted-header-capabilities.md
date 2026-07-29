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

ADR 010 fixed the static connector boundary, but persistent catalog identities
also need schemas that are closed to primitive leaves. In particular, semantic
material must not serialize provenance-bearing runtime descriptors, and an
immutable process revision needs a non-circular identity envelope beside its
behavioral operation or trigger.

This decision refines ADR 010. Its fixed-origin, licensing, static-runtime,
construction-authority, no-admin, and no-runtime-registration boundaries
remain in force.

## Decision

### Canonical primitive algebra

All catalog epochs, including `canonical_schema_epoch`,
`source_record_schema_epoch`, `value_language_epoch`, `runtime_abi_epoch`,
`classifier_epoch`, `generator_epoch`, and every
`implementation_revision`, are `Epoch = u32` and encode as I-JSON numbers.
There is no `u16` epoch and no processor `version`. Product versions are
`StableSemver = {"major":u32,"minor":u32,"patch":u32}`; prerelease and build
components reject in phase 1. A processor-like reference is exactly
`{"id":Id,"implementation_revision":Epoch}`.

The following table closes every primitive leaf used below:

| Leaf | Canonical JSON v1 |
| --- | --- |
| `Id` and its typed wrappers | JSON string, ASCII, 1–96 bytes, the ABI `InlineId` grammar; wrapper type is known from the containing field |
| ordinary string | valid Unicode scalar values, no surrogate or Unicode noncharacter, no normalization |
| `Hash256` / `Hash512` | exactly 64 / 128 lowercase hexadecimal ASCII characters |
| `GitCommit` | exactly 40 lowercase hexadecimal ASCII characters |
| `Date` | validated Gregorian `YYYY-MM-DD` string |
| `RepoPath` / `SourcePath` | nonempty relative UTF-8 path with `/`, no empty, `.`, `..`, NUL, prefix, or backslash component |
| `ExactHttpsUrl` | validated absolute HTTPS URL string, preserved byte-for-byte after validation |
| `bool`, `u8`, `u16`, `u32` | I-JSON boolean or exact nonnegative I-JSON number |
| `u64`, `NonZeroU64`, `i64`, exact decimal | minimal decimal JSON string; nonzero is enforced where declared |
| bytes | RFC 4648 base64url without padding |
| optional value | member is always present; its value or JSON `null` |
| enum | `{"kind":"<snake_case_tag>","value":<payload-or-null>}` |
| ordered list | array in declared order |
| set-like list | array sorted by the stated key; duplicate keys reject |
| map | object; member names recursively sort by unsigned UTF-16 code units |

Raw input must satisfy RFC 7493 I-JSON before schema decoding and then RFC 8785
JCS. The raw-byte decoder has a closed rejection surface:
`catalog_jcs_invalid_utf8`, `catalog_jcs_disallowed_unicode`,
`catalog_jcs_invalid_surrogate`, `catalog_jcs_duplicate_member`,
`catalog_jcs_number_not_i_json`, or `catalog_jcs_schema_mismatch`. It rejects
escaped and unescaped noncharacters, lone surrogates and invalid pairs,
duplicate names after escape decoding, and numbers outside exact binary64
acceptance (including `1e400` and integer `9007199254740992`). Full-width
catalog integers and decimals use the tagged strings defined here; the parser
never rounds them.

### Source-record material

`SourceRecordMaterialV1` has exactly:

```text
{
  "record_version":Epoch, "record_id":SourceRecordId,
  "subject":SourceSubjectMaterialV1,
  "reacquisition":ReacquisitionMaterialV1,
  "artifact_hashes":[ArtifactHashMaterialV1...],
  "license":LicenseDecisionMaterialV1, "notice":NoticeMaterialV1,
  "entrypoints":[SourcePath...],
  "dependencies":[DependencyDecisionMaterialV1...],
  "embedded_material":[EmbeddedDecisionMaterialV1...],
  "provider_contracts":[ProviderContractMaterialV1...],
  "compatibility":CompatibilityMaterialV1,
  "admission":AdmissionMaterialV1,
  "safety_findings":[SafetyFindingMaterialV1...],
  "reviewer":ReviewIdentity, "approval_date":Date,
  "proposed_manifest":RepoPath|null,
  "proposed_destinations":[RepoPath...], "red_tests":[TestId...]
}
```

The closed nested forms are:

```text
SourceSubjectMaterialV1 =
  tagged "donat_owned" {
    "files":[{"path":RepoPath,"sha256":Hash256}...],
    "repository_commit":GitCommit
  } |
  tagged "provider_artifact" {
    "api_version":string, "artifact_url":ExactHttpsUrl|null,
    "repository_url":ExactHttpsUrl|null, "revision":string,
    "schema_paths":[SourcePath...], "terms_evidence":SourcePath
  } |
  tagged "exact_npm" {
    "integrity":Hash512, "maintainers":[Id...], "name":string,
    "npm_git_head":GitCommit, "package_repository":ExactHttpsUrl,
    "provenance":NpmProvenanceMaterialV1,
    "provenance_commit":GitCommit|null, "repository":ExactHttpsUrl,
    "repository_owner":RepositoryOwnerMaterialV1,
    "signature":NpmSignatureMaterialV1, "tag_commit":GitCommit|null,
    "tarball_url":ExactHttpsUrl, "version":StableSemver
  }

ReacquisitionMaterialV1 =
  tagged "exact_npm_review" null |
  tagged "provider_repository_review" null |
  tagged "provider_versioned_artifact_review" null |
  tagged "donat_owned_no_network" null

ArtifactHashMaterialV1 = {
  "algorithm": tagged "sha256" null | tagged "sha512" null,
  "artifact_id":Id, "digest":Hash256|Hash512, "path":SourcePath|null
}
LicenseDecisionMaterialV1 =
  tagged "permissive" {
    "license_file_path":SourcePath,"license_file_sha256":Hash256,
    "selected_dual_license_branch":string|null,"spdx_id":string
  } |
  tagged "written_grant" {"decision_id":Id,"grant_sha256":Hash256} |
  tagged "rejected" {"finding":Id}
NoticeMaterialV1 = {
  "id":NoticeId,"license_file_path":SourcePath,
  "license_file_sha256":Hash256,
  "notice_bundle_destination":RepoPath,
  "required_copyright_lines":[string...]
}
DependencyDecisionMaterialV1 = {
  "dependency":Id, "disposition":
    tagged "admitted" {"license":LicenseDecisionMaterialV1} |
    tagged "excluded" {"reason":Id} |
    tagged "behavior_only" {"reason":Id} |
    tagged "replaced" {"replacement":Id} |
    tagged "rejected" {"finding":Id}
}
EmbeddedDecisionMaterialV1 = {
  "id":Id, "path":SourcePath, "sha256":Hash256,
  "decision": tagged "admitted" {"license":LicenseDecisionMaterialV1} |
              tagged "excluded" {"reason":Id} |
              tagged "rejected" {"finding":Id}
}
ProviderContractMaterialV1 = {
  "contract_id":ProviderContractId,
  "facts":[
    tagged "provider_evidence" {
      "fact_id":ProviderFactId,"source_record_id":SourceRecordId
    } |
    tagged "donat_policy" {"policy_id":DonatPolicyId,
                           "value":TypedValueMaterialV1}
  ...]
}
CompatibilityMaterialV1 =
  tagged "tier_a" null | tagged "tier_b" null | tagged "tier_c" null |
  tagged "rejected" null
AdmissionMaterialV1 =
  tagged "inventory_only" {"findings":[FindingId...]} |
  tagged "approved_for_port" {"operations":[OperationId...]} |
  tagged "evidence_accepted" {"contracts":[ProviderContractId...]}
SafetyFindingMaterialV1 = {
  "finding_id":FindingId, "kind":Id, "location":SourcePath|null,
  "message":string
}
```

`NpmSignatureMaterialV1`, `NpmProvenanceMaterialV1`, and
`RepositoryOwnerMaterialV1` use the tags and fields from Spec 007 §4.1:
`verified {signatures:[{key_id,signature_sha256}],registry_metadata_sha256}`,
`verified_absent {registry_metadata_sha256}`, or `rejected {finding}`;
`verified {source_commit,statement_sha256}`, `verified_absent
{registry_metadata_sha256}`, or `rejected {finding}`; and `consistent
{package_owner,repository_owner}`, `reviewed_mismatch {decision_id}`, or
`rejected {finding}` respectively.

Sort keys are `artifact_id`, dependency ID, embedded ID, `contract_id`, fact
tag plus fact/policy ID, `finding_id`, path, maintainer ID, operation ID, and
test ID as applicable. `entrypoints` retain manifest order. A source record
cannot contain `record_sha256`.

### Behavioral semantic material

Semantic material never serializes `OperationSpec`, `TriggerSpec`, or any
runtime type that contains provenance. It recursively projects only behavior:

```text
SemanticMaterialV1 = {
  "canonical_schema_epoch":Epoch, "value_language_epoch":Epoch,
  "connector":{"api_identity":string,"id":ConnectorId,
               "provider_id":Id,"version":StableSemver},
  "credentials":[SemanticCredentialMaterialV1...],
  "origins":[SemanticOriginMaterialV1...],
  "operations":[SemanticOperationMaterialV1...],
  "triggers":[SemanticTriggerMaterialV1...],
  "resolved_fact_values":[ResolvedFactValueMaterialV1...]
}

ResolvedFactValueMaterialV1 = {"use_site":Id,"value":TypedValueMaterialV1}
SemanticOriginMaterialV1 = {
  "host":string, "network_policy":
    tagged "direct_https" null | tagged "private_https" {"policy":Id},
  "origin":OriginId, "port":u16, "scheme":tagged "https" null
}
SemanticCredentialMaterialV1 = {
  "auth_plan":CredentialAuthMaterialV1,
  "credential":CredentialSpecId, "credential_test_operation":OperationId|null,
  "credential_test_operation_version":StableSemver|null,
  "maximum_secret_bytes":u32, "origins":[OriginId...],
  "scopes":[string...], "version":StableSemver
}
CredentialAuthMaterialV1 =
  tagged "fixed_header_api_key" {"header":string} |
  tagged "fixed_query_api_key" {"key":string} |
  tagged "bearer" null |
  tagged "http_basic" null |
  tagged "oauth2_client_credentials" {
    "client_auth":tagged "basic" null | tagged "body" null,
    "processor":{"id":Id,"implementation_revision":Epoch}|null,
    "token_origin":OriginId,"token_path":string
  } |
  tagged "preprovisioned_oauth_access_token" null
```

An operation is this exact non-circular shape:

```text
SemanticOperationMaterialV1 = {
  "bounds":OperationBoundsMaterialV1, "capacity":CapacityMaterialV1|null,
  "connector":ConnectorId, "connector_version":StableSemver,
  "credential":{"id":CredentialSpecId,"version":StableSemver}|null,
  "effect":OperationEffectMaterialV1, "error_map":ErrorMapMaterialV1,
  "input":ValueContractMaterialV1, "input_contract_sha256":Hash256,
  "operation":OperationId,
  "operation_processor":{"id":Id,"implementation_revision":Epoch}|null,
  "operation_version":StableSemver, "origins":[OriginId...],
  "output":ValueContractMaterialV1, "output_contract_sha256":Hash256,
  "pagination":PaginationMaterialV1,
  "post_response_transforms":[{"id":Id,"implementation_revision":Epoch}...],
  "pre_request_transforms":[{"id":Id,"implementation_revision":Epoch}...],
  "rate":RateMaterialV1|null,
  "resolved_fact_values":[ResolvedFactValueMaterialV1...],
  "runtime_abi_epoch":Epoch,
  "serialization_key_default":TypedValueMaterialV1|null,
  "steps":[SemanticStepMaterialV1...], "value_language_epoch":Epoch
}
```

`SemanticStepMaterialV1` has `bounds`, `credential_action`, `headers`,
`method`, `origin`, `path`, `query`, `request`, `response`,
`selected_response_headers`, `step`, and `success_statuses`. Header/query
entries are `{"binding":BindingMaterialV1,"name":string}` sorted by canonical
name; `BindingMaterialV1` is
`{"default":TypedValueMaterialV1|null,"field":Id,"mapping":Id|null,
"required":bool,"source":tagged "input" null | tagged "constant"
{"value":TypedValueMaterialV1}}`. Request is tagged `none`, `json`,
`form_urlencoded`, `multipart`, or `raw_bytes`; its non-null payload is
`{"bindings":[Id...]}` in declared order. Response is tagged `json` or
`raw_bytes`, with `{"mappings":[{"pointer":string,"target":Id}...]}`.
Selected headers are
`{"canonical_lowercase_header_name":string,"capability":CapabilityId}` sorted
by header. Statuses are ascending distinct `u16`.
`credential_action` is null or tagged `apply`
`{"credential":CredentialSpecId}`. `method` is an uppercase admitted HTTP
method string, `path` is a validated static absolute path template, and
`origin`/`step` are their typed IDs.

The remaining operation composites are closed as follows:

```text
StepBoundsMaterialV1 = {
  "maximum_headers":u32,"maximum_header_bytes":u32,
  "maximum_url_bytes":u32,"maximum_request_bytes":u32,
  "maximum_response_bytes":u32,"maximum_json_depth":u32,
  "maximum_json_nodes":u32,"maximum_inline_binary_bytes":u32,
  "deadline_ms":u64-string
}
OperationBoundsMaterialV1 = {
  "maximum_calls":u32,"maximum_pages":u32,"maximum_items":u32,
  "maximum_aggregate_request_bytes":u32,
  "maximum_aggregate_response_bytes":u32,
  "maximum_output_canonical_bytes":u32,"maximum_redirects":u8,
  "deadline_ms":u64-string
}
CapacityMaterialV1 = {"maximum_in_flight":u32}
RateMaterialV1 = {"burst":u32,"refill_interval_ms":u64-string}
OperationEffectMaterialV1 =
  tagged "read_only" null |
  tagged "provider_idempotent" {"side_effect_steps":[{
    "fixed_binding":{"field":Id,"header":string,"scope":Id},
    "margin_ms":u64-string,"retention_ms":u64-string,"step":CompiledStepId
  }...]}
PaginationMaterialV1 =
  tagged "none" null |
  tagged "cursor" {"input_field":Id,"output_pointer":string} |
  tagged "offset" {"initial":u64-string,"input_field":Id,"step":u64-string} |
  tagged "link_header" {"capability":CapabilityId,"relation":string} |
  tagged "processor" {"id":Id,"implementation_revision":Epoch}
ErrorMapMaterialV1 = {"fallback":ErrorActionMaterialV1,
  "rules":[{"action":ErrorActionMaterialV1,
            "matcher":ErrorMatcherMaterialV1}...]}
ErrorMatcherMaterialV1 = {
  "body_code":string|null,"header":{"capability":CapabilityId,
  "equals":string|null}|null,"status":u16|null
}
ErrorActionMaterialV1 = {"code":Id,"message":string,
  "retry":tagged "never" null | tagged "safe" {"after_ms":u64-string|null}}
```

The effect side-effect list sorts by `step`; error rules and transforms retain
declared order. Operation origins sort by ID; steps retain declared order.
The operation-level `resolved_fact_values` sort by use site and must equal the
corresponding subset of the top-level list.

`SemanticTriggerMaterialV1` is
`{"connector":ConnectorId,"connector_version":StableSemver,
"credential":{"id":CredentialSpecId,"version":StableSemver}|null,
"event_contract":ValueContractMaterialV1,"event_contract_sha256":Hash256,
"kind":tagged "webhook" {"method":string,"path":string,
"signature_processor":{"id":Id,"implementation_revision":Epoch}|null} |
tagged "poll" {"operation":OperationId,"operation_version":StableSemver,
"interval_ms":u64-string},
"origins":[OriginId...],"resolved_fact_values":[ResolvedFactValueMaterialV1...],
"runtime_abi_epoch":Epoch,"trigger":TriggerId,"trigger_version":StableSemver,
"value_language_epoch":Epoch}`. Trigger origins and fact values use the same
sort rules.

### Value-contract and typed-value material

```text
ValueContractMaterialV1 = {
  "named_objects":{name:{"fields":{name:FieldMaterialV1}}},
  "roots":{name:FieldMaterialV1}, "value_language_epoch":Epoch
}
FieldMaterialV1 = {"required":bool,"type_ref":TypeRefMaterialV1}
TypeRefMaterialV1 = {"nullable":bool,"value_type":ValueTypeMaterialV1}
ValueTypeMaterialV1 =
  tagged "scalar" ScalarMaterialV1 |
  tagged "enum" {"name":string,"values":[string...]} |
  tagged "object" {"fields":{name:FieldMaterialV1}} |
  tagged "list" {"element":TypeRefMaterialV1} |
  tagged "ref" {"name":string}
ScalarMaterialV1 =
  tagged "null" null | tagged "boolean" null | tagged "string" null |
  tagged "i64" null | tagged "u64" null | tagged "decimal" null |
  tagged "inline_bytes" null | tagged "custom" {"name":string}
```

Maps use UTF-16 member order; enum values retain declared order; the full
named-object closure, including unreachable declarations, is present.

`TypedValueMaterialV1` is tagged: `null` with null; `boolean`; `string`; `i64`,
`u64`, and `decimal` with their exact decimal strings; `list` with ordered
typed values; `object` with recursively JCS-ordered members; or `inline_bytes`
with `{"$binary":bytes,"file_name":string|null,"media_type":string|null}`.
This catalog adapter adds no Serde/JCS dependency to `donat-value-contract`
and does not alter its separate `canonical_size` contract.

### Provenance material

`ResolvedFactOriginMaterialV1` is
`{"origin":tagged "provider_evidence"
{"artifact_id":Id,"fact_id":ProviderFactId,"location":SourcePath,
"source_record_id":SourceRecordId} | tagged "donat_policy"
{"policy_id":DonatPolicyId},"use_site":Id}`. It never contains a fact value.

```text
ProvenanceMaterialV1 = {
  "artifacts":[ArtifactDecisionMaterialV1...],
  "canonical_schema_epoch":Epoch, "classifier_epoch":Epoch,
  "connector":{"id":ConnectorId,"semantic_sha256":Hash256,
               "version":StableSemver},
  "dependencies":[DependencyDecisionMaterialV1...],
  "donat_policy_ids":[DonatPolicyId...],
  "embedded_material":[EmbeddedDecisionMaterialV1...],
  "files":[FileDecisionMaterialV1...], "generator_epoch":Epoch,
  "licenses":[LicenseDecisionMaterialV1...],
  "notices":[NoticeMaterialV1...],
  "resolved_fact_origins":[ResolvedFactOriginMaterialV1...],
  "sources":[SourceIdentityMaterialV1...]
}
SourceIdentityMaterialV1 = {"record_id":SourceRecordId,
                            "record_sha256":Hash256}
ArtifactDecisionMaterialV1 = {
  "algorithm":tagged "sha256" null | tagged "sha512" null,
  "artifact_id":Id,"digest":Hash256|Hash512,"path":SourcePath|null,
  "source_record_id":SourceRecordId
}
FileDecisionMaterialV1 = {
  "path":SourcePath,"sha256":Hash256,"source_record_id":SourceRecordId
}
```

Sources sort by `record_id`; artifacts by `(source_record_id,artifact_id)`;
files by `(source_record_id,path)`; licenses by their canonical bytes;
dependencies and embedded decisions by ID; notices by `id`; direct origins by
`use_site`; policy IDs lexically. Direct provenance-origin bytes do not change
when only a resolved value changes. The final provenance bytes and hash do
change because `connector.semantic_sha256` commits the resulting semantic
hash. Conversely, an origin-only mutation leaves semantic bytes and hash
unchanged.

### Hashes, snapshot identity, and deployment identity

The calculation is non-circular:

```text
record_sha256 =
  SHA256("donat.connector.source-record.v1\0" || JCS(SourceRecordMaterialV1))
value_contract_sha256 =
  SHA256("donat.connector.value-contract.v1\0" ||
         JCS(ValueContractMaterialV1))
semantic_sha256 =
  SHA256("donat.connector.semantic.v1\0" || JCS(SemanticMaterialV1))
provenance_sha256 =
  SHA256("donat.connector.provenance.v1\0" || JCS(ProvenanceMaterialV1))
operation_snapshot_sha256 =
  SHA256("donat.connector.operation-snapshot.v1\0" ||
         JCS(SemanticOperationMaterialV1))
trigger_snapshot_sha256 =
  SHA256("donat.connector.trigger-snapshot.v1\0" ||
         JCS(SemanticTriggerMaterialV1))
```

`DeploymentMaterialV1` is compiler-owned, contains no secret value, and has
exactly:

```text
{
  "canonical_schema_epoch":Epoch, "source_name":string,
  "connector_instance":string,
  "connector":{"id":ConnectorId,"version":StableSemver},
  "endpoint_identity":Id,
  "credential_identity":{"credential":CredentialSpecId,
    "instance":string,"version":StableSemver}|null,
  "resolver_identity":Id,
  "non_secret_config":[{"key":Id,"value":TypedValueMaterialV1}...],
  "enabled_operations":[{"id":OperationId,"version":StableSemver}...],
  "enabled_triggers":[{"id":TriggerId,"version":StableSemver}...],
  "capacity_override":u32|null,
  "rate_override":{"burst":u32,"refill_interval_ms":u64-string}|null,
  "serialization_override":TypedValueMaterialV1|null
}
deployment_fingerprint =
  SHA256("donat.connector.deployment.v1\0" || JCS(DeploymentMaterialV1))
```

Config, operation, and trigger lists sort by key/ID and reject duplicates.
Endpoint and credential fields are identities of deploy-time references, never
resolved URL/token/password bytes.

Every process pin stores beside its behavioral operation or trigger:

```text
CatalogIdentityEnvelopeV1 = {
  "canonical_schema_epoch":Epoch,
  "source_record_schema_epoch":Epoch,
  "source_records":[{"record_id":SourceRecordId,
                     "record_sha256":Hash256}...],
  "semantic_sha256":Hash256, "provenance_sha256":Hash256,
  "value_contracts":[{"use_site":Id,
                      "value_contract_sha256":Hash256}...],
  "snapshot_identity":
    tagged "operation" {"connector":ConnectorId,
      "connector_version":StableSemver,"operation":OperationId,
      "operation_version":StableSemver,"snapshot_sha256":Hash256} |
    tagged "trigger" {"connector":ConnectorId,
      "connector_version":StableSemver,"snapshot_sha256":Hash256,
      "trigger":TriggerId,"trigger_version":StableSemver},
  "deployment_fingerprint":Hash256
}
```

Sources sort by record ID and contracts by use-site ID. The contract list
includes input, output, event, and every named/transitive contract used by the
snapshot. Reload recomputes and compares every envelope field and the complete
behavioral snapshot field-for-field.

### Persisted selected-response-header capabilities

Each selected header stores
`{"canonical_lowercase_header_name":string,"capability":CapabilityId}`.
Normalization ASCII-case-folds the name and computes:

```text
digest = SHA256(
  "donat.connector.response-header-capability.v1\0" ||
  JCS({"connector":connector_id,"header":canonical_lowercase_header,
       "operation":operation_id,"operation_version":StableSemver,
       "step":compiled_step_id}))
CapabilityId = "response-header." || lowercase_hex(digest)
```

The result is exactly 80 ASCII bytes. Missing, ambiguous, duplicate, or more
than 64 selections reject. Runtime code only consumes stored mappings and
never contains this domain string or derives a capability.

### Normative vectors and executable gates

The empty-domain vectors remain normative:

| Domain | `{}` SHA-256 |
| --- | --- |
| `donat.connector.source-record.v1\0` | `210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4` |
| `donat.connector.semantic.v1\0` | `799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19` |
| `donat.connector.provenance.v1\0` | `a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4` |
| `donat.connector.value-contract.v1\0` | `6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f` |

These independently reproducible valid full-material vectors are also pinned
(each displayed line is the exact canonical byte sequence):

```text
source-record:
{"admission":{"kind":"approved_for_port","value":{"operations":["op.read"]}},"approval_date":"2026-07-29","artifact_hashes":[],"compatibility":{"kind":"tier_a","value":null},"dependencies":[],"embedded_material":[],"entrypoints":["src/lib.rs"],"license":{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"0000000000000000000000000000000000000000000000000000000000000000","selected_dual_license_branch":null,"spdx_id":"MIT"}},"notice":{"id":"notice.demo","license_file_path":"LICENSE","license_file_sha256":"0000000000000000000000000000000000000000000000000000000000000000","notice_bundle_destination":"THIRD_PARTY_NOTICES.md","required_copyright_lines":[]},"proposed_destinations":["connector-catalog/manifests/demo.yaml"],"proposed_manifest":"connector-catalog/manifests/demo.yaml","provider_contracts":[],"reacquisition":{"kind":"donat_owned_no_network","value":null},"record_id":"source.demo.v1","record_version":1,"red_tests":["catalog_demo_red"],"reviewer":"reviewer.demo","safety_findings":[],"subject":{"kind":"donat_owned","value":{"files":[{"path":"src/lib.rs","sha256":"1111111111111111111111111111111111111111111111111111111111111111"}],"repository_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}
SHA-256: b60c588b7be34ac8ca1093cb64c4e6bfe0ed8fd422978873a3f66d47ca75a369

value-contract:
{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}
SHA-256: 79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a

semantic:
{"canonical_schema_epoch":1,"connector":{"api_identity":"demo.v1","id":"demo","provider_id":"provider.demo","version":{"major":1,"minor":0,"patch":0}},"credentials":[],"operations":[{"bounds":{"deadline_ms":"1000","maximum_aggregate_request_bytes":1024,"maximum_aggregate_response_bytes":1024,"maximum_calls":1,"maximum_items":1,"maximum_output_canonical_bytes":1024,"maximum_pages":1,"maximum_redirects":0},"capacity":null,"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"credential":null,"effect":{"kind":"read_only","value":null},"error_map":{"fallback":{"code":"connector_error","message":"safe","retry":{"kind":"never","value":null}},"rules":[]},"input":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"input_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","operation":"op.read","operation_processor":null,"operation_version":{"major":1,"minor":0,"patch":0},"origins":["origin.demo"],"output":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"output_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","pagination":{"kind":"none","value":null},"post_response_transforms":[],"pre_request_transforms":[],"rate":null,"resolved_fact_values":[],"runtime_abi_epoch":1,"serialization_key_default":null,"steps":[{"bounds":{"deadline_ms":"1000","maximum_header_bytes":128,"maximum_headers":1,"maximum_inline_binary_bytes":0,"maximum_json_depth":4,"maximum_json_nodes":16,"maximum_request_bytes":1024,"maximum_response_bytes":1024,"maximum_url_bytes":256},"credential_action":null,"headers":[],"method":"GET","origin":"origin.demo","path":"/","query":[],"request":{"kind":"none","value":null},"response":{"kind":"json","value":{"mappings":[]}},"selected_response_headers":[],"step":"request","success_statuses":[200]}],"value_language_epoch":1}],"origins":[{"host":"api.example.test","network_policy":{"kind":"direct_https","value":null},"origin":"origin.demo","port":443,"scheme":{"kind":"https","value":null}}],"resolved_fact_values":[],"triggers":[],"value_language_epoch":1}
SHA-256: d7ff89799d022988f1a8b9caef28d3aad87358698b5d6759c010430310c0b59d

provenance:
{"artifacts":[],"canonical_schema_epoch":1,"classifier_epoch":1,"connector":{"id":"demo","semantic_sha256":"d7ff89799d022988f1a8b9caef28d3aad87358698b5d6759c010430310c0b59d","version":{"major":1,"minor":0,"patch":0}},"dependencies":[],"donat_policy_ids":[],"embedded_material":[],"files":[{"path":"src/lib.rs","sha256":"1111111111111111111111111111111111111111111111111111111111111111","source_record_id":"source.demo.v1"}],"generator_epoch":1,"licenses":[{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"0000000000000000000000000000000000000000000000000000000000000000","selected_dual_license_branch":null,"spdx_id":"MIT"}}],"notices":[{"id":"notice.demo","license_file_path":"LICENSE","license_file_sha256":"0000000000000000000000000000000000000000000000000000000000000000","notice_bundle_destination":"THIRD_PARTY_NOTICES.md","required_copyright_lines":[]}],"resolved_fact_origins":[],"sources":[{"record_id":"source.demo.v1","record_sha256":"b60c588b7be34ac8ca1093cb64c4e6bfe0ed8fd422978873a3f66d47ca75a369"}]}
SHA-256: 380471591f41d54ecfecf4d17988e55e8f5705a40a4611a0ed17f7ea792f201a
```

For connector `donat.http`, operation `get` version `1.0.0`, step `request`,
and `X-Request-ID`, the capability derivation bytes are
`{"connector":"donat.http","header":"x-request-id","operation":"get","operation_version":{"major":1,"minor":0,"patch":0},"step":"request"}`.
The digest is
`fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`
and the capability is
`response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`.

Task 3 raw-byte fixtures cover escaped and unescaped U+FDD0, `\ud800`,
`\ud800\u0041`, invalid UTF-8, `{"a":1,"\u0061":2}`, `1e400`,
`9007199254740992`, and recursive U+10000-before-U+FFFD ordering. Tests assert
the exact codes above. Further gates prove origin-only mutation leaves semantic
bytes/hash unchanged, value-only mutation leaves direct origin material
unchanged while final provenance changes through `semantic_sha256`, and no
provenance-bearing `OperationSpec`/`TriggerSpec` is serialized as semantic
material.

## Alternatives

| Option | Why not |
| --- | --- |
| Hash runtime structs directly | provenance leaks into semantics and Serde evolution silently changes persistence |
| Persist only current catalog IDs | an old revision silently adopts new behavior or attribution |
| Put deployment identity in semantic material | deployment changes would redefine portable catalog semantics and create circular pins |
| Derive header capabilities at runtime | rolling binaries could disagree and callers could influence authorization |

## Consequences

Task 3 owns explicit projections and raw-byte validation. Schema changes require
a new epoch/domain. Generated entries and process pins are larger, but record,
behavior, attribution, deployment, and value-contract identities are
deterministic, non-circular, and independently testable.
