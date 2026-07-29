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
components reject in phase 1. A processor-like reference is exactly
`{"id":Id,"implementation_revision":Epoch}`.

The following table closes every primitive leaf used below:

| Leaf | Canonical JSON v1 |
| --- | --- |
| `Id` and typed wrappers | JSON string, ASCII, 1–96 bytes, the ABI `InlineId` grammar; wrapper type is known from the containing field |
| ordinary string | valid Unicode scalar values, no surrogate or Unicode noncharacter, no normalization |
| `Hash256` / `Hash512` | exactly 64 / 128 lowercase hexadecimal ASCII characters |
| `GitCommit` / `GitTree` | exactly 40 lowercase hexadecimal ASCII characters |
| `Date` | validated Gregorian `YYYY-MM-DD` string |
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
    "version":StableSemver
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
  "resolved_fact_values":[ResolvedFactValueMaterialV1...],
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

Credentials sort by `credential`, manifest origins by `origin`, operations by
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
  tagged "scalar" ScalarMaterialV1 |
  tagged "enum" {"name":string,"values":[string...]} |
  tagged "object" {"fields":{name:FieldMaterialV1}} |
  tagged "list" {"element":TypeRefMaterialV1} |
  tagged "ref" {"name":string}
ScalarMaterialV1 =
  tagged "null" null | tagged "boolean" null |
  tagged "string" null | tagged "i64" null |
  tagged "u64" null | tagged "decimal" null |
  tagged "inline_bytes" null | tagged "custom" {"name":string}
