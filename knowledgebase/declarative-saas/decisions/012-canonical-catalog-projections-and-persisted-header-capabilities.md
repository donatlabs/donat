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

ADR 010 fixes the static connector boundary. Persistent catalog identities
also need canonical schemas that are field-total against the normalized
Task-3 model. A canonical type is a projection of that owner, not a second
behavioral schema. Semantic material must not serialize provenance-bearing
runtime descriptors, and an immutable process revision needs a non-circular
identity envelope beside its behavioral operation or trigger.

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
components reject in phase 1. `ExactSemver` is a distinct validated string
containing one canonical SemVer 2.0.0 version. It permits canonical
prerelease and build components, preserves the admitted spelling exactly,
and rejects ranges, distribution tags, a leading `v`, empty identifiers,
non-ASCII identifiers, and numeric identifiers with noncanonical leading
zeros. It is used only for immutable donor-package versions. A processor-like
reference is exactly `{"id":Id,"implementation_revision":Epoch}`.

The following table closes every primitive leaf used below:

| Leaf | Canonical JSON v1 |
| --- | --- |
| `Id` and typed wrappers | JSON string, ASCII, 1–96 bytes, the ABI `InlineId` grammar; wrapper type is known from the containing field |
| ordinary string | valid Unicode scalar values, no surrogate or Unicode noncharacter, no normalization |
| `Hash256` / `Hash512` | exactly 64 / 128 lowercase hexadecimal ASCII characters |
| `GitCommit` / `GitTree` | exactly 40 lowercase hexadecimal ASCII characters |
| `Date` | validated Gregorian `YYYY-MM-DD` string |
| `ExactSemver` | one exact canonical SemVer 2.0.0 string; prerelease/build admitted, range/tag/leading-`v`/leading-zero forms rejected |
| `RepoPath` / `SourcePath` | nonempty relative UTF-8 path with `/`, no empty, `.`, `..`, NUL, prefix, or backslash component |
| `ExactHttpsUrl` / `RepositoryUrl` | validated absolute HTTPS URL string, preserved byte-for-byte after validation |
| `bool`, `u8`, `u16`, `u32` | I-JSON boolean or exact nonnegative I-JSON number |
| `u64`, `NonZeroU64`, `i64`, exact decimal | minimal decimal JSON string |
| bytes | RFC 4648 base64url without padding |
| optional value | member is always present; its value or JSON `null` |
| enum | `{"kind":"<snake_case_tag>","value":<payload-or-null>}` |
| ordered list | array in declared order |
| set-like list | array sorted by the stated key; duplicate keys reject |
| map | object; member names recursively sort by unsigned UTF-16 code units |

Raw input must satisfy RFC 7493 I-JSON before schema decoding and then RFC
8785 JCS. The raw-byte decoder has a closed rejection surface:
`catalog_jcs_invalid_utf8`, `catalog_jcs_disallowed_unicode`,
`catalog_jcs_invalid_surrogate`, `catalog_jcs_duplicate_member`,
`canonical_json_number_not_exact`, or `catalog_jcs_schema_mismatch`.

A raw JSON number is accepted only when parsing it as finite IEEE-754
binary64, serializing it with the RFC-8785/ECMAScript number algorithm, and
parsing that serialization preserves the same mathematical numeric value.
Thus raw `9007199254740992` (`2^53`) is accepted because it is exactly
representable; raw `9007199254740993` is rejected with
`canonical_json_number_not_exact`; `1e400` rejects because it is not finite.
RFC 7493's interoperable-integer recommendation is not a blanket rejection of
the exactly representable `2^53` boundary. Full-width catalog integers and
decimals remain tagged strings and are never rounded.

### Source-record material

`SourceRecordMaterialV1` has exactly the normalized
`ConnectorSourceRecord` fields:

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
  "safety_findings":{"findings":[SafetyFindingMaterialV1...]},
  "reviewer":ReviewIdentity, "approval_date":Date,
  "proposed_manifest":RepoPath|null,
  "proposed_destinations":[RepoPath...], "red_tests":[TestId...]
}
```

The source-subject branches are field-for-field:

```text
SourceSubjectMaterialV1 =
  tagged "exact_npm" {
    "integrity":{"algorithm":tagged "sha512" null,"digest":bytes},
    "maintainers":[NpmMaintainerIdentity...], "name":string,
    "npm_git_head":GitCommit,
    "package_repository":RepositoryUrl,
    "provenance":NpmProvenanceMaterialV1,
    "provenance_commit":GitCommit|null,
    "repository":{"commit":GitCommit,"tree":GitTree,"url":RepositoryUrl},
    "repository_owner":RepositoryOwnerMaterialV1,
    "signature":NpmSignatureMaterialV1,
    "tag_commit":GitCommit|null, "tarball_url":ExactHttpsUrl,
    "version":ExactSemver
  } |
  tagged "provider_artifact" {
    "evidence":[ProviderEvidenceMaterialV1...], "provider":string
  } |
  tagged "donat_owned" {
    "files":[{"path":RepoPath,"sha256":Hash256}...],
    "repository_commit":GitCommit
  }

ProviderEvidenceMaterialV1 = {
  "accessed_on":Date, "content_sha256":Hash256,
  "facts":[ProviderFactMaterialV1...],
  "source":
    tagged "repository_file" {
      "commit":GitCommit,"path":SourcePath,"repository":RepositoryUrl
    } |
    tagged "versioned_artifact" {
      "provider_revision":string,"url":ExactHttpsUrl
    },
  "terms":EvidenceTermsMaterialV1
}

EvidenceTermsMaterialV1 =
  tagged "permissive" {
    "evidence_url":ExactHttpsUrl,"license":LicenseDecisionMaterialV1
  } |
  tagged "reviewed_use" {
    "decision_id":ReviewDecisionId,"evidence_url":ExactHttpsUrl
  } |
  tagged "rejected" {"finding":FindingId}

ProviderFactMaterialV1 = {
  "fact_id":ProviderFactId,
  "location":
    tagged "json_pointer" {"path":SourcePath,"pointer":string} |
    tagged "document_section" {"path":SourcePath,"section":string},
  "normalized_value":TypedValueMaterialV1
}
```

The remaining source composites are:

```text
ReacquisitionMaterialV1 =
  tagged "exact_npm_review" null |
  tagged "provider_repository_review" null |
  tagged "provider_versioned_artifact_review" null |
  tagged "donat_owned_no_network" null

ArtifactHashMaterialV1 = {
  "algorithm":tagged "sha256" null | tagged "sha512" null,
  "artifact_id":ArtifactId, "digest":Hash256|Hash512,
  "path":SourcePath|null
}
LicenseDecisionMaterialV1 =
  tagged "permissive" {
    "license_file_path":SourcePath,"license_file_sha256":Hash256,
    "selected_dual_license_branch":string|null,"spdx_id":string
  } |
  tagged "written_grant" {
    "decision_id":ReviewDecisionId,"grant_sha256":Hash256
  } |
  tagged "rejected" {"finding":FindingId}
NoticeMaterialV1 = {
  "id":NoticeId,"license_file_path":SourcePath,
  "license_file_sha256":Hash256,
  "notice_bundle_destination":RepoPath,
  "required_copyright_lines":[string...]
}
DependencyDecisionMaterialV1 = {
  "dependency":Id,
  "disposition":
    tagged "shipped" {"license":LicenseDecisionMaterialV1} |
    tagged "build_only" {"license":LicenseDecisionMaterialV1} |
    tagged "type_only_replaced" {"replacement":Id} |
    tagged "behavior_only" {"reason":FindingId} |
    tagged "rejected" {"finding":FindingId}
}
EmbeddedDecisionMaterialV1 = {
  "disposition":
    tagged "shipped" {"license":LicenseDecisionMaterialV1} |
    tagged "behavior_only" {"reason":FindingId} |
    tagged "rejected" {"finding":FindingId},
  "material_id":Id,"path":SourcePath,"sha256":Hash256
}
ProviderContractMaterialV1 = {
  "contract_id":ProviderContractId,
  "facts":[
    tagged "provider_evidence" {
      "fact_id":ProviderFactId,"source_record_id":SourceRecordId
    } |
    tagged "donat_policy" {
      "policy_id":DonatPolicyId,"value":TypedValueMaterialV1
    }
  ...]
}
CompatibilityMaterialV1 =
  tagged "tier_a" null | tagged "tier_b" null |
  tagged "tier_c" null | tagged "rejected" null
AdmissionMaterialV1 =
  tagged "inventory_only" {"findings":[FindingId...]} |
  tagged "approved_for_port" {"operations":[OperationId...]} |
  tagged "evidence_accepted" {"contracts":[ProviderContractId...]}
SafetyFindingMaterialV1 = {
  "finding_id":FindingId,"kind":Id,"location":SourcePath|null,
  "message":string
}
SafetyFindingsMaterialV1 = {
  "findings":[SafetyFindingMaterialV1...]
}
```

`NpmSignatureMaterialV1` is tagged `verified
{registry_metadata_sha256,signatures:[{key_id,signature_sha256}]}`,
`verified_absent {registry_metadata_sha256}`, or `rejected {finding}`.
`NpmProvenanceMaterialV1` is tagged `verified
{source_commit,statement_sha256}`, `verified_absent
{registry_metadata_sha256}`, or `rejected {finding}`.
`RepositoryOwnerMaterialV1` is tagged `consistent
{package_owner,repository_owner}`, `reviewed_mismatch {decision_id}`, or
`rejected {finding}`.

Sort keys are: artifact `artifact_id`; dependency `dependency`; embedded
`material_id`; contract `contract_id`; contract fact `(kind,fact_id|policy_id)`;
safety `finding_id`; provider evidence `(source kind, canonical source bytes,
content_sha256)`; provider fact `fact_id`; Donat file `path`; npm maintainer
identity; signature `key_id`; proposed destination path; operation ID; and
test ID. Entrypoints and copyright lines retain declared order. A source
record cannot contain `record_sha256`.

### Behavioral semantic material

Semantic material never serializes `OperationSpec`, `TriggerSpec`, or any
runtime type that contains provenance. It projects each normalized behavioral
field into the following independent material structs:

```text
SemanticMaterialV1 = {
  "canonical_schema_epoch":Epoch,
  "connector":SemanticConnectorMaterialV1,
  "credentials":[SemanticCredentialMaterialV1...],
  "operations":[SemanticOperationMaterialV1...],
  "origins":[SemanticOriginMaterialV1...],
  "triggers":[SemanticTriggerMaterialV1...],
  "value_language_epoch":Epoch
}

SemanticConnectorMaterialV1 = {
  "api_identity":string,"id":ConnectorId,
  "manifest_version":Epoch,"provider":ProviderId,
  "runtime_abi_epoch":Epoch,"version":StableSemver
}
ResolvedFactValueMaterialV1 = {"use_site":Id,"value":TypedValueMaterialV1}
SemanticOriginMaterialV1 = {
  "host":string,
  "network_policy":
    tagged "public_only" null |
    tagged "private_allowed" {"policy":Id},
  "origin":OriginId,"port":u16,"scheme":tagged "https" null
}
```

Credentials are exact projections of `CredentialSpec`:

```text
SemanticCredentialMaterialV1 = {
  "allowed_origins":[OriginId...],
  "auth_plan":CredentialAuthMaterialV1,
  "auth_processor":{"id":AuthenticatorId,
                    "implementation_revision":Epoch}|null,
  "bounds":{
    "maximum_aggregate_bytes":u32,
    "maximum_field_bytes":u32,
    "maximum_token_bytes":u32
  },
  "credential":CredentialSpecId,
  "credential_test_operation":
    {"operation":OperationId,"version":StableSemver}|null,
  "fields":[{
    "field":CredentialFieldId,"maximum_bytes":u32,
    "redaction":RedactionMaterialV1,"required":bool,
    "secret":
      tagged "secret" null | tagged "sensitive" null |
      tagged "non_secret" null
  }...],
  "scopes":[string...],"version":StableSemver
}

CredentialAuthMaterialV1 =
  tagged "fixed_header_api_key" {
    "field":CredentialFieldId,"header":string
  } |
  tagged "fixed_query_api_key" {
    "field":CredentialFieldId,"query":string
  } |
  tagged "bearer" {"token":CredentialFieldId} |
  tagged "http_basic" {
    "password":CredentialFieldId,"username":CredentialFieldId
  } |
  tagged "oauth2_client_credentials" {
    "client_id":CredentialFieldId,"client_secret":CredentialFieldId,
    "scopes":[string...],"token_origin":OriginId,
    "token_pointer":string,"token_step":CompiledStepId
  } |
  tagged "preprovisioned_oauth_access_token" {
    "token":CredentialFieldId
  }

RedactionMaterialV1 =
  tagged "omit" null |
  tagged "fixed" {"replacement":string} |
  tagged "preserve_last" {"characters":u8}
```

An operation is the exact behavioral projection of every normalized
`OperationSpec` field:

```text
SemanticOperationMaterialV1 = {
  "bounds":OperationBoundsMaterialV1,
  "capacity":{"maximum_in_flight":u32},
  "connector":ConnectorId,"connector_version":StableSemver,
  "credential":{"credential":CredentialSpecId,
                "version":StableSemver}|null,
  "effect":OperationEffectMaterialV1,
  "error_map":ErrorMapMaterialV1,
  "input":ValueContractMaterialV1,
  "input_contract_sha256":Hash256,
  "operation":OperationId,
  "operation_processor":{"id":ProcessorFamilyId,
                         "implementation_revision":Epoch}|null,
  "operation_version":StableSemver,
  "origins":[SemanticOriginMaterialV1...],
  "output":ValueContractMaterialV1,
  "output_contract_sha256":Hash256,
  "pagination":PaginationMaterialV1,
  "post_response_transforms":[
    {"id":ProcessorFamilyId,"implementation_revision":Epoch}...],
  "pre_request_transforms":[
    {"id":ProcessorFamilyId,"implementation_revision":Epoch}...],
  "rate":{"burst":u32,"refill_interval_ms":u64-string},
  "resolved_fact_values":[ResolvedFactValueMaterialV1...],
  "runtime_abi_epoch":Epoch,
  "serialization_key_default":
    {"field":Id,"value":TypedValueMaterialV1}|null,
  "steps":[SemanticStepMaterialV1...],
  "value_language_epoch":Epoch
}
```

The complete step projection is:

```text
SemanticStepMaterialV1 = {
  "bounds":StepBoundsMaterialV1,
  "credential_action":{"credential":CredentialSpecId}|null,
  "headers":[{"binding":BindingMaterialV1,"name":string}...],
  "method":string,"origin":OriginId,"path":string,
  "query":[{"binding":BindingMaterialV1,"name":string}...],
  "request":
    tagged "none" null |
    tagged "json" {"bindings":[Id...]} |
    tagged "form_urlencoded" {"bindings":[Id...]} |
    tagged "multipart" {"bindings":[Id...]} |
    tagged "raw_bytes" {"binding":Id},
  "response":
    tagged "json" {
      "mappings":[{"pointer":string,"target":Id}...]
    } |
    tagged "raw_bytes" {"target":Id},
  "selected_response_headers":[{
    "canonical_lowercase_header_name":string,
    "capability":CapabilityId
  }...],
  "step":CompiledStepId,
  "success_statuses":[{"maximum":u16,"minimum":u16}...]
}
BindingMaterialV1 = {
  "default":TypedValueMaterialV1|null,"field":Id,
  "mapping":Id|null,"required":bool,
  "source":
    tagged "input" null |
    tagged "constant" {"value":TypedValueMaterialV1}
}
StepBoundsMaterialV1 = {
  "deadline_ms":u64-string,"maximum_header_bytes":u32,
  "maximum_headers":u32,"maximum_inline_binary_bytes":u32,
  "maximum_json_depth":u32,"maximum_json_nodes":u32,
  "maximum_request_bytes":u32,"maximum_response_bytes":u32,
  "maximum_url_bytes":u32
}
OperationBoundsMaterialV1 = {
  "deadline_ms":u64-string,
  "maximum_aggregate_request_bytes":u32,
  "maximum_aggregate_response_bytes":u32,
  "maximum_calls":u32,"maximum_items":u32,
  "maximum_output_canonical_bytes":u32,
  "maximum_pages":u32,"maximum_redirects":u8
}
```

Effects, pagination, and errors are branch-complete:

```text
OperationEffectMaterialV1 =
  tagged "read_only" null |
  tagged "at_most_once" null |
  tagged "provider_idempotent" {"side_effect_steps":[{
    "clock_safety_margin_ms":u64-string,
    "fixed_binding":
      tagged "header" {"name":string} |
      tagged "body_field" {"pointer":string},
    "minimum_retention_ms":u64-string,
    "scope":ProviderIdempotencyScope,"step":CompiledStepId
  }...]}

