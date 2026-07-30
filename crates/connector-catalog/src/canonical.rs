use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::ops::Range;

use base64::Engine;
use donat_connector_abi::{
    AuthenticatorId, CapabilityId, CodecId, CompiledStepId, ConnectorErrorClass, ConnectorId,
    CredentialFieldId, CredentialSpecId, Hash256 as AbiHash256, NormalizerId, OperationId,
    OriginId, ProcessorFamilyId, TriggerId,
};
use donat_value_contract::{
    TypeRef, TypedValue, ValueContractCatalog, ValueContractField, ValueObjectContract,
    ValueScalar, ValueType,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::{
    AcceptedRecordCatalog, ArtifactHash, CatalogError, ConnectorSourceRecord, ContractFact,
    DonatPolicyId, ExactFactLocation, ProviderContractReference, ResolvedContractFactBinding,
    ResolvedFactValue, SelectedResponseHeader, SourceRecordId, SourceSubject, StableSemver,
    TypedValueMaterialV1,
};

const SOURCE_RECORD_DOMAIN: &[u8] = b"donat.connector.source-record.v1\0";
const SEMANTIC_DOMAIN: &[u8] = b"donat.connector.semantic.v1\0";
const PROVENANCE_DOMAIN: &[u8] = b"donat.connector.provenance.v1\0";
const VALUE_CONTRACT_DOMAIN: &[u8] = b"donat.connector.value-contract.v1\0";
const RESPONSE_HEADER_DOMAIN: &[u8] = b"donat.connector.response-header-capability.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogHashDomain {
    SourceRecord,
    Semantic,
    Provenance,
    ValueContract,
}

impl CatalogHashDomain {
    const fn prefix(self) -> &'static [u8] {
        match self {
            Self::SourceRecord => SOURCE_RECORD_DOMAIN,
            Self::Semantic => SEMANTIC_DOMAIN,
            Self::Provenance => PROVENANCE_DOMAIN,
            Self::ValueContract => VALUE_CONTRACT_DOMAIN,
        }
    }
}

fn domain_hash_bytes(domain: CatalogHashDomain, canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain.prefix());
    hash.update(canonical_bytes);
    hash.finalize().into()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[allow(clippy::large_enum_variant)]
