use std::fmt;
use std::path::Path;

use base64::Engine;
use donat_connector_abi::{InlineId, OperationId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::CatalogError;

macro_rules! catalog_id {
    ($name:ident) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(InlineId);

        impl $name {
            pub const fn literal(value: &'static str) -> Self {
                Self(InlineId::literal(value))
            }

            pub fn parse(value: &str) -> Result<Self, CatalogError> {
                InlineId::parse(value)
                    .map(Self)
                    .map_err(|_| CatalogError::new("source_record_invalid_id", value))
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.as_str())
                    .finish()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

catalog_id!(SourceRecordId);
catalog_id!(ProviderContractId);
catalog_id!(ProviderFactId);
catalog_id!(DonatPolicyId);
catalog_id!(NoticeId);

pub type ArtifactId = String;
pub type Date = String;
pub type ExactHttpsUrl = String;
pub type FindingId = String;
pub type GitCommit = String;
pub type GitTree = String;
pub type Hash256 = String;
pub type NonEmptyString = String;
pub type NpmMaintainerIdentity = String;
pub type NpmOwnerIdentity = String;
pub type RepoPath = String;
pub type RepositoryOwnerIdentity = String;
pub type RepositoryUrl = String;
pub type ReviewDecisionId = String;
pub type ReviewIdentity = String;
pub type SourcePath = String;
pub type TestId = String;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExactSemver(String);

impl ExactSemver {
    pub fn try_new(value: &str) -> Result<Self, CatalogError> {
        if valid_exact_semver(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CatalogError::new(
                "source_record_invalid_semver",
                "expected one canonical exact SemVer 2.0.0 version",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ExactSemver {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExactSemver {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(&value).map_err(serde::de::Error::custom)
    }
}

fn valid_exact_semver(value: &str) -> bool {
    if value.is_empty() || !value.is_ascii() {
        return false;
    }
    let (without_build, build) = match value.split_once('+') {
        Some((left, right)) if !right.is_empty() && !right.contains('+') => (left, Some(right)),
        Some(_) => return false,
        None => (value, None),
    };
    if let Some(build) = build
        && !valid_identifiers(build, false)
    {
        return false;
    }
    let (core, prerelease) = match without_build.split_once('-') {
        Some((left, right)) if !right.is_empty() => (left, Some(right)),
        Some(_) => return false,
        None => (without_build, None),
    };
    if let Some(prerelease) = prerelease
        && !valid_identifiers(prerelease, true)
    {
        return false;
    }
    let components: Vec<_> = core.split('.').collect();
    components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == &"0" || !component.starts_with('0'))
                && component.parse::<u32>().is_ok()
        })
}

fn valid_identifiers(value: &str, numeric_leading_zero_rejects: bool) -> bool {
    value.split('.').all(|identifier| {
        !identifier.is_empty()
            && identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !(numeric_leading_zero_rejects
                && identifier.len() > 1
                && identifier.bytes().all(|byte| byte.is_ascii_digit())
                && identifier.starts_with('0'))
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSourceRecord {
    pub record_version: u32,
    pub record_id: SourceRecordId,
    pub subject: SourceSubject,
    pub reacquisition: ReacquisitionPlan,
    pub artifact_hashes: Vec<ArtifactHash>,
    pub license: LicenseDecision,
    pub notice: NoticeIdentity,
    pub entrypoints: Vec<SourcePath>,
    pub dependencies: Vec<DependencyDecision>,
    pub embedded_material: Vec<EmbeddedMaterialDecision>,
    pub provider_contracts: Vec<ProviderContractReference>,
    pub compatibility: CompatibilityDecision,
    pub admission: AdmissionState,
    pub safety_findings: SafetyFindings,
    pub reviewer: ReviewIdentity,
    pub approval_date: Date,
    pub proposed_manifest: Option<RepoPath>,
    pub proposed_destinations: Vec<RepoPath>,
    pub red_tests: Vec<TestId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum SourceSubject {
    ExactNpm(ExactNpmPackage),
    ProviderArtifact(ExactProviderArtifact),
    DonatOwned(DonatOwnedSource),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ReacquisitionPlan {
    ExactNpmReview,
    ProviderRepositoryReview,
    ProviderVersionedArtifactReview,
    DonatOwnedNoNetwork,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExactNpmPackage {
    pub name: String,
    pub version: ExactSemver,
    pub tarball_url: ExactHttpsUrl,
    pub integrity: NpmIntegrity,
    pub repository: ImmutableRepository,
    pub npm_git_head: GitCommit,
    pub package_repository: RepositoryUrl,
    pub signature: NpmSignatureDecision,
    pub provenance: NpmProvenanceDecision,
    pub tag_commit: Option<GitCommit>,
    pub provenance_commit: Option<GitCommit>,
    pub maintainers: Vec<NpmMaintainerIdentity>,
    pub repository_owner: RepositoryOwnerDecision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NpmIntegrity {
    pub algorithm: String,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableRepository {
    pub url: RepositoryUrl,
    pub commit: GitCommit,
    pub tree: GitTree,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NpmSignatureDecision {
    Verified {
        signatures: Vec<VerifiedNpmSignature>,
        registry_metadata_sha256: Hash256,
    },
    VerifiedAbsent {
        registry_metadata_sha256: Hash256,
    },
    Rejected {
        finding: FindingId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum NpmProvenanceDecision {
    Verified {
        statement_sha256: Hash256,
        source_commit: GitCommit,
    },
    VerifiedAbsent {
        registry_metadata_sha256: Hash256,
    },
    Rejected {
        finding: FindingId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RepositoryOwnerDecision {
    Consistent {
        package_owner: NpmOwnerIdentity,
        repository_owner: RepositoryOwnerIdentity,
    },
    ReviewedMismatch {
        decision_id: ReviewDecisionId,
    },
    Rejected {
        finding: FindingId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedNpmSignature {
    pub key_id: String,
    pub signature_sha256: Hash256,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExactProviderArtifact {
    pub provider: String,
    pub evidence: Vec<ProviderEvidenceArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEvidenceArtifact {
    pub source: ImmutableProviderEvidenceSource,
    pub accessed_on: Date,
    pub content_sha256: Hash256,
    pub terms: EvidenceTermsDisposition,
    pub facts: Vec<ProviderFact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ImmutableProviderEvidenceSource {
    RepositoryFile {
        repository: RepositoryUrl,
        commit: GitCommit,
        path: SourcePath,
    },
    VersionedArtifact {
        url: ExactHttpsUrl,
        provider_revision: NonEmptyString,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EvidenceTermsDisposition {
    Permissive {
        license: LicenseDecision,
        evidence_url: ExactHttpsUrl,
    },
    ReviewedUse {
        decision_id: ReviewDecisionId,
        evidence_url: ExactHttpsUrl,
    },
    Rejected {
        finding: FindingId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFact {
    pub fact_id: ProviderFactId,
    pub location: ExactFactLocation,
    pub normalized_value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExactFactLocation {
    JsonPointer { path: SourcePath, pointer: String },
    DocumentSection { path: SourcePath, section: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TypedValueMaterialV1 {
    Null,
    Boolean(bool),
    String(String),
    I64(String),
    U64(String),
    Decimal(String),
    List(Vec<TypedValueMaterialV1>),
    Object(std::collections::BTreeMap<String, TypedValueMaterialV1>),
    InlineBytes {
        #[serde(rename = "$binary")]
        binary: String,
        file_name: Option<String>,
        media_type: Option<String>,
    },
}

impl Serialize for TypedValueMaterialV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut tagged = serializer.serialize_struct("TypedValueMaterialV1", 2)?;
        match self {
            Self::Null => {
                tagged.serialize_field("kind", "null")?;
                tagged.serialize_field("value", &Option::<()>::None)?;
            }
            Self::Boolean(value) => {
                tagged.serialize_field("kind", "boolean")?;
                tagged.serialize_field("value", value)?;
            }
            Self::String(value) => {
                tagged.serialize_field("kind", "string")?;
                tagged.serialize_field("value", value)?;
            }
            Self::I64(value) => {
                tagged.serialize_field("kind", "i64")?;
                tagged.serialize_field("value", value)?;
            }
            Self::U64(value) => {
                tagged.serialize_field("kind", "u64")?;
                tagged.serialize_field("value", value)?;
            }
            Self::Decimal(value) => {
                tagged.serialize_field("kind", "decimal")?;
                tagged.serialize_field("value", value)?;
            }
            Self::List(value) => {
                tagged.serialize_field("kind", "list")?;
                tagged.serialize_field("value", value)?;
            }
            Self::Object(value) => {
                tagged.serialize_field("kind", "object")?;
                tagged.serialize_field("value", value)?;
            }
            Self::InlineBytes {
                binary,
                file_name,
                media_type,
            } => {
                #[derive(Serialize)]
                struct Inline<'a> {
                    #[serde(rename = "$binary")]
                    binary: &'a str,
                    file_name: &'a Option<String>,
                    media_type: &'a Option<String>,
                }
                tagged.serialize_field("kind", "inline_bytes")?;
                tagged.serialize_field(
                    "value",
                    &Inline {
                        binary,
                        file_name,
                        media_type,
                    },
                )?;
            }
        }
        tagged.end()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ContractFact {
    ProviderEvidence {
        source_record_id: SourceRecordId,
        fact_id: ProviderFactId,
    },
    DonatPolicy {
        policy_id: DonatPolicyId,
        value: TypedValueMaterialV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContractReference {
    pub contract_id: ProviderContractId,
    pub facts: Vec<ContractFact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DonatOwnedSource {
    pub repository_commit: GitCommit,
    pub files: Vec<RepoFileHash>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFileHash {
    pub path: RepoPath,
    pub sha256: Hash256,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CompatibilityDecision {
    TierA,
    TierB,
    TierC,
    Rejected,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AdmissionState {
    InventoryOnly {
        findings: Vec<FindingId>,
    },
    ApprovedForPort {
        #[serde(with = "operation_ids")]
        operations: Vec<OperationId>,
    },
    EvidenceAccepted {
        contracts: Vec<ProviderContractId>,
    },
}

impl fmt::Debug for AdmissionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InventoryOnly { findings } => formatter
                .debug_struct("InventoryOnly")
                .field("findings", findings)
                .finish(),
            Self::ApprovedForPort { operations } => formatter
                .debug_struct("ApprovedForPort")
                .field(
                    "operations",
                    &operations
                        .iter()
                        .map(OperationId::as_str)
                        .collect::<Vec<_>>(),
                )
                .finish(),
            Self::EvidenceAccepted { contracts } => formatter
                .debug_struct("EvidenceAccepted")
                .field("contracts", contracts)
                .finish(),
        }
    }
}

mod operation_ids {
    use donat_connector_abi::OperationId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(values: &[OperationId], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(OperationId::as_str)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<OperationId>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|value| {
                OperationId::parse(&value)
                    .map_err(|_| serde::de::Error::custom("invalid operation ID"))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactHash {
    pub artifact_id: ArtifactId,
    pub algorithm: HashAlgorithm,
    pub digest: String,
    pub path: Option<SourcePath>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HashAlgorithm {
    Sha256,
    Sha512,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LicenseDecision {
    Permissive {
        spdx_id: String,
        selected_dual_license_branch: Option<String>,
        license_file_path: SourcePath,
        license_file_sha256: Hash256,
    },
    WrittenGrant {
        decision_id: ReviewDecisionId,
        grant_sha256: Hash256,
    },
    Rejected {
        finding: FindingId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeIdentity {
    pub id: NoticeId,
    pub license_file_path: SourcePath,
    pub license_file_sha256: Hash256,
    pub required_copyright_lines: Vec<String>,
    pub notice_bundle_destination: RepoPath,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyDecision {
    pub dependency: String,
    pub disposition: DependencyDisposition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DependencyDisposition {
    Shipped { license: LicenseDecision },
    BuildOnly { license: LicenseDecision },
    TypeOnlyReplaced { replacement: String },
    BehaviorOnly { reason: FindingId },
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedMaterialDecision {
    pub material_id: String,
    pub path: SourcePath,
    pub sha256: Hash256,
    pub disposition: EmbeddedMaterialDisposition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EmbeddedMaterialDisposition {
    Shipped { license: LicenseDecision },
    BehaviorOnly { reason: FindingId },
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFindings {
    pub findings: Vec<SafetyFinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFinding {
    pub finding_id: FindingId,
    pub kind: String,
    pub location: Option<SourcePath>,
    pub message: String,
}

pub fn load_record(path: impl AsRef<Path>) -> Result<ConnectorSourceRecord, CatalogError> {
    let bytes = std::fs::read(path)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))?;
    load_record_bytes(&bytes)
}

pub fn load_record_bytes(bytes: &[u8]) -> Result<ConnectorSourceRecord, CatalogError> {
    let record: ConnectorSourceRecord = serde_yaml::from_slice(bytes)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))?;
    validate_record(&record)?;
    Ok(record)
}

pub fn canonical_yaml(record: &ConnectorSourceRecord) -> Result<Vec<u8>, CatalogError> {
    serde_yaml::to_string(record)
        .map(String::into_bytes)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))
}

fn validate_record(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    if record.record_version == 0
        || record.proposed_destinations.is_empty()
        || record.red_tests.is_empty()
        || record.reviewer.is_empty()
        || !valid_date(&record.approval_date)
    {
        return incomplete("required record identity/review fields");
    }
    validate_license(&record.license)?;
    if !valid_path(&record.notice.license_file_path)
        || !valid_hash(&record.notice.license_file_sha256, 64)
        || !valid_path(&record.notice.notice_bundle_destination)
    {
        return incomplete("notice identity");
    }
    for path in record
        .entrypoints
        .iter()
        .chain(record.proposed_destinations.iter())
        .chain(record.proposed_manifest.iter())
    {
        if !valid_path(path) {
            return incomplete("repository-relative path");
        }
    }
    for artifact in &record.artifact_hashes {
        let width = match artifact.algorithm {
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::Sha512 => 128,
        };
        if artifact.artifact_id.is_empty() || !valid_hash(&artifact.digest, width) {
            return incomplete("artifact hash");
        }
    }
    match (&record.subject, record.reacquisition) {
        (SourceSubject::ExactNpm(package), ReacquisitionPlan::ExactNpmReview) => {
            validate_npm(package)?;
            if matches!(record.admission, AdmissionState::EvidenceAccepted { .. }) {
                return incomplete("npm source cannot be evidence-only");
            }
        }
        (
            SourceSubject::ProviderArtifact(provider),
            ReacquisitionPlan::ProviderRepositoryReview
            | ReacquisitionPlan::ProviderVersionedArtifactReview,
        ) => {
            validate_provider(record, provider)?;
            if !matches!(
                record.admission,
                AdmissionState::InventoryOnly { .. } | AdmissionState::EvidenceAccepted { .. }
            ) {
                return incomplete("provider evidence cannot approve an operation port");
            }
        }
        (SourceSubject::DonatOwned(source), ReacquisitionPlan::DonatOwnedNoNetwork) => {
            if !valid_git(&source.repository_commit) || source.files.is_empty() {
                return incomplete("Donat-owned source identity");
            }
            for file in &source.files {
                if !valid_path(&file.path) || !valid_hash(&file.sha256, 64) {
                    return incomplete("Donat-owned file");
                }
            }
            if matches!(record.admission, AdmissionState::EvidenceAccepted { .. }) {
                return incomplete("Donat-owned source cannot substitute provider evidence");
            }
        }
        _ => return incomplete("reacquisition plan does not match source subject"),
    }
    Ok(())
}

fn validate_npm(package: &ExactNpmPackage) -> Result<(), CatalogError> {
    if package.name.is_empty()
        || !valid_https(&package.tarball_url)
        || !valid_https(&package.repository.url)
        || package.package_repository != package.repository.url
        || package.npm_git_head != package.repository.commit
        || !valid_git(&package.repository.commit)
        || !valid_git(&package.repository.tree)
        || package.integrity.algorithm != "sha512"
    {
        return incomplete("exact npm identity");
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&package.integrity.digest)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(&package.integrity.digest)
        })
        .map_err(|_| CatalogError::new("source_record_incomplete", "invalid npm SRI digest"))?;
    if decoded.len() != 64 {
        return incomplete("npm SHA-512 digest");
    }
    match &package.signature {
        NpmSignatureDecision::Verified {
            signatures,
            registry_metadata_sha256,
        } => {
            if signatures.is_empty() || !valid_hash(registry_metadata_sha256, 64) {
                return incomplete("npm signature decision");
            }
            for signature in signatures {
                if signature.key_id.is_empty() || !valid_hash(&signature.signature_sha256, 64) {
                    return incomplete("npm signature");
                }
            }
        }
        NpmSignatureDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } if !valid_hash(registry_metadata_sha256, 64) => {
            return incomplete("npm absent-signature evidence");
        }
        NpmSignatureDecision::Rejected { finding } if finding.is_empty() => {
            return incomplete("npm rejected signature");
        }
        _ => {}
    }
    match &package.provenance {
        NpmProvenanceDecision::Verified {
            statement_sha256,
            source_commit,
        } => {
            if !valid_hash(statement_sha256, 64)
                || package.provenance_commit.as_ref() != Some(source_commit)
            {
                return incomplete("npm provenance commit");
            }
        }
        NpmProvenanceDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } => {
            if !valid_hash(registry_metadata_sha256, 64) {
                return incomplete("npm absent-provenance evidence");
            }
            if let NpmSignatureDecision::Verified {
                registry_metadata_sha256: signature_metadata,
                ..
            }
            | NpmSignatureDecision::VerifiedAbsent {
                registry_metadata_sha256: signature_metadata,
            } = &package.signature
                && signature_metadata != registry_metadata_sha256
            {
                return incomplete("registry metadata decisions disagree");
            }
        }
        NpmProvenanceDecision::Rejected { finding } if finding.is_empty() => {
            return incomplete("npm rejected provenance");
        }
        _ => {}
    }
    if package.maintainers.is_empty() {
        return incomplete("reviewed npm maintainer set");
    }
    Ok(())
}

fn validate_provider(
    record: &ConnectorSourceRecord,
    provider: &ExactProviderArtifact,
) -> Result<(), CatalogError> {
    if provider.provider.is_empty() || provider.evidence.is_empty() {
        return incomplete("provider artifact evidence");
    }
    for evidence in &provider.evidence {
        if !valid_date(&evidence.accessed_on)
            || !valid_hash(&evidence.content_sha256, 64)
            || evidence.facts.is_empty()
        {
            return incomplete("provider evidence");
        }
        match &evidence.source {
            ImmutableProviderEvidenceSource::RepositoryFile {
                repository,
                commit,
                path,
            } if valid_https(repository) && valid_git(commit) && valid_path(path) => {}
            ImmutableProviderEvidenceSource::VersionedArtifact {
                url,
                provider_revision,
            } if valid_https(url) && !provider_revision.is_empty() => {}
            _ => return incomplete("immutable provider source"),
        }
    }
    for contract in &record.provider_contracts {
        if contract.facts.is_empty() {
            return incomplete("provider contract facts");
        }
        for fact in &contract.facts {
            if let ContractFact::ProviderEvidence {
                source_record_id,
                fact_id,
            } = fact
                && (source_record_id != &record.record_id
                    || !provider
                        .evidence
                        .iter()
                        .flat_map(|evidence| &evidence.facts)
                        .any(|fact| &fact.fact_id == fact_id))
            {
                return incomplete("provider contract fact reference");
            }
        }
    }
    Ok(())
}

fn validate_license(license: &LicenseDecision) -> Result<(), CatalogError> {
    match license {
        LicenseDecision::Permissive {
            spdx_id,
            selected_dual_license_branch,
            license_file_path,
            license_file_sha256,
        } => {
            const ALLOWED: &[&str] = &[
                "MIT",
                "Apache-2.0",
                "BSD-2-Clause",
                "BSD-3-Clause",
                "ISC",
                "0BSD",
            ];
            if !ALLOWED.contains(&spdx_id.as_str())
                || selected_dual_license_branch
                    .as_ref()
                    .is_some_and(|branch| !ALLOWED.contains(&branch.as_str()))
                || !valid_path(license_file_path)
                || !valid_hash(license_file_sha256, 64)
            {
                return incomplete("permissive license decision");
            }
        }
        LicenseDecision::WrittenGrant {
            decision_id,
            grant_sha256,
        } if decision_id.is_empty() || !valid_hash(grant_sha256, 64) => {
            return incomplete("written grant");
        }
        LicenseDecision::Rejected { finding } if finding.is_empty() => {
            return incomplete("rejected license");
        }
        _ => {}
    }
    Ok(())
}

fn valid_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn valid_hash(value: &str, width: usize) -> bool {
    value.len() == width
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_git(value: &str) -> bool {
    valid_hash(value, 40)
}

fn valid_https(value: &str) -> bool {
    value.starts_with("https://") && value.len() > "https://".len()
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.contains('\0')
        && value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn incomplete<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new("source_record_incomplete", detail))
}