PaginationBoundsMaterialV1 = {
  "maximum_aggregate_response_bytes":u32,
  "maximum_calls":u32,"maximum_items":u32,
  "maximum_output_canonical_bytes":u32,
  "maximum_pages":u32,"maximum_response_bytes":u32
}
PaginationMaterialV1 =
  tagged "none" null |
  tagged "cursor" {
    "bounds":PaginationBoundsMaterialV1,
    "request_binding":Id,"response_pointer":string
  } |
  tagged "offset_limit" {
    "bounds":PaginationBoundsMaterialV1,
    "initial_offset":u64-string,"limit_binding":Id,
    "offset_binding":Id,"page_size":u32
  } |
  tagged "page_number" {
    "bounds":PaginationBoundsMaterialV1,
    "initial_page":u64-string,"page_binding":Id,
    "page_size":u32,"page_size_binding":Id
  } |
  tagged "link_relation" {
    "bounds":PaginationBoundsMaterialV1,"relation":string,
    "selected_header":{
      "canonical_lowercase_header_name":string,
      "capability":CapabilityId
    }
  } |
  tagged "processor" {
    "bounds":PaginationBoundsMaterialV1,
    "processor":{"id":ProcessorFamilyId,
                 "implementation_revision":Epoch}
  }

ErrorMapMaterialV1 = {
  "fallback":{
    "authentication":ErrorActionMaterialV1,
    "http_429":ErrorActionMaterialV1,
    "http_5xx":ErrorActionMaterialV1,
    "invariant":ErrorActionMaterialV1,
    "permanent":ErrorActionMaterialV1,
    "timeout":ErrorActionMaterialV1,
    "transport":ErrorActionMaterialV1,
    "validation":ErrorActionMaterialV1
  },
  "rules":[{
    "action":ErrorActionMaterialV1,
    "matcher":ErrorMatcherMaterialV1
  }...]
}
ErrorActionMaterialV1 = {
  "class":
    tagged "transport" null | tagged "timeout" null |
    tagged "http_429" null | tagged "http_5xx" null |
    tagged "authentication" null | tagged "validation" null |
    tagged "permanent" null | tagged "invariant" null,
  "code":Id,
  "correlations":[{
    "canonical_lowercase_header_name":string,
    "capability":CapabilityId,"step":CompiledStepId
  }...],
  "retry_after":
    tagged "never" null |
    tagged "retry_after_header" {
      "capability":CapabilityId,"maximum_seconds":u32,
      "step":CompiledStepId
    },
  "safe_message":string
}
ErrorMatcherMaterialV1 =
  tagged "status" {"maximum":u16,"minimum":u16} |
  tagged "provider_code" {"codes":[string...],"pointer":string} |
  tagged "header" {"name":string,"values":[string...]} |
  tagged "malformed_declared_success" null
```

Side-effect steps sort by `step`; correlations by
`(step,canonical_lowercase_header_name)`; provider codes and header values
sort lexically. Error rules, transforms, request bindings, response mappings,
and steps retain declared order. Query/header bindings sort by canonical
name; statuses sort by `(minimum,maximum)`; selected headers sort by canonical
name. Operation origins sort by `origin`. Resolved values sort by `use_site`.

Triggers are exact projections of both normalized `TriggerSpec` branches:

```text
SemanticTriggerMaterialV1 =
  tagged "webhook" {
    "authenticator":{"id":AuthenticatorId,
                     "implementation_revision":Epoch},
    "codec":{"id":CodecId,"implementation_revision":Epoch},
    "connector":ConnectorId,"connector_version":StableSemver,
    "event_id":ValueContractMaterialV1,
    "event_type":ValueContractMaterialV1,
    "event_version":StableSemver,
    "normalizer":{"id":NormalizerId,
                  "implementation_revision":Epoch},
    "output":ValueContractMaterialV1,
    "raw_body_max_bytes":u32,
    "redaction":RedactionMaterialV1,
    "runtime_abi_epoch":Epoch,
    "selected_headers":[string...],
    "subscription_operations":{
      "check":OperationId|null,"create":OperationId,
      "delete":OperationId
    }|null,
    "timestamp_window_ms":u64-string,
    "trigger":TriggerId,"trigger_version":StableSemver
  } |
  tagged "poll" {
    "bounds":OperationBoundsMaterialV1,
    "checkpoint":ValueContractMaterialV1,
    "connector":ConnectorId,"connector_version":StableSemver,
    "event_type":ValueContractMaterialV1,
    "event_version":StableSemver,
    "per_poll_event_limit":u32,
    "processor":{"id":ProcessorFamilyId,
                 "implementation_revision":Epoch},
    "runtime_abi_epoch":Epoch,
    "trigger":TriggerId,"trigger_version":StableSemver
  }
```

Each normalized `OperationSpec.resolved_fact_values` entry is serialized once
at `operations[].resolved_fact_values[]`, its stable behavioral use site. No
top-level semantic fact collection exists. Credentials sort by `credential`,
manifest origins by `origin`, operations by
`operation`, and triggers by `(kind,trigger)`. Credential fields sort by
`field`; allowed origins and scopes sort lexically. Webhook selected headers
sort by canonical lowercase name. Empty vectors remain `[]`; every optional
member remains present with `null`.

### Value-contract and typed-value material

```text
ValueContractMaterialV1 = {
  "named_objects":{name:NamedObjectMaterialV1},
  "roots":{name:FieldMaterialV1},"value_language_epoch":Epoch
}
NamedObjectMaterialV1 = {"fields":{name:FieldMaterialV1}}
FieldMaterialV1 = {"required":bool,"type_ref":TypeRefMaterialV1}
TypeRefMaterialV1 = {"nullable":bool,"value_type":ValueTypeMaterialV1}
ValueTypeMaterialV1 =
  tagged "scalar" ValueScalarMaterialV1 |
  tagged "enum" {"name":string,"values":[string...]} |
  tagged "object" {"fields":{name:FieldMaterialV1}} |
  tagged "list" {"element":TypeRefMaterialV1} |
  tagged "ref" {"name":string}
ValueScalarMaterialV1 =
  tagged "boolean" null | tagged "string" null |
  tagged "int32" null | tagged "int64" null |
  tagged "uint64" null | tagged "decimal" null |
  tagged "uuid" null | tagged "date" null |
  tagged "timestamp" null | tagged "timestamptz" null |
  tagged "json" null | tagged "custom" {"name":string}