enum SourceSubjectMaterialV1 {
    ExactNpm(ExactNpmMaterialV1),
    ProviderArtifact(ProviderArtifactMaterialV1),
    DonatOwned(DonatOwnedMaterialV1),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExactNpmMaterialV1 {
    name: String,
    version: String,
    tarball_url: String,
    integrity: NpmIntegrityMaterialV1,
    repository: ImmutableRepositoryMaterialV1,
    npm_git_head: String,
    package_repository: String,
    signature: NpmSignatureMaterialV1,
    provenance: NpmProvenanceMaterialV1,
    tag_commit: Option<String>,
    provenance_commit: Option<String>,
    maintainers: Vec<String>,
    repository_owner: RepositoryOwnerMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct NpmIntegrityMaterialV1 {
    algorithm: NpmIntegrityAlgorithmMaterialV1,
    digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum NpmIntegrityAlgorithmMaterialV1 {
    Sha512(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ImmutableRepositoryMaterialV1 {
    url: String,
    commit: String,
    tree: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VerifiedNpmSignatureMaterialV1 {
    key_id: String,
    signature_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum NpmSignatureMaterialV1 {
    Verified {
        signatures: Vec<VerifiedNpmSignatureMaterialV1>,
        registry_metadata_sha256: String,
    },
    VerifiedAbsent {
        registry_metadata_sha256: String,
    },
    Rejected {
        finding: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum NpmProvenanceMaterialV1 {
    Verified {
        statement_sha256: String,
        source_commit: String,
    },
    VerifiedAbsent {
        registry_metadata_sha256: String,
    },
    Rejected {
        finding: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum RepositoryOwnerMaterialV1 {
    Consistent {
        package_owner: String,
        repository_owner: String,
    },
    ReviewedMismatch {
        decision_id: String,
    },
    Rejected {
        finding: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderArtifactMaterialV1 {
    provider: String,
    evidence: Vec<ProviderEvidenceMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderEvidenceMaterialV1 {
    source: ProviderEvidenceSourceMaterialV1,
    accessed_on: String,
    content_sha256: String,
    terms: EvidenceTermsMaterialV1,
    facts: Vec<ProviderFactMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ProviderEvidenceSourceMaterialV1 {
    RepositoryFile {
        repository: String,
        commit: String,
        path: String,
    },
    VersionedArtifact {
        url: String,
        provider_revision: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum EvidenceTermsMaterialV1 {
    Permissive {
        license: LicenseDecisionMaterialV1,
        evidence_url: String,
    },
    ReviewedUse {
        decision_id: String,
        evidence_url: String,
    },
    Rejected {
        finding: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderFactMaterialV1 {
    fact_id: String,
    location: ExactFactLocationMaterialV1,
    #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
    normalized_value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ExactFactLocationMaterialV1 {
    JsonPointer { path: String, pointer: String },
    DocumentSection { path: String, section: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DonatOwnedMaterialV1 {
    repository_commit: String,
    files: Vec<RepoFileHashMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RepoFileHashMaterialV1 {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ReacquisitionMaterialV1 {
    ExactNpmReview(()),
    ProviderRepositoryReview(()),
    ProviderVersionedArtifactReview(()),
    DonatOwnedNoNetwork(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactHashMaterialV1 {
    artifact_id: String,
    algorithm: HashAlgorithmMaterialV1,
    digest: String,
    path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum HashAlgorithmMaterialV1 {
    Sha256(()),
    Sha512(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum LicenseDecisionMaterialV1 {
    Permissive {
        spdx_id: String,
        selected_dual_license_branch: Option<String>,
        license_file_path: String,
        license_file_sha256: String,
    },
    WrittenGrant {
        decision_id: String,
        grant_sha256: String,
    },
    Rejected {
        finding: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct NoticeMaterialV1 {
    id: String,
    license_file_path: String,
    license_file_sha256: String,
    required_copyright_lines: Vec<String>,
    notice_bundle_destination: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DependencyDecisionMaterialV1 {
    dependency: String,
    disposition: DependencyDispositionMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum DependencyDispositionMaterialV1 {
    Shipped { license: LicenseDecisionMaterialV1 },
    BuildOnly { license: LicenseDecisionMaterialV1 },
    TypeOnlyReplaced { replacement: String },
    BehaviorOnly { reason: String },
    Rejected { finding: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedDecisionMaterialV1 {
    material_id: String,
    path: String,
    sha256: String,
    disposition: EmbeddedMaterialDispositionMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum EmbeddedMaterialDispositionMaterialV1 {
    Shipped { license: LicenseDecisionMaterialV1 },
    BehaviorOnly { reason: String },
    Rejected { finding: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderContractMaterialV1 {
    contract_id: String,
    facts: Vec<ContractFactMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ContractFactMaterialV1 {
    ProviderEvidence {
        source_record_id: String,
        fact_id: String,
    },
    DonatPolicy {
        policy_id: String,
        #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
        value: TypedValueMaterialV1,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum CompatibilityMaterialV1 {
    TierA(()),
    TierB(()),
    TierC(()),
    Rejected(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum AdmissionMaterialV1 {
    InventoryOnly { findings: Vec<String> },
    ApprovedForPort { operations: Vec<String> },
    EvidenceAccepted { contracts: Vec<String> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SafetyFindingsMaterialV1 {
    findings: Vec<SafetyFindingMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SafetyFindingMaterialV1 {
    finding_id: String,
    kind: String,
    location: Option<String>,
    message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Closed source-record hash material.
///
/// Raw JSON cannot construct this permanent hash input.
///
/// ```compile_fail
/// use donat_connector_catalog::SourceRecordMaterialV1;
/// let _: SourceRecordMaterialV1 = serde_json::from_str("{}").unwrap();
/// ```
pub struct SourceRecordMaterialV1 {
    record_version: u32,
    record_id: String,
    subject: SourceSubjectMaterialV1,
    reacquisition: ReacquisitionMaterialV1,
    artifact_hashes: Vec<ArtifactHashMaterialV1>,
    license: LicenseDecisionMaterialV1,
    notice: NoticeMaterialV1,
    entrypoints: Vec<String>,
    dependencies: Vec<DependencyDecisionMaterialV1>,
    embedded_material: Vec<EmbeddedDecisionMaterialV1>,
    provider_contracts: Vec<ProviderContractMaterialV1>,
    compatibility: CompatibilityMaterialV1,
    admission: AdmissionMaterialV1,
    safety_findings: SafetyFindingsMaterialV1,
    reviewer: String,
    approval_date: String,
    proposed_manifest: Option<String>,
    proposed_destinations: Vec<String>,
    red_tests: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticCredentialMaterialV1 {
    credential: String,
    version: StableSemver,
    fields: Vec<CredentialFieldMaterialV1>,
    auth_plan: CredentialAuthMaterialV1,
    allowed_origins: Vec<String>,
    scopes: Vec<String>,
    auth_processor: Option<VersionedProcessorMaterialV1>,
    credential_test_operation: Option<VersionedOperationReferenceMaterialV1>,
    bounds: CredentialBoundsMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialFieldMaterialV1 {
    field: String,
    required: bool,
    secret: SecretClassificationMaterialV1,
    maximum_bytes: u32,
    redaction: RedactionMaterialV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum SecretClassificationMaterialV1 {
    Secret(()),
    Sensitive(()),
    NonSecret(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum RedactionMaterialV1 {
    Omit(()),
    Fixed { replacement: String },
    PreserveLast { characters: u8 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum CredentialAuthMaterialV1 {
    FixedHeaderApiKey {
        field: String,
        header: String,
    },
    FixedQueryApiKey {
        field: String,
        query: String,
    },
    Bearer {
        token: String,
    },
    HttpBasic {
        username: String,
        password: String,
    },
    #[serde(rename = "oauth2_client_credentials")]
    OAuth2ClientCredentials {
        client_id: String,
        client_secret: String,
        token_origin: String,
        token_step: String,
        scopes: Vec<String>,
        token_pointer: String,
    },
    #[serde(rename = "preprovisioned_oauth_access_token")]
    PreprovisionedOAuthAccessToken {
        token: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialBoundsMaterialV1 {
    maximum_field_bytes: u32,
    maximum_aggregate_bytes: u32,
    maximum_token_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VersionedProcessorMaterialV1 {
    id: String,
    implementation_revision: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VersionedOperationReferenceMaterialV1 {
    operation: String,
    version: StableSemver,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VersionedCredentialMaterialV1 {
    credential: String,
    version: StableSemver,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticOriginMaterialV1 {
    origin: String,
    scheme: HttpsMaterialV1,
    host: String,
    port: u16,
    network_policy: NetworkPolicyMaterialV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum HttpsMaterialV1 {
    #[serde(rename = "https")]
    HttpsOnly(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum NetworkPolicyMaterialV1 {
    PublicOnly(()),
    PrivateAllowed { policy: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticOperationMaterialV1 {
    connector: String,
    connector_version: StableSemver,
    operation: String,
    operation_version: StableSemver,
    runtime_abi_epoch: u32,
    value_language_epoch: u32,
    #[serde(deserialize_with = "deserialize_value_contract_material")]
    input: ValueContractMaterialV1,
    input_contract_sha256: String,
    #[serde(deserialize_with = "deserialize_value_contract_material")]
    output: ValueContractMaterialV1,
    output_contract_sha256: String,
    credential: Option<VersionedCredentialMaterialV1>,
    origins: Vec<SemanticOriginMaterialV1>,
    steps: Vec<SemanticStepMaterialV1>,
    pre_request_transforms: Vec<VersionedProcessorMaterialV1>,
    post_response_transforms: Vec<VersionedProcessorMaterialV1>,
    operation_processor: Option<VersionedProcessorMaterialV1>,
    effect: OperationEffectMaterialV1,
    pagination: PaginationMaterialV1,
    error_map: ErrorMapMaterialV1,
    capacity: CapacityDefaultsMaterialV1,
    rate: RateDefaultsMaterialV1,
    serialization_key_default: Option<TypedSerializationKeyDefaultMaterialV1>,
    bounds: OperationBoundsMaterialV1,
    #[serde(deserialize_with = "deserialize_resolved_fact_values")]
    resolved_fact_values: Vec<ResolvedFactValueMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticStepMaterialV1 {
    step: String,
    method: String,
    origin: String,
    path: String,
    query: Vec<CompiledQueryBindingMaterialV1>,
    headers: Vec<CompiledHeaderBindingMaterialV1>,
    credential_action: Option<CompiledCredentialActionMaterialV1>,
    request: CompiledRequestMaterialV1,
    success_statuses: Vec<StatusRangeMaterialV1>,
    response: CompiledResponseMaterialV1,
    selected_response_headers: Vec<SelectedResponseHeaderMaterialV1>,
    bounds: StepBoundsMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingMaterialV1 {
    field: String,
    source: CompiledBindingSourceMaterialV1,
    required: bool,
    #[serde(deserialize_with = "crate::source::deserialize_optional_typed_value_material")]
    default: Option<TypedValueMaterialV1>,
    mapping: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum CompiledBindingSourceMaterialV1 {
    Input(()),
    Constant {
        #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
        value: TypedValueMaterialV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompiledQueryBindingMaterialV1 {
    name: String,
    binding: BindingMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompiledHeaderBindingMaterialV1 {
    name: String,
    binding: BindingMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompiledCredentialActionMaterialV1 {
    credential: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum CompiledRequestMaterialV1 {
    None(()),
    Json { bindings: Vec<String> },
    FormUrlencoded { bindings: Vec<String> },
    Multipart { bindings: Vec<String> },
    RawBytes { binding: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResponseMappingMaterialV1 {
    pointer: String,
    target: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum CompiledResponseMaterialV1 {
    Json {
        mappings: Vec<ResponseMappingMaterialV1>,
    },
    RawBytes {
        target: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectedResponseHeaderMaterialV1 {
    canonical_lowercase_header_name: String,
    capability: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StatusRangeMaterialV1 {
    minimum: u16,
    maximum: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StepBoundsMaterialV1 {
    maximum_headers: u32,
    maximum_header_bytes: u32,
    maximum_url_bytes: u32,
    maximum_request_bytes: u32,
    maximum_response_bytes: u32,
    maximum_json_depth: u32,
    maximum_json_nodes: u32,
    maximum_inline_binary_bytes: u32,
    deadline_ms: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationBoundsMaterialV1 {
    maximum_calls: u32,
    maximum_pages: u32,
    maximum_items: u32,
    maximum_aggregate_request_bytes: u32,
    maximum_aggregate_response_bytes: u32,
    maximum_output_canonical_bytes: u32,
    maximum_redirects: u8,
    deadline_ms: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum OperationEffectMaterialV1 {
    ReadOnly(()),
    ProviderIdempotent {
        side_effect_steps: Vec<ProviderIdempotentStepMaterialV1>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderIdempotentStepMaterialV1 {
    step: String,
    fixed_binding: FixedIdempotencyBindingMaterialV1,
    scope: String,
    minimum_retention_ms: String,
    clock_safety_margin_ms: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum FixedIdempotencyBindingMaterialV1 {
    Header { name: String },
    BodyField { pointer: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CapacityDefaultsMaterialV1 {
    maximum_in_flight: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RateDefaultsMaterialV1 {
    burst: u32,
    refill_interval_ms: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TypedSerializationKeyDefaultMaterialV1 {
    field: String,
    #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
    value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PaginationBoundsMaterialV1 {
    maximum_calls: u32,
    maximum_pages: u32,
    maximum_items: u32,
    maximum_response_bytes: u32,
    maximum_aggregate_response_bytes: u32,
    maximum_output_canonical_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum PaginationMaterialV1 {
    None(()),
    Cursor {
        request_binding: String,
        response_pointer: String,
        bounds: PaginationBoundsMaterialV1,
    },
    OffsetLimit {
        offset_binding: String,
        limit_binding: String,
        initial_offset: String,
        page_size: u32,
        bounds: PaginationBoundsMaterialV1,
    },
    PageNumber {
        page_binding: String,
        page_size_binding: String,
        initial_page: String,
        page_size: u32,
        bounds: PaginationBoundsMaterialV1,
    },
    LinkRelation {
        relation: String,
        selected_header: SelectedResponseHeaderMaterialV1,
        bounds: PaginationBoundsMaterialV1,
    },
    Processor {
        processor: VersionedProcessorMaterialV1,
        bounds: PaginationBoundsMaterialV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorMapMaterialV1 {
    rules: Vec<ErrorRuleMaterialV1>,
    fallback: CompleteErrorFallbackMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorRuleMaterialV1 {
    matcher: ErrorMatcherMaterialV1,
    action: ErrorActionMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorActionMaterialV1 {
    class: ConnectorErrorClassMaterialV1,
    code: String,
    safe_message: String,
    retry_after: RetryAfterMaterialV1,
    correlations: Vec<ErrorCorrelationMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorCorrelationMaterialV1 {
    canonical_lowercase_header_name: String,
    capability: String,
    step: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompleteErrorFallbackMaterialV1 {
    transport: ErrorActionMaterialV1,
    timeout: ErrorActionMaterialV1,
    http_429: ErrorActionMaterialV1,
    http_5xx: ErrorActionMaterialV1,
    authentication: ErrorActionMaterialV1,
    validation: ErrorActionMaterialV1,
    permanent: ErrorActionMaterialV1,
    invariant: ErrorActionMaterialV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ConnectorErrorClassMaterialV1 {
    Transport(()),
    Timeout(()),
    #[serde(rename = "http_429")]
    Http429(()),
    #[serde(rename = "http_5xx")]
    Http5xx(()),
    Authentication(()),
    Validation(()),
    Permanent(()),
    Invariant(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ErrorMatcherMaterialV1 {
    Status(StatusRangeMaterialV1),
    ProviderCode { pointer: String, codes: Vec<String> },
    Header { name: String, values: Vec<String> },
    MalformedDeclaredSuccess(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum RetryAfterMaterialV1 {
    Never(()),
    RetryAfterHeader {
        step: String,
        capability: String,
        maximum_seconds: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[allow(clippy::large_enum_variant)]
enum SemanticTriggerMaterialV1 {
    Webhook {
        connector: String,
        connector_version: StableSemver,
        trigger: String,
        trigger_version: StableSemver,
        event_version: StableSemver,
        runtime_abi_epoch: u32,
        authenticator: VersionedProcessorMaterialV1,
        codec: VersionedProcessorMaterialV1,
        normalizer: VersionedProcessorMaterialV1,
        selected_headers: Vec<String>,
        raw_body_max_bytes: u32,
        timestamp_window_ms: String,
        #[serde(deserialize_with = "deserialize_value_contract_material")]
        event_id: ValueContractMaterialV1,
        #[serde(deserialize_with = "deserialize_value_contract_material")]
        event_type: ValueContractMaterialV1,
        #[serde(deserialize_with = "deserialize_value_contract_material")]
        output: ValueContractMaterialV1,
        redaction: RedactionMaterialV1,
        subscription_operations: Option<SubscriptionOperationIdsMaterialV1>,
    },
    Poll {
        connector: String,
        connector_version: StableSemver,
        trigger: String,
        trigger_version: StableSemver,
        event_version: StableSemver,
        runtime_abi_epoch: u32,
        #[serde(deserialize_with = "deserialize_value_contract_material")]
        checkpoint: ValueContractMaterialV1,
        processor: VersionedProcessorMaterialV1,
        #[serde(deserialize_with = "deserialize_value_contract_material")]
        event_type: ValueContractMaterialV1,
        per_poll_event_limit: u32,
        bounds: OperationBoundsMaterialV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionOperationIdsMaterialV1 {
    create: String,
    delete: String,
    check: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConnectorManifestDto {
    connector: String,
    connector_version: StableSemver,
    manifest_version: u32,
    runtime_abi_epoch: u32,
    value_language_epoch: u32,
    provider: String,
    api_identity: String,
    credentials: Vec<SemanticCredentialMaterialV1>,
    origins: Vec<SemanticOriginMaterialV1>,
    operations: Vec<SemanticOperationMaterialV1>,
    triggers: Vec<SemanticTriggerMaterialV1>,
    provenance: Vec<ManifestProvenanceInputDto>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestProvenanceInputDto {
    source_record_id: SourceRecordId,
    artifact_hashes: Vec<ArtifactHash>,
    license_id: String,
    notice_id: crate::NoticeId,
    contract_facts: Vec<ResolvedContractFactBindingDto>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResolvedContractFactBindingDto {
    use_site: String,
    fact: ContractFact,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConnectorMaterialV1 {
    api_identity: String,
    id: String,
    manifest_version: u32,
    provider: String,
    runtime_abi_epoch: u32,
    version: StableSemver,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Closed semantic hash material produced only from a checked manifest.
///
/// Nested semantic operation material is intentionally not public.
///
/// ```compile_fail
/// use donat_connector_catalog::SemanticOperationMaterialV1;
/// ```
pub struct SemanticMaterialV1 {
    canonical_schema_epoch: u32,
    connector: SemanticConnectorMaterialV1,
    credentials: Vec<SemanticCredentialMaterialV1>,
    operations: Vec<SemanticOperationMaterialV1>,
    origins: Vec<SemanticOriginMaterialV1>,
    triggers: Vec<SemanticTriggerMaterialV1>,
    value_language_epoch: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentityMaterialV1 {
    record_id: String,
    record_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactDecisionMaterialV1 {
    source_record_id: String,
    artifact_id: String,
    algorithm: HashAlgorithmMaterialV1,
    digest: String,
    path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FileDecisionMaterialV1 {
    source_record_id: String,
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestProvenanceMaterialV1 {
    source_record_id: String,
    artifact_hashes: Vec<ArtifactHashMaterialV1>,
    license_id: String,
    notice_id: String,
    #[serde(deserialize_with = "deserialize_resolved_fact_origins")]
    contract_fact_origins: Vec<ResolvedFactOriginMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderEvidenceOriginMaterialV1 {
    source_record_id: String,
    provider: String,
    evidence: Vec<ProviderEvidenceOriginEntryMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderEvidenceOriginEntryMaterialV1 {
    source: ProviderEvidenceSourceMaterialV1,
    accessed_on: String,
    content_sha256: String,
    terms: EvidenceTermsMaterialV1,
    facts: Vec<ProviderEvidenceOriginFactMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderEvidenceOriginFactMaterialV1 {
    fact_id: String,
    location: ExactFactLocationMaterialV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceConnectorIdentity {
    id: String,
    semantic_sha256: String,
    version: StableSemver,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceMaterialV1 {
    artifacts: Vec<ArtifactDecisionMaterialV1>,
    canonical_schema_epoch: u32,
    classifier_epoch: u32,
    connector: ProvenanceConnectorIdentity,
    dependencies: Vec<DependencyDecisionMaterialV1>,
    donat_policy_ids: Vec<String>,
    embedded_material: Vec<EmbeddedDecisionMaterialV1>,
    files: Vec<FileDecisionMaterialV1>,
    generator_epoch: u32,
    licenses: Vec<LicenseDecisionMaterialV1>,
    manifest_references: Vec<ManifestProvenanceMaterialV1>,
    notices: Vec<NoticeMaterialV1>,
    provider_evidence: Vec<ProviderEvidenceOriginMaterialV1>,
    sources: Vec<SourceIdentityMaterialV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValueContractMaterialV1 {
    named_objects: BTreeMap<String, NamedObjectMaterialV1>,
    roots: BTreeMap<String, FieldMaterialV1>,
    value_language_epoch: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ValueContractMaterialDto {
    named_objects: BTreeMap<String, NamedObjectMaterialV1>,
    roots: BTreeMap<String, FieldMaterialV1>,
    value_language_epoch: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct NamedObjectMaterialV1 {
    fields: BTreeMap<String, FieldMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FieldMaterialV1 {
    required: bool,
    type_ref: TypeRefMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TypeRefMaterialV1 {
    nullable: bool,
    value_type: ValueTypeMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct ValueTypeMaterialV1(ValueTypeMaterial);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ValueTypeMaterial {
    Scalar(ValueScalarMaterialV1),
    Enum {
        name: String,
        values: Vec<String>,
    },
    Object {
        fields: BTreeMap<String, FieldMaterialV1>,
    },
    List {
        element: Box<TypeRefMaterialV1>,
    },
    Ref {
        name: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct ValueScalarMaterialV1(ValueScalarMaterial);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ValueScalarMaterial {
    Boolean(()),
    String(()),
    Int32(()),
    Int64(()),
    #[serde(rename = "uint64")]
    UInt64(()),
    Decimal(()),
    Uuid(()),
    Date(()),
    Timestamp(()),
    #[serde(rename = "timestamptz")]
    TimestampTz(()),
    Json(()),
    Custom {
        name: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFactValueMaterialV1 {
    use_site: String,
    value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFactOriginMaterialV1 {
    use_site: String,
    origin: ResolvedFactOriginV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedFactValueMaterialDto {
    use_site: String,
    #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
    value: TypedValueMaterialV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedFactOriginMaterialDto {
    use_site: String,
    origin: ResolvedFactOriginV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum ResolvedFactOriginV1 {
    ProviderEvidence {
        source_record_id: String,
        fact_id: String,
        artifact_content_sha256: String,
        location: ExactFactLocationMaterialV1,
    },
    DonatPolicy {
        policy_id: String,
    },
}

fn deserialize_value_contract_material<'de, D>(
    deserializer: D,
) -> Result<ValueContractMaterialV1, D::Error>
where
    D: Deserializer<'de>,
{
    value_contract_material_from_dto(ValueContractMaterialDto::deserialize(deserializer)?)
        .map_err(serde::de::Error::custom)
}

fn deserialize_resolved_fact_values<'de, D>(
    deserializer: D,
) -> Result<Vec<ResolvedFactValueMaterialV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<ResolvedFactValueMaterialDto>::deserialize(deserializer)?;
    let mut use_sites = BTreeSet::new();
    values
        .into_iter()
        .map(|value| {
            validate_material_name(&value.use_site).map_err(serde::de::Error::custom)?;
            if !use_sites.insert(value.use_site.clone()) {
                return Err(serde::de::Error::custom(
                    "duplicate resolved fact-value use site",
                ));
            }
            Ok(ResolvedFactValueMaterialV1 {
                use_site: value.use_site,
                value: value.value,
            })
        })
        .collect()
}

fn deserialize_resolved_fact_origins<'de, D>(
    deserializer: D,
) -> Result<Vec<ResolvedFactOriginMaterialV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<ResolvedFactOriginMaterialDto>::deserialize(deserializer)?;
    let mut use_sites = BTreeSet::new();
    values
        .into_iter()
        .map(|value| {
            validate_material_name(&value.use_site).map_err(serde::de::Error::custom)?;
            if !use_sites.insert(value.use_site.clone()) {
                return Err(serde::de::Error::custom(
                    "duplicate resolved fact-origin use site",
                ));
            }
            Ok(ResolvedFactOriginMaterialV1 {
                use_site: value.use_site,
                origin: value.origin,
            })
        })
        .collect()
}

/// Decode the repository-owned normalized connector manifest.
///
/// This loader is deliberately separate from canonical-material decoding:
/// canonical materials are output-only hash inputs, while this DTO is the
/// complete reviewed input to the offline compiler.
pub fn load_connector_manifest_bytes(bytes: &[u8]) -> Result<ConnectorManifest, CatalogError> {
    let parsed_value = serde_yaml::from_slice::<serde_yaml::Value>(bytes)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let dto = serde_yaml::from_slice::<ConnectorManifestDto>(bytes)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let parsed_json = serde_json::to_vec(&parsed_value)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let rebuilt_json = serde_json::to_vec(&dto)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    if canonicalize_raw(&parsed_json)? != canonicalize_raw(&rebuilt_json)? {
        return Err(CatalogError::new(
            "catalog_manifest_incomplete",
            "normalized manifest omitted or changed a required member",
        ));
    }
    connector_manifest_from_dto(dto)
}

fn invalid_manifest_primitive(detail: impl Into<String>) -> CatalogError {
    CatalogError::new("catalog_manifest_invalid_primitive", detail)
}

fn checked_manifest_primitive<T, E>(
    value: Result<T, E>,
    detail: &'static str,
) -> Result<T, CatalogError> {
    value.map_err(|_| invalid_manifest_primitive(detail))
}

fn nonzero_u16(value: u16, detail: &'static str) -> Result<NonZeroU16, CatalogError> {
    NonZeroU16::new(value).ok_or_else(|| invalid_manifest_primitive(detail))
}

fn nonzero_u32(value: u32, detail: &'static str) -> Result<NonZeroU32, CatalogError> {
    NonZeroU32::new(value).ok_or_else(|| invalid_manifest_primitive(detail))
}

fn nonzero_u64_text(value: &str, detail: &'static str) -> Result<NonZeroU64, CatalogError> {
    let parsed = value
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .and_then(NonZeroU64::new);
    parsed.ok_or_else(|| invalid_manifest_primitive(detail))
}

fn u64_text(value: &str, detail: &'static str) -> Result<u64, CatalogError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .ok_or_else(|| invalid_manifest_primitive(detail))
}

fn hash256_text(value: &str, detail: &'static str) -> Result<[u8; 32], CatalogError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_manifest_primitive(detail));
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(pair[0]).ok_or_else(|| invalid_manifest_primitive(detail))?;
        let low = hex_value(pair[1]).ok_or_else(|| invalid_manifest_primitive(detail))?;
        output[index] = high << 4 | low;
    }
    if hex_bytes(&output) != value {
        return Err(invalid_manifest_primitive(detail));
    }
    Ok(output)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn value_contract_from_material(value: ValueContractMaterialV1) -> ValueContractCatalog {
    fn field(value: FieldMaterialV1) -> ValueContractField {
        ValueContractField {
            required: value.required,
            type_ref: type_ref(value.type_ref),
        }
    }

    fn type_ref(value: TypeRefMaterialV1) -> TypeRef {
        TypeRef {
            nullable: value.nullable,
            value_type: value_type(value.value_type),
        }
    }

    fn value_type(value: ValueTypeMaterialV1) -> ValueType {
        match value.0 {
            ValueTypeMaterial::Scalar(value) => ValueType::Scalar {
                scalar: match value.0 {
                    ValueScalarMaterial::Boolean(()) => ValueScalar::Boolean,
                    ValueScalarMaterial::String(()) => ValueScalar::String,
                    ValueScalarMaterial::Int32(()) => ValueScalar::Int32,
                    ValueScalarMaterial::Int64(()) => ValueScalar::Int64,
                    ValueScalarMaterial::UInt64(()) => ValueScalar::UInt64,
                    ValueScalarMaterial::Decimal(()) => ValueScalar::Decimal,
                    ValueScalarMaterial::Uuid(()) => ValueScalar::Uuid,
                    ValueScalarMaterial::Date(()) => ValueScalar::Date,
                    ValueScalarMaterial::Timestamp(()) => ValueScalar::Timestamp,
                    ValueScalarMaterial::TimestampTz(()) => ValueScalar::TimestampTz,
                    ValueScalarMaterial::Json(()) => ValueScalar::Json,
                    ValueScalarMaterial::Custom { name } => ValueScalar::Custom { name },
                },
            },
            ValueTypeMaterial::Enum { name, values } => ValueType::Enum { name, values },
            ValueTypeMaterial::Object { fields } => ValueType::Object {
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, field(value)))
                    .collect(),
            },
            ValueTypeMaterial::List { element } => ValueType::List {
                element: Box::new(type_ref(*element)),
            },
            ValueTypeMaterial::Ref { name } => ValueType::Ref { name },
        }
    }

    ValueContractCatalog {
        roots: value
            .roots
            .into_iter()
            .map(|(name, value)| (name, field(value)))
            .collect(),
        named_objects: value
            .named_objects
            .into_iter()
            .map(|(name, object)| {
                (
                    name,
                    ValueObjectContract {
                        fields: object
                            .fields
                            .into_iter()
                            .map(|(name, value)| (name, field(value)))
                            .collect(),
                    },
                )
            })
            .collect(),
    }
}

fn typed_value_from_material(value: TypedValueMaterialV1) -> Result<TypedValue, CatalogError> {
    value
        .to_typed_value()
        .map_err(|_| invalid_manifest_primitive("typed value"))
}

fn connector_manifest_from_dto(
    value: ConnectorManifestDto,
) -> Result<ConnectorManifest, CatalogError> {
    let ConnectorManifestDto {
        connector,
        connector_version,
        manifest_version,
        runtime_abi_epoch,
        value_language_epoch,
        provider,
        api_identity,
        credentials,
        origins,
        operations,
        triggers,
        provenance,
    } = value;
    Ok(ConnectorManifest {
        connector: checked_manifest_primitive(
            ConnectorId::parse(&connector),
            "connector identity",
        )?,
        connector_version,
        manifest_version,
        runtime_abi_epoch,
        value_language_epoch,
        provider,
        api_identity,
        credentials: credentials
            .into_iter()
            .map(credential_from_material)
            .collect::<Result<_, _>>()?,
        origins: origins
            .into_iter()
            .map(origin_from_material)
            .collect::<Result<_, _>>()?,
        operations: operations
            .into_iter()
            .map(operation_from_material)
            .collect::<Result<_, _>>()?,
        triggers: triggers
            .into_iter()
            .map(trigger_from_material)
            .collect::<Result<_, _>>()?,
        provenance: provenance
            .into_iter()
            .map(|reference| ManifestProvenanceReference {
                source_record_id: reference.source_record_id,
                artifact_hashes: reference.artifact_hashes,
                license_id: reference.license_id,
                notice_id: reference.notice_id,
                contract_facts: reference
                    .contract_facts
                    .into_iter()
                    .map(|binding| ResolvedContractFactBinding {
                        use_site: binding.use_site,
                        fact: binding.fact,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn credential_from_material(
    value: SemanticCredentialMaterialV1,
) -> Result<CredentialSpec, CatalogError> {
    let SemanticCredentialMaterialV1 {
        credential,
        version,
        fields,
        auth_plan,
        allowed_origins,
        scopes,
        auth_processor,
        credential_test_operation,
        bounds,
    } = value;
    Ok(CredentialSpec {
        credential: checked_manifest_primitive(
            CredentialSpecId::parse(&credential),
            "credential identity",
        )?,
        version,
        fields: fields
            .into_iter()
            .map(credential_field_from_material)
            .collect::<Result<_, _>>()?,
        auth_plan: auth_plan_from_material(auth_plan)?,
        allowed_origins: allowed_origins
            .into_iter()
            .map(|origin| {
                checked_manifest_primitive(OriginId::parse(&origin), "allowed origin identity")
            })
            .collect::<Result<_, _>>()?,
        scopes,
        auth_processor: auth_processor
            .map(authenticator_ref_from_material)
            .transpose()?,
        credential_test_operation: credential_test_operation
            .map(|reference| {
                Ok(VersionedOperationReference {
                    operation: checked_manifest_primitive(
                        OperationId::parse(&reference.operation),
                        "credential-test operation identity",
                    )?,
                    version: reference.version,
                })
            })
            .transpose()?,
        bounds: CredentialBounds {
            maximum_field_bytes: nonzero_u32(
                bounds.maximum_field_bytes,
                "maximum credential field bytes",
            )?,
            maximum_aggregate_bytes: nonzero_u32(
                bounds.maximum_aggregate_bytes,
                "maximum aggregate credential bytes",
            )?,
            maximum_token_bytes: nonzero_u32(
                bounds.maximum_token_bytes,
                "maximum credential token bytes",
            )?,
        },
    })
}

fn credential_field_from_material(
    value: CredentialFieldMaterialV1,
) -> Result<CredentialFieldSpec, CatalogError> {
    Ok(CredentialFieldSpec {
        field: checked_manifest_primitive(
            CredentialFieldId::parse(&value.field),
            "credential field identity",
        )?,
        required: value.required,
        secret: match value.secret {
            SecretClassificationMaterialV1::Secret(()) => SecretClassification::Secret,
            SecretClassificationMaterialV1::Sensitive(()) => SecretClassification::Sensitive,
            SecretClassificationMaterialV1::NonSecret(()) => SecretClassification::NonSecret,
        },
        maximum_bytes: nonzero_u32(value.maximum_bytes, "maximum credential field bytes")?,
        redaction: redaction_from_material(value.redaction),
    })
}

fn redaction_from_material(value: RedactionMaterialV1) -> RedactionPlan {
    match value {
        RedactionMaterialV1::Omit(()) => RedactionPlan::Omit,
        RedactionMaterialV1::Fixed { replacement } => RedactionPlan::Fixed { replacement },
        RedactionMaterialV1::PreserveLast { characters } => {
            RedactionPlan::PreserveLast { characters }
        }
    }
}

fn auth_plan_from_material(value: CredentialAuthMaterialV1) -> Result<AuthPlan, CatalogError> {
    Ok(match value {
        CredentialAuthMaterialV1::FixedHeaderApiKey { field, header } => {
            AuthPlan::FixedHeaderApiKey {
                field: checked_manifest_primitive(
                    CredentialFieldId::parse(&field),
                    "auth field identity",
                )?,
                header,
            }
        }
        CredentialAuthMaterialV1::FixedQueryApiKey { field, query } => AuthPlan::FixedQueryApiKey {
            field: checked_manifest_primitive(
                CredentialFieldId::parse(&field),
                "auth field identity",
            )?,
            query,
        },
        CredentialAuthMaterialV1::Bearer { token } => AuthPlan::Bearer {
            token: checked_manifest_primitive(
                CredentialFieldId::parse(&token),
                "auth token identity",
            )?,
        },
        CredentialAuthMaterialV1::HttpBasic { username, password } => AuthPlan::HttpBasic {
            username: checked_manifest_primitive(
                CredentialFieldId::parse(&username),
                "auth username identity",
            )?,
            password: checked_manifest_primitive(
                CredentialFieldId::parse(&password),
                "auth password identity",
            )?,
        },
        CredentialAuthMaterialV1::OAuth2ClientCredentials {
            client_id,
            client_secret,
            token_origin,
            token_step,
            scopes,
            token_pointer,
        } => AuthPlan::OAuth2ClientCredentials {
            client_id: checked_manifest_primitive(
                CredentialFieldId::parse(&client_id),
                "OAuth client identity",
            )?,
            client_secret: checked_manifest_primitive(
                CredentialFieldId::parse(&client_secret),
                "OAuth secret identity",
            )?,
            token_origin: checked_manifest_primitive(
                OriginId::parse(&token_origin),
                "OAuth token origin",
            )?,
            token_step: checked_manifest_primitive(
                CompiledStepId::parse(&token_step),
                "OAuth token step",
            )?,
            scopes,
            token_pointer,
        },
        CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken { token } => {
            AuthPlan::PreprovisionedOAuthAccessToken {
                token: checked_manifest_primitive(
                    CredentialFieldId::parse(&token),
                    "OAuth access-token identity",
                )?,
            }
        }
    })
}

fn origin_from_material(value: SemanticOriginMaterialV1) -> Result<FixedOrigin, CatalogError> {
    let HttpsMaterialV1::HttpsOnly(()) = value.scheme;
    Ok(FixedOrigin {
        origin: checked_manifest_primitive(OriginId::parse(&value.origin), "origin identity")?,
        scheme: HttpsOnly,
        host: value.host,
        port: nonzero_u16(value.port, "origin port")?,
        network_policy: match value.network_policy {
            NetworkPolicyMaterialV1::PublicOnly(()) => NetworkPolicy::PublicOnly,
            NetworkPolicyMaterialV1::PrivateAllowed { policy } => {
                NetworkPolicy::PrivateAllowed { policy }
            }
        },
    })
}

fn authenticator_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<AuthenticatorId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(
            AuthenticatorId::parse(&value.id),
            "authenticator identity",
        )?,
        implementation_revision: value.implementation_revision,
    })
}

fn codec_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<CodecId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(CodecId::parse(&value.id), "codec identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn normalizer_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<NormalizerId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(NormalizerId::parse(&value.id), "normalizer identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn processor_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<ProcessorFamilyId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(ProcessorFamilyId::parse(&value.id), "processor identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn operation_from_material(
    value: SemanticOperationMaterialV1,
) -> Result<OperationSpec, CatalogError> {
    let SemanticOperationMaterialV1 {
        connector,
        connector_version,
        operation,
        operation_version,
        runtime_abi_epoch,
        value_language_epoch,
        input,
        input_contract_sha256,
        output,
        output_contract_sha256,
        credential,
        origins,
        steps,
        pre_request_transforms,
        post_response_transforms,
        operation_processor,
        effect,
        pagination,
        error_map,
        capacity,
        rate,
        serialization_key_default,
        bounds,
        resolved_fact_values,
    } = value;
    Ok(OperationSpec {
        connector: checked_manifest_primitive(
            ConnectorId::parse(&connector),
            "operation connector identity",
        )?,
        connector_version,
        operation: checked_manifest_primitive(
            OperationId::parse(&operation),
            "operation identity",
        )?,
        operation_version,
        runtime_abi_epoch,
        value_language_epoch,
        input: value_contract_from_material(input),
        input_contract_sha256: hash256_text(&input_contract_sha256, "input contract hash")?,
        output: value_contract_from_material(output),
        output_contract_sha256: hash256_text(&output_contract_sha256, "output contract hash")?,
        credential: credential
            .map(|reference| {
                Ok(VersionedCredentialReference {
                    credential: checked_manifest_primitive(
                        CredentialSpecId::parse(&reference.credential),
                        "operation credential identity",
                    )?,
                    version: reference.version,
                })
            })
            .transpose()?,
        origins: origins
            .into_iter()
            .map(origin_from_material)
            .collect::<Result<_, _>>()?,
        steps: steps
            .into_iter()
            .map(step_from_material)
            .collect::<Result<_, _>>()?,
        pre_request_transforms: pre_request_transforms
            .into_iter()
            .map(processor_ref_from_material)
            .collect::<Result<_, _>>()?,
        post_response_transforms: post_response_transforms
            .into_iter()
            .map(processor_ref_from_material)
            .collect::<Result<_, _>>()?,
        operation_processor: operation_processor
            .map(processor_ref_from_material)
            .transpose()?,
        effect: effect_from_material(effect)?,
        pagination: pagination_from_material(pagination)?,
        error_map: error_map_from_material(error_map)?,
        capacity: CapacityDefaults {
            maximum_in_flight: nonzero_u32(
                capacity.maximum_in_flight,
                "maximum in-flight operations",
            )?,
        },
        rate: RateDefaults {
            burst: nonzero_u32(rate.burst, "rate burst")?,
            refill_interval_ms: nonzero_u64_text(&rate.refill_interval_ms, "rate refill interval")?,
        },
        serialization_key_default: serialization_key_default
            .map(|value| {
                Ok(TypedSerializationKeyDefault {
                    field: value.field,
                    value: typed_value_from_material(value.value)?,
                })
            })
            .transpose()?,
        bounds: operation_bounds_from_material(bounds)?,
        resolved_fact_values: resolved_fact_values
            .into_iter()
            .map(|binding| {
                Ok(ResolvedFactValue {
                    use_site: binding.use_site,
                    value: typed_value_from_material(binding.value)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
    })
}

fn step_from_material(value: SemanticStepMaterialV1) -> Result<CompiledStepSpec, CatalogError> {
    Ok(CompiledStepSpec {
        step: checked_manifest_primitive(CompiledStepId::parse(&value.step), "step identity")?,
        method: value.method,
        origin: checked_manifest_primitive(OriginId::parse(&value.origin), "step origin")?,
        path: value.path,
        query: value
            .query
            .into_iter()
            .map(|binding| {
                Ok(CompiledQueryBinding {
                    name: binding.name,
                    binding: binding_from_material(binding.binding)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        headers: value
            .headers
            .into_iter()
            .map(|binding| {
                Ok(CompiledHeaderBinding {
                    name: binding.name,
                    binding: binding_from_material(binding.binding)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        credential_action: value
            .credential_action
            .map(|action| {
                Ok(CompiledCredentialAction {
                    credential: checked_manifest_primitive(
                        CredentialSpecId::parse(&action.credential),
                        "step credential identity",
                    )?,
                })
            })
            .transpose()?,
        request: match value.request {
            CompiledRequestMaterialV1::None(()) => CompiledRequestShape::None,
            CompiledRequestMaterialV1::Json { bindings } => CompiledRequestShape::Json { bindings },
            CompiledRequestMaterialV1::FormUrlencoded { bindings } => {
                CompiledRequestShape::FormUrlencoded { bindings }
            }
            CompiledRequestMaterialV1::Multipart { bindings } => {
                CompiledRequestShape::Multipart { bindings }
            }
            CompiledRequestMaterialV1::RawBytes { binding } => {
                CompiledRequestShape::RawBytes { binding }
            }
        },
        success_statuses: value
            .success_statuses
            .into_iter()
            .map(|range| StatusRange {
                minimum: range.minimum,
                maximum: range.maximum,
            })
            .collect(),
        response: match value.response {
            CompiledResponseMaterialV1::Json { mappings } => CompiledResponseShape::Json {
                mappings: mappings
                    .into_iter()
                    .map(|mapping| ResponseMapping {
                        pointer: mapping.pointer,
                        target: mapping.target,
                    })
                    .collect(),
            },
            CompiledResponseMaterialV1::RawBytes { target } => {
                CompiledResponseShape::RawBytes { target }
            }
        },
        selected_response_headers: value
            .selected_response_headers
            .into_iter()
            .map(selected_header_from_material)
            .collect::<Result<_, _>>()?,
        bounds: StepBounds {
            maximum_headers: nonzero_u32(value.bounds.maximum_headers, "maximum step headers")?,
            maximum_header_bytes: nonzero_u32(
                value.bounds.maximum_header_bytes,
                "maximum step header bytes",
            )?,
            maximum_url_bytes: nonzero_u32(
                value.bounds.maximum_url_bytes,
                "maximum step URL bytes",
            )?,
            maximum_request_bytes: nonzero_u32(
                value.bounds.maximum_request_bytes,
                "maximum step request bytes",
            )?,
            maximum_response_bytes: nonzero_u32(
                value.bounds.maximum_response_bytes,
                "maximum step response bytes",
            )?,
            maximum_json_depth: nonzero_u32(value.bounds.maximum_json_depth, "maximum JSON depth")?,
            maximum_json_nodes: nonzero_u32(value.bounds.maximum_json_nodes, "maximum JSON nodes")?,
            maximum_inline_binary_bytes: nonzero_u32(
                value.bounds.maximum_inline_binary_bytes,
                "maximum inline binary bytes",
            )?,
            deadline_ms: nonzero_u64_text(&value.bounds.deadline_ms, "step deadline")?,
        },
    })
}

fn binding_from_material(value: BindingMaterialV1) -> Result<CompiledBinding, CatalogError> {
    Ok(CompiledBinding {
        field: value.field,
        source: match value.source {
            CompiledBindingSourceMaterialV1::Input(()) => CompiledBindingSource::Input,
            CompiledBindingSourceMaterialV1::Constant { value } => {
                CompiledBindingSource::Constant {
                    value: typed_value_from_material(value)?,
                }
            }
        },
        required: value.required,
        default: value.default.map(typed_value_from_material).transpose()?,
        mapping: value.mapping,
    })
}

fn selected_header_from_material(
    value: SelectedResponseHeaderMaterialV1,
) -> Result<SelectedResponseHeader, CatalogError> {
    Ok(SelectedResponseHeader {
        canonical_lowercase_header_name: value.canonical_lowercase_header_name,
        capability: checked_manifest_primitive(
            CapabilityId::parse(&value.capability),
            "selected-header capability",
        )?,
    })
}

fn operation_bounds_from_material(
    value: OperationBoundsMaterialV1,
) -> Result<OperationBounds, CatalogError> {
    Ok(OperationBounds {
        maximum_calls: nonzero_u32(value.maximum_calls, "maximum operation calls")?,
        maximum_pages: nonzero_u32(value.maximum_pages, "maximum operation pages")?,
        maximum_items: nonzero_u32(value.maximum_items, "maximum operation items")?,
        maximum_aggregate_request_bytes: nonzero_u32(
            value.maximum_aggregate_request_bytes,
            "maximum aggregate request bytes",
        )?,
        maximum_aggregate_response_bytes: nonzero_u32(
            value.maximum_aggregate_response_bytes,
            "maximum aggregate response bytes",
        )?,
        maximum_output_canonical_bytes: nonzero_u32(
            value.maximum_output_canonical_bytes,
            "maximum canonical output bytes",
        )?,
        maximum_redirects: value.maximum_redirects,
        deadline_ms: nonzero_u64_text(&value.deadline_ms, "operation deadline")?,
    })
}

fn effect_from_material(value: OperationEffectMaterialV1) -> Result<OperationEffect, CatalogError> {
    Ok(match value {
        OperationEffectMaterialV1::ReadOnly(()) => OperationEffect::ReadOnly,
        OperationEffectMaterialV1::ProviderIdempotent { side_effect_steps } => {
            OperationEffect::ProviderIdempotent {
                side_effect_steps: side_effect_steps
                    .into_iter()
                    .map(|step| {
                        Ok(ProviderIdempotentStep {
                            step: checked_manifest_primitive(
                                CompiledStepId::parse(&step.step),
                                "idempotent step identity",
                            )?,
                            fixed_binding: match step.fixed_binding {
                                FixedIdempotencyBindingMaterialV1::Header { name } => {
                                    FixedIdempotencyBinding::Header { name }
                                }
                                FixedIdempotencyBindingMaterialV1::BodyField { pointer } => {
                                    FixedIdempotencyBinding::BodyField { pointer }
                                }
                            },
                            scope: step.scope,
                            minimum_retention_ms: nonzero_u64_text(
                                &step.minimum_retention_ms,
                                "idempotency retention",
                            )?,
                            clock_safety_margin_ms: nonzero_u64_text(
                                &step.clock_safety_margin_ms,
                                "idempotency clock margin",
                            )?,
                        })
                    })
                    .collect::<Result<_, CatalogError>>()?,
            }
        }
    })
}

fn pagination_bounds_from_material(
    value: PaginationBoundsMaterialV1,
) -> Result<PaginationBounds, CatalogError> {
    Ok(PaginationBounds {
        maximum_calls: nonzero_u32(value.maximum_calls, "maximum pagination calls")?,
        maximum_pages: nonzero_u32(value.maximum_pages, "maximum pagination pages")?,
        maximum_items: nonzero_u32(value.maximum_items, "maximum pagination items")?,
        maximum_response_bytes: nonzero_u32(
            value.maximum_response_bytes,
            "maximum page response bytes",
        )?,
        maximum_aggregate_response_bytes: nonzero_u32(
            value.maximum_aggregate_response_bytes,
            "maximum paginated response bytes",
        )?,
        maximum_output_canonical_bytes: nonzero_u32(
            value.maximum_output_canonical_bytes,
            "maximum paginated output bytes",
        )?,
    })
}

fn pagination_from_material(value: PaginationMaterialV1) -> Result<PaginationPlan, CatalogError> {
    Ok(match value {
        PaginationMaterialV1::None(()) => PaginationPlan::None,
        PaginationMaterialV1::Cursor {
            request_binding,
            response_pointer,
            bounds,
        } => PaginationPlan::Cursor {
            request_binding,
            response_pointer,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::OffsetLimit {
            offset_binding,
            limit_binding,
            initial_offset,
            page_size,
            bounds,
        } => PaginationPlan::OffsetLimit {
            offset_binding,
            limit_binding,
            initial_offset: u64_text(&initial_offset, "initial pagination offset")?,
            page_size: nonzero_u32(page_size, "pagination page size")?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::PageNumber {
            page_binding,
            page_size_binding,
            initial_page,
            page_size,
            bounds,
        } => PaginationPlan::PageNumber {
            page_binding,
            page_size_binding,
            initial_page: nonzero_u64_text(&initial_page, "initial page number")?,
            page_size: nonzero_u32(page_size, "pagination page size")?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::LinkRelation {
            relation,
            selected_header,
            bounds,
        } => PaginationPlan::LinkRelation {
            relation,
            selected_header: selected_header_from_material(selected_header)?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::Processor { processor, bounds } => PaginationPlan::Processor {
            processor: processor_ref_from_material(processor)?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
    })
}

fn error_map_from_material(value: ErrorMapMaterialV1) -> Result<ErrorMap, CatalogError> {
    Ok(ErrorMap {
        rules: value
            .rules
            .into_iter()
            .map(|rule| {
                Ok(ErrorRule {
                    matcher: error_matcher_from_material(rule.matcher),
                    action: error_action_from_material(rule.action)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        fallback: CompleteErrorFallback {
            transport: error_action_from_material(value.fallback.transport)?,
            timeout: error_action_from_material(value.fallback.timeout)?,
            http_429: error_action_from_material(value.fallback.http_429)?,
            http_5xx: error_action_from_material(value.fallback.http_5xx)?,
            authentication: error_action_from_material(value.fallback.authentication)?,
            validation: error_action_from_material(value.fallback.validation)?,
            permanent: error_action_from_material(value.fallback.permanent)?,
            invariant: error_action_from_material(value.fallback.invariant)?,
        },
    })
}

fn error_matcher_from_material(value: ErrorMatcherMaterialV1) -> ErrorMatcher {
    match value {
        ErrorMatcherMaterialV1::Status(value) => ErrorMatcher::Status(StatusRange {
            minimum: value.minimum,
            maximum: value.maximum,
        }),
        ErrorMatcherMaterialV1::ProviderCode { pointer, codes } => {
            ErrorMatcher::ProviderCode { pointer, codes }
        }
        ErrorMatcherMaterialV1::Header { name, values } => ErrorMatcher::Header { name, values },
        ErrorMatcherMaterialV1::MalformedDeclaredSuccess(()) => {
            ErrorMatcher::MalformedDeclaredSuccess
        }
    }
}

fn error_action_from_material(value: ErrorActionMaterialV1) -> Result<ErrorAction, CatalogError> {
    let retry_after = match value.retry_after {
        RetryAfterMaterialV1::Never(()) => RetryAfterPolicy::Never,
        RetryAfterMaterialV1::RetryAfterHeader {
            step,
            capability,
            maximum_seconds,
        } => RetryAfterPolicy::RetryAfterHeader {
            step: checked_manifest_primitive(
                CompiledStepId::parse(&step),
                "retry-after step identity",
            )?,
            capability: checked_manifest_primitive(
                CapabilityId::parse(&capability),
                "retry-after capability",
            )?,
            maximum_seconds: nonzero_u32(maximum_seconds, "maximum retry-after seconds")?,
        },
    };
    let correlations = value
        .correlations
        .into_iter()
        .map(|correlation| {
            Ok(ErrorCorrelationBinding {
                canonical_lowercase_header_name: correlation.canonical_lowercase_header_name,
                capability: checked_manifest_primitive(
                    CapabilityId::parse(&correlation.capability),
                    "error-correlation capability",
                )?,
                step: checked_manifest_primitive(
                    CompiledStepId::parse(&correlation.step),
                    "error-correlation step identity",
                )?,
            })
        })
        .collect::<Result<_, CatalogError>>()?;
    ErrorAction::try_new(
        match value.class {
            ConnectorErrorClassMaterialV1::Transport(()) => ConnectorErrorClass::Transport,
            ConnectorErrorClassMaterialV1::Timeout(()) => ConnectorErrorClass::Timeout,
            ConnectorErrorClassMaterialV1::Http429(()) => ConnectorErrorClass::Http429,
            ConnectorErrorClassMaterialV1::Http5xx(()) => ConnectorErrorClass::Http5xx,
            ConnectorErrorClassMaterialV1::Authentication(()) => {
                ConnectorErrorClass::Authentication
            }
            ConnectorErrorClassMaterialV1::Validation(()) => ConnectorErrorClass::Validation,
            ConnectorErrorClassMaterialV1::Permanent(()) => ConnectorErrorClass::Permanent,
            ConnectorErrorClassMaterialV1::Invariant(()) => ConnectorErrorClass::Invariant,
        },
        &value.code,
        &value.safe_message,
        retry_after,
        correlations,
    )
    .map_err(|_| invalid_manifest_primitive("static error action"))
}

fn trigger_from_material(value: SemanticTriggerMaterialV1) -> Result<TriggerSpec, CatalogError> {
    Ok(match value {
        SemanticTriggerMaterialV1::Webhook {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            authenticator,
            codec,
            normalizer,
            selected_headers,
            raw_body_max_bytes,
            timestamp_window_ms,
            event_id,
            event_type,
            output,
            redaction,
            subscription_operations,
        } => TriggerSpec::Webhook {
            connector: checked_manifest_primitive(
                ConnectorId::parse(&connector),
                "webhook connector identity",
            )?,
            connector_version,
            trigger: checked_manifest_primitive(
                TriggerId::parse(&trigger),
                "webhook trigger identity",
            )?,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            authenticator: authenticator_ref_from_material(authenticator)?,
            codec: codec_ref_from_material(codec)?,
            normalizer: normalizer_ref_from_material(normalizer)?,
            selected_headers,
            raw_body_max_bytes: nonzero_u32(raw_body_max_bytes, "webhook body bound")?,
            timestamp_window_ms: nonzero_u64_text(
                &timestamp_window_ms,
                "webhook timestamp window",
            )?,
            event_id: value_contract_from_material(event_id),
            event_type: value_contract_from_material(event_type),
            output: value_contract_from_material(output),
            redaction: redaction_from_material(redaction),
            subscription_operations: subscription_operations
                .map(|operations| {
                    Ok(SubscriptionOperationIds {
                        create: checked_manifest_primitive(
                            OperationId::parse(&operations.create),
                            "subscription create operation",
                        )?,
                        delete: checked_manifest_primitive(
                            OperationId::parse(&operations.delete),
                            "subscription delete operation",
                        )?,
                        check: operations
                            .check
                            .map(|operation| {
                                checked_manifest_primitive(
                                    OperationId::parse(&operation),
                                    "subscription check operation",
                                )
                            })
                            .transpose()?,
                    })
                })
                .transpose()?,
        },
        SemanticTriggerMaterialV1::Poll {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            checkpoint,
            processor,
            event_type,
            per_poll_event_limit,
            bounds,
        } => TriggerSpec::Poll {
            connector: checked_manifest_primitive(
                ConnectorId::parse(&connector),
                "poll connector identity",
            )?,
            connector_version,
            trigger: checked_manifest_primitive(
                TriggerId::parse(&trigger),
                "poll trigger identity",
            )?,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            checkpoint: value_contract_from_material(checkpoint),
            processor: processor_ref_from_material(processor)?,
            event_type: value_contract_from_material(event_type),
            per_poll_event_limit: nonzero_u32(per_poll_event_limit, "per-poll event limit")?,
            bounds: operation_bounds_from_material(bounds)?,
        },
    })
}

impl ResolvedFactValueMaterialV1 {
    pub fn use_site(&self) -> &str {
        &self.use_site
    }
}

impl ResolvedFactOriginMaterialV1 {
    pub fn use_site(&self) -> &str {
        &self.use_site
    }

    pub fn provider_evidence(&self) -> Option<(&str, &str, &str, ExactFactLocation)> {
        match &self.origin {
            ResolvedFactOriginV1::ProviderEvidence {
                source_record_id,
                fact_id,
                artifact_content_sha256,
                location,
            } => Some((
                source_record_id,
                fact_id,
                artifact_content_sha256,
                match location {
                    ExactFactLocationMaterialV1::JsonPointer { path, pointer } => {
                        ExactFactLocation::JsonPointer {
                            path: crate::SourcePath::parse(path)
                                .expect("material source paths are checked"),
                            pointer: pointer.clone(),
                        }
                    }
                    ExactFactLocationMaterialV1::DocumentSection { path, section } => {
                        ExactFactLocation::DocumentSection {
                            path: crate::SourcePath::parse(path)
                                .expect("material source paths are checked"),
                            section: section.clone(),
                        }
                    }
                },
            )),
            ResolvedFactOriginV1::DonatPolicy { .. } => None,
        }
    }
}

pub fn resolve_fact_bindings(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    catalog: &AcceptedRecordCatalog,
    reviewed_policies: &BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ResolvedFactOriginMaterialV1>,
    ),
    CatalogError,
> {
    let mut semantic_by_use_site = BTreeMap::new();
    for binding in values {
        if binding.use_site.is_empty()
            || semantic_by_use_site
                .insert(binding.use_site.as_str(), &binding.value)
                .is_some()
        {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "semantic fact use sites must be nonempty and unique",
            ));
        }
    }
    let mut origin_by_use_site = BTreeMap::new();
    for binding in origins {
        if binding.use_site.is_empty()
            || origin_by_use_site
                .insert(binding.use_site.as_str(), &binding.fact)
                .is_some()
        {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "provenance fact use sites must be nonempty and unique",
            ));
        }
    }
    if semantic_by_use_site.keys().ne(origin_by_use_site.keys()) {
        return Err(CatalogError::new(
            "catalog_fact_binding_mismatch",
            "semantic and provenance fact use-site sets differ",
        ));
    }

    let mut semantic = Vec::with_capacity(values.len());
    let mut provenance = Vec::with_capacity(origins.len());
    for (use_site, value) in semantic_by_use_site {
        let fact = origin_by_use_site
            .get(use_site)
            .expect("equal key sets were checked");
        let expected = typed_value_material(value);
        let origin = match fact {
            ContractFact::ProviderEvidence {
                source_record_id,
                fact_id,
            } => {
                let accepted = catalog.evidence_accepted(*source_record_id).map_err(|_| {
                    CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider source record is not accepted evidence",
                    )
                })?;
                let record = accepted.record();
                let contract_membership = record
                    .provider_contracts
                    .iter()
                    .filter(|contract| accepted.contracts().contains(&contract.contract_id))
                    .flat_map(|contract| &contract.facts)
                    .filter(|candidate| {
                        matches!(
                            candidate,
                            ContractFact::ProviderEvidence {
                                source_record_id: candidate_record,
                                fact_id: candidate_fact,
                            } if candidate_record == source_record_id && candidate_fact == fact_id
                        )
                    })
                    .count();
                if contract_membership != 1 {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider fact is not in the exact admitted contract closure",
                    ));
                }
                let SourceSubject::ProviderArtifact(provider) = &record.subject else {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "accepted evidence record has the wrong subject",
                    ));
                };
                let matches = provider
                    .evidence
                    .iter()
                    .flat_map(|artifact| {
                        artifact
                            .facts
                            .iter()
                            .filter(move |candidate| candidate.fact_id == *fact_id)
                            .map(move |candidate| (artifact, candidate))
                    })
                    .collect::<Vec<_>>();
                let [(artifact, provider_fact)] = matches.as_slice() else {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider fact does not resolve exactly once",
                    ));
                };
                if provider_fact.normalized_value != expected {
                    return Err(CatalogError::new(
                        "catalog_fact_binding_mismatch",
                        "semantic value differs from accepted provider evidence",
                    ));
                }
                ResolvedFactOriginV1::ProviderEvidence {
                    source_record_id: source_record_id.as_str().to_owned(),
                    fact_id: fact_id.as_str().to_owned(),
                    artifact_content_sha256: artifact.content_sha256.to_string(),
                    location: fact_location_material(&provider_fact.location),
                }
            }
            ContractFact::DonatPolicy {
                policy_id,
                value: declared,
            } => {
                let registered = reviewed_policies.get(policy_id).ok_or_else(|| {
                    CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "Donat policy is not in the reviewed registry",
                    )
                })?;
                let registered = typed_value_material(registered);
                if registered != *declared || registered != expected {
                    return Err(CatalogError::new(
                        "catalog_fact_binding_mismatch",
                        "semantic, declared, and reviewed policy values differ",
                    ));
                }
                ResolvedFactOriginV1::DonatPolicy {
                    policy_id: policy_id.as_str().to_owned(),
                }
            }
        };
        semantic.push(ResolvedFactValueMaterialV1 {
            use_site: use_site.to_owned(),
            value: expected,
        });
        provenance.push(ResolvedFactOriginMaterialV1 {
            use_site: use_site.to_owned(),
            origin,
        });
    }
    Ok((semantic, provenance))
}

pub fn source_record_material(
    record: &ConnectorSourceRecord,
) -> Result<SourceRecordMaterialV1, CatalogError> {
    let encoded = crate::canonical_yaml(record)?;
    let checked = crate::load_record_bytes(&encoded)?;
    if checked != *record {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "source record changed during checked reconstruction",
        ));
    }
    let ConnectorSourceRecord {
        record_version,
        record_id,
        subject,
        reacquisition,
        artifact_hashes,
        license,
        notice,
        entrypoints,
        dependencies,
        embedded_material,
        provider_contracts,
        compatibility,
        admission,
        safety_findings,
        reviewer,
        approval_date,
        proposed_manifest,
        proposed_destinations,
        red_tests,
    } = record;
    Ok(SourceRecordMaterialV1 {
        record_version: *record_version,
        record_id: record_id.as_str().to_owned(),
        subject: source_subject_material(subject)?,
        reacquisition: reacquisition_material(reacquisition),
        artifact_hashes: sorted_artifacts(artifact_hashes)
            .into_iter()
            .map(artifact_hash_material)
            .collect(),
        license: license_material(license),
        notice: notice_material(notice),
        entrypoints: entrypoints.iter().map(ToString::to_string).collect(),
        dependencies: sorted_dependencies(dependencies)?,
        embedded_material: sorted_embedded_material(embedded_material)?,
        provider_contracts: sorted_provider_contracts(provider_contracts)?,
        compatibility: compatibility_material(compatibility),
        admission: admission_material(admission)?,
        safety_findings: safety_findings_material(safety_findings)?,
        reviewer: reviewer.to_string(),
        approval_date: approval_date.to_string(),
        proposed_manifest: proposed_manifest.as_ref().map(ToString::to_string),
        proposed_destinations: sorted_unique_strings(proposed_destinations)?,
        red_tests: sorted_unique_strings(red_tests)?,
    })
}

pub fn decode_source_record_material(bytes: &[u8]) -> Result<SourceRecordMaterialV1, CatalogError> {
    let record = crate::load_record_bytes(bytes)
        .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
    source_record_material(&record)
}

fn sorted_artifacts(values: &[ArtifactHash]) -> Vec<&ArtifactHash> {
    let mut values: Vec<_> = values.iter().collect();
    values.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    values
}

fn sorted_unique_strings<T: AsRef<str>>(values: &[T]) -> Result<Vec<String>, CatalogError> {
    let mut values = values
        .iter()
        .map(|value| value.as_ref().to_owned())
        .collect::<Vec<_>>();
    values.sort();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "duplicate set-like string",
        ));
    }
    Ok(values)
}

fn source_subject_material(
    subject: &SourceSubject,
) -> Result<SourceSubjectMaterialV1, CatalogError> {
    Ok(match subject {
        SourceSubject::ExactNpm(package) => {
            let crate::ExactNpmPackage {
                name,
                version,
                tarball_url,
                integrity,
                repository,
                npm_git_head,
                package_repository,
                signature,
                provenance,
                tag_commit,
                provenance_commit,
                maintainers,
                repository_owner,
            } = package;
            SourceSubjectMaterialV1::ExactNpm(ExactNpmMaterialV1 {
                name: name.clone(),
                version: version.as_str().to_owned(),
                tarball_url: tarball_url.to_string(),
                integrity: NpmIntegrityMaterialV1 {
                    algorithm: NpmIntegrityAlgorithmMaterialV1::Sha512(()),
                    digest: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(integrity.as_bytes()),
                },
                repository: ImmutableRepositoryMaterialV1 {
                    url: repository.url.to_string(),
                    commit: repository.commit.to_string(),
                    tree: repository.tree.to_string(),
                },
                npm_git_head: npm_git_head.to_string(),
                package_repository: package_repository.to_string(),
                signature: npm_signature_material(signature)?,
                provenance: npm_provenance_material(provenance),
                tag_commit: tag_commit.as_ref().map(ToString::to_string),
                provenance_commit: provenance_commit.as_ref().map(ToString::to_string),
                maintainers: sorted_unique_strings(maintainers)?,
                repository_owner: repository_owner_material(repository_owner),
            })
        }
        SourceSubject::ProviderArtifact(provider) => {
            let crate::ExactProviderArtifact { provider, evidence } = provider;
            let mut evidence = evidence
                .iter()
                .map(provider_evidence_artifact_material)
                .collect::<Result<Vec<_>, _>>()?;
            evidence.sort_by(|left, right| {
                provider_evidence_source_key(&left.source)
                    .cmp(&provider_evidence_source_key(&right.source))
            });
            SourceSubjectMaterialV1::ProviderArtifact(ProviderArtifactMaterialV1 {
                provider: provider.clone(),
                evidence,
            })
        }
        SourceSubject::DonatOwned(source) => {
            let crate::DonatOwnedSource {
                repository_commit,
                files,
            } = source;
            let mut files = files
                .iter()
                .map(|file| RepoFileHashMaterialV1 {
                    path: file.path.to_string(),
                    sha256: file.sha256.to_string(),
                })
                .collect::<Vec<_>>();
            files.sort_by(|left, right| left.path.cmp(&right.path));
            SourceSubjectMaterialV1::DonatOwned(DonatOwnedMaterialV1 {
                repository_commit: repository_commit.to_string(),
                files,
            })
        }
    })
}

fn npm_signature_material(
    signature: &crate::NpmSignatureDecision,
) -> Result<NpmSignatureMaterialV1, CatalogError> {
    Ok(match signature {
        crate::NpmSignatureDecision::Verified {
            signatures,
            registry_metadata_sha256,
        } => {
            let mut signatures = signatures
                .iter()
                .map(|signature| VerifiedNpmSignatureMaterialV1 {
                    key_id: signature.key_id.clone(),
                    signature_sha256: signature.signature_sha256.to_string(),
                })
                .collect::<Vec<_>>();
            signatures.sort_by(|left, right| left.key_id.cmp(&right.key_id));
            if signatures
                .windows(2)
                .any(|pair| pair[0].key_id == pair[1].key_id)
            {
                return Err(CatalogError::new(
                    "catalog_jcs_schema_mismatch",
                    "duplicate npm signature key",
                ));
            }
            NpmSignatureMaterialV1::Verified {
                signatures,
                registry_metadata_sha256: registry_metadata_sha256.to_string(),
            }
        }
        crate::NpmSignatureDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } => NpmSignatureMaterialV1::VerifiedAbsent {
            registry_metadata_sha256: registry_metadata_sha256.to_string(),
        },
        crate::NpmSignatureDecision::Rejected { finding } => NpmSignatureMaterialV1::Rejected {
            finding: finding.to_string(),
        },
    })
}

fn npm_provenance_material(provenance: &crate::NpmProvenanceDecision) -> NpmProvenanceMaterialV1 {
    match provenance {
        crate::NpmProvenanceDecision::Verified {
            statement_sha256,
            source_commit,
        } => NpmProvenanceMaterialV1::Verified {
            statement_sha256: statement_sha256.to_string(),
            source_commit: source_commit.to_string(),
        },
        crate::NpmProvenanceDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } => NpmProvenanceMaterialV1::VerifiedAbsent {
            registry_metadata_sha256: registry_metadata_sha256.to_string(),
        },
        crate::NpmProvenanceDecision::Rejected { finding } => NpmProvenanceMaterialV1::Rejected {
            finding: finding.to_string(),
        },
    }
}

fn repository_owner_material(owner: &crate::RepositoryOwnerDecision) -> RepositoryOwnerMaterialV1 {
    match owner {
        crate::RepositoryOwnerDecision::Consistent {
            package_owner,
            repository_owner,
        } => RepositoryOwnerMaterialV1::Consistent {
            package_owner: package_owner.to_string(),
            repository_owner: repository_owner.to_string(),
        },
        crate::RepositoryOwnerDecision::ReviewedMismatch { decision_id } => {
            RepositoryOwnerMaterialV1::ReviewedMismatch {
                decision_id: decision_id.to_string(),
            }
        }
        crate::RepositoryOwnerDecision::Rejected { finding } => {
            RepositoryOwnerMaterialV1::Rejected {
                finding: finding.to_string(),
            }
        }
    }
}

fn provider_evidence_artifact_material(
    evidence: &crate::ProviderEvidenceArtifact,
) -> Result<ProviderEvidenceMaterialV1, CatalogError> {
    let crate::ProviderEvidenceArtifact {
        source,
        accessed_on,
        content_sha256,
        terms,
        facts,
    } = evidence;
    let mut facts = facts.iter().map(provider_fact_material).collect::<Vec<_>>();
    facts.sort_by(|left, right| left.fact_id.cmp(&right.fact_id));
    if facts
        .windows(2)
        .any(|pair| pair[0].fact_id == pair[1].fact_id)
    {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "duplicate provider fact ID",
        ));
    }
    Ok(ProviderEvidenceMaterialV1 {
        source: provider_evidence_source_material(source),
        accessed_on: accessed_on.to_string(),
        content_sha256: content_sha256.to_string(),
        terms: evidence_terms_material(terms),
        facts,
    })
}

fn provider_evidence_source_material(
    source: &crate::ImmutableProviderEvidenceSource,
) -> ProviderEvidenceSourceMaterialV1 {
    match source {
        crate::ImmutableProviderEvidenceSource::RepositoryFile {
            repository,
            commit,
            path,
        } => ProviderEvidenceSourceMaterialV1::RepositoryFile {
            repository: repository.to_string(),
            commit: commit.to_string(),
            path: path.to_string(),
        },
        crate::ImmutableProviderEvidenceSource::VersionedArtifact {
            url,
            provider_revision,
        } => ProviderEvidenceSourceMaterialV1::VersionedArtifact {
            url: url.to_string(),
            provider_revision: provider_revision.to_string(),
        },
    }
}

fn provider_evidence_source_key(source: &ProviderEvidenceSourceMaterialV1) -> String {
    match source {
        ProviderEvidenceSourceMaterialV1::RepositoryFile {
            repository,
            commit,
            path,
        } => format!("repository:{repository}\0{commit}\0{path}"),
        ProviderEvidenceSourceMaterialV1::VersionedArtifact {
            url,
            provider_revision,
        } => format!("artifact:{url}\0{provider_revision}"),
    }
}

fn evidence_terms_material(terms: &crate::EvidenceTermsDisposition) -> EvidenceTermsMaterialV1 {
    match terms {
        crate::EvidenceTermsDisposition::Permissive {
            license,
            evidence_url,
        } => EvidenceTermsMaterialV1::Permissive {
            license: license_material(license),
            evidence_url: evidence_url.to_string(),
        },
        crate::EvidenceTermsDisposition::ReviewedUse {
            decision_id,
            evidence_url,
        } => EvidenceTermsMaterialV1::ReviewedUse {
            decision_id: decision_id.to_string(),
            evidence_url: evidence_url.to_string(),
        },
        crate::EvidenceTermsDisposition::Rejected { finding } => {
            EvidenceTermsMaterialV1::Rejected {
                finding: finding.to_string(),
            }
        }
    }
}

fn provider_fact_material(fact: &crate::ProviderFact) -> ProviderFactMaterialV1 {
    let crate::ProviderFact {
        fact_id,
        location,
        normalized_value,
    } = fact;
    ProviderFactMaterialV1 {
        fact_id: fact_id.as_str().to_owned(),
        location: fact_location_material(location),
        normalized_value: normalized_value.clone(),
    }
}

fn fact_location_material(location: &ExactFactLocation) -> ExactFactLocationMaterialV1 {
    match location {
        ExactFactLocation::JsonPointer { path, pointer } => {
            ExactFactLocationMaterialV1::JsonPointer {
                path: path.to_string(),
                pointer: pointer.clone(),
            }
        }
        ExactFactLocation::DocumentSection { path, section } => {
            ExactFactLocationMaterialV1::DocumentSection {
                path: path.to_string(),
                section: section.clone(),
            }
        }
    }
}

fn reacquisition_material(value: &crate::ReacquisitionPlan) -> ReacquisitionMaterialV1 {
    match value {
        crate::ReacquisitionPlan::ExactNpmReview => ReacquisitionMaterialV1::ExactNpmReview(()),
        crate::ReacquisitionPlan::ProviderRepositoryReview => {
            ReacquisitionMaterialV1::ProviderRepositoryReview(())
        }
        crate::ReacquisitionPlan::ProviderVersionedArtifactReview => {
            ReacquisitionMaterialV1::ProviderVersionedArtifactReview(())
        }
        crate::ReacquisitionPlan::DonatOwnedNoNetwork => {
            ReacquisitionMaterialV1::DonatOwnedNoNetwork(())
        }
    }
}

fn artifact_hash_material(value: &ArtifactHash) -> ArtifactHashMaterialV1 {
    let ArtifactHash {
        artifact_id,
        algorithm,
        digest,
        path,
    } = value;
    ArtifactHashMaterialV1 {
        artifact_id: artifact_id.to_string(),
        algorithm: hash_algorithm_material(algorithm),
        digest: digest.clone(),
        path: path.as_ref().map(ToString::to_string),
    }
}

fn hash_algorithm_material(value: &crate::HashAlgorithm) -> HashAlgorithmMaterialV1 {
    match value {
        crate::HashAlgorithm::Sha256 => HashAlgorithmMaterialV1::Sha256(()),
        crate::HashAlgorithm::Sha512 => HashAlgorithmMaterialV1::Sha512(()),
    }
}

fn license_material(value: &crate::LicenseDecision) -> LicenseDecisionMaterialV1 {
    match value {
        crate::LicenseDecision::Permissive {
            spdx_id,
            selected_dual_license_branch,
            license_file_path,
            license_file_sha256,
        } => LicenseDecisionMaterialV1::Permissive {
            spdx_id: spdx_id.clone(),
            selected_dual_license_branch: selected_dual_license_branch.clone(),
            license_file_path: license_file_path.to_string(),
            license_file_sha256: license_file_sha256.to_string(),
        },
        crate::LicenseDecision::WrittenGrant {
            decision_id,
            grant_sha256,
        } => LicenseDecisionMaterialV1::WrittenGrant {
            decision_id: decision_id.to_string(),
            grant_sha256: grant_sha256.to_string(),
        },
        crate::LicenseDecision::Rejected { finding } => LicenseDecisionMaterialV1::Rejected {
            finding: finding.to_string(),
        },
    }
}

fn notice_material(value: &crate::NoticeIdentity) -> NoticeMaterialV1 {
    let crate::NoticeIdentity {
        id,
        license_file_path,
        license_file_sha256,
        required_copyright_lines,
        notice_bundle_destination,
    } = value;
    NoticeMaterialV1 {
        id: id.as_str().to_owned(),
        license_file_path: license_file_path.to_string(),
        license_file_sha256: license_file_sha256.to_string(),
        required_copyright_lines: required_copyright_lines.clone(),
        notice_bundle_destination: notice_bundle_destination.to_string(),
    }
}

fn sorted_dependencies(
    values: &[crate::DependencyDecision],
) -> Result<Vec<DependencyDecisionMaterialV1>, CatalogError> {
    let mut values = values
        .iter()
        .map(|value| DependencyDecisionMaterialV1 {
            dependency: value.dependency.clone(),
            disposition: dependency_disposition_material(&value.disposition),
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.dependency.cmp(&right.dependency));
    reject_adjacent_duplicate(values.iter().map(|value| value.dependency.as_str()))?;
    Ok(values)
}

fn dependency_disposition_material(
    value: &crate::DependencyDisposition,
) -> DependencyDispositionMaterialV1 {
    match value {
        crate::DependencyDisposition::Shipped { license } => {
            DependencyDispositionMaterialV1::Shipped {
                license: license_material(license),
            }
        }
        crate::DependencyDisposition::BuildOnly { license } => {
            DependencyDispositionMaterialV1::BuildOnly {
                license: license_material(license),
            }
        }
        crate::DependencyDisposition::TypeOnlyReplaced { replacement } => {
            DependencyDispositionMaterialV1::TypeOnlyReplaced {
                replacement: replacement.clone(),
            }
        }
        crate::DependencyDisposition::BehaviorOnly { reason } => {
            DependencyDispositionMaterialV1::BehaviorOnly {
                reason: reason.to_string(),
            }
        }
        crate::DependencyDisposition::Rejected { finding } => {
            DependencyDispositionMaterialV1::Rejected {
                finding: finding.to_string(),
            }
        }
    }
}

fn sorted_embedded_material(
    values: &[crate::EmbeddedMaterialDecision],
) -> Result<Vec<EmbeddedDecisionMaterialV1>, CatalogError> {
    let mut values = values
        .iter()
        .map(|value| EmbeddedDecisionMaterialV1 {
            material_id: value.material_id.clone(),
            path: value.path.to_string(),
            sha256: value.sha256.to_string(),
            disposition: embedded_disposition_material(&value.disposition),
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.material_id.cmp(&right.material_id));
    reject_adjacent_duplicate(values.iter().map(|value| value.material_id.as_str()))?;
    Ok(values)
}

fn embedded_disposition_material(
    value: &crate::EmbeddedMaterialDisposition,
) -> EmbeddedMaterialDispositionMaterialV1 {
    match value {
        crate::EmbeddedMaterialDisposition::Shipped { license } => {
            EmbeddedMaterialDispositionMaterialV1::Shipped {
                license: license_material(license),
            }
        }
        crate::EmbeddedMaterialDisposition::BehaviorOnly { reason } => {
            EmbeddedMaterialDispositionMaterialV1::BehaviorOnly {
                reason: reason.to_string(),
            }
        }
        crate::EmbeddedMaterialDisposition::Rejected { finding } => {
            EmbeddedMaterialDispositionMaterialV1::Rejected {
                finding: finding.to_string(),
            }
        }
    }
}

fn sorted_provider_contracts(
    values: &[ProviderContractReference],
) -> Result<Vec<ProviderContractMaterialV1>, CatalogError> {
    let mut values = values
        .iter()
        .map(|value| {
            let mut facts = value
                .facts
                .iter()
                .map(contract_fact_material)
                .collect::<Vec<_>>();
            facts.sort_by_key(contract_fact_material_key);
            ProviderContractMaterialV1 {
                contract_id: value.contract_id.as_str().to_owned(),
                facts,
            }
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.contract_id.cmp(&right.contract_id));
    reject_adjacent_duplicate(values.iter().map(|value| value.contract_id.as_str()))?;
    Ok(values)
}

fn contract_fact_material(value: &ContractFact) -> ContractFactMaterialV1 {
    match value {
        ContractFact::ProviderEvidence {
            source_record_id,
            fact_id,
        } => ContractFactMaterialV1::ProviderEvidence {
            source_record_id: source_record_id.as_str().to_owned(),
            fact_id: fact_id.as_str().to_owned(),
        },
        ContractFact::DonatPolicy { policy_id, value } => ContractFactMaterialV1::DonatPolicy {
            policy_id: policy_id.as_str().to_owned(),
            value: value.clone(),
        },
    }
}

fn contract_fact_material_key(value: &ContractFactMaterialV1) -> String {
    match value {
        ContractFactMaterialV1::ProviderEvidence {
            source_record_id,
            fact_id,
        } => format!("provider:{source_record_id}:{fact_id}"),
        ContractFactMaterialV1::DonatPolicy { policy_id, .. } => {
            format!("policy:{policy_id}")
        }
    }
}

fn compatibility_material(value: &crate::CompatibilityDecision) -> CompatibilityMaterialV1 {
    match value {
        crate::CompatibilityDecision::TierA => CompatibilityMaterialV1::TierA(()),
        crate::CompatibilityDecision::TierB => CompatibilityMaterialV1::TierB(()),
        crate::CompatibilityDecision::TierC => CompatibilityMaterialV1::TierC(()),
        crate::CompatibilityDecision::Rejected => CompatibilityMaterialV1::Rejected(()),
    }
}

fn admission_material(value: &crate::AdmissionState) -> Result<AdmissionMaterialV1, CatalogError> {
    Ok(match value {
        crate::AdmissionState::InventoryOnly { findings } => AdmissionMaterialV1::InventoryOnly {
            findings: sorted_unique_strings(findings)?,
        },
        crate::AdmissionState::ApprovedForPort { operations } => {
            AdmissionMaterialV1::ApprovedForPort {
                operations: sorted_unique_strings(
                    &operations
                        .iter()
                        .map(|operation| operation.as_str())
                        .collect::<Vec<_>>(),
                )?,
            }
        }
        crate::AdmissionState::EvidenceAccepted { contracts } => {
            AdmissionMaterialV1::EvidenceAccepted {
                contracts: sorted_unique_strings(
                    &contracts
                        .iter()
                        .map(|contract| contract.as_str())
                        .collect::<Vec<_>>(),
                )?,
            }
        }
    })
}

fn safety_findings_material(
    value: &crate::SafetyFindings,
) -> Result<SafetyFindingsMaterialV1, CatalogError> {
    let mut findings = value
        .findings
        .iter()
        .map(|finding| SafetyFindingMaterialV1 {
            finding_id: finding.finding_id.to_string(),
            kind: finding.kind.clone(),
            location: finding.location.as_ref().map(ToString::to_string),
            message: finding.message.clone(),
        })
        .collect::<Vec<_>>();
    findings.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    reject_adjacent_duplicate(findings.iter().map(|finding| finding.finding_id.as_str()))?;
    Ok(SafetyFindingsMaterialV1 { findings })
}

fn reject_adjacent_duplicate<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<(), CatalogError> {
    let mut previous: Option<&str> = None;
    for value in values {
        if previous == Some(value) {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "duplicate set-like material key",
            ));
        }
        previous = Some(value);
    }
    Ok(())
}

pub fn semantic_material(
    checked: &crate::CheckedConnectorManifest<'_>,
    canonical_schema_epoch: u32,
) -> Result<SemanticMaterialV1, CatalogError> {
    if canonical_schema_epoch == 0 {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "canonical schema epoch must be nonzero",
        ));
    }
    let manifest = checked.manifest();
    let crate::ConnectorManifest {
        connector,
        connector_version,
        manifest_version,
        runtime_abi_epoch,
        value_language_epoch,
        provider,
        api_identity,
        credentials,
        origins,
        operations,
        triggers,
        provenance: _,
    } = manifest;
    let mut credentials = credentials
        .iter()
        .map(semantic_credential_material)
        .collect::<Result<Vec<_>, _>>()?;
    credentials.sort_by(|left, right| {
        (&left.credential, left.version).cmp(&(&right.credential, right.version))
    });
    let mut origins = origins
        .iter()
        .map(semantic_origin_material)
        .collect::<Vec<_>>();
    origins.sort_by(|left, right| left.origin.cmp(&right.origin));
    let mut operations = operations
        .iter()
        .map(semantic_operation_material)
        .collect::<Result<Vec<_>, _>>()?;
    operations.sort_by(|left, right| {
        (&left.operation, left.operation_version).cmp(&(&right.operation, right.operation_version))
    });
    let mut triggers = triggers
        .iter()
        .map(|trigger| semantic_trigger_material(trigger, *value_language_epoch))
        .collect::<Result<Vec<_>, _>>()?;
    triggers.sort_by(|left, right| semantic_trigger_key(left).cmp(&semantic_trigger_key(right)));
    Ok(SemanticMaterialV1 {
        canonical_schema_epoch,
        connector: SemanticConnectorMaterialV1 {
            api_identity: api_identity.clone(),
            id: connector.as_str().to_owned(),
            manifest_version: *manifest_version,
            provider: provider.clone(),
            runtime_abi_epoch: *runtime_abi_epoch,
            version: *connector_version,
        },
        credentials,
        operations,
        origins,
        triggers,
        value_language_epoch: *value_language_epoch,
    })
}

fn semantic_credential_material(
    value: &crate::CredentialSpec,
) -> Result<SemanticCredentialMaterialV1, CatalogError> {
    let crate::CredentialSpec {
        credential,
        version,
        fields,
        auth_plan,
        allowed_origins,
        scopes,
        auth_processor,
        credential_test_operation,
        bounds,
    } = value;
    let mut fields = fields
        .iter()
        .map(credential_field_material)
        .collect::<Vec<_>>();
    fields.sort_by(|left, right| left.field.cmp(&right.field));
    let mut allowed_origins = allowed_origins
        .iter()
        .map(|origin| origin.as_str().to_owned())
        .collect::<Vec<_>>();
    allowed_origins.sort();
    let mut scopes = scopes.clone();
    scopes.sort();
    Ok(SemanticCredentialMaterialV1 {
        credential: credential.as_str().to_owned(),
        version: *version,
        fields,
        auth_plan: credential_auth_material(auth_plan),
        allowed_origins,
        scopes,
        auth_processor: auth_processor.as_ref().map(versioned_processor_material),
        credential_test_operation: credential_test_operation.as_ref().map(|reference| {
            VersionedOperationReferenceMaterialV1 {
                operation: reference.operation.as_str().to_owned(),
                version: reference.version,
            }
        }),
        bounds: CredentialBoundsMaterialV1 {
            maximum_field_bytes: bounds.maximum_field_bytes.get(),
            maximum_aggregate_bytes: bounds.maximum_aggregate_bytes.get(),
            maximum_token_bytes: bounds.maximum_token_bytes.get(),
        },
    })
}

fn credential_field_material(value: &crate::CredentialFieldSpec) -> CredentialFieldMaterialV1 {
    let crate::CredentialFieldSpec {
        field,
        required,
        secret,
        maximum_bytes,
        redaction,
    } = value;
    CredentialFieldMaterialV1 {
        field: field.as_str().to_owned(),
        required: *required,
        secret: secret_material(secret),
        maximum_bytes: maximum_bytes.get(),
        redaction: redaction_material(redaction),
    }
}

fn secret_material(value: &crate::SecretClassification) -> SecretClassificationMaterialV1 {
    match value {
        crate::SecretClassification::Secret => SecretClassificationMaterialV1::Secret(()),
        crate::SecretClassification::Sensitive => SecretClassificationMaterialV1::Sensitive(()),
        crate::SecretClassification::NonSecret => SecretClassificationMaterialV1::NonSecret(()),
    }
}

fn redaction_material(value: &crate::RedactionPlan) -> RedactionMaterialV1 {
    match value {
        crate::RedactionPlan::Omit => RedactionMaterialV1::Omit(()),
        crate::RedactionPlan::Fixed { replacement } => RedactionMaterialV1::Fixed {
            replacement: replacement.clone(),
        },
        crate::RedactionPlan::PreserveLast { characters } => RedactionMaterialV1::PreserveLast {
            characters: *characters,
        },
    }
}

fn credential_auth_material(value: &crate::AuthPlan) -> CredentialAuthMaterialV1 {
    match value {
        crate::AuthPlan::FixedHeaderApiKey { field, header } => {
            CredentialAuthMaterialV1::FixedHeaderApiKey {
                field: field.as_str().to_owned(),
                header: header.clone(),
            }
        }
        crate::AuthPlan::FixedQueryApiKey { field, query } => {
            CredentialAuthMaterialV1::FixedQueryApiKey {
                field: field.as_str().to_owned(),
                query: query.clone(),
            }
        }
        crate::AuthPlan::Bearer { token } => CredentialAuthMaterialV1::Bearer {
            token: token.as_str().to_owned(),
        },
        crate::AuthPlan::HttpBasic { username, password } => CredentialAuthMaterialV1::HttpBasic {
            username: username.as_str().to_owned(),
            password: password.as_str().to_owned(),
        },
        crate::AuthPlan::OAuth2ClientCredentials {
            client_id,
            client_secret,
            token_origin,
            token_step,
            scopes,
            token_pointer,
        } => {
            let mut scopes = scopes.clone();
            scopes.sort();
            CredentialAuthMaterialV1::OAuth2ClientCredentials {
                client_id: client_id.as_str().to_owned(),
                client_secret: client_secret.as_str().to_owned(),
                token_origin: token_origin.as_str().to_owned(),
                token_step: token_step.as_str().to_owned(),
                scopes,
                token_pointer: token_pointer.clone(),
            }
        }
        crate::AuthPlan::PreprovisionedOAuthAccessToken { token } => {
            CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken {
                token: token.as_str().to_owned(),
            }
        }
    }
}

fn versioned_processor_material<Id>(
    value: &crate::VersionedProcessorRef<Id>,
) -> VersionedProcessorMaterialV1
where
    Id: MaterialId,
{
    VersionedProcessorMaterialV1 {
        id: value.id.material_id().to_owned(),
        implementation_revision: value.implementation_revision,
    }
}

trait MaterialId {
    fn material_id(&self) -> &str;
}

macro_rules! material_id_impl {
    ($($type:ty),+ $(,)?) => {
        $(impl MaterialId for $type {
            fn material_id(&self) -> &str {
                self.as_str()
            }
        })+
    };
}

material_id_impl!(
    donat_connector_abi::AuthenticatorId,
    donat_connector_abi::CodecId,
    donat_connector_abi::NormalizerId,
    donat_connector_abi::ProcessorFamilyId,
);

fn semantic_origin_material(value: &crate::FixedOrigin) -> SemanticOriginMaterialV1 {
    let crate::FixedOrigin {
        origin,
        scheme: _,
        host,
        port,
        network_policy,
    } = value;
    SemanticOriginMaterialV1 {
        origin: origin.as_str().to_owned(),
        scheme: HttpsMaterialV1::HttpsOnly(()),
        host: host.clone(),
        port: port.get(),
        network_policy: match network_policy {
            crate::NetworkPolicy::PublicOnly => NetworkPolicyMaterialV1::PublicOnly(()),
            crate::NetworkPolicy::PrivateAllowed { policy } => {
                NetworkPolicyMaterialV1::PrivateAllowed {
                    policy: policy.clone(),
                }
            }
        },
    }
}

fn semantic_operation_material(
    value: &crate::OperationSpec,
) -> Result<SemanticOperationMaterialV1, CatalogError> {
    let crate::OperationSpec {
        connector,
        connector_version,
        operation,
        operation_version,
        runtime_abi_epoch,
        value_language_epoch,
        input,
        input_contract_sha256,
        output,
        output_contract_sha256,
        credential,
        origins,
        steps,
        pre_request_transforms,
        post_response_transforms,
        operation_processor,
        effect,
        pagination,
        error_map,
        capacity,
        rate,
        serialization_key_default,
        bounds,
        resolved_fact_values,
    } = value;
    let mut origins = origins
        .iter()
        .map(semantic_origin_material)
        .collect::<Vec<_>>();
    origins.sort_by(|left, right| left.origin.cmp(&right.origin));
    let mut facts = resolved_fact_values
        .iter()
        .map(|binding| ResolvedFactValueMaterialV1 {
            use_site: binding.use_site.clone(),
            value: typed_value_material(&binding.value),
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| left.use_site.cmp(&right.use_site));
    Ok(SemanticOperationMaterialV1 {
        connector: connector.as_str().to_owned(),
        connector_version: *connector_version,
        operation: operation.as_str().to_owned(),
        operation_version: *operation_version,
        runtime_abi_epoch: *runtime_abi_epoch,
        value_language_epoch: *value_language_epoch,
        input: value_contract_material(input, *value_language_epoch)?,
        input_contract_sha256: hex_bytes(input_contract_sha256),
        output: value_contract_material(output, *value_language_epoch)?,
        output_contract_sha256: hex_bytes(output_contract_sha256),
        credential: credential
            .as_ref()
            .map(|reference| VersionedCredentialMaterialV1 {
                credential: reference.credential.as_str().to_owned(),
                version: reference.version,
            }),
        origins,
        steps: steps.iter().map(semantic_step_material).collect(),
        pre_request_transforms: pre_request_transforms
            .iter()
            .map(versioned_processor_material)
            .collect(),
        post_response_transforms: post_response_transforms
            .iter()
            .map(versioned_processor_material)
            .collect(),
        operation_processor: operation_processor
            .as_ref()
            .map(versioned_processor_material),
        effect: operation_effect_material(effect),
        pagination: pagination_material(pagination),
        error_map: error_map_material(error_map),
        capacity: CapacityDefaultsMaterialV1 {
            maximum_in_flight: capacity.maximum_in_flight.get(),
        },
        rate: RateDefaultsMaterialV1 {
            burst: rate.burst.get(),
            refill_interval_ms: rate.refill_interval_ms.get().to_string(),
        },
        serialization_key_default: serialization_key_default.as_ref().map(|value| {
            TypedSerializationKeyDefaultMaterialV1 {
                field: value.field.clone(),
                value: typed_value_material(&value.value),
            }
        }),
        bounds: operation_bounds_material(bounds),
        resolved_fact_values: facts,
    })
}

fn semantic_step_material(value: &crate::CompiledStepSpec) -> SemanticStepMaterialV1 {
    let crate::CompiledStepSpec {
        step,
        method,
        origin,
        path,
        query,
        headers,
        credential_action,
        request,
        success_statuses,
        response,
        selected_response_headers,
        bounds,
    } = value;
    let mut query = query
        .iter()
        .map(|binding| CompiledQueryBindingMaterialV1 {
            name: binding.name.clone(),
            binding: compiled_binding_material(&binding.binding),
        })
        .collect::<Vec<_>>();
    query.sort_by(|left, right| left.name.cmp(&right.name));
    let mut headers = headers
        .iter()
        .map(|binding| CompiledHeaderBindingMaterialV1 {
            name: binding.name.clone(),
            binding: compiled_binding_material(&binding.binding),
        })
        .collect::<Vec<_>>();
    headers.sort_by(|left, right| left.name.cmp(&right.name));
    let mut selected_response_headers = selected_response_headers
        .iter()
        .map(selected_header_material)
        .collect::<Vec<_>>();
    selected_response_headers.sort_by(|left, right| {
        left.canonical_lowercase_header_name
            .cmp(&right.canonical_lowercase_header_name)
    });
    SemanticStepMaterialV1 {
        step: step.as_str().to_owned(),
        method: method.clone(),
        origin: origin.as_str().to_owned(),
        path: path.clone(),
        query,
        headers,
        credential_action: credential_action.as_ref().map(|action| {
            CompiledCredentialActionMaterialV1 {
                credential: action.credential.as_str().to_owned(),
            }
        }),
        request: request_shape_material(request),
        success_statuses: success_statuses.iter().map(status_range_material).collect(),
        response: response_shape_material(response),
        selected_response_headers,
        bounds: StepBoundsMaterialV1 {
            maximum_headers: bounds.maximum_headers.get(),
            maximum_header_bytes: bounds.maximum_header_bytes.get(),
            maximum_url_bytes: bounds.maximum_url_bytes.get(),
            maximum_request_bytes: bounds.maximum_request_bytes.get(),
            maximum_response_bytes: bounds.maximum_response_bytes.get(),
            maximum_json_depth: bounds.maximum_json_depth.get(),
            maximum_json_nodes: bounds.maximum_json_nodes.get(),
            maximum_inline_binary_bytes: bounds.maximum_inline_binary_bytes.get(),
            deadline_ms: bounds.deadline_ms.get().to_string(),
        },
    }
}

fn compiled_binding_material(value: &crate::CompiledBinding) -> BindingMaterialV1 {
    let crate::CompiledBinding {
        field,
        source,
        required,
        default,
        mapping,
    } = value;
    BindingMaterialV1 {
        field: field.clone(),
        source: match source {
            crate::CompiledBindingSource::Input => CompiledBindingSourceMaterialV1::Input(()),
            crate::CompiledBindingSource::Constant { value } => {
                CompiledBindingSourceMaterialV1::Constant {
                    value: typed_value_material(value),
                }
            }
        },
        required: *required,
        default: default.as_ref().map(typed_value_material),
        mapping: mapping.clone(),
    }
}

fn request_shape_material(value: &crate::CompiledRequestShape) -> CompiledRequestMaterialV1 {
    match value {
        crate::CompiledRequestShape::None => CompiledRequestMaterialV1::None(()),
        crate::CompiledRequestShape::Json { bindings } => CompiledRequestMaterialV1::Json {
            bindings: bindings.clone(),
        },
        crate::CompiledRequestShape::FormUrlencoded { bindings } => {
            CompiledRequestMaterialV1::FormUrlencoded {
                bindings: bindings.clone(),
            }
        }
        crate::CompiledRequestShape::Multipart { bindings } => {
            CompiledRequestMaterialV1::Multipart {
                bindings: bindings.clone(),
            }
        }
        crate::CompiledRequestShape::RawBytes { binding } => CompiledRequestMaterialV1::RawBytes {
            binding: binding.clone(),
        },
    }
}

fn response_shape_material(value: &crate::CompiledResponseShape) -> CompiledResponseMaterialV1 {
    match value {
        crate::CompiledResponseShape::Json { mappings } => CompiledResponseMaterialV1::Json {
            mappings: mappings
                .iter()
                .map(|mapping| ResponseMappingMaterialV1 {
                    pointer: mapping.pointer.clone(),
                    target: mapping.target.clone(),
                })
                .collect(),
        },
        crate::CompiledResponseShape::RawBytes { target } => CompiledResponseMaterialV1::RawBytes {
            target: target.clone(),
        },
    }
}

fn selected_header_material(value: &SelectedResponseHeader) -> SelectedResponseHeaderMaterialV1 {
    SelectedResponseHeaderMaterialV1 {
        canonical_lowercase_header_name: value.canonical_lowercase_header_name.clone(),
        capability: value.capability.as_str().to_owned(),
    }
}

fn status_range_material(value: &crate::StatusRange) -> StatusRangeMaterialV1 {
    StatusRangeMaterialV1 {
        minimum: value.minimum,
        maximum: value.maximum,
    }
}

fn operation_bounds_material(value: &crate::OperationBounds) -> OperationBoundsMaterialV1 {
    let crate::OperationBounds {
        maximum_calls,
        maximum_pages,
        maximum_items,
        maximum_aggregate_request_bytes,
        maximum_aggregate_response_bytes,
        maximum_output_canonical_bytes,
        maximum_redirects,
        deadline_ms,
    } = value;
    OperationBoundsMaterialV1 {
        maximum_calls: maximum_calls.get(),
        maximum_pages: maximum_pages.get(),
        maximum_items: maximum_items.get(),
        maximum_aggregate_request_bytes: maximum_aggregate_request_bytes.get(),
        maximum_aggregate_response_bytes: maximum_aggregate_response_bytes.get(),
        maximum_output_canonical_bytes: maximum_output_canonical_bytes.get(),
        maximum_redirects: *maximum_redirects,
        deadline_ms: deadline_ms.get().to_string(),
    }
}

fn operation_effect_material(value: &crate::OperationEffect) -> OperationEffectMaterialV1 {
    match value {
        crate::OperationEffect::ReadOnly => OperationEffectMaterialV1::ReadOnly(()),
        crate::OperationEffect::ProviderIdempotent { side_effect_steps } => {
            OperationEffectMaterialV1::ProviderIdempotent {
                side_effect_steps: side_effect_steps
                    .iter()
                    .map(|step| ProviderIdempotentStepMaterialV1 {
                        step: step.step.as_str().to_owned(),
                        fixed_binding: match &step.fixed_binding {
                            crate::FixedIdempotencyBinding::Header { name } => {
                                FixedIdempotencyBindingMaterialV1::Header { name: name.clone() }
                            }
                            crate::FixedIdempotencyBinding::BodyField { pointer } => {
                                FixedIdempotencyBindingMaterialV1::BodyField {
                                    pointer: pointer.clone(),
                                }
                            }
                        },
                        scope: step.scope.clone(),
                        minimum_retention_ms: step.minimum_retention_ms.get().to_string(),
                        clock_safety_margin_ms: step.clock_safety_margin_ms.get().to_string(),
                    })
                    .collect(),
            }
        }
    }
}

fn pagination_bounds_material(value: &crate::PaginationBounds) -> PaginationBoundsMaterialV1 {
    let crate::PaginationBounds {
        maximum_calls,
        maximum_pages,
        maximum_items,
        maximum_response_bytes,
        maximum_aggregate_response_bytes,
        maximum_output_canonical_bytes,
    } = value;
    PaginationBoundsMaterialV1 {
        maximum_calls: maximum_calls.get(),
        maximum_pages: maximum_pages.get(),
        maximum_items: maximum_items.get(),
        maximum_response_bytes: maximum_response_bytes.get(),
        maximum_aggregate_response_bytes: maximum_aggregate_response_bytes.get(),
        maximum_output_canonical_bytes: maximum_output_canonical_bytes.get(),
    }
}

fn pagination_material(value: &crate::PaginationPlan) -> PaginationMaterialV1 {
    match value {
        crate::PaginationPlan::None => PaginationMaterialV1::None(()),
        crate::PaginationPlan::Cursor {
            request_binding,
            response_pointer,
            bounds,
        } => PaginationMaterialV1::Cursor {
            request_binding: request_binding.clone(),
            response_pointer: response_pointer.clone(),
            bounds: pagination_bounds_material(bounds),
        },
        crate::PaginationPlan::OffsetLimit {
            offset_binding,
            limit_binding,
            initial_offset,
            page_size,
            bounds,
        } => PaginationMaterialV1::OffsetLimit {
            offset_binding: offset_binding.clone(),
            limit_binding: limit_binding.clone(),
            initial_offset: initial_offset.to_string(),
            page_size: page_size.get(),
            bounds: pagination_bounds_material(bounds),
        },
        crate::PaginationPlan::PageNumber {
            page_binding,
            page_size_binding,
            initial_page,
            page_size,
            bounds,
        } => PaginationMaterialV1::PageNumber {
            page_binding: page_binding.clone(),
            page_size_binding: page_size_binding.clone(),
            initial_page: initial_page.get().to_string(),
            page_size: page_size.get(),
            bounds: pagination_bounds_material(bounds),
        },
        crate::PaginationPlan::LinkRelation {
            relation,
            selected_header,
            bounds,
        } => PaginationMaterialV1::LinkRelation {
            relation: relation.clone(),
            selected_header: selected_header_material(selected_header),
            bounds: pagination_bounds_material(bounds),
        },
        crate::PaginationPlan::Processor { processor, bounds } => PaginationMaterialV1::Processor {
            processor: versioned_processor_material(processor),
            bounds: pagination_bounds_material(bounds),
        },
    }
}

fn error_map_material(value: &crate::ErrorMap) -> ErrorMapMaterialV1 {
    let crate::ErrorMap { rules, fallback } = value;
    ErrorMapMaterialV1 {
        rules: rules
            .iter()
            .map(|rule| ErrorRuleMaterialV1 {
                matcher: error_matcher_material(&rule.matcher),
                action: error_action_material(&rule.action),
            })
            .collect(),
        fallback: CompleteErrorFallbackMaterialV1 {
            transport: error_action_material(&fallback.transport),
            timeout: error_action_material(&fallback.timeout),
            http_429: error_action_material(&fallback.http_429),
            http_5xx: error_action_material(&fallback.http_5xx),
            authentication: error_action_material(&fallback.authentication),
            validation: error_action_material(&fallback.validation),
            permanent: error_action_material(&fallback.permanent),
            invariant: error_action_material(&fallback.invariant),
        },
    }
}

fn error_action_material(value: &crate::ErrorAction) -> ErrorActionMaterialV1 {
    let crate::ErrorAction {
        class,
        code,
        safe_message,
        retry_after,
        correlations,
    } = value;
    let mut correlations = correlations
        .iter()
        .map(|correlation| ErrorCorrelationMaterialV1 {
            canonical_lowercase_header_name: correlation.canonical_lowercase_header_name.clone(),
            capability: correlation.capability.as_str().to_owned(),
            step: correlation.step.as_str().to_owned(),
        })
        .collect::<Vec<_>>();
    correlations.sort_by(|left, right| {
        (
            &left.step,
            &left.canonical_lowercase_header_name,
            &left.capability,
        )
            .cmp(&(
                &right.step,
                &right.canonical_lowercase_header_name,
                &right.capability,
            ))
    });
    ErrorActionMaterialV1 {
        class: match class {
            crate::ConnectorErrorClass::Transport => ConnectorErrorClassMaterialV1::Transport(()),
            crate::ConnectorErrorClass::Timeout => ConnectorErrorClassMaterialV1::Timeout(()),
            crate::ConnectorErrorClass::Http429 => ConnectorErrorClassMaterialV1::Http429(()),
            crate::ConnectorErrorClass::Http5xx => ConnectorErrorClassMaterialV1::Http5xx(()),
            crate::ConnectorErrorClass::Authentication => {
                ConnectorErrorClassMaterialV1::Authentication(())
            }
            crate::ConnectorErrorClass::Validation => ConnectorErrorClassMaterialV1::Validation(()),
            crate::ConnectorErrorClass::Permanent => ConnectorErrorClassMaterialV1::Permanent(()),
            crate::ConnectorErrorClass::Invariant => ConnectorErrorClassMaterialV1::Invariant(()),
        },
        code: code.as_str().to_owned(),
        safe_message: safe_message.as_str().to_owned(),
        retry_after: match retry_after {
            crate::RetryAfterPolicy::Never => RetryAfterMaterialV1::Never(()),
            crate::RetryAfterPolicy::RetryAfterHeader {
                step,
                capability,
                maximum_seconds,
            } => RetryAfterMaterialV1::RetryAfterHeader {
                step: step.as_str().to_owned(),
                capability: capability.as_str().to_owned(),
                maximum_seconds: maximum_seconds.get(),
            },
        },
        correlations,
    }
}

fn error_matcher_material(value: &crate::ErrorMatcher) -> ErrorMatcherMaterialV1 {
    match value {
        crate::ErrorMatcher::Status(status) => {
            ErrorMatcherMaterialV1::Status(status_range_material(status))
        }
        crate::ErrorMatcher::ProviderCode { pointer, codes } => {
            let mut codes = codes.to_vec();
            codes.sort();
            ErrorMatcherMaterialV1::ProviderCode {
                pointer: pointer.clone(),
                codes,
            }
        }
        crate::ErrorMatcher::Header { name, values } => {
            let mut values = values.clone();
            values.sort();
            ErrorMatcherMaterialV1::Header {
                name: name.clone(),
                values,
            }
        }
        crate::ErrorMatcher::MalformedDeclaredSuccess => {
            ErrorMatcherMaterialV1::MalformedDeclaredSuccess(())
        }
    }
}

fn semantic_trigger_material(
    value: &crate::TriggerSpec,
    value_language_epoch: u32,
) -> Result<SemanticTriggerMaterialV1, CatalogError> {
    Ok(match value {
        crate::TriggerSpec::Webhook {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            authenticator,
            codec,
            normalizer,
            selected_headers,
            raw_body_max_bytes,
            timestamp_window_ms,
            event_id,
            event_type,
            output,
            redaction,
            subscription_operations,
        } => {
            let mut selected_headers = selected_headers.clone();
            selected_headers.sort();
            SemanticTriggerMaterialV1::Webhook {
                connector: connector.as_str().to_owned(),
                connector_version: *connector_version,
                trigger: trigger.as_str().to_owned(),
                trigger_version: *trigger_version,
                event_version: *event_version,
                runtime_abi_epoch: *runtime_abi_epoch,
                authenticator: versioned_processor_material(authenticator),
                codec: versioned_processor_material(codec),
                normalizer: versioned_processor_material(normalizer),
                selected_headers,
                raw_body_max_bytes: raw_body_max_bytes.get(),
                timestamp_window_ms: timestamp_window_ms.get().to_string(),
                event_id: value_contract_material(event_id, value_language_epoch)?,
                event_type: value_contract_material(event_type, value_language_epoch)?,
                output: value_contract_material(output, value_language_epoch)?,
                redaction: redaction_material(redaction),
                subscription_operations: subscription_operations.as_ref().map(|operations| {
                    SubscriptionOperationIdsMaterialV1 {
                        create: operations.create.as_str().to_owned(),
                        delete: operations.delete.as_str().to_owned(),
                        check: operations
                            .check
                            .as_ref()
                            .map(|operation| operation.as_str().to_owned()),
                    }
                }),
            }
        }
        crate::TriggerSpec::Poll {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            checkpoint,
            processor,
            event_type,
            per_poll_event_limit,
            bounds,
        } => SemanticTriggerMaterialV1::Poll {
            connector: connector.as_str().to_owned(),
            connector_version: *connector_version,
            trigger: trigger.as_str().to_owned(),
            trigger_version: *trigger_version,
            event_version: *event_version,
            runtime_abi_epoch: *runtime_abi_epoch,
            checkpoint: value_contract_material(checkpoint, value_language_epoch)?,
            processor: versioned_processor_material(processor),
            event_type: value_contract_material(event_type, value_language_epoch)?,
            per_poll_event_limit: per_poll_event_limit.get(),
            bounds: operation_bounds_material(bounds),
        },
    })
}

fn semantic_trigger_key(value: &SemanticTriggerMaterialV1) -> (&'static str, &str) {
    match value {
        SemanticTriggerMaterialV1::Poll { trigger, .. } => ("poll", trigger),
        SemanticTriggerMaterialV1::Webhook { trigger, .. } => ("webhook", trigger),
    }
}

pub fn provenance_material(
    checked: &crate::CheckedConnectorManifest<'_>,
    accepted_records: &AcceptedRecordCatalog,
    reviewed_policies: &BTreeMap<DonatPolicyId, TypedValue>,
    semantic_hash: AbiHash256,
    canonical_schema_epoch: u32,
    classifier_epoch: u32,
    generator_epoch: u32,
) -> Result<ProvenanceMaterialV1, CatalogError> {
    if canonical_schema_epoch == 0 || classifier_epoch == 0 || generator_epoch == 0 {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "provenance epochs must be nonzero",
        ));
    }
    let manifest = checked.manifest();
    let records = accepted_records
        .records()
        .map(|record| (record.record_id, record))
        .collect::<BTreeMap<_, _>>();
    let referenced_ids = manifest
        .provenance
        .iter()
        .map(|reference| reference.source_record_id)
        .collect::<BTreeSet<_>>();
    if referenced_ids.len() != manifest.provenance.len() || referenced_ids.is_empty() {
        return projection_input_mismatch("provenance source references are empty or duplicate");
    }
    let referenced_records = referenced_ids
        .iter()
        .map(|record_id| {
            records.get(record_id).copied().ok_or_else(|| {
                CatalogError::new(
                    "catalog_projection_input_mismatch",
                    "provenance source record is absent from the accepted catalog",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let semantic_values = manifest
        .operations
        .iter()
        .flat_map(|operation| operation.resolved_fact_values.iter())
        .cloned()
        .collect::<Vec<_>>();
    let fact_origins = manifest
        .provenance
        .iter()
        .flat_map(|reference| reference.contract_facts.iter())
        .map(|binding| ResolvedContractFactBinding {
            use_site: binding.use_site.clone(),
            fact: binding.fact.clone(),
        })
        .collect::<Vec<_>>();
    let (_, resolved_origins) = resolve_fact_bindings(
        &semantic_values,
        &fact_origins,
        accepted_records,
        reviewed_policies,
    )?;
    let origins_by_site = resolved_origins
        .into_iter()
        .map(|origin| (origin.use_site.clone(), origin))
        .collect::<BTreeMap<_, _>>();

    let mut sources = Vec::new();
    let mut artifacts = Vec::new();
    let mut files = Vec::new();
    let mut dependencies = Vec::new();
    let mut embedded_material = Vec::new();
    let mut licenses = Vec::new();
    let mut notices = Vec::new();
    let mut provider_evidence = Vec::new();
    for record in referenced_records {
        let record_material = source_record_material(record)?;
        let source_hash = record_sha256(&record_material)?;
        sources.push(SourceIdentityMaterialV1 {
            record_id: record.record_id.as_str().to_owned(),
            record_sha256: hex_bytes(source_hash.as_bytes()),
        });
        for artifact in &record.artifact_hashes {
            artifacts.push(ArtifactDecisionMaterialV1 {
                source_record_id: record.record_id.as_str().to_owned(),
                artifact_id: artifact.artifact_id.to_string(),
                algorithm: hash_algorithm_material(&artifact.algorithm),
                digest: artifact.digest.clone(),
                path: artifact.path.as_ref().map(ToString::to_string),
            });
        }
        if let SourceSubject::DonatOwned(source) = &record.subject {
            for file in &source.files {
                files.push(FileDecisionMaterialV1 {
                    source_record_id: record.record_id.as_str().to_owned(),
                    path: file.path.to_string(),
                    sha256: file.sha256.to_string(),
                });
            }
        }
        dependencies.extend(sorted_dependencies(&record.dependencies)?);
        embedded_material.extend(sorted_embedded_material(&record.embedded_material)?);
        licenses.push(license_material(&record.license));
        notices.push(notice_material(&record.notice));
        if let SourceSubject::ProviderArtifact(provider) = &record.subject {
            provider_evidence.push(provider_evidence_origin_material(
                record.record_id,
                provider,
            )?);
        }
    }
    sources.sort_by(|left, right| left.record_id.cmp(&right.record_id));
    artifacts.sort_by(|left, right| {
        (&left.source_record_id, &left.artifact_id)
            .cmp(&(&right.source_record_id, &right.artifact_id))
    });
    files.sort_by(|left, right| {
        (&left.source_record_id, &left.path).cmp(&(&right.source_record_id, &right.path))
    });
    dependencies.sort_by(|left, right| left.dependency.cmp(&right.dependency));
    embedded_material.sort_by(|left, right| left.material_id.cmp(&right.material_id));
    sort_and_deduplicate_materials(&mut licenses)?;
    notices.sort_by(|left, right| left.id.cmp(&right.id));
    notices.dedup_by(|left, right| left == right);
    provider_evidence.sort_by(|left, right| left.source_record_id.cmp(&right.source_record_id));

    let mut manifest_references = manifest
        .provenance
        .iter()
        .map(|reference| manifest_provenance_material(reference, &origins_by_site))
        .collect::<Result<Vec<_>, _>>()?;
    manifest_references.sort_by(|left, right| left.source_record_id.cmp(&right.source_record_id));
    let mut donat_policy_ids = fact_origins
        .iter()
        .filter_map(|binding| match &binding.fact {
            ContractFact::DonatPolicy { policy_id, .. } => Some(policy_id.as_str().to_owned()),
            ContractFact::ProviderEvidence { .. } => None,
        })
        .collect::<Vec<_>>();
    donat_policy_ids.sort();
    donat_policy_ids.dedup();

    Ok(ProvenanceMaterialV1 {
        artifacts,
        canonical_schema_epoch,
        classifier_epoch,
        connector: ProvenanceConnectorIdentity {
            id: manifest.connector.as_str().to_owned(),
            semantic_sha256: hex_bytes(semantic_hash.as_bytes()),
            version: manifest.connector_version,
        },
        dependencies,
        donat_policy_ids,
        embedded_material,
        files,
        generator_epoch,
        licenses,
        manifest_references,
        notices,
        provider_evidence,
        sources,
    })
}

fn provider_evidence_origin_material(
    source_record_id: crate::SourceRecordId,
    provider: &crate::ExactProviderArtifact,
) -> Result<ProviderEvidenceOriginMaterialV1, CatalogError> {
    let crate::ExactProviderArtifact { provider, evidence } = provider;
    let mut evidence = evidence
        .iter()
        .map(|entry| {
            let crate::ProviderEvidenceArtifact {
                source,
                accessed_on,
                content_sha256,
                terms,
                facts,
            } = entry;
            let mut facts = facts
                .iter()
                .map(|fact| ProviderEvidenceOriginFactMaterialV1 {
                    fact_id: fact.fact_id.as_str().to_owned(),
                    location: fact_location_material(&fact.location),
                })
                .collect::<Vec<_>>();
            facts.sort_by(|left, right| left.fact_id.cmp(&right.fact_id));
            ProviderEvidenceOriginEntryMaterialV1 {
                source: provider_evidence_source_material(source),
                accessed_on: accessed_on.to_string(),
                content_sha256: content_sha256.to_string(),
                terms: evidence_terms_material(terms),
                facts,
            }
        })
        .collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        provider_evidence_source_key(&left.source).cmp(&provider_evidence_source_key(&right.source))
    });
    Ok(ProviderEvidenceOriginMaterialV1 {
        source_record_id: source_record_id.as_str().to_owned(),
        provider: provider.clone(),
        evidence,
    })
}

fn manifest_provenance_material(
    reference: &crate::ManifestProvenanceReference,
    origins_by_site: &BTreeMap<String, ResolvedFactOriginMaterialV1>,
) -> Result<ManifestProvenanceMaterialV1, CatalogError> {
    let crate::ManifestProvenanceReference {
        source_record_id,
        artifact_hashes,
        license_id,
        notice_id,
        contract_facts,
    } = reference;
    let mut origins = contract_facts
        .iter()
        .map(|binding| {
            origins_by_site
                .get(&binding.use_site)
                .cloned()
                .ok_or_else(|| {
                    CatalogError::new(
                        "catalog_projection_input_mismatch",
                        "manifest provenance fact has no resolved origin",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    origins.sort_by(|left, right| left.use_site.cmp(&right.use_site));
    Ok(ManifestProvenanceMaterialV1 {
        source_record_id: source_record_id.as_str().to_owned(),
        artifact_hashes: sorted_artifacts(artifact_hashes)
            .into_iter()
            .map(artifact_hash_material)
            .collect(),
        license_id: license_id.clone(),
        notice_id: notice_id.as_str().to_owned(),
        contract_fact_origins: origins,
    })
}

fn sort_and_deduplicate_materials<T>(values: &mut Vec<T>) -> Result<(), CatalogError>
where
    T: Clone + Eq + Serialize,
{
    let mut keyed = values
        .drain(..)
        .map(|value| canonical_material_bytes(&value).map(|key| (key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed.dedup_by(|left, right| left.0 == right.0);
    values.extend(keyed.into_iter().map(|(_, value)| value));
    Ok(())
}

fn projection_input_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_projection_input_mismatch",
        detail,
    ))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn value_contract_material(
    value: &ValueContractCatalog,
    value_language_epoch: u32,
) -> Result<ValueContractMaterialV1, CatalogError> {
    if value_language_epoch == 0 {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-language epoch must be nonzero",
        ));
    }
    let ValueContractCatalog {
        roots,
        named_objects,
    } = value;
    let roots = roots
        .iter()
        .map(|(name, field)| field_material(field).map(|material| (name.clone(), material)))
        .collect::<Result<_, _>>()?;
    let named_objects = named_objects
        .iter()
        .map(|(name, object)| {
            validate_material_name(name)?;
            let donat_value_contract::ValueObjectContract { fields } = object;
            let fields = fields
                .iter()
                .map(|(field_name, field)| {
                    validate_material_name(field_name)?;
                    field_material(field).map(|material| (field_name.clone(), material))
                })
                .collect::<Result<BTreeMap<_, _>, CatalogError>>()?;
            Ok((name.clone(), NamedObjectMaterialV1 { fields }))
        })
        .collect::<Result<_, CatalogError>>()?;
    Ok(ValueContractMaterialV1 {
        named_objects,
        roots,
        value_language_epoch,
    })
}

fn field_material(
    field: &donat_value_contract::ValueContractField,
) -> Result<FieldMaterialV1, CatalogError> {
    let donat_value_contract::ValueContractField { required, type_ref } = field;
    let donat_value_contract::TypeRef {
        nullable,
        value_type,
    } = type_ref;
    Ok(FieldMaterialV1 {
        required: *required,
        type_ref: TypeRefMaterialV1 {
            nullable: *nullable,
            value_type: value_type_material(value_type)?,
        },
    })
}

fn value_type_material(value: &ValueType) -> Result<ValueTypeMaterialV1, CatalogError> {
    let material = match value {
        ValueType::Scalar { scalar } => ValueTypeMaterial::Scalar(scalar_material(scalar)?),
        ValueType::Enum { name, values } => {
            validate_material_name(name)?;
            for value in values {
                validate_material_name(value)?;
            }
            ValueTypeMaterial::Enum {
                name: name.clone(),
                values: values.clone(),
            }
        }
        ValueType::Object { fields } => {
            let fields = fields
                .iter()
                .map(|(name, field)| {
                    validate_material_name(name)?;
                    field_material(field).map(|value| (name.clone(), value))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            ValueTypeMaterial::Object { fields }
        }
        ValueType::List { element } => {
            let donat_value_contract::TypeRef {
                nullable,
                value_type,
            } = element.as_ref();
            ValueTypeMaterial::List {
                element: Box::new(TypeRefMaterialV1 {
                    nullable: *nullable,
                    value_type: value_type_material(value_type)?,
                }),
            }
        }
        ValueType::Ref { name } => {
            validate_material_name(name)?;
            ValueTypeMaterial::Ref { name: name.clone() }
        }
    };
    Ok(ValueTypeMaterialV1(material))
}

fn scalar_material(value: &ValueScalar) -> Result<ValueScalarMaterialV1, CatalogError> {
    let material = match value {
        ValueScalar::Boolean => ValueScalarMaterial::Boolean(()),
        ValueScalar::String => ValueScalarMaterial::String(()),
        ValueScalar::Int32 => ValueScalarMaterial::Int32(()),
        ValueScalar::Int64 => ValueScalarMaterial::Int64(()),
        ValueScalar::UInt64 => ValueScalarMaterial::UInt64(()),
        ValueScalar::Decimal => ValueScalarMaterial::Decimal(()),
        ValueScalar::Uuid => ValueScalarMaterial::Uuid(()),
        ValueScalar::Date => ValueScalarMaterial::Date(()),
        ValueScalar::Timestamp => ValueScalarMaterial::Timestamp(()),
        ValueScalar::TimestampTz => ValueScalarMaterial::TimestampTz(()),
        ValueScalar::Json => ValueScalarMaterial::Json(()),
        ValueScalar::Custom { name } => {
            validate_material_name(name)?;
            ValueScalarMaterial::Custom { name: name.clone() }
        }
    };
    Ok(ValueScalarMaterialV1(material))
}

fn validate_material_name(value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.chars().any(|character| {
            let scalar = u32::from(character);
            (0xfdd0..=0xfdef).contains(&scalar)
                || scalar & 0xffff == 0xfffe
                || scalar & 0xffff == 0xffff
        })
    {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-contract name is empty or not I-JSON",
        ));
    }
    Ok(())
}

pub fn decode_value_contract_material(
    bytes: &[u8],
) -> Result<ValueContractMaterialV1, CatalogError> {
    let canonical = canonicalize_raw(bytes)?;
    let decoded: ValueContractMaterialDto =
        serde_json::from_slice(&canonical).map_err(|error| {
            CatalogError::new("catalog_projection_input_mismatch", error.to_string())
        })?;
    value_contract_material_from_dto(decoded)
}

fn value_contract_material_from_dto(
    decoded: ValueContractMaterialDto,
) -> Result<ValueContractMaterialV1, CatalogError> {
    if decoded.value_language_epoch == 0 {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-language epoch must be nonzero",
        ));
    }
    validate_decoded_value_contract(&decoded)?;
    Ok(ValueContractMaterialV1 {
        named_objects: decoded.named_objects,
        roots: decoded.roots,
        value_language_epoch: decoded.value_language_epoch,
    })
}

fn validate_decoded_value_contract(value: &ValueContractMaterialDto) -> Result<(), CatalogError> {
    let mut type_refs = Vec::new();
    for (name, field) in &value.roots {
        validate_material_name(name)?;
        type_refs.push(&field.type_ref);
    }
    for (name, object) in &value.named_objects {
        validate_material_name(name)?;
        for (field_name, field) in &object.fields {
            validate_material_name(field_name)?;
            type_refs.push(&field.type_ref);
        }
    }
    while let Some(type_ref) = type_refs.pop() {
        let TypeRefMaterialV1 {
            nullable: _,
            value_type,
        } = type_ref;
        match &value_type.0 {
            ValueTypeMaterial::Scalar(ValueScalarMaterialV1(ValueScalarMaterial::Custom {
                name,
            }))
            | ValueTypeMaterial::Ref { name } => validate_material_name(name)?,
            ValueTypeMaterial::Enum { name, values } => {
                validate_material_name(name)?;
                for value in values {
                    validate_material_name(value)?;
                }
            }
            ValueTypeMaterial::Object {
                fields: nested_fields,
            } => {
                for (name, field) in nested_fields {
                    validate_material_name(name)?;
                    type_refs.push(&field.type_ref);
                }
            }
            ValueTypeMaterial::List { element } => type_refs.push(element.as_ref()),
            ValueTypeMaterial::Scalar(_) => {}
        }
    }
    Ok(())
}

pub fn typed_value_material(value: &TypedValue) -> TypedValueMaterialV1 {
    TypedValueMaterialV1::from_typed_value(value)
}

pub fn canonical_material_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CatalogError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
    canonicalize_raw(&bytes)
}

fn hash_material<T: Serialize>(
    domain: CatalogHashDomain,
    value: &T,
) -> Result<AbiHash256, CatalogError> {
    Ok(AbiHash256::new(domain_hash_bytes(
        domain,
        &canonical_material_bytes(value)?,
    )))
}

pub fn record_sha256(value: &SourceRecordMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::SourceRecord, value)
}

pub fn semantic_sha256(value: &SemanticMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Semantic, value)
}

pub fn provenance_sha256(value: &ProvenanceMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Provenance, value)
}

pub fn value_contract_sha256(value: &ValueContractMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::ValueContract, value)
}

pub fn canonical_projection_owner_manifest() -> &'static str {
    const DOCUMENT: &str = include_str!(
        "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
    );
    const OPEN: &str = "```text\nnormalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type\n";
    let start = DOCUMENT
        .find(OPEN)
        .expect("accepted ADR 012 must contain the owner manifest")
        + "```text\n".len();
    let end = DOCUMENT[start..]
        .find("\n```")
        .expect("owner manifest code block must terminate")
        + start;
    &DOCUMENT[start..end]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerManifestValidation {
    pub mapping_rows: usize,
    pub normalized_leaf_and_branch_total: usize,
}

pub fn validate_canonical_owner_manifest() -> Result<OwnerManifestValidation, CatalogError> {
    validate_canonical_owner_manifest_text(canonical_projection_owner_manifest())
}

fn validate_canonical_owner_manifest_text(
    manifest: &str,
) -> Result<OwnerManifestValidation, CatalogError> {
    let mut lines = manifest.lines();
    let header = lines.next().ok_or_else(|| {
        CatalogError::new("catalog_projection_manifest_invalid", "missing header")
    })?;
    if header != "normalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type" {
        return Err(CatalogError::new(
            "catalog_projection_manifest_invalid",
            "unexpected owner-manifest header",
        ));
    }
    let mut owner_domains = BTreeSet::new();
    let mut canonical_paths = BTreeSet::new();
    let mut mapping_rows = 0;
    for line in lines {
        let columns: Vec<_> = line.split('|').collect();
        if columns.len() != 7
            || columns.iter().any(|column| column.is_empty())
            || columns.iter().any(|column| {
                *column == "*"
                    || *column == "family"
                    || column.contains("<family>")
                    || column.contains("all fields")
                    || column.contains("corresponding")
            })
            || !matches!(
                columns[1],
                "source-record" | "semantic" | "provenance" | "value-contract"
            )
            || (!columns[3].eq("normalized")
                && !columns[3].eq("constant")
                && !columns[3].starts_with("derived:"))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_invalid",
                line,
            ));
        }
        if !owner_domains.insert((columns[0], columns[1]))
            || !canonical_paths.insert((columns[1], columns[2]))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_invalid",
                line,
            ));
        }
        mapping_rows += 1;
    }
    if mapping_rows == 0 || owner_domains.is_empty() {
        return Err(CatalogError::new(
            "catalog_projection_manifest_incomplete",
            "owner manifest contains no mappings",
        ));
    }
    Ok(OwnerManifestValidation {
        mapping_rows,
        normalized_leaf_and_branch_total: owner_domains.len(),
    })
}

pub fn selected_response_header(
    connector: ConnectorId,
    operation: OperationId,
    operation_version: StableSemver,
    step: CompiledStepId,
    header: &str,
) -> Result<SelectedResponseHeader, CatalogError> {
    let canonical_header = canonical_header_name(header)?;
    let bytes = format!(
        "{{\"connector\":{},\"header\":{},\"operation\":{},\"operation_version\":{{\"major\":{},\"minor\":{},\"patch\":{}}},\"step\":{}}}",
        json_string(connector.as_str()),
        json_string(&canonical_header),
        json_string(operation.as_str()),
        operation_version.major,
        operation_version.minor,
        operation_version.patch,
        json_string(step.as_str()),
    );
    let mut hash = Sha256::new();
    hash.update(RESPONSE_HEADER_DOMAIN);
    hash.update(bytes.as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut value = String::from("response-header.");
    for byte in digest {
        use core::fmt::Write;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    let capability = CapabilityId::parse(&value)
        .map_err(|_| CatalogError::new("catalog_selected_header_invalid", "capability ID"))?;
    Ok(SelectedResponseHeader {
        canonical_lowercase_header_name: canonical_header,
        capability,
    })
}

fn canonical_header_name(header: &str) -> Result<String, CatalogError> {
    if header.is_empty()
        || !header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(CatalogError::new(
            "catalog_selected_header_invalid",
            "header name is not an ASCII token",
        ));
    }
    Ok(header.to_ascii_lowercase())
}

pub fn canonicalize_raw(bytes: &[u8]) -> Result<Vec<u8>, CatalogError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| CatalogError::new("catalog_jcs_invalid_utf8", "input is not valid UTF-8"))?;
    validate_json_string_unicode(source)?;
    let number_tokens = scan_number_tokens(source)?;
    let number_cursor = Cell::new(0);

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = JValueSeed {
        source,
        number_tokens: &number_tokens,
        number_cursor: &number_cursor,
    }
    .deserialize(&mut deserializer)
    .map_err(map_json_error)?;
    deserializer.end().map_err(map_json_error)?;
    if number_cursor.get() != number_tokens.len() {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "raw number token cursor did not consume the complete input",
        ));
    }
    let mut output = Vec::new();
    value.write_canonical(&mut output);
    Ok(output)
}

fn scan_number_tokens(source: &str) -> Result<Vec<Range<usize>>, CatalogError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut in_string = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if !in_string => {
                in_string = true;
                index += 1;
            }
            b'"' if in_string => {
                in_string = false;
                index += 1;
            }
            b'\\' if in_string => {
                index = index.saturating_add(2);
            }
            b'-' | b'0'..=b'9' if !in_string => {
                let end = scan_json_number(bytes, index)?;
                tokens.push(index..end);
                index = end;
            }
            _ => index += 1,
        }
    }
    Ok(tokens)
}

fn scan_json_number(bytes: &[u8], start: usize) -> Result<usize, CatalogError> {
    let mut index = start;
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    }
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number integer component",
            ));
        }
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number fraction",
            ));
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number exponent",
            ));
        }
    }
    Ok(index)
}

fn canonical_number(raw: &str) -> Result<String, CatalogError> {
    let number = raw.parse::<f64>().map_err(|_| {
        CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number is not finite binary64",
        )
    })?;
    if !number.is_finite() {
        return Err(CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number is not finite binary64",
        ));
    }
    let mut buffer = ryu_js::Buffer::new();
    let canonical = buffer.format_finite(number);
    if decimal_identity(raw)? != decimal_identity(canonical)? {
        return Err(CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number changes mathematical value as binary64",
        ));
    }
    Ok(canonical.to_owned())
}

#[derive(Debug, Eq, PartialEq)]
enum DecimalIdentity {
    Zero,
    NonZero {
        negative: bool,
        coefficient: String,
        exponent: i64,
    },
}

fn decimal_identity(raw: &str) -> Result<DecimalIdentity, CatalogError> {
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, "0"), |parts| parts);
    let (integer, fraction) = mantissa
        .split_once('.')
        .map_or((mantissa, ""), |parts| parts);
    let mut coefficient = String::with_capacity(integer.len() + fraction.len());
    coefficient.push_str(integer);
    coefficient.push_str(fraction);
    let coefficient = coefficient.trim_start_matches('0');
    if coefficient.is_empty() {
        return Ok(DecimalIdentity::Zero);
    }
    let trailing_zeros = coefficient.len() - coefficient.trim_end_matches('0').len();
    let coefficient = coefficient[..coefficient.len() - trailing_zeros].to_owned();
    let exponent = parse_decimal_exponent(exponent)?
        .checked_sub(i64::try_from(fraction.len()).map_err(|_| number_not_exact())?)
        .and_then(|value| value.checked_add(i64::try_from(trailing_zeros).ok()?))
        .ok_or_else(number_not_exact)?;
    Ok(DecimalIdentity::NonZero {
        negative,
        coefficient,
        exponent,
    })
}

fn parse_decimal_exponent(value: &str) -> Result<i64, CatalogError> {
    let (negative, digits) = value.strip_prefix('-').map_or_else(
        || (false, value.strip_prefix('+').unwrap_or(value)),
        |digits| (true, digits),
    );
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let magnitude = digits.parse::<i64>().map_err(|_| number_not_exact())?;
    Ok(if negative { -magnitude } else { magnitude })
}

fn number_not_exact() -> CatalogError {
    CatalogError::new(
        "canonical_json_number_not_exact",
        "raw number exponent is outside the exact comparison range",
    )
}

fn map_json_error(error: serde_json::Error) -> CatalogError {
    let message = error.to_string();
    if message.contains("duplicate decoded member") {
        CatalogError::new("catalog_jcs_duplicate_member", message)
    } else if message.contains("canonical_json_number_not_exact")
        || message.contains("number out of range")
        || message.contains("number is not exactly representable")
    {
        CatalogError::new("canonical_json_number_not_exact", message)
    } else {
        CatalogError::new("catalog_jcs_schema_mismatch", message)
    }
}

fn validate_json_string_unicode(source: &str) -> Result<(), CatalogError> {
    let bytes = source.as_bytes();
    let mut byte_index = 0;
    let mut in_string = false;
    while byte_index < bytes.len() {
        let byte = bytes[byte_index];
        if !in_string {
            if byte == b'"' {
                in_string = true;
            }
            byte_index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = false;
                byte_index += 1;
            }
            b'\\' => {
                byte_index += 1;
                if byte_index >= bytes.len() {
                    break;
                }
                if bytes[byte_index] != b'u' {
                    byte_index += 1;
                    continue;
                }
                let (first, next) = read_hex_escape(bytes, byte_index)?;
                byte_index = next;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if byte_index + 1 >= bytes.len()
                        || bytes[byte_index] != b'\\'
                        || bytes[byte_index + 1] != b'u'
                    {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "lone high surrogate",
                        ));
                    }
                    let (second, following) = read_hex_escape(bytes, byte_index + 1)?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "high surrogate is not followed by a low surrogate",
                        ));
                    }
                    byte_index = following;
                    0x1_0000 + (((u32::from(first) - 0xd800) << 10) | (u32::from(second) - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(CatalogError::new(
                        "catalog_jcs_invalid_surrogate",
                        "lone low surrogate",
                    ));
                } else {
                    u32::from(first)
                };
                reject_noncharacter(scalar)?;
            }
            _ if byte.is_ascii() => byte_index += 1,
            _ => {
                let character = source[byte_index..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 has a character");
                reject_noncharacter(u32::from(character))?;
                byte_index += character.len_utf8();
            }
        }
    }
    Ok(())
}

fn read_hex_escape(bytes: &[u8], u_index: usize) -> Result<(u16, usize), CatalogError> {
    if u_index + 5 > bytes.len() || bytes[u_index] != b'u' {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "incomplete Unicode escape",
        ));
    }
    let mut value = 0_u16;
    for byte in &bytes[u_index + 1..u_index + 5] {
        value = value
            .checked_mul(16)
            .and_then(|current| {
                let digit = match byte {
                    b'0'..=b'9' => u16::from(*byte - b'0'),
                    b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                    _ => return None,
                };
                current.checked_add(digit)
            })
            .ok_or_else(|| {
                CatalogError::new("catalog_jcs_schema_mismatch", "invalid Unicode escape")
            })?;
    }
    Ok((value, u_index + 5))
}

fn reject_noncharacter(scalar: u32) -> Result<(), CatalogError> {
    if (0xfdd0..=0xfdef).contains(&scalar) || scalar & 0xffff == 0xfffe || scalar & 0xffff == 0xffff
    {
        Err(CatalogError::new(
            "catalog_jcs_disallowed_unicode",
            "Unicode noncharacters are not I-JSON",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
enum JValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl JValue {
    fn write_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Null => output.extend_from_slice(b"null"),
            Self::Bool(true) => output.extend_from_slice(b"true"),
            Self::Bool(false) => output.extend_from_slice(b"false"),
            Self::Number(value) => output.extend_from_slice(value.as_bytes()),
            Self::String(value) => output.extend_from_slice(json_string(value).as_bytes()),
            Self::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    value.write_canonical(output);
                }
                output.push(b']');
            }
            Self::Object(values) => {
                let mut values: Vec<_> = values.iter().collect();
                values.sort_by(|left, right| utf16_cmp(&left.0, &right.0));
                output.push(b'{');
                for (index, (name, value)) in values.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    output.extend_from_slice(json_string(name).as_bytes());
                    output.push(b':');
                    value.write_canonical(output);
                }
                output.push(b'}');
            }
        }
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("valid Rust strings are JSON strings")
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[derive(Clone, Copy)]
struct JValueSeed<'a> {
    source: &'a str,
    number_tokens: &'a [Range<usize>],
    number_cursor: &'a Cell<usize>,
}

impl<'de> DeserializeSeed<'de> for JValueSeed<'_> {
    type Value = JValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(JValueVisitor {
            source: self.source,
            number_tokens: self.number_tokens,
            number_cursor: self.number_cursor,
        })
    }
}

struct JValueVisitor<'a> {
    source: &'a str,
    number_tokens: &'a [Range<usize>],
    number_cursor: &'a Cell<usize>,
}

impl JValueVisitor<'_> {
    fn next_number<E>(&self) -> Result<String, E>
    where
        E: serde::de::Error,
    {
        let index = self.number_cursor.get();
        let range = self
            .number_tokens
            .get(index)
            .ok_or_else(|| E::custom("raw number token cursor exhausted"))?;
        self.number_cursor.set(index + 1);
        let raw = self
            .source
            .get(range.clone())
            .ok_or_else(|| E::custom("raw number token range is outside the input"))?;
        canonical_number(raw).map_err(E::custom)
    }
}

impl<'de> Visitor<'de> for JValueVisitor<'_> {
    type Value = JValue;

    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("one I-JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(JValue::Bool(value))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(JValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(JValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JValueSeed {
            source: self.source,
            number_tokens: self.number_tokens,
            number_cursor: self.number_cursor,
        })? {
            values.push(value);
        }
        Ok(JValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        let mut values = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            if !names.insert(name.clone()) {
                return Err(serde::de::Error::custom("duplicate decoded member"));
            }
            values.push((
                name,
                map.next_value_seed(JValueSeed {
                    source: self.source,
                    number_tokens: self.number_tokens,
                    number_cursor: self.number_cursor,
                })?,
            ));
        }
        Ok(JValue::Object(values))
    }
}