```

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
  "resolved_fact_origins":[ResolvedFactOriginMaterialV1...],
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

`EvidenceTermsMaterialV1` is the same complete `permissive`, `reviewed_use`,
or `rejected` tagged branch used in source material; it has no fact value to
remove. Sources sort by `record_id`;
artifacts by `(source_record_id,artifact_id)`; files by
`(source_record_id,path)`; licenses by canonical bytes; dependencies and
embedded material by their normalized ID; notices by `id`; manifest
references and provider evidence by `source_record_id`; evidence by canonical
source identity; facts by `fact_id`; origins by `use_site`; policy IDs
lexically.

Direct provenance-origin bytes do not change when only a resolved value
changes. Final provenance bytes and hash do change because
`connector.semantic_sha256` commits the resulting semantic hash. Conversely,
an origin-only mutation leaves semantic bytes and hash unchanged.

### Normative field-totality matrix

This matrix is bidirectional: every normalized owner field appears in exactly
one row, and every canonical member above has an owner here. The only
intentional one-to-many operation is the stated resolved fact `(value,
origin)` split.

| Normalized owner and all fields/variants | Canonical projection path | Class | Order / null and empty rule |
| --- | --- | --- | --- |
| `ConnectorSourceRecord`: `record_version`, `record_id`, `subject`, `reacquisition`, `artifact_hashes`, `license`, `notice`, `entrypoints`, `dependencies`, `embedded_material`, `provider_contracts`, `compatibility`, `admission`, `safety_findings`, `reviewer`, `approval_date`, `proposed_manifest`, `proposed_destinations`, `red_tests` | `SourceRecordMaterialV1.*` | both, via `record_sha256` | field-specific rules below; optional manifest is explicit null |
| `SourceSubject::{ExactNpm,ProviderArtifact,DonatOwned}` | `source.subject.{kind,value}` | both, via record hash | branch tag required |
| `ExactNpmPackage`: `name`, `version`, `tarball_url`, `integrity`, `repository`, `npm_git_head`, `package_repository`, `signature`, `provenance`, `tag_commit`, `provenance_commit`, `maintainers`, `repository_owner`; all nested integrity/repository/signature/provenance/owner fields and branches | `source.subject.value.*` | provenance | maintainers/signatures sorted; optional commits null |
| `ExactProviderArtifact`: `provider`, `evidence`; `ProviderEvidenceArtifact`: `source`, `accessed_on`, `content_sha256`, `terms`, `facts`; both source branches; all term branches; `ProviderFact`: `fact_id`, `location`, `normalized_value`; both location branches | `source.subject.value.*`; `semantic.resolved_fact_values`; `provenance.provider_evidence` and `.resolved_fact_origins` | both | evidence by source identity; facts by ID; value/origin split only after resolution |
| `DonatOwnedSource`: `repository_commit`, `files.{path,sha256}` | `source.subject.value.*`; `provenance.files` | provenance | files by path; nonempty |
| `ArtifactHash`: `artifact_id`, `algorithm`, `digest`, `path`; all hash branches | `source.artifact_hashes`; `provenance.artifacts` and `.manifest_references[].artifact_hashes` | provenance | by artifact ID; optional path null |
| `LicenseDecision`, `NoticeIdentity`, `DependencyDecision`, `EmbeddedMaterialDecision`, `SafetyFindings`; every field and enum branch | same-named source members; provenance license/notice/dependency/embedded/file members | provenance | IDs as specified; findings by finding ID; empty findings explicit |
| `DependencyDisposition::{Shipped,BuildOnly,TypeOnlyReplaced,BehaviorOnly,Rejected}` | `DependencyDecisionMaterialV1.disposition` | provenance | exact five tags; payload fields never omitted |
| `ProviderContractReference`: `contract_id`, `facts`; `ContractFact::{ProviderEvidence,DonatPolicy}` and every payload field | `source.provider_contracts`; resolved semantic values and provenance origins/policy IDs | both | contract/fact stable keys; exact value/origin split |
| `CompatibilityDecision` and `AdmissionState`; every branch/payload | `source.compatibility`, `source.admission` | provenance | set payloads sorted; nonempty where normalized |
| `ConnectorManifest`: `connector`, `connector_version`, `manifest_version`, `runtime_abi_epoch`, `value_language_epoch`, `provider`, `api_identity`, `credentials`, `origins`, `operations`, `triggers`, `provenance` | `SemanticMaterialV1.connector`, epochs, behavioral arrays; `ProvenanceMaterialV1.manifest_references` | both | behavioral arrays sort by typed ID; provenance refs by source ID |
| `CredentialSpec`: `credential`, `version`, `fields`, `auth_plan`, `allowed_origins`, `scopes`, `auth_processor`, `credential_test_operation`, `bounds` | `semantic.credentials[]` | semantic | fields/origins/scopes sorted; optionals null |
| `CredentialFieldSpec`: `field`, `required`, `secret`, `maximum_bytes`, `redaction`; all secret/redaction branches | `semantic.credentials[].fields[]` | semantic | fields by ID; branch payload explicit |
| `CredentialBounds`: `maximum_field_bytes`, `maximum_aggregate_bytes`, `maximum_token_bytes` | `semantic.credentials[].bounds` | semantic | all required nonzero numbers |
| `AuthPlan`: all six branches and every field binding | `semantic.credentials[].auth_plan` | semantic | exact tag; scopes sorted |
| `FixedOrigin`: `origin`, `scheme`, `host`, `port`, `network_policy`; all network branches | `semantic.origins[]` and `semantic.operations[].origins[]` | semantic | origins by ID; no nulls |
| `OperationSpec`: `connector`, `connector_version`, `operation`, `operation_version`, `runtime_abi_epoch`, `value_language_epoch`, `input`, `input_contract_sha256`, `output`, `output_contract_sha256`, `credential`, `origins`, `steps`, `pre_request_transforms`, `post_response_transforms`, `operation_processor`, `effect`, `pagination`, `error_map`, `capacity`, `rate`, `serialization_key_default`, `bounds`, `resolved_fact_values` | `semantic.operations[].*` | semantic | steps/transforms ordered; credential/processor/default null; origins/facts sorted |
| `CompiledStepSpec`: `step`, `method`, `origin`, `path`, `query`, `headers`, `credential_action`, `request`, `success_statuses`, `response`, `selected_response_headers`, `bounds` | `semantic.operations[].steps[]` | semantic | steps ordered; action null; named bindings/headers/statuses sorted |
| `CompiledBinding`, query/header wrappers, binding source branches, credential action | corresponding step binding/action members | semantic | named bindings by name; optional default/mapping null |
| `CompiledRequestShape`: `None`, `Json`, `FormUrlencoded`, `Multipart`, `RawBytes`; every payload | step `.request` | semantic | declared binding order; exact five tags |
| `CompiledResponseShape`: `Json`, `RawBytes`; `ResponseMapping`; `StatusRange`; `SelectedResponseHeader` | step `.response`, `.success_statuses`, `.selected_response_headers` | semantic | mappings ordered; statuses/header mappings sorted |
| `VersionedProcessorRef`: `id`, `implementation_revision`; pre/post transforms and operation processor | corresponding operation/credential/trigger/pagination processor paths | semantic | transform arrays ordered; optional processor null |
| `OperationEffect`: both branches; `ProviderIdempotentStep`: `step`, `fixed_binding`, `scope`, `minimum_retention_ms`, `clock_safety_margin_ms`; both binding branches | operation `.effect` | semantic value; provenance origin through fact use site | steps by ID; exact tag/payload |
| `PaginationPlan`: `None`, `Cursor`, `OffsetLimit`, `PageNumber`, `LinkRelation`, `Processor`; every binding/pointer/relation/header/processor field and mandatory `PaginationBounds` | operation `.pagination` | semantic | exact six tags; none has null payload; every other branch has bounds |
| `ErrorMap`: `rules`, `fallback`; `CompleteErrorFallback`: all eight named actions | operation `.error_map` | semantic | rules ordered; all fallback members required |
| `ErrorAction`: `class`, `code`, `safe_message`, `retry_after`, `correlations`; all class/retry branches; every correlation field | every error action path | semantic | correlations by step/header; retry payload explicit |
| `ErrorMatcher`: `Status`, `ProviderCode`, `Header`, `MalformedDeclaredSuccess`; every payload field | error rule `.matcher` | semantic | codes/values sorted; exact four tags |
| `CapacityDefaults`, `RateDefaults`, `TypedSerializationKeyDefault`, `StepBounds`, `OperationBounds`, `PaginationBounds`; every field | corresponding semantic operation/step/pagination member | semantic | all fields required; optional serialization default null |
| `TriggerSpec::Webhook`: connector/version/trigger/event versions, runtime epoch, authenticator, codec, normalizer, selected headers, raw-body/time bounds, event ID/type/output contracts, redaction, subscription operations and all nested fields | `semantic.triggers[].{kind=webhook,value.*}` | semantic | selected headers sorted; subscription/check explicit null |
| `TriggerSpec::Poll`: connector/version/trigger/event versions, runtime epoch, checkpoint, processor, event type, per-poll limit, bounds | `semantic.triggers[].{kind=poll,value.*}` | semantic | no nulls |
| `ManifestProvenanceReference`: `source_record_id`, `artifact_hashes`, `license_id`, `notice_id`, `contract_facts`; each `ResolvedContractFactBinding.{use_site,fact}` and both fact branches/payloads | `provenance.manifest_references[]` plus resolved semantic value/provenance origin split | both | refs by source ID; facts by use site |
| `ValueContractCatalog`, field/type/ref/scalar branches, complete named-object closure | every value-contract material embedded above | semantic | maps by UTF-16; enum values ordered |
| Projection constants and join-derived values: canonical/classifier/generator epochs; record/contract/semantic/provenance/snapshot hashes; provenance `source_record_id`, `record_sha256`, `artifact_content_sha256`, and collected `donat_policy_ids` | named material/envelope/provenance fields | both | fixed owner constant, domain function, or validated source/fact join; never a free projection field |

Task 3 implements
`canonical_projection_field_matrix_is_total` from this table. It must fail
when a normalized field or enum variant has no projection, when a projection
member has no normalized/constant/derived owner, or when a field is mapped
twice outside the fact split. A generated mutation matrix changes every field
and visits every enum branch. Each mutation must alter its applicable direct
canonical bytes/hash and preserve the other direct domain; final provenance
continues to commit the semantic hash.

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
{"canonical_schema_epoch":1,"connector":{"api_identity":"demo.v1","id":"demo","manifest_version":1,"provider":"provider.demo","runtime_abi_epoch":1,"version":{"major":1,"minor":0,"patch":0}},"credentials":[{"allowed_origins":["origin.demo"],"auth_plan":{"kind":"oauth2_client_credentials","value":{"client_id":"field.client_id","client_secret":"field.client_secret","scopes":["widgets.read"],"token_origin":"origin.demo","token_pointer":"/access_token","token_step":"token"}},"auth_processor":{"id":"auth.demo","implementation_revision":1},"bounds":{"maximum_aggregate_bytes":512,"maximum_field_bytes":256,"maximum_token_bytes":256},"credential":"credential.demo","credential_test_operation":{"operation":"op.read","version":{"major":1,"minor":0,"patch":0}},"fields":[{"field":"field.client_id","maximum_bytes":128,"redaction":{"kind":"omit","value":null},"required":true,"secret":{"kind":"non_secret","value":null}},{"field":"field.client_secret","maximum_bytes":256,"redaction":{"kind":"omit","value":null},"required":true,"secret":{"kind":"secret","value":null}}],"scopes":["widgets.read"],"version":{"major":1,"minor":0,"patch":0}}],"operations":[{"bounds":{"deadline_ms":"1000","maximum_aggregate_request_bytes":2048,"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_redirects":0},"capacity":{"maximum_in_flight":4},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"credential":{"credential":"credential.demo","version":{"major":1,"minor":0,"patch":0}},"effect":{"kind":"provider_idempotent","value":{"side_effect_steps":[{"clock_safety_margin_ms":"1000","fixed_binding":{"kind":"header","value":{"name":"idempotency-key"}},"minimum_retention_ms":"86400000","scope":"scope.demo","step":"request"}]}},"error_map":{"fallback":{"authentication":{"class":{"kind":"authentication","value":null},"code":"auth_error","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"authentication failed"},"http_429":{"class":{"kind":"http_429","value":null},"code":"rate_limited","correlations":[{"canonical_lowercase_header_name":"x-request-id","capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3","step":"request"}],"retry_after":{"kind":"retry_after_header","value":{"capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3","maximum_seconds":86400,"step":"request"}},"safe_message":"rate limited"},"http_5xx":{"class":{"kind":"http_5xx","value":null},"code":"provider_unavailable","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider unavailable"},"invariant":{"class":{"kind":"invariant","value":null},"code":"connector_invariant","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"connector invariant"},"permanent":{"class":{"kind":"permanent","value":null},"code":"provider_rejected","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider rejected request"},"timeout":{"class":{"kind":"timeout","value":null},"code":"provider_timeout","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider timed out"},"transport":{"class":{"kind":"transport","value":null},"code":"provider_transport","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider transport failed"},"validation":{"class":{"kind":"validation","value":null},"code":"provider_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"provider validation failed"}},"rules":[{"action":{"class":{"kind":"validation","value":null},"code":"status_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"status validation"},"matcher":{"kind":"status","value":{"maximum":422,"minimum":400}}},{"action":{"class":{"kind":"validation","value":null},"code":"code_validation","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"code validation"},"matcher":{"kind":"provider_code","value":{"codes":["invalid"],"pointer":"/error/code"}}},{"action":{"class":{"kind":"permanent","value":null},"code":"header_error","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"header error"},"matcher":{"kind":"header","value":{"name":"x-error","values":["true"]}}},{"action":{"class":{"kind":"validation","value":null},"code":"malformed_success","correlations":[],"retry_after":{"kind":"never","value":null},"safe_message":"malformed success"},"matcher":{"kind":"malformed_declared_success","value":null}}]},"input":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"input_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","operation":"op.read","operation_processor":{"id":"processor.demo","implementation_revision":1},"operation_version":{"major":1,"minor":0,"patch":0},"origins":[{"host":"api.example.test","network_policy":{"kind":"public_only","value":null},"origin":"origin.demo","port":443,"scheme":{"kind":"https","value":null}}],"output":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"output_contract_sha256":"79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a","pagination":{"kind":"cursor","value":{"bounds":{"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_response_bytes":1024},"request_binding":"query","response_pointer":"/next_cursor"}},"post_response_transforms":[{"id":"transform.response","implementation_revision":1}],"pre_request_transforms":[{"id":"transform.request","implementation_revision":1}],"rate":{"burst":10,"refill_interval_ms":"1000"},"resolved_fact_values":[{"use_site":"effect.request.binding","value":{"kind":"string","value":"Idempotency-Key"}}],"runtime_abi_epoch":1,"serialization_key_default":{"field":"query","value":{"kind":"string","value":"default"}},"steps":[{"bounds":{"deadline_ms":"1000","maximum_header_bytes":1024,"maximum_headers":16,"maximum_inline_binary_bytes":1,"maximum_json_depth":8,"maximum_json_nodes":128,"maximum_request_bytes":1024,"maximum_response_bytes":1024,"maximum_url_bytes":1024},"credential_action":{"credential":"credential.demo"},"headers":[{"binding":{"default":null,"field":"query","mapping":null,"required":true,"source":{"kind":"input","value":null}},"name":"x-query"}],"method":"POST","origin":"origin.demo","path":"/widgets","query":[],"request":{"kind":"json","value":{"bindings":["query"]}},"response":{"kind":"json","value":{"mappings":[{"pointer":"/result","target":"query"}]}},"selected_response_headers":[{"canonical_lowercase_header_name":"x-request-id","capability":"response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3"}],"step":"request","success_statuses":[{"maximum":299,"minimum":200}]}],"value_language_epoch":1}],"origins":[{"host":"api.example.test","network_policy":{"kind":"public_only","value":null},"origin":"origin.demo","port":443,"scheme":{"kind":"https","value":null}}],"resolved_fact_values":[{"use_site":"effect.request.binding","value":{"kind":"string","value":"Idempotency-Key"}}],"triggers":[{"kind":"poll","value":{"bounds":{"deadline_ms":"1000","maximum_aggregate_request_bytes":2048,"maximum_aggregate_response_bytes":4096,"maximum_calls":4,"maximum_items":100,"maximum_output_canonical_bytes":4096,"maximum_pages":4,"maximum_redirects":0},"checkpoint":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"event_type":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_version":{"major":1,"minor":0,"patch":0},"per_poll_event_limit":100,"processor":{"id":"poll.demo","implementation_revision":1},"runtime_abi_epoch":1,"trigger":"trigger.poll","trigger_version":{"major":1,"minor":0,"patch":0}}},{"kind":"webhook","value":{"authenticator":{"id":"auth.webhook","implementation_revision":1},"codec":{"id":"codec.json","implementation_revision":1},"connector":"demo","connector_version":{"major":1,"minor":0,"patch":0},"event_id":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_type":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"event_version":{"major":1,"minor":0,"patch":0},"normalizer":{"id":"normalize.webhook","implementation_revision":1},"output":{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1},"raw_body_max_bytes":1024,"redaction":{"kind":"omit","value":null},"runtime_abi_epoch":1,"selected_headers":["x-signature"],"subscription_operations":{"check":null,"create":"subscription.create","delete":"subscription.delete"},"timestamp_window_ms":"300000","trigger":"trigger.webhook","trigger_version":{"major":1,"minor":0,"patch":0}}}],"value_language_epoch":1}
SHA-256: 86758001d76edf0087fbe3e734462391c4855b0862a00fa1e20b93610aa53419

provenance:
{"artifacts":[{"algorithm":{"kind":"sha256","value":null},"artifact_id":"artifact.openapi","digest":"1111111111111111111111111111111111111111111111111111111111111111","path":"openapi.json","source_record_id":"source.demo.provider.v1"}],"canonical_schema_epoch":1,"classifier_epoch":1,"connector":{"id":"demo","semantic_sha256":"86758001d76edf0087fbe3e734462391c4855b0862a00fa1e20b93610aa53419","version":{"major":1,"minor":0,"patch":0}},"dependencies":[],"donat_policy_ids":[],"embedded_material":[],"files":[],"generator_epoch":1,"licenses":[{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}}],"manifest_references":[{"artifact_hashes":[{"algorithm":{"kind":"sha256","value":null},"artifact_id":"artifact.openapi","digest":"1111111111111111111111111111111111111111111111111111111111111111","path":"openapi.json"}],"contract_fact_origins":[{"origin":{"kind":"provider_evidence","value":{"artifact_content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}},"source_record_id":"source.demo.provider.v1"}},"use_site":"effect.request.binding"}],"license_id":"license.demo","notice_id":"notice.demo","source_record_id":"source.demo.provider.v1"}],"notices":[{"id":"notice.demo","license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","notice_bundle_destination":"THIRD_PARTY_NOTICES.md","required_copyright_lines":["Copyright Demo"]}],"provider_evidence":[{"evidence":[{"accessed_on":"2026-07-29","content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","facts":[{"fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}}}],"source":{"kind":"repository_file","value":{"commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","path":"openapi.json","repository":"https://github.com/example/demo"}},"terms":{"kind":"permissive","value":{"evidence_url":"https://example.test/terms/v1","license":{"kind":"permissive","value":{"license_file_path":"LICENSE","license_file_sha256":"2222222222222222222222222222222222222222222222222222222222222222","selected_dual_license_branch":null,"spdx_id":"MIT"}}}}}],"provider":"demo","source_record_id":"source.demo.provider.v1"}],"resolved_fact_origins":[{"origin":{"kind":"provider_evidence","value":{"artifact_content_sha256":"1111111111111111111111111111111111111111111111111111111111111111","fact_id":"fact.idempotency","location":{"kind":"json_pointer","value":{"path":"openapi.json","pointer":"/paths/~1widgets/post"}},"source_record_id":"source.demo.provider.v1"}},"use_site":"effect.request.binding"}],"sources":[{"record_id":"source.demo.provider.v1","record_sha256":"420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991"}]}
SHA-256: 4147281e4df2d68b86e3b9909a083355991a342b36766c8ea732a6a264ea9b59
```

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