```

`ValueScalarMaterialV1` is an exact projection of the Spec-005 Task-1 owner,
`donat_value_contract::ValueScalar`. Nullability is represented only by
`TypeRefMaterialV1.nullable`; there is no `null` scalar tag. `InlineBytes`
remains only an inert `TypedValueMaterialV1` branch and cannot enter a
value-contract scalar projection until a separately approved value-language
epoch adds a corresponding `ValueScalar`.

Maps use UTF-16 member order; enum values retain declared order; the full
named-object closure, including unreachable declarations, is present.

`TypedValueMaterialV1` is tagged: `null` with null; `boolean`; `string`;
`i64`, `u64`, and `decimal` with exact decimal strings; `list` with ordered
typed values; `object` with recursively JCS-ordered members; or
`inline_bytes` with
`{"$binary":bytes,"file_name":string|null,"media_type":string|null}`.
This catalog adapter adds no Serde/JCS dependency to
`donat-value-contract` and does not alter its separate `canonical_size`
contract.

### Provenance material

The provenance projection is field-total for normalized
`ManifestProvenanceReference` plus the accepted records it resolves. It never
contains a resolved fact value:

```text
ProvenanceMaterialV1 = {
  "artifacts":[ArtifactDecisionMaterialV1...],
  "canonical_schema_epoch":Epoch,"classifier_epoch":Epoch,
  "connector":{
    "id":ConnectorId,"semantic_sha256":Hash256,
    "version":StableSemver
  },
  "dependencies":[DependencyDecisionMaterialV1...],
  "donat_policy_ids":[DonatPolicyId...],
  "embedded_material":[EmbeddedDecisionMaterialV1...],
  "files":[FileDecisionMaterialV1...],"generator_epoch":Epoch,
  "licenses":[LicenseDecisionMaterialV1...],
  "manifest_references":[ManifestProvenanceMaterialV1...],
  "notices":[NoticeMaterialV1...],
  "provider_evidence":[ProviderEvidenceOriginMaterialV1...],
  "sources":[SourceIdentityMaterialV1...]
}
SourceIdentityMaterialV1 = {
  "record_id":SourceRecordId,"record_sha256":Hash256
}
ArtifactDecisionMaterialV1 = {
  "algorithm":tagged "sha256" null | tagged "sha512" null,
  "artifact_id":ArtifactId,"digest":Hash256|Hash512,
  "path":SourcePath|null,"source_record_id":SourceRecordId
}
FileDecisionMaterialV1 = {
  "path":RepoPath,"sha256":Hash256,
  "source_record_id":SourceRecordId
}
ManifestProvenanceMaterialV1 = {
  "artifact_hashes":[ArtifactHashMaterialV1...],
  "contract_fact_origins":[ResolvedFactOriginMaterialV1...],
  "license_id":LicenseIdentity,"notice_id":NoticeId,
  "source_record_id":SourceRecordId
}
ProviderEvidenceOriginMaterialV1 = {
  "evidence":[{
    "accessed_on":Date,"content_sha256":Hash256,
    "facts":[{
      "fact_id":ProviderFactId,
      "location":
        tagged "json_pointer" {"path":SourcePath,"pointer":string} |
        tagged "document_section" {"path":SourcePath,"section":string}
    }...],
    "source":
      tagged "repository_file" {
        "commit":GitCommit,"path":SourcePath,
        "repository":RepositoryUrl
      } |
      tagged "versioned_artifact" {
        "provider_revision":string,"url":ExactHttpsUrl
      },
    "terms":EvidenceTermsMaterialV1
  }...],
  "provider":string,"source_record_id":SourceRecordId
}
ResolvedFactOriginMaterialV1 = {
  "origin":
    tagged "provider_evidence" {
      "artifact_content_sha256":Hash256,
      "fact_id":ProviderFactId,
      "location":
        tagged "json_pointer" {"path":SourcePath,"pointer":string} |
        tagged "document_section" {"path":SourcePath,"section":string},
      "source_record_id":SourceRecordId
    } |
    tagged "donat_policy" {"policy_id":DonatPolicyId},
  "use_site":Id
}
```

Each normalized `ManifestProvenanceReference.contract_facts` entry is
serialized once at
`manifest_references[].contract_fact_origins[]`, the matching stable
provenance use site. No top-level provenance fact-origin collection exists.
The provider-evidence inventory remains distinct immutable source evidence:
its facts retain identity and location but never add a resolved use-site
origin.

`EvidenceTermsMaterialV1` is the same complete `permissive`, `reviewed_use`,
or `rejected` tagged branch used in source material; it has no fact value to
remove. Sources sort by `record_id`;
artifacts by `(source_record_id,artifact_id)`; files by
`(source_record_id,path)`; licenses by canonical bytes; dependencies and
embedded material by their normalized ID; notices by `id`; manifest
references and provider evidence by `source_record_id`; evidence by canonical
source identity; facts by `fact_id`; contract-fact origins by `use_site`;
policy IDs
lexically.

Direct provenance-origin bytes do not change when only a resolved value
changes. Final provenance bytes and hash do change because
`connector.semantic_sha256` commits the resulting semantic hash. Conversely,
an origin-only mutation leaves semantic bytes and hash unchanged.

### Normative machine-checkable owner manifest

The block below is the exact Task-3 test input. It is pipe-delimited UTF-8
with one header and one mapping per line:
`normalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type`.
`[]` means one array element and `{kind=x}` qualifies a mutually exclusive
tag. Paths are schema paths, so a composite member recursively uses the
separately listed composite owner. `owner_class` is `normalized`, `constant`,
or an exact named `derived:<rule>`. A normalized owner may occur in different
domains. Inside one direct material it has one path unless a named derived
aggregate owns a different value.

```text
normalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type
StableSemver.major|semantic|StableSemver.major|normalized|scalar|required|u32
StableSemver.minor|semantic|StableSemver.minor|normalized|scalar|required|u32
StableSemver.patch|semantic|StableSemver.patch|normalized|scalar|required|u32
StableSemver.major|provenance|StableSemver.major|normalized|scalar|required|u32
StableSemver.minor|provenance|StableSemver.minor|normalized|scalar|required|u32
StableSemver.patch|provenance|StableSemver.patch|normalized|scalar|required|u32
ConnectorSourceRecord.record_version|source-record|SourceRecordMaterialV1.record_version|normalized|scalar|required|Epoch
ConnectorSourceRecord.record_id|source-record|SourceRecordMaterialV1.record_id|normalized|scalar|required|SourceRecordId
ConnectorSourceRecord.subject|source-record|SourceRecordMaterialV1.subject|normalized|scalar|required|SourceSubjectMaterialV1
ConnectorSourceRecord.reacquisition|source-record|SourceRecordMaterialV1.reacquisition|normalized|scalar|required|ReacquisitionMaterialV1
ConnectorSourceRecord.artifact_hashes|source-record|SourceRecordMaterialV1.artifact_hashes|normalized|artifact_id|empty_array|Vec<ArtifactHashMaterialV1>
ConnectorSourceRecord.license|source-record|SourceRecordMaterialV1.license|normalized|scalar|required|LicenseDecisionMaterialV1
ConnectorSourceRecord.notice|source-record|SourceRecordMaterialV1.notice|normalized|scalar|required|NoticeMaterialV1
ConnectorSourceRecord.entrypoints|source-record|SourceRecordMaterialV1.entrypoints|normalized|declared|empty_array|Vec<SourcePath>
ConnectorSourceRecord.dependencies|source-record|SourceRecordMaterialV1.dependencies|normalized|dependency|empty_array|Vec<DependencyDecisionMaterialV1>
ConnectorSourceRecord.embedded_material|source-record|SourceRecordMaterialV1.embedded_material|normalized|material_id|empty_array|Vec<EmbeddedDecisionMaterialV1>
ConnectorSourceRecord.provider_contracts|source-record|SourceRecordMaterialV1.provider_contracts|normalized|contract_id|empty_array|Vec<ProviderContractMaterialV1>
ConnectorSourceRecord.compatibility|source-record|SourceRecordMaterialV1.compatibility|normalized|scalar|required|CompatibilityMaterialV1
ConnectorSourceRecord.admission|source-record|SourceRecordMaterialV1.admission|normalized|scalar|required|AdmissionMaterialV1
ConnectorSourceRecord.safety_findings|source-record|SourceRecordMaterialV1.safety_findings|normalized|scalar|required|SafetyFindingsMaterialV1
ConnectorSourceRecord.reviewer|source-record|SourceRecordMaterialV1.reviewer|normalized|scalar|required|ReviewIdentity
ConnectorSourceRecord.approval_date|source-record|SourceRecordMaterialV1.approval_date|normalized|scalar|required|Date
ConnectorSourceRecord.proposed_manifest|source-record|SourceRecordMaterialV1.proposed_manifest|normalized|scalar|explicit_null|Option<RepoPath>
ConnectorSourceRecord.proposed_destinations|source-record|SourceRecordMaterialV1.proposed_destinations|normalized|lexical|nonempty_array|NonEmptyVec<RepoPath>
ConnectorSourceRecord.red_tests|source-record|SourceRecordMaterialV1.red_tests|normalized|lexical|nonempty_array|NonEmptyVec<TestId>
SourceSubject::ExactNpm|source-record|SourceSubjectMaterialV1{kind=exact_npm}.kind|normalized|scalar|required|exact_npm
SourceSubject::ProviderArtifact|source-record|SourceSubjectMaterialV1{kind=provider_artifact}.kind|normalized|scalar|required|provider_artifact
SourceSubject::DonatOwned|source-record|SourceSubjectMaterialV1{kind=donat_owned}.kind|normalized|scalar|required|donat_owned
ExactNpmPackage.name|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.name|normalized|scalar|required|string
ExactNpmPackage.version|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.version|normalized|scalar|required|ExactSemver
ExactNpmPackage.tarball_url|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.tarball_url|normalized|scalar|required|ExactHttpsUrl
ExactNpmPackage.integrity|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.integrity|normalized|scalar|required|NpmIntegrity
ExactNpmPackage.repository|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.repository|normalized|scalar|required|ImmutableRepository
ExactNpmPackage.npm_git_head|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.npm_git_head|normalized|scalar|required|GitCommit
ExactNpmPackage.package_repository|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.package_repository|normalized|scalar|required|RepositoryUrl
ExactNpmPackage.signature|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.signature|normalized|scalar|required|NpmSignatureMaterialV1
ExactNpmPackage.provenance|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.provenance|normalized|scalar|required|NpmProvenanceMaterialV1
ExactNpmPackage.tag_commit|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.tag_commit|normalized|scalar|explicit_null|Option<GitCommit>
ExactNpmPackage.provenance_commit|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.provenance_commit|normalized|scalar|explicit_null|Option<GitCommit>
ExactNpmPackage.maintainers|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.maintainers|normalized|identity|empty_array|Vec<NpmMaintainerIdentity>
ExactNpmPackage.repository_owner|source-record|SourceSubjectMaterialV1{kind=exact_npm}.value.repository_owner|normalized|scalar|required|RepositoryOwnerMaterialV1
NpmIntegrity.algorithm|source-record|NpmIntegrity.algorithm|normalized|scalar|required|sha512
NpmIntegrity.digest|source-record|NpmIntegrity.digest|normalized|scalar|required|bytes64
ImmutableRepository.url|source-record|ImmutableRepository.url|normalized|scalar|required|RepositoryUrl
ImmutableRepository.commit|source-record|ImmutableRepository.commit|normalized|scalar|required|GitCommit
ImmutableRepository.tree|source-record|ImmutableRepository.tree|normalized|scalar|required|GitTree
NpmSignatureDecision::Verified|source-record|NpmSignatureMaterialV1{kind=verified}.kind|normalized|scalar|required|verified
NpmSignatureDecision::VerifiedAbsent|source-record|NpmSignatureMaterialV1{kind=verified_absent}.kind|normalized|scalar|required|verified_absent
NpmSignatureDecision::Rejected|source-record|NpmSignatureMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
NpmSignatureDecision::Verified.signatures|source-record|NpmSignatureMaterialV1{kind=verified}.value.signatures|normalized|key_id|nonempty_array|NonEmptyVec<VerifiedNpmSignature>
NpmSignatureDecision::Verified.registry_metadata_sha256|source-record|NpmSignatureMaterialV1{kind=verified}.value.registry_metadata_sha256|normalized|scalar|required|Hash256
NpmSignatureDecision::VerifiedAbsent.registry_metadata_sha256|source-record|NpmSignatureMaterialV1{kind=verified_absent}.value.registry_metadata_sha256|normalized|scalar|required|Hash256
NpmSignatureDecision::Rejected.finding|source-record|NpmSignatureMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
VerifiedNpmSignature.key_id|source-record|VerifiedNpmSignatureMaterialV1.key_id|normalized|scalar|required|Id
VerifiedNpmSignature.signature_sha256|source-record|VerifiedNpmSignatureMaterialV1.signature_sha256|normalized|scalar|required|Hash256
NpmProvenanceDecision::Verified|source-record|NpmProvenanceMaterialV1{kind=verified}.kind|normalized|scalar|required|verified
NpmProvenanceDecision::VerifiedAbsent|source-record|NpmProvenanceMaterialV1{kind=verified_absent}.kind|normalized|scalar|required|verified_absent
NpmProvenanceDecision::Rejected|source-record|NpmProvenanceMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
NpmProvenanceDecision::Verified.statement_sha256|source-record|NpmProvenanceMaterialV1{kind=verified}.value.statement_sha256|normalized|scalar|required|Hash256
NpmProvenanceDecision::Verified.source_commit|source-record|NpmProvenanceMaterialV1{kind=verified}.value.source_commit|normalized|scalar|required|GitCommit
NpmProvenanceDecision::VerifiedAbsent.registry_metadata_sha256|source-record|NpmProvenanceMaterialV1{kind=verified_absent}.value.registry_metadata_sha256|normalized|scalar|required|Hash256
NpmProvenanceDecision::Rejected.finding|source-record|NpmProvenanceMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
RepositoryOwnerDecision::Consistent|source-record|RepositoryOwnerMaterialV1{kind=consistent}.kind|normalized|scalar|required|consistent
RepositoryOwnerDecision::ReviewedMismatch|source-record|RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.kind|normalized|scalar|required|reviewed_mismatch
RepositoryOwnerDecision::Rejected|source-record|RepositoryOwnerMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
RepositoryOwnerDecision::Consistent.package_owner|source-record|RepositoryOwnerMaterialV1{kind=consistent}.value.package_owner|normalized|scalar|required|NpmOwnerIdentity
RepositoryOwnerDecision::Consistent.repository_owner|source-record|RepositoryOwnerMaterialV1{kind=consistent}.value.repository_owner|normalized|scalar|required|RepositoryOwnerIdentity
RepositoryOwnerDecision::ReviewedMismatch.decision_id|source-record|RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.value.decision_id|normalized|scalar|required|ReviewDecisionId
RepositoryOwnerDecision::Rejected.finding|source-record|RepositoryOwnerMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
ExactProviderArtifact.provider|source-record|SourceSubjectMaterialV1{kind=provider_artifact}.value.provider|normalized|scalar|required|string
ExactProviderArtifact.evidence|source-record|SourceSubjectMaterialV1{kind=provider_artifact}.value.evidence|normalized|canonical_source_identity_content_sha256|nonempty_array|NonEmptyVec<ProviderEvidenceMaterialV1>
ProviderEvidenceArtifact.source|source-record|ProviderEvidenceMaterialV1.source|normalized|scalar|required|ImmutableProviderEvidenceSource
ProviderEvidenceArtifact.accessed_on|source-record|ProviderEvidenceMaterialV1.accessed_on|normalized|scalar|required|Date
ProviderEvidenceArtifact.content_sha256|source-record|ProviderEvidenceMaterialV1.content_sha256|normalized|scalar|required|Hash256
ProviderEvidenceArtifact.terms|source-record|ProviderEvidenceMaterialV1.terms|normalized|scalar|required|EvidenceTermsMaterialV1
ProviderEvidenceArtifact.facts|source-record|ProviderEvidenceMaterialV1.facts|normalized|fact_id|nonempty_array|NonEmptyVec<ProviderFactMaterialV1>
ImmutableProviderEvidenceSource::RepositoryFile|source-record|ProviderEvidenceSourceMaterialV1{kind=repository_file}.kind|normalized|scalar|required|repository_file
ImmutableProviderEvidenceSource::VersionedArtifact|source-record|ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.kind|normalized|scalar|required|versioned_artifact
ImmutableProviderEvidenceSource::RepositoryFile.repository|source-record|ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.repository|normalized|scalar|required|RepositoryUrl
ImmutableProviderEvidenceSource::RepositoryFile.commit|source-record|ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.commit|normalized|scalar|required|GitCommit
ImmutableProviderEvidenceSource::RepositoryFile.path|source-record|ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.path|normalized|scalar|required|SourcePath
ImmutableProviderEvidenceSource::VersionedArtifact.url|source-record|ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.url|normalized|scalar|required|ExactHttpsUrl
ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision|source-record|ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.provider_revision|normalized|scalar|required|NonEmptyString
EvidenceTermsDisposition::Permissive|source-record|EvidenceTermsMaterialV1{kind=permissive}.kind|normalized|scalar|required|permissive
EvidenceTermsDisposition::ReviewedUse|source-record|EvidenceTermsMaterialV1{kind=reviewed_use}.kind|normalized|scalar|required|reviewed_use
EvidenceTermsDisposition::Rejected|source-record|EvidenceTermsMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
EvidenceTermsDisposition::Permissive.license|source-record|EvidenceTermsMaterialV1{kind=permissive}.value.license|normalized|scalar|required|LicenseDecisionMaterialV1
EvidenceTermsDisposition::Permissive.evidence_url|source-record|EvidenceTermsMaterialV1{kind=permissive}.value.evidence_url|normalized|scalar|required|ExactHttpsUrl
EvidenceTermsDisposition::ReviewedUse.decision_id|source-record|EvidenceTermsMaterialV1{kind=reviewed_use}.value.decision_id|normalized|scalar|required|ReviewDecisionId
EvidenceTermsDisposition::ReviewedUse.evidence_url|source-record|EvidenceTermsMaterialV1{kind=reviewed_use}.value.evidence_url|normalized|scalar|required|ExactHttpsUrl
EvidenceTermsDisposition::Rejected.finding|source-record|EvidenceTermsMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
ProviderFact.fact_id|source-record|ProviderFactMaterialV1.fact_id|normalized|scalar|required|ProviderFactId
ProviderFact.location|source-record|ProviderFactMaterialV1.location|normalized|scalar|required|ExactFactLocationMaterialV1
ProviderFact.normalized_value|source-record|ProviderFactMaterialV1.normalized_value|normalized|scalar|required|TypedValueMaterialV1
ExactFactLocation::JsonPointer|source-record|ExactFactLocationMaterialV1{kind=json_pointer}.kind|normalized|scalar|required|json_pointer
ExactFactLocation::DocumentSection|source-record|ExactFactLocationMaterialV1{kind=document_section}.kind|normalized|scalar|required|document_section
ExactFactLocation::JsonPointer.path|source-record|ExactFactLocationMaterialV1{kind=json_pointer}.value.path|normalized|scalar|required|SourcePath
ExactFactLocation::JsonPointer.pointer|source-record|ExactFactLocationMaterialV1{kind=json_pointer}.value.pointer|normalized|scalar|required|StaticJsonPointer
ExactFactLocation::DocumentSection.path|source-record|ExactFactLocationMaterialV1{kind=document_section}.value.path|normalized|scalar|required|SourcePath
ExactFactLocation::DocumentSection.section|source-record|ExactFactLocationMaterialV1{kind=document_section}.value.section|normalized|scalar|required|string
DonatOwnedSource.repository_commit|source-record|SourceSubjectMaterialV1{kind=donat_owned}.value.repository_commit|normalized|scalar|required|GitCommit
DonatOwnedSource.files|source-record|SourceSubjectMaterialV1{kind=donat_owned}.value.files|normalized|path|nonempty_array|NonEmptyVec<RepoFileHash>
RepoFileHash.path|source-record|RepoFileHashMaterialV1.path|normalized|scalar|required|RepoPath
RepoFileHash.sha256|source-record|RepoFileHashMaterialV1.sha256|normalized|scalar|required|Hash256
ReacquisitionPlan::ExactNpmReview|source-record|ReacquisitionMaterialV1{kind=exact_npm_review}.kind|normalized|scalar|required|exact_npm_review
ReacquisitionPlan::ProviderRepositoryReview|source-record|ReacquisitionMaterialV1{kind=provider_repository_review}.kind|normalized|scalar|required|provider_repository_review
ReacquisitionPlan::ProviderVersionedArtifactReview|source-record|ReacquisitionMaterialV1{kind=provider_versioned_artifact_review}.kind|normalized|scalar|required|provider_versioned_artifact_review
ReacquisitionPlan::DonatOwnedNoNetwork|source-record|ReacquisitionMaterialV1{kind=donat_owned_no_network}.kind|normalized|scalar|required|donat_owned_no_network
ArtifactHash.artifact_id|source-record|ArtifactHashMaterialV1.artifact_id|normalized|scalar|required|ArtifactId
ArtifactHash.algorithm|source-record|ArtifactHashMaterialV1.algorithm|normalized|scalar|required|HashAlgorithm
ArtifactHash.digest|source-record|ArtifactHashMaterialV1.digest|normalized|scalar|required|Hash256_or_Hash512
ArtifactHash.path|source-record|ArtifactHashMaterialV1.path|normalized|scalar|explicit_null|Option<SourcePath>
HashAlgorithm::Sha256|source-record|HashAlgorithmMaterialV1{kind=sha256}.kind|normalized|scalar|required|sha256
HashAlgorithm::Sha512|source-record|HashAlgorithmMaterialV1{kind=sha512}.kind|normalized|scalar|required|sha512
LicenseDecision::Permissive|source-record|LicenseDecisionMaterialV1{kind=permissive}.kind|normalized|scalar|required|permissive
LicenseDecision::WrittenGrant|source-record|LicenseDecisionMaterialV1{kind=written_grant}.kind|normalized|scalar|required|written_grant
LicenseDecision::Rejected|source-record|LicenseDecisionMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
LicenseDecision::Permissive.spdx_id|source-record|LicenseDecisionMaterialV1{kind=permissive}.value.spdx_id|normalized|scalar|required|string
LicenseDecision::Permissive.selected_dual_license_branch|source-record|LicenseDecisionMaterialV1{kind=permissive}.value.selected_dual_license_branch|normalized|scalar|explicit_null|Option<string>
LicenseDecision::Permissive.license_file_path|source-record|LicenseDecisionMaterialV1{kind=permissive}.value.license_file_path|normalized|scalar|required|SourcePath
LicenseDecision::Permissive.license_file_sha256|source-record|LicenseDecisionMaterialV1{kind=permissive}.value.license_file_sha256|normalized|scalar|required|Hash256
LicenseDecision::WrittenGrant.decision_id|source-record|LicenseDecisionMaterialV1{kind=written_grant}.value.decision_id|normalized|scalar|required|ReviewDecisionId
LicenseDecision::WrittenGrant.grant_sha256|source-record|LicenseDecisionMaterialV1{kind=written_grant}.value.grant_sha256|normalized|scalar|required|Hash256
LicenseDecision::Rejected.finding|source-record|LicenseDecisionMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
NoticeIdentity.id|source-record|NoticeMaterialV1.id|normalized|scalar|required|NoticeId
NoticeIdentity.license_file_path|source-record|NoticeMaterialV1.license_file_path|normalized|scalar|required|SourcePath
NoticeIdentity.license_file_sha256|source-record|NoticeMaterialV1.license_file_sha256|normalized|scalar|required|Hash256
NoticeIdentity.required_copyright_lines|source-record|NoticeMaterialV1.required_copyright_lines|normalized|declared|empty_array|Vec<string>
NoticeIdentity.notice_bundle_destination|source-record|NoticeMaterialV1.notice_bundle_destination|normalized|scalar|required|RepoPath
DependencyDecision.dependency|source-record|DependencyDecisionMaterialV1.dependency|normalized|scalar|required|Id
DependencyDecision.disposition|source-record|DependencyDecisionMaterialV1.disposition|normalized|scalar|required|DependencyDispositionMaterialV1
DependencyDisposition::Shipped|source-record|DependencyDispositionMaterialV1{kind=shipped}.kind|normalized|scalar|required|shipped
DependencyDisposition::BuildOnly|source-record|DependencyDispositionMaterialV1{kind=build_only}.kind|normalized|scalar|required|build_only
DependencyDisposition::TypeOnlyReplaced|source-record|DependencyDispositionMaterialV1{kind=type_only_replaced}.kind|normalized|scalar|required|type_only_replaced
DependencyDisposition::BehaviorOnly|source-record|DependencyDispositionMaterialV1{kind=behavior_only}.kind|normalized|scalar|required|behavior_only
DependencyDisposition::Rejected|source-record|DependencyDispositionMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
DependencyDisposition::Shipped.license|source-record|DependencyDispositionMaterialV1{kind=shipped}.value.license|normalized|scalar|required|LicenseDecisionMaterialV1
DependencyDisposition::BuildOnly.license|source-record|DependencyDispositionMaterialV1{kind=build_only}.value.license|normalized|scalar|required|LicenseDecisionMaterialV1
DependencyDisposition::TypeOnlyReplaced.replacement|source-record|DependencyDispositionMaterialV1{kind=type_only_replaced}.value.replacement|normalized|scalar|required|Id
DependencyDisposition::BehaviorOnly.reason|source-record|DependencyDispositionMaterialV1{kind=behavior_only}.value.reason|normalized|scalar|required|FindingId
DependencyDisposition::Rejected.finding|source-record|DependencyDispositionMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
EmbeddedMaterialDecision.material_id|source-record|EmbeddedDecisionMaterialV1.material_id|normalized|scalar|required|Id
EmbeddedMaterialDecision.path|source-record|EmbeddedDecisionMaterialV1.path|normalized|scalar|required|SourcePath
EmbeddedMaterialDecision.sha256|source-record|EmbeddedDecisionMaterialV1.sha256|normalized|scalar|required|Hash256
EmbeddedMaterialDecision.disposition|source-record|EmbeddedDecisionMaterialV1.disposition|normalized|scalar|required|EmbeddedMaterialDispositionMaterialV1
EmbeddedMaterialDisposition::Shipped|source-record|EmbeddedMaterialDispositionMaterialV1{kind=shipped}.kind|normalized|scalar|required|shipped
EmbeddedMaterialDisposition::BehaviorOnly|source-record|EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.kind|normalized|scalar|required|behavior_only
EmbeddedMaterialDisposition::Rejected|source-record|EmbeddedMaterialDispositionMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
EmbeddedMaterialDisposition::Shipped.license|source-record|EmbeddedMaterialDispositionMaterialV1{kind=shipped}.value.license|normalized|scalar|required|LicenseDecisionMaterialV1
EmbeddedMaterialDisposition::BehaviorOnly.reason|source-record|EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.value.reason|normalized|scalar|required|FindingId
EmbeddedMaterialDisposition::Rejected.finding|source-record|EmbeddedMaterialDispositionMaterialV1{kind=rejected}.value.finding|normalized|scalar|required|FindingId
ProviderContractReference.contract_id|source-record|ProviderContractMaterialV1.contract_id|normalized|scalar|required|ProviderContractId
ProviderContractReference.facts|source-record|ProviderContractMaterialV1.facts|normalized|kind_then_fact_or_policy_id|nonempty_array|NonEmptyVec<ContractFactMaterialV1>
ContractFact::ProviderEvidence|source-record|ContractFactMaterialV1{kind=provider_evidence}.kind|normalized|scalar|required|provider_evidence
ContractFact::DonatPolicy|source-record|ContractFactMaterialV1{kind=donat_policy}.kind|normalized|scalar|required|donat_policy
ContractFact::ProviderEvidence.source_record_id|source-record|ContractFactMaterialV1{kind=provider_evidence}.value.source_record_id|normalized|scalar|required|SourceRecordId
ContractFact::ProviderEvidence.fact_id|source-record|ContractFactMaterialV1{kind=provider_evidence}.value.fact_id|normalized|scalar|required|ProviderFactId
ContractFact::DonatPolicy.policy_id|source-record|ContractFactMaterialV1{kind=donat_policy}.value.policy_id|normalized|scalar|required|DonatPolicyId
ContractFact::DonatPolicy.value|source-record|ContractFactMaterialV1{kind=donat_policy}.value.value|normalized|scalar|required|TypedValueMaterialV1
CompatibilityDecision::TierA|source-record|CompatibilityMaterialV1{kind=tier_a}.kind|normalized|scalar|required|tier_a
CompatibilityDecision::TierB|source-record|CompatibilityMaterialV1{kind=tier_b}.kind|normalized|scalar|required|tier_b
CompatibilityDecision::TierC|source-record|CompatibilityMaterialV1{kind=tier_c}.kind|normalized|scalar|required|tier_c
CompatibilityDecision::Rejected|source-record|CompatibilityMaterialV1{kind=rejected}.kind|normalized|scalar|required|rejected
AdmissionState::InventoryOnly|source-record|AdmissionMaterialV1{kind=inventory_only}.kind|normalized|scalar|required|inventory_only
AdmissionState::ApprovedForPort|source-record|AdmissionMaterialV1{kind=approved_for_port}.kind|normalized|scalar|required|approved_for_port
AdmissionState::EvidenceAccepted|source-record|AdmissionMaterialV1{kind=evidence_accepted}.kind|normalized|scalar|required|evidence_accepted
AdmissionState::InventoryOnly.findings|source-record|AdmissionMaterialV1{kind=inventory_only}.value.findings|normalized|lexical|nonempty_array|NonEmptyVec<FindingId>
AdmissionState::ApprovedForPort.operations|source-record|AdmissionMaterialV1{kind=approved_for_port}.value.operations|normalized|lexical|nonempty_array|NonEmptyVec<OperationId>
AdmissionState::EvidenceAccepted.contracts|source-record|AdmissionMaterialV1{kind=evidence_accepted}.value.contracts|normalized|lexical|nonempty_array|NonEmptyVec<ProviderContractId>
SafetyFindings.findings|source-record|SafetyFindingsMaterialV1.findings|normalized|finding_id|empty_array|Vec<SafetyFindingMaterialV1>
SafetyFinding.finding_id|source-record|SafetyFindingMaterialV1.finding_id|normalized|scalar|required|FindingId
SafetyFinding.kind|source-record|SafetyFindingMaterialV1.kind|normalized|scalar|required|Id
SafetyFinding.location|source-record|SafetyFindingMaterialV1.location|normalized|scalar|explicit_null|Option<SourcePath>
SafetyFinding.message|source-record|SafetyFindingMaterialV1.message|normalized|scalar|required|string
ConnectorManifest.connector|semantic|SemanticMaterialV1.connector.id|normalized|scalar|required|ConnectorId
ConnectorManifest.connector_version|semantic|SemanticMaterialV1.connector.version|normalized|scalar|required|StableSemver
ConnectorManifest.connector|provenance|ProvenanceMaterialV1.connector.id|normalized|scalar|required|ConnectorId
ConnectorManifest.connector_version|provenance|ProvenanceMaterialV1.connector.version|normalized|scalar|required|StableSemver
ConnectorManifest.manifest_version|semantic|SemanticMaterialV1.connector.manifest_version|normalized|scalar|required|Epoch
ConnectorManifest.runtime_abi_epoch|semantic|SemanticMaterialV1.connector.runtime_abi_epoch|normalized|scalar|required|Epoch
ConnectorManifest.value_language_epoch|semantic|SemanticMaterialV1.value_language_epoch|normalized|scalar|required|Epoch
ConnectorManifest.provider|semantic|SemanticMaterialV1.connector.provider|normalized|scalar|required|ProviderId
ConnectorManifest.api_identity|semantic|SemanticMaterialV1.connector.api_identity|normalized|scalar|required|ApiIdentity
ConnectorManifest.credentials|semantic|SemanticMaterialV1.credentials|normalized|credential|empty_array|Vec<SemanticCredentialMaterialV1>
ConnectorManifest.origins|semantic|SemanticMaterialV1.origins|normalized|origin|nonempty_array|NonEmptyVec<SemanticOriginMaterialV1>
ConnectorManifest.operations|semantic|SemanticMaterialV1.operations|normalized|operation|nonempty_array|NonEmptyVec<SemanticOperationMaterialV1>
ConnectorManifest.triggers|semantic|SemanticMaterialV1.triggers|normalized|kind_then_trigger|empty_array|Vec<SemanticTriggerMaterialV1>
ConnectorManifest.provenance|provenance|ProvenanceMaterialV1.manifest_references|normalized|source_record_id|nonempty_array|NonEmptyVec<ManifestProvenanceMaterialV1>
CredentialSpec.credential|semantic|SemanticCredentialMaterialV1.credential|normalized|scalar|required|CredentialSpecId
CredentialSpec.version|semantic|SemanticCredentialMaterialV1.version|normalized|scalar|required|StableSemver
CredentialSpec.fields|semantic|SemanticCredentialMaterialV1.fields|normalized|field|nonempty_array|NonEmptyVec<CredentialFieldMaterialV1>
CredentialSpec.auth_plan|semantic|SemanticCredentialMaterialV1.auth_plan|normalized|scalar|required|CredentialAuthMaterialV1
CredentialSpec.allowed_origins|semantic|SemanticCredentialMaterialV1.allowed_origins|normalized|lexical|nonempty_array|NonEmptyVec<OriginId>
CredentialSpec.scopes|semantic|SemanticCredentialMaterialV1.scopes|normalized|lexical|empty_array|Vec<StaticScope>
CredentialSpec.auth_processor|semantic|SemanticCredentialMaterialV1.auth_processor|normalized|scalar|explicit_null|Option<VersionedProcessorRef>
CredentialSpec.credential_test_operation|semantic|SemanticCredentialMaterialV1.credential_test_operation|normalized|scalar|explicit_null|Option<VersionedOperationReference>
CredentialSpec.bounds|semantic|SemanticCredentialMaterialV1.bounds|normalized|scalar|required|CredentialBoundsMaterialV1
CredentialFieldSpec.field|semantic|CredentialFieldMaterialV1.field|normalized|scalar|required|CredentialFieldId
CredentialFieldSpec.required|semantic|CredentialFieldMaterialV1.required|normalized|scalar|required|bool
CredentialFieldSpec.secret|semantic|CredentialFieldMaterialV1.secret|normalized|scalar|required|SecretClassificationMaterialV1
CredentialFieldSpec.maximum_bytes|semantic|CredentialFieldMaterialV1.maximum_bytes|normalized|scalar|required|u32
CredentialFieldSpec.redaction|semantic|CredentialFieldMaterialV1.redaction|normalized|scalar|required|RedactionMaterialV1
CredentialBounds.maximum_field_bytes|semantic|CredentialBoundsMaterialV1.maximum_field_bytes|normalized|scalar|required|u32
CredentialBounds.maximum_aggregate_bytes|semantic|CredentialBoundsMaterialV1.maximum_aggregate_bytes|normalized|scalar|required|u32
CredentialBounds.maximum_token_bytes|semantic|CredentialBoundsMaterialV1.maximum_token_bytes|normalized|scalar|required|u32
SecretClassification::Secret|semantic|SecretClassificationMaterialV1{kind=secret}.kind|normalized|scalar|required|secret
SecretClassification::Sensitive|semantic|SecretClassificationMaterialV1{kind=sensitive}.kind|normalized|scalar|required|sensitive
SecretClassification::NonSecret|semantic|SecretClassificationMaterialV1{kind=non_secret}.kind|normalized|scalar|required|non_secret
RedactionPlan::Omit|semantic|RedactionMaterialV1{kind=omit}.kind|normalized|scalar|required|omit
RedactionPlan::Fixed|semantic|RedactionMaterialV1{kind=fixed}.kind|normalized|scalar|required|fixed
RedactionPlan::PreserveLast|semantic|RedactionMaterialV1{kind=preserve_last}.kind|normalized|scalar|required|preserve_last
RedactionPlan::Fixed.replacement|semantic|RedactionMaterialV1{kind=fixed}.value.replacement|normalized|scalar|required|string
RedactionPlan::PreserveLast.characters|semantic|RedactionMaterialV1{kind=preserve_last}.value.characters|normalized|scalar|required|u8
AuthPlan::FixedHeaderApiKey|semantic|CredentialAuthMaterialV1{kind=fixed_header_api_key}.kind|normalized|scalar|required|fixed_header_api_key
AuthPlan::FixedQueryApiKey|semantic|CredentialAuthMaterialV1{kind=fixed_query_api_key}.kind|normalized|scalar|required|fixed_query_api_key
AuthPlan::Bearer|semantic|CredentialAuthMaterialV1{kind=bearer}.kind|normalized|scalar|required|bearer
AuthPlan::HttpBasic|semantic|CredentialAuthMaterialV1{kind=http_basic}.kind|normalized|scalar|required|http_basic
AuthPlan::OAuth2ClientCredentials|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.kind|normalized|scalar|required|oauth2_client_credentials
AuthPlan::PreprovisionedOAuthAccessToken|semantic|CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.kind|normalized|scalar|required|preprovisioned_oauth_access_token
AuthPlan::FixedHeaderApiKey.field|semantic|CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.field|normalized|scalar|required|CredentialFieldId
AuthPlan::FixedHeaderApiKey.header|semantic|CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.header|normalized|scalar|required|StaticHeaderName
AuthPlan::FixedQueryApiKey.field|semantic|CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.field|normalized|scalar|required|CredentialFieldId
AuthPlan::FixedQueryApiKey.query|semantic|CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.query|normalized|scalar|required|StaticQueryKey
AuthPlan::Bearer.token|semantic|CredentialAuthMaterialV1{kind=bearer}.value.token|normalized|scalar|required|CredentialFieldId
AuthPlan::HttpBasic.username|semantic|CredentialAuthMaterialV1{kind=http_basic}.value.username|normalized|scalar|required|CredentialFieldId
AuthPlan::HttpBasic.password|semantic|CredentialAuthMaterialV1{kind=http_basic}.value.password|normalized|scalar|required|CredentialFieldId
AuthPlan::OAuth2ClientCredentials.client_id|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_id|normalized|scalar|required|CredentialFieldId
AuthPlan::OAuth2ClientCredentials.client_secret|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_secret|normalized|scalar|required|CredentialFieldId
AuthPlan::OAuth2ClientCredentials.token_origin|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_origin|normalized|scalar|required|OriginId
AuthPlan::OAuth2ClientCredentials.token_step|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_step|normalized|scalar|required|CompiledStepId
AuthPlan::OAuth2ClientCredentials.scopes|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.scopes|normalized|lexical|empty_array|Vec<StaticScope>
AuthPlan::OAuth2ClientCredentials.token_pointer|semantic|CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_pointer|normalized|scalar|required|StaticJsonPointer
AuthPlan::PreprovisionedOAuthAccessToken.token|semantic|CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.value.token|normalized|scalar|required|CredentialFieldId
FixedOrigin.origin|semantic|SemanticOriginMaterialV1.origin|normalized|scalar|required|OriginId
FixedOrigin.scheme|semantic|SemanticOriginMaterialV1.scheme|normalized|scalar|required|HttpsOnly
FixedOrigin.host|semantic|SemanticOriginMaterialV1.host|normalized|scalar|required|StaticDnsName
FixedOrigin.port|semantic|SemanticOriginMaterialV1.port|normalized|scalar|required|u16
FixedOrigin.network_policy|semantic|SemanticOriginMaterialV1.network_policy|normalized|scalar|required|NetworkPolicyMaterialV1
NetworkPolicy::PublicOnly|semantic|NetworkPolicyMaterialV1{kind=public_only}.kind|normalized|scalar|required|public_only
NetworkPolicy::PrivateAllowed|semantic|NetworkPolicyMaterialV1{kind=private_allowed}.kind|normalized|scalar|required|private_allowed
NetworkPolicy::PrivateAllowed.policy|semantic|NetworkPolicyMaterialV1{kind=private_allowed}.value.policy|normalized|scalar|required|Id
OperationSpec.connector|semantic|SemanticOperationMaterialV1.connector|normalized|scalar|required|ConnectorId
OperationSpec.connector_version|semantic|SemanticOperationMaterialV1.connector_version|normalized|scalar|required|StableSemver
OperationSpec.operation|semantic|SemanticOperationMaterialV1.operation|normalized|scalar|required|OperationId
OperationSpec.operation_version|semantic|SemanticOperationMaterialV1.operation_version|normalized|scalar|required|StableSemver
OperationSpec.runtime_abi_epoch|semantic|SemanticOperationMaterialV1.runtime_abi_epoch|normalized|scalar|required|Epoch
OperationSpec.value_language_epoch|semantic|SemanticOperationMaterialV1.value_language_epoch|normalized|scalar|required|Epoch
OperationSpec.input|semantic|SemanticOperationMaterialV1.input|normalized|scalar|required|ValueContractMaterialV1
OperationSpec.input_contract_sha256|semantic|SemanticOperationMaterialV1.input_contract_sha256|normalized|scalar|required|Hash256
OperationSpec.output|semantic|SemanticOperationMaterialV1.output|normalized|scalar|required|ValueContractMaterialV1
OperationSpec.output_contract_sha256|semantic|SemanticOperationMaterialV1.output_contract_sha256|normalized|scalar|required|Hash256
OperationSpec.credential|semantic|SemanticOperationMaterialV1.credential|normalized|scalar|explicit_null|Option<VersionedCredentialMaterialV1>
OperationSpec.origins|semantic|SemanticOperationMaterialV1.origins|normalized|origin|nonempty_array|NonEmptyVec<SemanticOriginMaterialV1>
OperationSpec.steps|semantic|SemanticOperationMaterialV1.steps|normalized|declared|nonempty_array|NonEmptyVec<SemanticStepMaterialV1>
OperationSpec.pre_request_transforms|semantic|SemanticOperationMaterialV1.pre_request_transforms|normalized|declared|empty_array|Vec<VersionedProcessorMaterialV1>
OperationSpec.post_response_transforms|semantic|SemanticOperationMaterialV1.post_response_transforms|normalized|declared|empty_array|Vec<VersionedProcessorMaterialV1>
OperationSpec.operation_processor|semantic|SemanticOperationMaterialV1.operation_processor|normalized|scalar|explicit_null|Option<VersionedProcessorMaterialV1>
OperationSpec.effect|semantic|SemanticOperationMaterialV1.effect|normalized|scalar|required|OperationEffectMaterialV1
OperationSpec.pagination|semantic|SemanticOperationMaterialV1.pagination|normalized|scalar|required|PaginationMaterialV1
OperationSpec.error_map|semantic|SemanticOperationMaterialV1.error_map|normalized|scalar|required|ErrorMapMaterialV1
OperationSpec.capacity|semantic|SemanticOperationMaterialV1.capacity|normalized|scalar|required|CapacityDefaultsMaterialV1
OperationSpec.rate|semantic|SemanticOperationMaterialV1.rate|normalized|scalar|required|RateDefaultsMaterialV1
OperationSpec.serialization_key_default|semantic|SemanticOperationMaterialV1.serialization_key_default|normalized|scalar|explicit_null|Option<TypedSerializationKeyDefaultMaterialV1>
OperationSpec.bounds|semantic|SemanticOperationMaterialV1.bounds|normalized|scalar|required|OperationBoundsMaterialV1
OperationSpec.resolved_fact_values|semantic|SemanticOperationMaterialV1.resolved_fact_values|normalized|use_site|empty_array|Vec<ResolvedFactValueMaterialV1>
VersionedCredentialReference.credential|semantic|VersionedCredentialMaterialV1.credential|normalized|scalar|required|CredentialSpecId
VersionedCredentialReference.version|semantic|VersionedCredentialMaterialV1.version|normalized|scalar|required|StableSemver
VersionedProcessorRef.id|semantic|VersionedProcessorMaterialV1.id|normalized|scalar|required|typed_processor_id
VersionedProcessorRef.implementation_revision|semantic|VersionedProcessorMaterialV1.implementation_revision|normalized|scalar|required|Epoch
VersionedOperationReference.operation|semantic|VersionedOperationReferenceMaterialV1.operation|normalized|scalar|required|OperationId
VersionedOperationReference.version|semantic|VersionedOperationReferenceMaterialV1.version|normalized|scalar|required|StableSemver
CompiledStepSpec.step|semantic|SemanticStepMaterialV1.step|normalized|scalar|required|CompiledStepId
CompiledStepSpec.method|semantic|SemanticStepMaterialV1.method|normalized|scalar|required|StaticHttpMethod
CompiledStepSpec.origin|semantic|SemanticStepMaterialV1.origin|normalized|scalar|required|OriginId
CompiledStepSpec.path|semantic|SemanticStepMaterialV1.path|normalized|scalar|required|StaticPathTemplate
CompiledStepSpec.query|semantic|SemanticStepMaterialV1.query|normalized|name|empty_array|Vec<CompiledQueryBindingMaterialV1>
CompiledStepSpec.headers|semantic|SemanticStepMaterialV1.headers|normalized|name|empty_array|Vec<CompiledHeaderBindingMaterialV1>
CompiledStepSpec.credential_action|semantic|SemanticStepMaterialV1.credential_action|normalized|scalar|explicit_null|Option<CompiledCredentialActionMaterialV1>
CompiledStepSpec.request|semantic|SemanticStepMaterialV1.request|normalized|scalar|required|CompiledRequestMaterialV1
CompiledStepSpec.success_statuses|semantic|SemanticStepMaterialV1.success_statuses|normalized|minimum_then_maximum|nonempty_array|NonEmptyVec<StatusRangeMaterialV1>
CompiledStepSpec.response|semantic|SemanticStepMaterialV1.response|normalized|scalar|required|CompiledResponseMaterialV1
CompiledStepSpec.selected_response_headers|semantic|SemanticStepMaterialV1.selected_response_headers|normalized|canonical_lowercase_header_name|empty_array|Vec<SelectedResponseHeaderMaterialV1>
CompiledStepSpec.bounds|semantic|SemanticStepMaterialV1.bounds|normalized|scalar|required|StepBoundsMaterialV1
CompiledQueryBinding.name|semantic|CompiledQueryBindingMaterialV1.name|normalized|scalar|required|StaticQueryKey
CompiledQueryBinding.binding|semantic|CompiledQueryBindingMaterialV1.binding|normalized|scalar|required|BindingMaterialV1
CompiledHeaderBinding.name|semantic|CompiledHeaderBindingMaterialV1.name|normalized|scalar|required|StaticHeaderName
CompiledHeaderBinding.binding|semantic|CompiledHeaderBindingMaterialV1.binding|normalized|scalar|required|BindingMaterialV1
CompiledBinding.field|semantic|BindingMaterialV1.field|normalized|scalar|required|Id
CompiledBinding.source|semantic|BindingMaterialV1.source|normalized|scalar|required|CompiledBindingSourceMaterialV1
CompiledBinding.required|semantic|BindingMaterialV1.required|normalized|scalar|required|bool
CompiledBinding.default|semantic|BindingMaterialV1.default|normalized|scalar|explicit_null|Option<TypedValueMaterialV1>
CompiledBinding.mapping|semantic|BindingMaterialV1.mapping|normalized|scalar|explicit_null|Option<Id>
CompiledBindingSource::Input|semantic|CompiledBindingSourceMaterialV1{kind=input}.kind|normalized|scalar|required|input
CompiledBindingSource::Constant|semantic|CompiledBindingSourceMaterialV1{kind=constant}.kind|normalized|scalar|required|constant
CompiledBindingSource::Constant.value|semantic|CompiledBindingSourceMaterialV1{kind=constant}.value.value|normalized|scalar|required|TypedValueMaterialV1
CompiledCredentialAction.credential|semantic|CompiledCredentialActionMaterialV1.credential|normalized|scalar|required|CredentialSpecId
CompiledRequestShape::None|semantic|CompiledRequestMaterialV1{kind=none}.kind|normalized|scalar|required|none
CompiledRequestShape::Json|semantic|CompiledRequestMaterialV1{kind=json}.kind|normalized|scalar|required|json
CompiledRequestShape::FormUrlencoded|semantic|CompiledRequestMaterialV1{kind=form_urlencoded}.kind|normalized|scalar|required|form_urlencoded
CompiledRequestShape::Multipart|semantic|CompiledRequestMaterialV1{kind=multipart}.kind|normalized|scalar|required|multipart
CompiledRequestShape::RawBytes|semantic|CompiledRequestMaterialV1{kind=raw_bytes}.kind|normalized|scalar|required|raw_bytes
CompiledRequestShape::Json.bindings|semantic|CompiledRequestMaterialV1{kind=json}.value.bindings|normalized|declared|empty_array|Vec<Id>
CompiledRequestShape::FormUrlencoded.bindings|semantic|CompiledRequestMaterialV1{kind=form_urlencoded}.value.bindings|normalized|declared|empty_array|Vec<Id>
CompiledRequestShape::Multipart.bindings|semantic|CompiledRequestMaterialV1{kind=multipart}.value.bindings|normalized|declared|empty_array|Vec<Id>
CompiledRequestShape::RawBytes.binding|semantic|CompiledRequestMaterialV1{kind=raw_bytes}.value.binding|normalized|scalar|required|Id
CompiledResponseShape::Json|semantic|CompiledResponseMaterialV1{kind=json}.kind|normalized|scalar|required|json
CompiledResponseShape::RawBytes|semantic|CompiledResponseMaterialV1{kind=raw_bytes}.kind|normalized|scalar|required|raw_bytes
CompiledResponseShape::Json.mappings|semantic|CompiledResponseMaterialV1{kind=json}.value.mappings|normalized|declared|empty_array|Vec<ResponseMappingMaterialV1>
CompiledResponseShape::RawBytes.target|semantic|CompiledResponseMaterialV1{kind=raw_bytes}.value.target|normalized|scalar|required|Id
ResponseMapping.pointer|semantic|ResponseMappingMaterialV1.pointer|normalized|scalar|required|StaticJsonPointer
ResponseMapping.target|semantic|ResponseMappingMaterialV1.target|normalized|scalar|required|Id
StatusRange.minimum|semantic|StatusRangeMaterialV1.minimum|normalized|scalar|required|u16
StatusRange.maximum|semantic|StatusRangeMaterialV1.maximum|normalized|scalar|required|u16
SelectedResponseHeader.canonical_lowercase_header_name|semantic|SelectedResponseHeaderMaterialV1.canonical_lowercase_header_name|normalized|scalar|required|StaticHeaderName
SelectedResponseHeader.capability|semantic|SelectedResponseHeaderMaterialV1.capability|normalized|scalar|required|CapabilityId
StepBounds.maximum_headers|semantic|StepBoundsMaterialV1.maximum_headers|normalized|scalar|required|u32
StepBounds.maximum_header_bytes|semantic|StepBoundsMaterialV1.maximum_header_bytes|normalized|scalar|required|u32
StepBounds.maximum_url_bytes|semantic|StepBoundsMaterialV1.maximum_url_bytes|normalized|scalar|required|u32
StepBounds.maximum_request_bytes|semantic|StepBoundsMaterialV1.maximum_request_bytes|normalized|scalar|required|u32
StepBounds.maximum_response_bytes|semantic|StepBoundsMaterialV1.maximum_response_bytes|normalized|scalar|required|u32
StepBounds.maximum_json_depth|semantic|StepBoundsMaterialV1.maximum_json_depth|normalized|scalar|required|u32
StepBounds.maximum_json_nodes|semantic|StepBoundsMaterialV1.maximum_json_nodes|normalized|scalar|required|u32
StepBounds.maximum_inline_binary_bytes|semantic|StepBoundsMaterialV1.maximum_inline_binary_bytes|normalized|scalar|required|u32
StepBounds.deadline_ms|semantic|StepBoundsMaterialV1.deadline_ms|normalized|scalar|required|u64-string
OperationBounds.maximum_calls|semantic|OperationBoundsMaterialV1.maximum_calls|normalized|scalar|required|u32
OperationBounds.maximum_pages|semantic|OperationBoundsMaterialV1.maximum_pages|normalized|scalar|required|u32
OperationBounds.maximum_items|semantic|OperationBoundsMaterialV1.maximum_items|normalized|scalar|required|u32
OperationBounds.maximum_aggregate_request_bytes|semantic|OperationBoundsMaterialV1.maximum_aggregate_request_bytes|normalized|scalar|required|u32
OperationBounds.maximum_aggregate_response_bytes|semantic|OperationBoundsMaterialV1.maximum_aggregate_response_bytes|normalized|scalar|required|u32
OperationBounds.maximum_output_canonical_bytes|semantic|OperationBoundsMaterialV1.maximum_output_canonical_bytes|normalized|scalar|required|u32
OperationBounds.maximum_redirects|semantic|OperationBoundsMaterialV1.maximum_redirects|normalized|scalar|required|u8
OperationBounds.deadline_ms|semantic|OperationBoundsMaterialV1.deadline_ms|normalized|scalar|required|u64-string
OperationEffect::ReadOnly|semantic|OperationEffectMaterialV1{kind=read_only}.kind|normalized|scalar|required|read_only
OperationEffect::AtMostOnce|semantic|OperationEffectMaterialV1{kind=at_most_once}.kind|normalized|scalar|required|at_most_once
OperationEffect::ProviderIdempotent|semantic|OperationEffectMaterialV1{kind=provider_idempotent}.kind|normalized|scalar|required|provider_idempotent
OperationEffect::ProviderIdempotent.side_effect_steps|semantic|OperationEffectMaterialV1{kind=provider_idempotent}.value.side_effect_steps|normalized|step|nonempty_array|NonEmptyVec<ProviderIdempotentStepMaterialV1>
ProviderIdempotentStep.step|semantic|ProviderIdempotentStepMaterialV1.step|normalized|scalar|required|CompiledStepId
ProviderIdempotentStep.fixed_binding|semantic|ProviderIdempotentStepMaterialV1.fixed_binding|normalized|scalar|required|FixedIdempotencyBindingMaterialV1
ProviderIdempotentStep.scope|semantic|ProviderIdempotentStepMaterialV1.scope|normalized|scalar|required|ProviderIdempotencyScope
ProviderIdempotentStep.minimum_retention_ms|semantic|ProviderIdempotentStepMaterialV1.minimum_retention_ms|normalized|scalar|required|u64-string
ProviderIdempotentStep.clock_safety_margin_ms|semantic|ProviderIdempotentStepMaterialV1.clock_safety_margin_ms|normalized|scalar|required|u64-string
FixedIdempotencyBinding::Header|semantic|FixedIdempotencyBindingMaterialV1{kind=header}.kind|normalized|scalar|required|header
FixedIdempotencyBinding::BodyField|semantic|FixedIdempotencyBindingMaterialV1{kind=body_field}.kind|normalized|scalar|required|body_field
FixedIdempotencyBinding::Header.name|semantic|FixedIdempotencyBindingMaterialV1{kind=header}.value.name|normalized|scalar|required|StaticHeaderName
FixedIdempotencyBinding::BodyField.pointer|semantic|FixedIdempotencyBindingMaterialV1{kind=body_field}.value.pointer|normalized|scalar|required|StaticBodyPointer
PaginationPlan::None|semantic|PaginationMaterialV1{kind=none}.kind|normalized|scalar|required|none
PaginationPlan::Cursor|semantic|PaginationMaterialV1{kind=cursor}.kind|normalized|scalar|required|cursor
PaginationPlan::OffsetLimit|semantic|PaginationMaterialV1{kind=offset_limit}.kind|normalized|scalar|required|offset_limit
PaginationPlan::PageNumber|semantic|PaginationMaterialV1{kind=page_number}.kind|normalized|scalar|required|page_number
PaginationPlan::LinkRelation|semantic|PaginationMaterialV1{kind=link_relation}.kind|normalized|scalar|required|link_relation
PaginationPlan::Processor|semantic|PaginationMaterialV1{kind=processor}.kind|normalized|scalar|required|processor
PaginationPlan::Cursor.request_binding|semantic|PaginationMaterialV1{kind=cursor}.value.request_binding|normalized|scalar|required|Id
PaginationPlan::Cursor.response_pointer|semantic|PaginationMaterialV1{kind=cursor}.value.response_pointer|normalized|scalar|required|StaticJsonPointer
PaginationPlan::Cursor.bounds|semantic|PaginationMaterialV1{kind=cursor}.value.bounds|normalized|scalar|required|PaginationBoundsMaterialV1
PaginationPlan::OffsetLimit.offset_binding|semantic|PaginationMaterialV1{kind=offset_limit}.value.offset_binding|normalized|scalar|required|Id
PaginationPlan::OffsetLimit.limit_binding|semantic|PaginationMaterialV1{kind=offset_limit}.value.limit_binding|normalized|scalar|required|Id
PaginationPlan::OffsetLimit.initial_offset|semantic|PaginationMaterialV1{kind=offset_limit}.value.initial_offset|normalized|scalar|required|u64-string
PaginationPlan::OffsetLimit.page_size|semantic|PaginationMaterialV1{kind=offset_limit}.value.page_size|normalized|scalar|required|u32
PaginationPlan::OffsetLimit.bounds|semantic|PaginationMaterialV1{kind=offset_limit}.value.bounds|normalized|scalar|required|PaginationBoundsMaterialV1
PaginationPlan::PageNumber.page_binding|semantic|PaginationMaterialV1{kind=page_number}.value.page_binding|normalized|scalar|required|Id
PaginationPlan::PageNumber.page_size_binding|semantic|PaginationMaterialV1{kind=page_number}.value.page_size_binding|normalized|scalar|required|Id
PaginationPlan::PageNumber.initial_page|semantic|PaginationMaterialV1{kind=page_number}.value.initial_page|normalized|scalar|required|u64-string
PaginationPlan::PageNumber.page_size|semantic|PaginationMaterialV1{kind=page_number}.value.page_size|normalized|scalar|required|u32
PaginationPlan::PageNumber.bounds|semantic|PaginationMaterialV1{kind=page_number}.value.bounds|normalized|scalar|required|PaginationBoundsMaterialV1
PaginationPlan::LinkRelation.relation|semantic|PaginationMaterialV1{kind=link_relation}.value.relation|normalized|scalar|required|string
PaginationPlan::LinkRelation.selected_header|semantic|PaginationMaterialV1{kind=link_relation}.value.selected_header|normalized|scalar|required|SelectedResponseHeaderMaterialV1
PaginationPlan::LinkRelation.bounds|semantic|PaginationMaterialV1{kind=link_relation}.value.bounds|normalized|scalar|required|PaginationBoundsMaterialV1
PaginationPlan::Processor.processor|semantic|PaginationMaterialV1{kind=processor}.value.processor|normalized|scalar|required|VersionedProcessorMaterialV1
PaginationPlan::Processor.bounds|semantic|PaginationMaterialV1{kind=processor}.value.bounds|normalized|scalar|required|PaginationBoundsMaterialV1
PaginationBounds.maximum_calls|semantic|PaginationBoundsMaterialV1.maximum_calls|normalized|scalar|required|u32
PaginationBounds.maximum_pages|semantic|PaginationBoundsMaterialV1.maximum_pages|normalized|scalar|required|u32
PaginationBounds.maximum_items|semantic|PaginationBoundsMaterialV1.maximum_items|normalized|scalar|required|u32
PaginationBounds.maximum_response_bytes|semantic|PaginationBoundsMaterialV1.maximum_response_bytes|normalized|scalar|required|u32
PaginationBounds.maximum_aggregate_response_bytes|semantic|PaginationBoundsMaterialV1.maximum_aggregate_response_bytes|normalized|scalar|required|u32
PaginationBounds.maximum_output_canonical_bytes|semantic|PaginationBoundsMaterialV1.maximum_output_canonical_bytes|normalized|scalar|required|u32
CapacityDefaults.maximum_in_flight|semantic|CapacityDefaultsMaterialV1.maximum_in_flight|normalized|scalar|required|u32
RateDefaults.burst|semantic|RateDefaultsMaterialV1.burst|normalized|scalar|required|u32
RateDefaults.refill_interval_ms|semantic|RateDefaultsMaterialV1.refill_interval_ms|normalized|scalar|required|u64-string
TypedSerializationKeyDefault.field|semantic|TypedSerializationKeyDefaultMaterialV1.field|normalized|scalar|required|Id
TypedSerializationKeyDefault.value|semantic|TypedSerializationKeyDefaultMaterialV1.value|normalized|scalar|required|TypedValueMaterialV1
ResolvedFactValue.use_site|semantic|ResolvedFactValueMaterialV1.use_site|normalized|scalar|required|Id
ResolvedFactValue.value|semantic|ResolvedFactValueMaterialV1.value|normalized|scalar|required|TypedValueMaterialV1
ErrorMap.rules|semantic|ErrorMapMaterialV1.rules|normalized|declared|empty_array|Vec<ErrorRuleMaterialV1>
ErrorMap.fallback|semantic|ErrorMapMaterialV1.fallback|normalized|scalar|required|CompleteErrorFallbackMaterialV1
ErrorRule.matcher|semantic|ErrorRuleMaterialV1.matcher|normalized|scalar|required|ErrorMatcherMaterialV1
ErrorRule.action|semantic|ErrorRuleMaterialV1.action|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.transport|semantic|CompleteErrorFallbackMaterialV1.transport|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.timeout|semantic|CompleteErrorFallbackMaterialV1.timeout|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.http_429|semantic|CompleteErrorFallbackMaterialV1.http_429|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.http_5xx|semantic|CompleteErrorFallbackMaterialV1.http_5xx|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.authentication|semantic|CompleteErrorFallbackMaterialV1.authentication|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.validation|semantic|CompleteErrorFallbackMaterialV1.validation|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.permanent|semantic|CompleteErrorFallbackMaterialV1.permanent|normalized|scalar|required|ErrorActionMaterialV1
CompleteErrorFallback.invariant|semantic|CompleteErrorFallbackMaterialV1.invariant|normalized|scalar|required|ErrorActionMaterialV1
ErrorAction.class|semantic|ErrorActionMaterialV1.class|normalized|scalar|required|ConnectorErrorClassMaterialV1
ErrorAction.code|semantic|ErrorActionMaterialV1.code|normalized|scalar|required|StaticErrorCode
ErrorAction.safe_message|semantic|ErrorActionMaterialV1.safe_message|normalized|scalar|required|StaticSafeMessage
ErrorAction.retry_after|semantic|ErrorActionMaterialV1.retry_after|normalized|scalar|required|RetryAfterMaterialV1
ErrorAction.correlations|semantic|ErrorActionMaterialV1.correlations|normalized|step_then_header|empty_array|Vec<ErrorCorrelationMaterialV1>
ErrorCorrelationBinding.canonical_lowercase_header_name|semantic|ErrorCorrelationMaterialV1.canonical_lowercase_header_name|normalized|scalar|required|StaticHeaderName
ErrorCorrelationBinding.capability|semantic|ErrorCorrelationMaterialV1.capability|normalized|scalar|required|CapabilityId
ErrorCorrelationBinding.step|semantic|ErrorCorrelationMaterialV1.step|normalized|scalar|required|CompiledStepId
ConnectorErrorClass::Transport|semantic|ConnectorErrorClassMaterialV1{kind=transport}.kind|normalized|scalar|required|transport
ConnectorErrorClass::Timeout|semantic|ConnectorErrorClassMaterialV1{kind=timeout}.kind|normalized|scalar|required|timeout
ConnectorErrorClass::Http429|semantic|ConnectorErrorClassMaterialV1{kind=http_429}.kind|normalized|scalar|required|http_429
ConnectorErrorClass::Http5xx|semantic|ConnectorErrorClassMaterialV1{kind=http_5xx}.kind|normalized|scalar|required|http_5xx
ConnectorErrorClass::Authentication|semantic|ConnectorErrorClassMaterialV1{kind=authentication}.kind|normalized|scalar|required|authentication
ConnectorErrorClass::Validation|semantic|ConnectorErrorClassMaterialV1{kind=validation}.kind|normalized|scalar|required|validation
ConnectorErrorClass::Permanent|semantic|ConnectorErrorClassMaterialV1{kind=permanent}.kind|normalized|scalar|required|permanent
ConnectorErrorClass::Invariant|semantic|ConnectorErrorClassMaterialV1{kind=invariant}.kind|normalized|scalar|required|invariant
RetryAfterPolicy::Never|semantic|RetryAfterMaterialV1{kind=never}.kind|normalized|scalar|required|never
RetryAfterPolicy::RetryAfterHeader|semantic|RetryAfterMaterialV1{kind=retry_after_header}.kind|normalized|scalar|required|retry_after_header
RetryAfterPolicy::RetryAfterHeader.step|semantic|RetryAfterMaterialV1{kind=retry_after_header}.value.step|normalized|scalar|required|CompiledStepId
RetryAfterPolicy::RetryAfterHeader.capability|semantic|RetryAfterMaterialV1{kind=retry_after_header}.value.capability|normalized|scalar|required|CapabilityId
RetryAfterPolicy::RetryAfterHeader.maximum_seconds|semantic|RetryAfterMaterialV1{kind=retry_after_header}.value.maximum_seconds|normalized|scalar|required|u32
ErrorMatcher::Status|semantic|ErrorMatcherMaterialV1{kind=status}.kind|normalized|scalar|required|status
ErrorMatcher::ProviderCode|semantic|ErrorMatcherMaterialV1{kind=provider_code}.kind|normalized|scalar|required|provider_code
ErrorMatcher::Header|semantic|ErrorMatcherMaterialV1{kind=header}.kind|normalized|scalar|required|header
ErrorMatcher::MalformedDeclaredSuccess|semantic|ErrorMatcherMaterialV1{kind=malformed_declared_success}.kind|normalized|scalar|required|malformed_declared_success
ErrorMatcher::Status.minimum|semantic|ErrorMatcherMaterialV1{kind=status}.value.minimum|normalized|scalar|required|u16
ErrorMatcher::Status.maximum|semantic|ErrorMatcherMaterialV1{kind=status}.value.maximum|normalized|scalar|required|u16
ErrorMatcher::ProviderCode.pointer|semantic|ErrorMatcherMaterialV1{kind=provider_code}.value.pointer|normalized|scalar|required|StaticJsonPointer
ErrorMatcher::ProviderCode.codes|semantic|ErrorMatcherMaterialV1{kind=provider_code}.value.codes|normalized|lexical|nonempty_array|NonEmptyVec<StaticProviderCode>
ErrorMatcher::Header.name|semantic|ErrorMatcherMaterialV1{kind=header}.value.name|normalized|scalar|required|StaticHeaderName
ErrorMatcher::Header.values|semantic|ErrorMatcherMaterialV1{kind=header}.value.values|normalized|lexical|nonempty_array|NonEmptyVec<StaticHeaderValue>
TriggerSpec::Webhook|semantic|SemanticTriggerMaterialV1{kind=webhook}.kind|normalized|scalar|required|webhook
TriggerSpec::Webhook.connector|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.connector|normalized|scalar|required|ConnectorId
TriggerSpec::Webhook.connector_version|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.connector_version|normalized|scalar|required|StableSemver
TriggerSpec::Webhook.trigger|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.trigger|normalized|scalar|required|TriggerId
TriggerSpec::Webhook.trigger_version|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.trigger_version|normalized|scalar|required|StableSemver
TriggerSpec::Webhook.event_version|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.event_version|normalized|scalar|required|StableSemver
TriggerSpec::Webhook.runtime_abi_epoch|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.runtime_abi_epoch|normalized|scalar|required|Epoch
TriggerSpec::Webhook.authenticator|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.authenticator|normalized|scalar|required|VersionedProcessorMaterialV1
TriggerSpec::Webhook.codec|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.codec|normalized|scalar|required|VersionedProcessorMaterialV1
TriggerSpec::Webhook.normalizer|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.normalizer|normalized|scalar|required|VersionedProcessorMaterialV1
TriggerSpec::Webhook.selected_headers|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.selected_headers|normalized|lexical|empty_array|Vec<StaticHeaderName>
TriggerSpec::Webhook.raw_body_max_bytes|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.raw_body_max_bytes|normalized|scalar|required|u32
TriggerSpec::Webhook.timestamp_window_ms|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.timestamp_window_ms|normalized|scalar|required|u64-string
TriggerSpec::Webhook.event_id|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.event_id|normalized|scalar|required|ValueContractMaterialV1
TriggerSpec::Webhook.event_type|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.event_type|normalized|scalar|required|ValueContractMaterialV1
TriggerSpec::Webhook.output|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.output|normalized|scalar|required|ValueContractMaterialV1
TriggerSpec::Webhook.redaction|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.redaction|normalized|scalar|required|RedactionMaterialV1
TriggerSpec::Webhook.subscription_operations|semantic|SemanticTriggerMaterialV1{kind=webhook}.value.subscription_operations|normalized|scalar|explicit_null|Option<SubscriptionOperationIdsMaterialV1>
TriggerSpec::Poll|semantic|SemanticTriggerMaterialV1{kind=poll}.kind|normalized|scalar|required|poll
TriggerSpec::Poll.connector|semantic|SemanticTriggerMaterialV1{kind=poll}.value.connector|normalized|scalar|required|ConnectorId
TriggerSpec::Poll.connector_version|semantic|SemanticTriggerMaterialV1{kind=poll}.value.connector_version|normalized|scalar|required|StableSemver
TriggerSpec::Poll.trigger|semantic|SemanticTriggerMaterialV1{kind=poll}.value.trigger|normalized|scalar|required|TriggerId
TriggerSpec::Poll.trigger_version|semantic|SemanticTriggerMaterialV1{kind=poll}.value.trigger_version|normalized|scalar|required|StableSemver
TriggerSpec::Poll.event_version|semantic|SemanticTriggerMaterialV1{kind=poll}.value.event_version|normalized|scalar|required|StableSemver
TriggerSpec::Poll.runtime_abi_epoch|semantic|SemanticTriggerMaterialV1{kind=poll}.value.runtime_abi_epoch|normalized|scalar|required|Epoch
TriggerSpec::Poll.checkpoint|semantic|SemanticTriggerMaterialV1{kind=poll}.value.checkpoint|normalized|scalar|required|ValueContractMaterialV1
TriggerSpec::Poll.processor|semantic|SemanticTriggerMaterialV1{kind=poll}.value.processor|normalized|scalar|required|VersionedProcessorMaterialV1
TriggerSpec::Poll.event_type|semantic|SemanticTriggerMaterialV1{kind=poll}.value.event_type|normalized|scalar|required|ValueContractMaterialV1
TriggerSpec::Poll.per_poll_event_limit|semantic|SemanticTriggerMaterialV1{kind=poll}.value.per_poll_event_limit|normalized|scalar|required|u32
TriggerSpec::Poll.bounds|semantic|SemanticTriggerMaterialV1{kind=poll}.value.bounds|normalized|scalar|required|OperationBoundsMaterialV1
SubscriptionOperationIds.create|semantic|SubscriptionOperationIdsMaterialV1.create|normalized|scalar|required|OperationId
SubscriptionOperationIds.delete|semantic|SubscriptionOperationIdsMaterialV1.delete|normalized|scalar|required|OperationId
SubscriptionOperationIds.check|semantic|SubscriptionOperationIdsMaterialV1.check|normalized|scalar|explicit_null|Option<OperationId>
ValueContractCatalog.value_language_epoch|value-contract|ValueContractMaterialV1.value_language_epoch|normalized|scalar|required|Epoch
ValueContractCatalog.roots|value-contract|ValueContractMaterialV1.roots|normalized|utf16_member_name|empty_object|Map<string,FieldMaterialV1>
ValueContractCatalog.named_objects|value-contract|ValueContractMaterialV1.named_objects|normalized|utf16_member_name|empty_object|Map<string,NamedObjectMaterialV1>
NamedObject.fields|value-contract|NamedObjectMaterialV1.fields|normalized|utf16_member_name|empty_object|Map<string,FieldMaterialV1>
Field.required|value-contract|FieldMaterialV1.required|normalized|scalar|required|bool
Field.type_ref|value-contract|FieldMaterialV1.type_ref|normalized|scalar|required|TypeRefMaterialV1
TypeRef.nullable|value-contract|TypeRefMaterialV1.nullable|normalized|scalar|required|bool
TypeRef.value_type|value-contract|TypeRefMaterialV1.value_type|normalized|scalar|required|ValueTypeMaterialV1
ValueType::Scalar|value-contract|ValueTypeMaterialV1{kind=scalar}.kind|normalized|scalar|required|scalar
ValueType::Enum|value-contract|ValueTypeMaterialV1{kind=enum}.kind|normalized|scalar|required|enum
ValueType::Object|value-contract|ValueTypeMaterialV1{kind=object}.kind|normalized|scalar|required|object
ValueType::List|value-contract|ValueTypeMaterialV1{kind=list}.kind|normalized|scalar|required|list
ValueType::Ref|value-contract|ValueTypeMaterialV1{kind=ref}.kind|normalized|scalar|required|ref
ValueType::Scalar.scalar|value-contract|ValueTypeMaterialV1{kind=scalar}.value|normalized|scalar|required|ValueScalarMaterialV1
ValueType::Enum.name|value-contract|ValueTypeMaterialV1{kind=enum}.value.name|normalized|scalar|required|string
ValueType::Enum.values|value-contract|ValueTypeMaterialV1{kind=enum}.value.values|normalized|declared|empty_array|Vec<string>
ValueType::Object.fields|value-contract|ValueTypeMaterialV1{kind=object}.value.fields|normalized|utf16_member_name|empty_object|Map<string,FieldMaterialV1>
ValueType::List.element|value-contract|ValueTypeMaterialV1{kind=list}.value.element|normalized|scalar|required|TypeRefMaterialV1
ValueType::Ref.name|value-contract|ValueTypeMaterialV1{kind=ref}.value.name|normalized|scalar|required|string
ValueScalar::Boolean|value-contract|ValueScalarMaterialV1{kind=boolean}.kind|normalized|scalar|required|boolean
ValueScalar::String|value-contract|ValueScalarMaterialV1{kind=string}.kind|normalized|scalar|required|string
ValueScalar::Int32|value-contract|ValueScalarMaterialV1{kind=int32}.kind|normalized|scalar|required|int32
ValueScalar::Int64|value-contract|ValueScalarMaterialV1{kind=int64}.kind|normalized|scalar|required|int64
ValueScalar::UInt64|value-contract|ValueScalarMaterialV1{kind=uint64}.kind|normalized|scalar|required|uint64
ValueScalar::Decimal|value-contract|ValueScalarMaterialV1{kind=decimal}.kind|normalized|scalar|required|decimal
ValueScalar::Uuid|value-contract|ValueScalarMaterialV1{kind=uuid}.kind|normalized|scalar|required|uuid
ValueScalar::Date|value-contract|ValueScalarMaterialV1{kind=date}.kind|normalized|scalar|required|date
ValueScalar::Timestamp|value-contract|ValueScalarMaterialV1{kind=timestamp}.kind|normalized|scalar|required|timestamp
ValueScalar::TimestampTz|value-contract|ValueScalarMaterialV1{kind=timestamptz}.kind|normalized|scalar|required|timestamptz
ValueScalar::Json|value-contract|ValueScalarMaterialV1{kind=json}.kind|normalized|scalar|required|json
ValueScalar::Custom|value-contract|ValueScalarMaterialV1{kind=custom}.kind|normalized|scalar|required|custom
ValueScalar::Custom.name|value-contract|ValueScalarMaterialV1{kind=custom}.value.name|normalized|scalar|required|string
TypedValue::Null|value-contract|TypedValueMaterialV1{kind=null}.kind|normalized|scalar|required|null
TypedValue::Boolean|value-contract|TypedValueMaterialV1{kind=boolean}.kind|normalized|scalar|required|boolean
TypedValue::String|value-contract|TypedValueMaterialV1{kind=string}.kind|normalized|scalar|required|string
TypedValue::I64|value-contract|TypedValueMaterialV1{kind=i64}.kind|normalized|scalar|required|i64
TypedValue::U64|value-contract|TypedValueMaterialV1{kind=u64}.kind|normalized|scalar|required|u64
TypedValue::Decimal|value-contract|TypedValueMaterialV1{kind=decimal}.kind|normalized|scalar|required|decimal
TypedValue::List|value-contract|TypedValueMaterialV1{kind=list}.kind|normalized|scalar|required|list
TypedValue::Object|value-contract|TypedValueMaterialV1{kind=object}.kind|normalized|scalar|required|object
TypedValue::InlineBytes|value-contract|TypedValueMaterialV1{kind=inline_bytes}.kind|normalized|scalar|required|inline_bytes
TypedValue::Boolean.value|value-contract|TypedValueMaterialV1{kind=boolean}.value|normalized|scalar|required|bool
TypedValue::String.value|value-contract|TypedValueMaterialV1{kind=string}.value|normalized|scalar|required|string
TypedValue::I64.value|value-contract|TypedValueMaterialV1{kind=i64}.value|normalized|scalar|required|i64-string
TypedValue::U64.value|value-contract|TypedValueMaterialV1{kind=u64}.value|normalized|scalar|required|u64-string
TypedValue::Decimal.value|value-contract|TypedValueMaterialV1{kind=decimal}.value|normalized|scalar|required|decimal-string
TypedValue::List.value|value-contract|TypedValueMaterialV1{kind=list}.value|normalized|declared|empty_array|Vec<TypedValueMaterialV1>
TypedValue::Object.value|value-contract|TypedValueMaterialV1{kind=object}.value|normalized|utf16_member_name|empty_object|Map<string,TypedValueMaterialV1>
TypedValue::InlineBytes.bytes|value-contract|TypedValueMaterialV1{kind=inline_bytes}.value.$binary|normalized|scalar|required|base64url
TypedValue::InlineBytes.media_type|value-contract|TypedValueMaterialV1{kind=inline_bytes}.value.media_type|normalized|scalar|explicit_null|Option<string>
TypedValue::InlineBytes.file_name|value-contract|TypedValueMaterialV1{kind=inline_bytes}.value.file_name|normalized|scalar|explicit_null|Option<string>
ManifestProvenanceReference.source_record_id|provenance|ManifestProvenanceMaterialV1.source_record_id|normalized|scalar|required|SourceRecordId
ManifestProvenanceReference.artifact_hashes|provenance|ManifestProvenanceMaterialV1.artifact_hashes|normalized|artifact_id|nonempty_array|NonEmptyVec<ArtifactHashMaterialV1>
ManifestProvenanceReference.license_id|provenance|ManifestProvenanceMaterialV1.license_id|normalized|scalar|required|LicenseIdentity
ManifestProvenanceReference.notice_id|provenance|ManifestProvenanceMaterialV1.notice_id|normalized|scalar|required|NoticeId
ManifestProvenanceReference.contract_facts|provenance|ManifestProvenanceMaterialV1.contract_fact_origins|normalized|use_site|empty_array|Vec<ResolvedFactOriginMaterialV1>
ResolvedContractFactBinding.use_site|provenance|ResolvedFactOriginMaterialV1.use_site|normalized|scalar|required|Id
ResolvedContractFactBinding.fact|provenance|ResolvedFactOriginMaterialV1.origin|normalized|scalar|required|ResolvedFactOrigin
ContractFact::ProviderEvidence.source_record_id|provenance|ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.source_record_id|normalized|scalar|required|SourceRecordId
ContractFact::ProviderEvidence.fact_id|provenance|ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.fact_id|normalized|scalar|required|ProviderFactId
ContractFact::DonatPolicy.policy_id|provenance|ResolvedFactOriginMaterialV1{kind=donat_policy}.value.policy_id|normalized|scalar|required|DonatPolicyId
ContractFact::ProviderEvidence|provenance|ResolvedFactOriginMaterialV1{kind=provider_evidence}.kind|normalized|scalar|required|provider_evidence
ContractFact::DonatPolicy|provenance|ResolvedFactOriginMaterialV1{kind=donat_policy}.kind|normalized|scalar|required|donat_policy
ManifestProvenanceReference.artifact_hashes[].artifact_id|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].artifact_id|normalized|scalar|required|ArtifactId
ManifestProvenanceReference.artifact_hashes[].algorithm|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].algorithm|normalized|scalar|required|HashAlgorithm
ManifestProvenanceReference.artifact_hashes[].digest|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].digest|normalized|scalar|required|Hash256_or_Hash512
ManifestProvenanceReference.artifact_hashes[].path|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].path|normalized|scalar|explicit_null|Option<SourcePath>
ManifestProvenanceReference.artifact_hashes[].algorithm::Sha256|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha256}.kind|normalized|scalar|required|sha256
ManifestProvenanceReference.artifact_hashes[].algorithm::Sha512|provenance|ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha512}.kind|normalized|scalar|required|sha512
ExactProviderArtifact.provider|provenance|ProvenanceMaterialV1.provider_evidence[].provider|normalized|scalar|required|string
ExactProviderArtifact.evidence|provenance|ProvenanceMaterialV1.provider_evidence[].evidence|normalized|canonical_source_identity|nonempty_array|NonEmptyVec<ProviderEvidenceOriginEntryMaterialV1>
ProviderEvidenceArtifact.accessed_on|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].accessed_on|normalized|scalar|required|Date
ProviderEvidenceArtifact.content_sha256|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].content_sha256|normalized|scalar|required|Hash256
ProviderEvidenceArtifact.source|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source|normalized|scalar|required|ImmutableProviderEvidenceSource
ProviderEvidenceArtifact.terms|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms|normalized|scalar|required|EvidenceTermsMaterialV1
ProviderEvidenceArtifact.facts|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts|normalized|fact_id|nonempty_array|ProviderEvidenceOriginFactMaterialV1
ImmutableProviderEvidenceSource::RepositoryFile|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.kind|normalized|scalar|required|repository_file
ImmutableProviderEvidenceSource::VersionedArtifact|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.kind|normalized|scalar|required|versioned_artifact
ImmutableProviderEvidenceSource::RepositoryFile.repository|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.repository|normalized|scalar|required|RepositoryUrl
ImmutableProviderEvidenceSource::RepositoryFile.commit|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.commit|normalized|scalar|required|GitCommit
ImmutableProviderEvidenceSource::RepositoryFile.path|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.path|normalized|scalar|required|SourcePath
ImmutableProviderEvidenceSource::VersionedArtifact.url|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.url|normalized|scalar|required|ExactHttpsUrl
ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.provider_revision|normalized|scalar|required|NonEmptyString
ProviderFact.fact_id|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].fact_id|normalized|scalar|required|ProviderFactId
ProviderFact.location|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location|normalized|scalar|required|ExactFactLocationMaterialV1
ExactFactLocation::JsonPointer|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.kind|normalized|scalar|required|json_pointer
ExactFactLocation::DocumentSection|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.kind|normalized|scalar|required|document_section
ExactFactLocation::JsonPointer.path|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.path|normalized|scalar|required|SourcePath
ExactFactLocation::JsonPointer.pointer|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.pointer|normalized|scalar|required|StaticJsonPointer
ExactFactLocation::DocumentSection.path|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.path|normalized|scalar|required|SourcePath
ExactFactLocation::DocumentSection.section|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.section|normalized|scalar|required|string
EvidenceTermsDisposition::Permissive|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.kind|normalized|scalar|required|permissive
EvidenceTermsDisposition::ReviewedUse|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.kind|normalized|scalar|required|reviewed_use
EvidenceTermsDisposition::Rejected|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.kind|normalized|scalar|required|rejected
EvidenceTermsDisposition::Permissive.license|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.license|normalized|scalar|required|LicenseDecisionMaterialV1
EvidenceTermsDisposition::Permissive.evidence_url|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.evidence_url|normalized|scalar|required|ExactHttpsUrl
EvidenceTermsDisposition::ReviewedUse.decision_id|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.decision_id|normalized|scalar|required|ReviewDecisionId
EvidenceTermsDisposition::ReviewedUse.evidence_url|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.evidence_url|normalized|scalar|required|ExactHttpsUrl
EvidenceTermsDisposition::Rejected.finding|provenance|ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.value.finding|normalized|scalar|required|FindingId
DonatOwnedSource.files[].path|provenance|ProvenanceMaterialV1.files[].path|normalized|source_record_id_then_path|required|RepoPath
DonatOwnedSource.files[].sha256|provenance|ProvenanceMaterialV1.files[].sha256|normalized|source_record_id_then_path|required|Hash256
LicenseDecision::Permissive|provenance|ProvenanceMaterialV1.licenses[]{kind=permissive}.kind|normalized|canonical_bytes|required|permissive
LicenseDecision::WrittenGrant|provenance|ProvenanceMaterialV1.licenses[]{kind=written_grant}.kind|normalized|canonical_bytes|required|written_grant
LicenseDecision::Rejected|provenance|ProvenanceMaterialV1.licenses[]{kind=rejected}.kind|normalized|canonical_bytes|required|rejected
LicenseDecision::Permissive.spdx_id|provenance|ProvenanceMaterialV1.licenses[]{kind=permissive}.value.spdx_id|normalized|canonical_bytes|required|string
LicenseDecision::Permissive.selected_dual_license_branch|provenance|ProvenanceMaterialV1.licenses[]{kind=permissive}.value.selected_dual_license_branch|normalized|canonical_bytes|explicit_null|Option<string>
LicenseDecision::Permissive.license_file_path|provenance|ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_path|normalized|canonical_bytes|required|SourcePath
LicenseDecision::Permissive.license_file_sha256|provenance|ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_sha256|normalized|canonical_bytes|required|Hash256
LicenseDecision::WrittenGrant.decision_id|provenance|ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.decision_id|normalized|canonical_bytes|required|ReviewDecisionId
LicenseDecision::WrittenGrant.grant_sha256|provenance|ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.grant_sha256|normalized|canonical_bytes|required|Hash256
LicenseDecision::Rejected.finding|provenance|ProvenanceMaterialV1.licenses[]{kind=rejected}.value.finding|normalized|canonical_bytes|required|FindingId
NoticeIdentity.id|provenance|ProvenanceMaterialV1.notices[].id|normalized|id|required|NoticeId
NoticeIdentity.license_file_path|provenance|ProvenanceMaterialV1.notices[].license_file_path|normalized|id|required|SourcePath
NoticeIdentity.license_file_sha256|provenance|ProvenanceMaterialV1.notices[].license_file_sha256|normalized|id|required|Hash256
NoticeIdentity.required_copyright_lines|provenance|ProvenanceMaterialV1.notices[].required_copyright_lines|normalized|declared|empty_array|Vec<string>
NoticeIdentity.notice_bundle_destination|provenance|ProvenanceMaterialV1.notices[].notice_bundle_destination|normalized|id|required|RepoPath
DependencyDecision.dependency|provenance|ProvenanceMaterialV1.dependencies[].dependency|normalized|dependency|required|Id
DependencyDecision.disposition|provenance|ProvenanceMaterialV1.dependencies[].disposition|normalized|dependency|required|DependencyDispositionMaterialV1
DependencyDisposition::Shipped|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.kind|normalized|dependency|required|shipped
DependencyDisposition::BuildOnly|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.kind|normalized|dependency|required|build_only
DependencyDisposition::TypeOnlyReplaced|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.kind|normalized|dependency|required|type_only_replaced
DependencyDisposition::BehaviorOnly|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.kind|normalized|dependency|required|behavior_only
DependencyDisposition::Rejected|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.kind|normalized|dependency|required|rejected
DependencyDisposition::Shipped.license|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.value.license|normalized|dependency|required|LicenseDecisionMaterialV1
DependencyDisposition::BuildOnly.license|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.value.license|normalized|dependency|required|LicenseDecisionMaterialV1
DependencyDisposition::TypeOnlyReplaced.replacement|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.value.replacement|normalized|dependency|required|Id
DependencyDisposition::BehaviorOnly.reason|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.value.reason|normalized|dependency|required|FindingId
DependencyDisposition::Rejected.finding|provenance|ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.value.finding|normalized|dependency|required|FindingId
EmbeddedMaterialDecision.material_id|provenance|ProvenanceMaterialV1.embedded_material[].material_id|normalized|material_id|required|Id
EmbeddedMaterialDecision.path|provenance|ProvenanceMaterialV1.embedded_material[].path|normalized|material_id|required|SourcePath
EmbeddedMaterialDecision.sha256|provenance|ProvenanceMaterialV1.embedded_material[].sha256|normalized|material_id|required|Hash256
EmbeddedMaterialDecision.disposition|provenance|ProvenanceMaterialV1.embedded_material[].disposition|normalized|material_id|required|EmbeddedMaterialDispositionMaterialV1
EmbeddedMaterialDisposition::Shipped|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.kind|normalized|material_id|required|shipped
EmbeddedMaterialDisposition::BehaviorOnly|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.kind|normalized|material_id|required|behavior_only
EmbeddedMaterialDisposition::Rejected|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.kind|normalized|material_id|required|rejected
EmbeddedMaterialDisposition::Shipped.license|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.value.license|normalized|material_id|required|LicenseDecisionMaterialV1
EmbeddedMaterialDisposition::BehaviorOnly.reason|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.value.reason|normalized|material_id|required|FindingId
EmbeddedMaterialDisposition::Rejected.finding|provenance|ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.value.finding|normalized|material_id|required|FindingId
derived::canonical_schema_epoch|semantic|SemanticMaterialV1.canonical_schema_epoch|constant|scalar|required|CANONICAL_SCHEMA_EPOCH
derived::canonical_schema_epoch|provenance|ProvenanceMaterialV1.canonical_schema_epoch|constant|scalar|required|CANONICAL_SCHEMA_EPOCH
derived::classifier_epoch|provenance|ProvenanceMaterialV1.classifier_epoch|constant|scalar|required|CLASSIFIER_EPOCH
derived::generator_epoch|provenance|ProvenanceMaterialV1.generator_epoch|constant|scalar|required|GENERATOR_EPOCH
derived::semantic_sha256|provenance|ProvenanceMaterialV1.connector.semantic_sha256|derived:semantic_domain_hash|scalar|required|Hash256
derived::source_identity.record_id|provenance|SourceIdentityMaterialV1.record_id|derived:accepted_record_join|record_id|required|SourceRecordId
derived::source_identity.record_sha256|provenance|SourceIdentityMaterialV1.record_sha256|derived:source_record_domain_hash|record_id|required|Hash256
derived::artifact.source_record_id|provenance|ArtifactDecisionMaterialV1.source_record_id|derived:accepted_record_join|source_record_id_then_artifact_id|required|SourceRecordId
derived::artifact.artifact_id|provenance|ArtifactDecisionMaterialV1.artifact_id|derived:accepted_record_artifact_inventory|source_record_id_then_artifact_id|required|ArtifactId
derived::artifact.algorithm|provenance|ArtifactDecisionMaterialV1.algorithm|derived:accepted_record_artifact_inventory|source_record_id_then_artifact_id|required|HashAlgorithm
derived::artifact.digest|provenance|ArtifactDecisionMaterialV1.digest|derived:accepted_record_artifact_inventory|source_record_id_then_artifact_id|required|Hash256_or_Hash512
derived::artifact.path|provenance|ArtifactDecisionMaterialV1.path|derived:accepted_record_artifact_inventory|source_record_id_then_artifact_id|explicit_null|Option<SourcePath>
derived::file.source_record_id|provenance|FileDecisionMaterialV1.source_record_id|derived:accepted_donat_record_join|source_record_id_then_path|required|SourceRecordId
derived::file.path|provenance|FileDecisionMaterialV1.path|derived:accepted_donat_file_inventory|source_record_id_then_path|required|RepoPath
derived::file.sha256|provenance|FileDecisionMaterialV1.sha256|derived:accepted_donat_file_inventory|source_record_id_then_path|required|Hash256
derived::source_identity|provenance|ProvenanceMaterialV1.sources|derived:accepted_record_join|record_id|empty_array|Vec<SourceIdentityMaterialV1>
derived::artifact|provenance|ProvenanceMaterialV1.artifacts|derived:accepted_record_artifact_inventory|source_record_id_then_artifact_id|empty_array|Vec<ArtifactDecisionMaterialV1>
derived::license|provenance|ProvenanceMaterialV1.licenses|derived:accepted_record_license_inventory|canonical_bytes|empty_array|Vec<LicenseDecisionMaterialV1>
derived::dependency|provenance|ProvenanceMaterialV1.dependencies|derived:accepted_record_dependency_inventory|dependency|empty_array|Vec<DependencyDecisionMaterialV1>
derived::embedded_material|provenance|ProvenanceMaterialV1.embedded_material|derived:accepted_record_embedded_inventory|material_id|empty_array|Vec<EmbeddedDecisionMaterialV1>
derived::notice|provenance|ProvenanceMaterialV1.notices|derived:accepted_record_notice_inventory|id|empty_array|Vec<NoticeMaterialV1>
derived::provider_evidence|provenance|ProvenanceMaterialV1.provider_evidence|derived:accepted_provider_record_inventory|source_record_id|empty_array|Vec<ProviderEvidenceOriginMaterialV1>
derived::provider_evidence.source_record_id|provenance|ProviderEvidenceOriginMaterialV1.source_record_id|derived:accepted_provider_record_join|source_record_id|required|SourceRecordId
derived::fact_origin.artifact_content_sha256|provenance|ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.artifact_content_sha256|derived:provider_fact_content_join|use_site|required|Hash256
derived::fact_origin.location|provenance|ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.location|derived:provider_fact_location_join|use_site|required|ExactFactLocationMaterialV1
derived::donat_policy_ids|provenance|ProvenanceMaterialV1.donat_policy_ids|derived:contract_fact_policy_set|lexical|empty_array|Vec<DonatPolicyId>
```

Task 3 parses this block into an immutable owner table. Its invariant is an
exact-set comparison among independently enumerated normalized owners,
canonical members, and manifest mappings; it never pins a historical row
count. The test rejects a row with `*`, a family placeholder, an unknown
domain, a duplicate `(domain,canonical_path)`, a duplicate normalized mapping
within one direct material, a missing normalized leaf or enum discriminant,
an extra or stale owner, or an unowned canonical member. It recursively
expands named composite types and proves the expanded normalized model has
exactly the same leaf/discriminant set.

The resolved fact split is exact: one normalized binding produces one value
at `SemanticOperationMaterialV1.resolved_fact_values[]` and one origin at
`ManifestProvenanceMaterialV1.contract_fact_origins[]`, never a second path
inside either direct material. Tests reject duplicate `(use_site,fact)`
values, duplicate `(use_site,fact)` origins, or unequal semantic/provenance
use-site sets. A generated mutation suite changes each leaf and visits each
discriminant. Every mutation changes its applicable direct bytes/hash and
preserves unrelated direct domains; final provenance still changes when its
committed `semantic_sha256` changes.

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
  "canonical_schema_epoch":Epoch,"source_name":string,
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
Endpoint and credential fields are identities of deploy-time references,
never resolved URL/token/password bytes.

Every process pin stores beside its behavioral operation or trigger:

```text
CatalogIdentityEnvelopeV1 = {
  "canonical_schema_epoch":Epoch,
  "source_record_schema_epoch":Epoch,
  "source_records":[{"record_id":SourceRecordId,
                     "record_sha256":Hash256}...],
  "semantic_sha256":Hash256,"provenance_sha256":Hash256,
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
includes input, output, event, and every named/transitive contract used by
the snapshot. Reload recomputes and compares every envelope field and the
complete behavioral snapshot field-for-field.

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

`ExactSemver` has a separate constructor vector:

| Input | Result |
| --- | --- |
| `1.2.3-alpha.1+build.5` | accepted and preserved byte-for-byte |
| `1.2.3` | accepted and preserved byte-for-byte |
| `^1.2.3` | rejected: range |
| `latest` | rejected: distribution tag |
| `v1.2.3` | rejected: leading `v` |
| `01.2.3` | rejected: leading-zero core |
| `1.2.3-01` | rejected: leading-zero numeric prerelease identifier |

The positive prerelease/build vector is serialized as the JSON string
`"1.2.3-alpha.1+build.5"` at
`SourceSubjectMaterialV1{kind=exact_npm}.value.version`; no conversion to
`StableSemver` is permitted.

The empty-domain vectors remain normative:

| Domain | `{}` SHA-256 |
| --- | --- |
| `donat.connector.source-record.v1\0` | `210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4` |
| `donat.connector.semantic.v1\0` | `799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19` |
| `donat.connector.provenance.v1\0` | `a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4` |
| `donat.connector.value-contract.v1\0` | `6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f` |

The valid full-material vectors are generated below from the exact literal
canonical byte lines and independently recomputed by Task 3:

```text
source-record:
{"admission":{"kind":"evidence_accepted","value":{"contracts":["contract.demo"]}},"approval_date":"2026-07-29","artifact_hashes":[{"algorithm":{"kind":"sha256","value":null},"artifact_id":"artifact.openapi","digest":"1111111111111111111111111111111111111111111111111111111111111111","path":"openapi.json"}],"compatibility":{"kind":"tier_a","value":null},"dependencies":[],"embedded_material":[],"entrypoints":["openapi.json"],"license":{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}},"notice":{"id":"notice.demo","license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","notice_bundle_destination":"THIRD_PARTY_NOTICES.md","required_copyright_lines":["Copyright Demo"]},"proposed_destinations":["connector-catalog/sources/records/demo.yaml"],"proposed_manifest":null,"provider_contracts":[{"contract_id":"contract.demo","facts":[{"kind":"provider_evidence","value":{"fact_id":"fact.idempotency","source_record_id":"source.demo.provider.v1"}}]}],"reacquisition":{"kind":"provider_repository_review","value":null},"record_id":"source.demo.provider.v1","record_version":1,"red_tests":["provider_fact_red"],"reviewer":"reviewer.demo","safety_findings":{"findings":[]},"subject":{"kind":"provider_artifact","value":{"evidence":[{"accessed_on":"2026-07-29","content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","facts":[{"fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}},"normalized_value":{"kind":"string","value":"Idempotency-Key"}}],"source":{"kind":"repository_file","value":{"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","path":"openapi.json","repository":"https://github.com/example/demo"}},"terms":{"kind":"permissive","value":{"evidence_url":"https://example.test/terms/v1","license":{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}}}}}],"provider":"demo"}}}
SHA-256: 420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991

value-contract:
{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}
SHA-256: 79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a

semantic:
{"canonical_schema_epoch":1,"connector":{"api_identity":"demo.v1","id":"demo","manifest_version":1,"provider":"provider.demo","runtime_abi_epoch":1,"version":{"major":1,"minor":0,"patch":0}},"credentials":[{"allowed_origins":["origin.demo"],"auth_plan":{"kind":"oauth2_client_credentials","value":{"client_id":"field.client_id","client_secret":"field.client_secret","scopes":["widgets.read"],"token_origin":"origin.demo","token_pointer":"/access_token","token_step":"token"}},"auth_processor":{"id":"auth.demo","implementation_revision":1},"bounds":{"maximum_aggregate_bytes":512,"maximum_field_bytes":256,"maximum_token_bytes":256},"credential":"credential.demo","credential_test_operation":{"operation":"op.read","version":{"major":1,"minor":0,"patch":0}},"fields":[{"field":"field.client_id","maximum_bytes":128,"redaction":{"kind":"omit","value":null},"required":true,"secret":{"kind":"non_secret","value":null}},{"field":"field.client_secret","maximum_bytes":256,"redaction":{"kind":"omit","value":null},"required":true,"secret":{"kind":"secret","value":null}}],"scopes":["widgets.read"],"version":{"major":1,"minor":0,"patch":0}}],"operations":[{"bounds":{"deadline_ms":"1000","maximum_aggregate_request_bytes":2048,"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_redirects":0},"capacity":{"maximum_in_flight":4},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"credential":{"credential":"credential.demo","version":{"major":1,"minor":0,"patch":0}},"effect":{"kind":"provider_idempotent","value":{"side_effect_steps":[{"clock_safety_margin_ms":"1000","fixed_binding":{"kind":"header","value":{"name":"idempotency-key"}},"minimum_retention_ms":"86400000","scope":"scope.demo","step":"request"}]}},"error_map":{"fallback":{"authentication":{"class":{"kind":"authentication","value":null},"code":"auth_error","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"authentication failed"},"http_429":{"class":{"kind":"http_429","value":null},"code":"rate_limited","correlations":[{"canonical_lowercase_header_name":"x-request-id","capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3","step":"request"}],"retry_after":{"kind":"retry_after_header","value":{"capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3","maximum_seconds":86400,"step":"request"}},"safe_message":"rate limited"},"http_5xx":{"class":{"kind":"http_5xx","value":null},"code":"provider_unavailable","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider unavailable"},"invariant":{"class":{"kind":"invariant","value":null},"code":"connector_invariant","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"connector invariant"},"permanent":{"class":{"kind":"permanent","value":null},"code":"provider_rejected","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider rejected request"},"timeout":{"class":{"kind":"timeout","value":null},"code":"provider_timeout","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider timed out"},"transport":{"class":{"kind":"transport","value":null},"code":"provider_transport","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider transport failed"},"validation":{"class":{"kind":"validation","value":null},"code":"provider_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider validation failed"}},"rules":[{"action":{"class":{"kind":"validation","value":null},"code":"status_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"status validation"},"matcher":{"kind":"status","value":{"maximum":422,"minimum":400}}},{"action":{"class":{"kind":"validation","value":null},"code":"code_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"code validation"},"matcher":{"kind":"provider_code","value":{"codes":["invalid"],"pointer":"/error/code"}}},{"action":{"class":{"kind":"permanent","value":null},"code":"header_error","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"header error"},"matcher":{"kind":"header","value":{"name":"x-error","values":["true"]}}},{"action":{"class":{"kind":"validation","value":null},"code":"malformed_success","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"malformed success"},"matcher":{"kind":"malformed_declared_success","value":null}}]},"input":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"input_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","operation":"op.read","operation_processor":{"id":"processor.demo","implementation_revision":1},"operation_version":{"major":1,"minor":0,"patch":0},"origins":[{"host":"api.example.test","network_policy":{"kind":"public_only","value":null},"origin":"origin.demo","port":443,"scheme":{"kind":"https","value":null}}],"output":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"output_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","pagination":{"kind":"cursor","value":{"bounds":{"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_response_bytes":1024},"request_binding":"query","response_pointer":"/next_cursor"}},"post_response_transforms":[{"id":"transform.response","implementation_revision":1}],"pre_request_transforms":[{"id":"transform.request","implementation_revision":1}],"rate":{"burst":10,"refill_interval_ms":"1000"},"resolved_fact_values":[{"use_site":"effect.request.binding","value":{"kind":"string","value":"Idempotency-Key"}}],"runtime_abi_epoch":1,"serialization_key_default":{"field":"query","value":{"kind":"string","value":"default"}},"steps":[{"bounds":{"deadline_ms":"1000","maximum_header_bytes":1024,"maximum_headers":16,"maximum_inline_binary_bytes":1,"maximum_json_depth":8,"maximum_json_nodes":128,"maximum_request_bytes":1024,"maximum_response_bytes":1024,"maximum_url_bytes":1024},"credential_action":{"credential":"credential.demo"},"headers":[{"binding":{"default":null,"field":"query","mapping":null,"required":true,"source":{"kind":"input","value":null}},"name":"x-query"}],"method":"POST","origin":"origin.demo","path":"/widgets","query":[],"request":{"kind":"json","value":{"bindings":["query"]}},"response":{"kind":"json","value":{"mappings":[{"pointer":"/result","target":"query"}]}},"selected_response_headers":[{"canonical_lowercase_header_name":"x-request-id","capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3"}],"step":"request","success_statuses":[{"maximum":299,"minimum":200}]}],"value_language_epoch":1}],"origins":[{"host":"api.example.test","network_policy":{"kind":"public_only","value":null},"origin":"origin.demo","port":443,"scheme":{"kind":"https","value":null}}],"triggers":[{"kind":"poll","value":{"bounds":{"deadline_ms":"1000","maximum_aggregate_request_bytes":2048,"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_redirects":0},"checkpoint":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"event_type":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_version":{"major":1,"minor":0,"patch":0},"per_poll_event_limit":100,"processor":{"id":"poll.demo","implementation_revision":1},"runtime_abi_epoch":1,"trigger":"trigger.poll","trigger_version":{"major":1,"minor":0,"patch":0}}},{"kind":"webhook","value":{"authenticator":{"id":"auth.webhook","implementation_revision":1},"codec":{"id":"codec.json","implementation_revision":1},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"event_id":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_type":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_version":{"major":1,"minor":0,"patch":0},"normalizer":{"id":"normalize.webhook","implementation_revision":1},"output":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"raw_body_max_bytes":1024,"redaction":{"kind":"omit","value":null},"runtime_abi_epoch":1,"selected_headers":["x-signature"],"subscription_operations":{"check":null,"create":"subscription.create","delete":"subscription.delete"},"timestamp_window_ms":"300000","trigger":"trigger.webhook","trigger_version":{"major":1,"minor":0,"patch":0}}}],"value_language_epoch":1}
SHA-256: f6bc86c9d5004885bb3156ab320fa76ad3ff7e9686320c54735dcfbd8c27e934

provenance:
{"artifacts":[{"algorithm":{"kind":"sha256","value":null},"artifact_id":"artifact.openapi","digest":"1111111111111111111111111111111111111111111111111111111111111111","path":"openapi.json","source_record_id":"source.demo.provider.v1"}],"canonical_schema_epoch":1,"classifier_epoch":1,"connector":{"id":"demo","semantic_sha256":"f6bc86c9d5004885bb3156ab320fa76ad3ff7e9686320c54735dcfbd8c27e934","version":{"major":1,"minor":0,"patch":0}},"dependencies":[],"donat_policy_ids":[],"embedded_material":[],"files":[],"generator_epoch":1,"licenses":[{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}}],"manifest_references":[{"artifact_hashes":[{"algorithm":{"kind":"sha256","value":null},"artifact_id":"artifact.openapi","digest":"1111111111111111111111111111111111111111111111111111111111111111","path":"openapi.json"}],"contract_fact_origins":[{"origin":{"kind":"provider_evidence","value":{"artifact_content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}},"source_record_id":"source.demo.provider.v1"}},"use_site":"effect.request.binding"}],"license_id":"license.demo","notice_id":"notice.demo","source_record_id":"source.demo.provider.v1"}],"notices":[{"id":"notice.demo","license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","notice_bundle_destination":"THIRD_PARTY_NOTICES.md","required_copyright_lines":["Copyright Demo"]}],"provider_evidence":[{"evidence":[{"accessed_on":"2026-07-29","content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","facts":[{"fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}}}],"source":{"kind":"repository_file","value":{"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","path":"openapi.json","repository":"https://github.com/example/demo"}},"terms":{"kind":"permissive","value":{"evidence_url":"https://example.test/terms/v1","license":{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}}}}}],"provider":"demo","source_record_id":"source.demo.provider.v1"}],"sources":[{"record_id":"source.demo.provider.v1","record_sha256":"420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991"}]}
SHA-256: 326236f741dfa72628b63ae308599b94e83b1c2aa1aa00bd80025ff5381a7531
```

The string-only value-contract bytes and hash above remain unchanged. The
following mutations exhaust the Spec-005 `ValueScalar` owner. For each row,
the exact canonical `ValueContractMaterialV1` bytes are the ASCII
concatenation of this prefix, the row's scalar bytes, and this suffix:

```text
prefix: {"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":
suffix: }}}},"value_language_epoch":1}
```

The hash is
`SHA256("donat.connector.value-contract.v1\0" || canonical_bytes)`:

| Spec-005 owner / mutation | Exact scalar bytes | SHA-256 |
| --- | --- | --- |
| `ValueScalar::Boolean` | `{"kind":"boolean","value":null}` | `d0b19f2e9f814ddc5457fd85728dfe4ef649042a5134f12d3ac42fb4009ecc58` |
| `ValueScalar::String` | `{"kind":"string","value":null}` | `79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a` |
| `ValueScalar::Int32` | `{"kind":"int32","value":null}` | `d91c7215c24937b62dc176287b48ca5c5f923d034777706323f0b61157a6a2f2` |
| `ValueScalar::Int64` | `{"kind":"int64","value":null}` | `d1f1966e3e49124f6cce79167814e323315c0e810143684321e7bc7ade23a972` |
| `ValueScalar::UInt64` | `{"kind":"uint64","value":null}` | `a64ccafb81f9b513c634d8f0e206e1aac705d5fcbd06fa8c5adacc34247f7ddb` |
| `ValueScalar::Decimal` | `{"kind":"decimal","value":null}` | `8f1a181165ec3d629693f106d9566d02f76eefe9317746044ee0693b7aa08f6b` |
| `ValueScalar::Uuid` | `{"kind":"uuid","value":null}` | `66c4b3a082f73831d439eb7c624409e9741621b4f7af14892896229f0bee524a` |
| `ValueScalar::Date` | `{"kind":"date","value":null}` | `93ed861bcc9b7f6213abcbdb87514515856c4d84eab444f13b3d678e3f3716a7` |
| `ValueScalar::Timestamp` | `{"kind":"timestamp","value":null}` | `f1c17e281b279e50480d60a9ee6568df17f7eef1da6e18fd60131a2300df971a` |
| `ValueScalar::TimestampTz` | `{"kind":"timestamptz","value":null}` | `d79bbe1e56bc00033fcae029d1c9b5826bb805e0657bf0acac285420bf42b169` |
| `ValueScalar::Json` | `{"kind":"json","value":null}` | `0b3c1359fac4024dc5dc65e6bace2144f075ba8cc55cfb62e8003ae244b0a879` |
| `ValueScalar::Custom { name: "custom.demo" }` | `{"kind":"custom","value":{"name":"custom.demo"}}` | `5f7c7c1db65b1e54751e4189a4ae314952912d31c0cab1b0d0f7b7ca6792e6ad` |
| `Custom.name mutation to "custom.changed"` | `{"kind":"custom","value":{"name":"custom.changed"}}` | `fec7767e4fc33c84ce50cc7a7b1e1c51395260f96a65aa1c8afa59b07937ea46` |

The independent nullable vector is exactly:

```text
{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":true,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}
SHA-256: 9630316fc75152223f33663a03f6be51d4953603a7fa9ccabf8560ca9585bd84
```

It changes only `TypeRefMaterialV1.nullable`; the nested scalar remains
`ValueScalar::String`. There is no `null` or `inline_bytes` scalar vector.
The separately tested `TypedValueMaterialV1` tags remain unchanged, including
`null`, `i64`, `u64`, decimal, and `inline_bytes`.

The source vector exercises complete provider-artifact evidence and fact
location/value. The semantic vector exercises a complete credential and
OAuth2 auth binding, provider-idempotent effect, non-`None` pagination,
complete eight-action fallback plus all matcher branches, every bound/default,
and both webhook and poll triggers. The provenance vector exercises the
matching provider evidence, manifest reference, license/notice/artifact, and
fact origin. The branch-complete mutation suite additionally visits every
source, auth, request, response, effect, pagination, matcher, retry,
redaction, trigger, value-type, and provenance-origin branch.

For connector `donat.http`, operation `get` version `1.0.0`, step `request`,
and `X-Request-ID`, the capability derivation bytes are
`{"connector":"donat.http","header":"x-request-id","operation":"get","operation_version":{"major":1,"minor":0,"patch":0},"step":"request"}`.
The digest is
`fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`
and the capability is
`response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3`.

Task 3 raw-byte fixtures cover escaped and unescaped U+FDD0, `\ud800`,
`\ud800\u0041`, invalid UTF-8, `{"a":1,"\u0061":2}`, `1e400`, exact accepted
boundary `9007199254740992`, rejected non-exact `9007199254740993`, and
recursive U+10000-before-U+FFFD ordering. Tests assert the exact codes above.
Further gates prove origin-only mutation leaves semantic bytes/hash unchanged,
value-only mutation leaves direct origin material unchanged while final
provenance changes through `semantic_sha256`, and no provenance-bearing
`OperationSpec`/`TriggerSpec` is serialized as semantic material.

## Alternatives

| Option | Why not |
| --- | --- |
| Hash runtime structs directly | provenance leaks into semantics and Serde evolution silently changes persistence |
| Maintain a smaller canonical behavioral schema | normalized fields or branches can disappear from persisted identity |
| Persist only current catalog IDs | an old revision silently adopts new behavior or attribution |
| Put deployment identity in semantic material | deployment changes would redefine portable catalog semantics and create circular pins |
| Derive header capabilities at runtime | rolling binaries could disagree and callers could influence authorization |

## Consequences

Task 3 owns explicit, field-total projections and raw-byte validation. Schema
changes require a new epoch/domain. Generated entries and process pins are
larger, but record, behavior, attribution, deployment, and value-contract
identities are deterministic, non-circular, and independently testable.
