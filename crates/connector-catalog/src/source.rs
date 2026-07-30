use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use base64::Engine;
use donat_connector_abi::{InlineId, OperationId};
use donat_value_contract::{BoundedInlineBytes, CanonicalNumber, TypedValue};
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
                    .map_err(|_| CatalogError::new("source_record_invalid_primitive", value))
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

macro_rules! checked_string {
    ($name:ident, $validator:ident, $expectation:literal) => {
        #[repr(transparent)]
        #[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, CatalogError> {
                if $validator(value) {
                    Ok(Self(value.to_owned()))
                } else {
                    Err(CatalogError::new(
                        "source_record_invalid_primitive",
                        $expectation,
                    ))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl core::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
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

checked_string!(ArtifactId, valid_id, "invalid artifact ID");
checked_string!(Date, valid_date, "invalid Gregorian date");
checked_string!(ExactHttpsUrl, valid_https, "invalid absolute HTTPS URL");
checked_string!(FindingId, valid_id, "invalid finding ID");
checked_string!(GitCommit, valid_git, "invalid Git commit");
checked_string!(GitTree, valid_git, "invalid Git tree");
checked_string!(Hash256, valid_hash256, "invalid SHA-256");
checked_string!(
    NonEmptyString,
    valid_nonempty_string,
    "invalid nonempty string"
);
checked_string!(
    NpmMaintainerIdentity,
    valid_id,
    "invalid npm maintainer identity"
);
checked_string!(NpmOwnerIdentity, valid_id, "invalid npm owner identity");
checked_string!(RepoPath, valid_path, "invalid repository path");
checked_string!(
    RepositoryOwnerIdentity,
    valid_id,
    "invalid repository owner identity"
);
checked_string!(RepositoryUrl, valid_https, "invalid HTTPS repository URL");
checked_string!(ReviewDecisionId, valid_id, "invalid review decision ID");
checked_string!(ReviewIdentity, valid_id, "invalid review identity");
checked_string!(SourcePath, valid_path, "invalid source path");
checked_string!(TestId, valid_id, "invalid RED-test ID");

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExactSemver(String);

impl ExactSemver {
    pub fn try_new(value: &str) -> Result<Self, CatalogError> {
        if valid_exact_semver(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CatalogError::new(
                "source_record_invalid_primitive",
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

/// A source record admitted by the strict byte loader.
///
/// Raw normative fields are read-only outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::ConnectorSourceRecord;
///
/// fn forge(record: &mut ConnectorSourceRecord) {
///     record.record_version = 0;
/// }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSourceRecord {
    pub(crate) record_version: u32,
    pub(crate) record_id: SourceRecordId,
    pub(crate) subject: SourceSubject,
    pub(crate) reacquisition: ReacquisitionPlan,
    pub(crate) artifact_hashes: Vec<ArtifactHash>,
    pub(crate) license: LicenseDecision,
    pub(crate) notice: NoticeIdentity,
    pub(crate) entrypoints: Vec<SourcePath>,
    pub(crate) dependencies: Vec<DependencyDecision>,
    pub(crate) embedded_material: Vec<EmbeddedMaterialDecision>,
    pub(crate) provider_contracts: Vec<ProviderContractReference>,
    pub(crate) compatibility: CompatibilityDecision,
    pub(crate) admission: AdmissionState,
    pub(crate) safety_findings: SafetyFindings,
    pub(crate) reviewer: ReviewIdentity,
    pub(crate) approval_date: Date,
    pub(crate) proposed_manifest: Option<RepoPath>,
    pub(crate) proposed_destinations: Vec<RepoPath>,
    pub(crate) red_tests: Vec<TestId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[allow(clippy::large_enum_variant)]
pub enum SourceSubject {
    ExactNpm(ExactNpmPackage),
    ProviderArtifact(ExactProviderArtifact),
    DonatOwned(DonatOwnedSource),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum ReacquisitionPlan {
    ExactNpmReview,
    ProviderRepositoryReview,
    ProviderVersionedArtifactReview,
    DonatOwnedNoNetwork,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Exact npm source identity admitted by the strict byte loader.
///
/// Nested identity fields are read-only outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::ExactNpmPackage;
///
/// fn forge(package: &mut ExactNpmPackage) {
///     package.name = "other-package".to_owned();
/// }
/// ```
pub struct ExactNpmPackage {
    pub(crate) name: String,
    pub(crate) version: ExactSemver,
    pub(crate) tarball_url: ExactHttpsUrl,
    pub(crate) integrity: NpmIntegrity,
    pub(crate) repository: ImmutableRepository,
    pub(crate) npm_git_head: GitCommit,
    pub(crate) package_repository: RepositoryUrl,
    pub(crate) signature: NpmSignatureDecision,
    pub(crate) provenance: NpmProvenanceDecision,
    pub(crate) tag_commit: Option<GitCommit>,
    pub(crate) provenance_commit: Option<GitCommit>,
    pub(crate) maintainers: Vec<NpmMaintainerIdentity>,
    pub(crate) repository_owner: RepositoryOwnerDecision,
}

#[derive(Clone, Eq, PartialEq)]
pub struct NpmIntegrity {
    algorithm: NpmIntegrityAlgorithm,
    digest: [u8; 64],
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum NpmIntegrityAlgorithm {
    Sha512,
}

impl NpmIntegrity {
    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        let encoded = value.strip_prefix("sha512-").ok_or_else(|| {
            CatalogError::new(
                "source_record_npm_integrity_invalid",
                "npm SRI must use the sha512- prefix",
            )
        })?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                CatalogError::new(
                    "source_record_npm_integrity_invalid",
                    "npm SRI must use padded standard base64",
                )
            })?;
        let digest: [u8; 64] = decoded.try_into().map_err(|_| {
            CatalogError::new(
                "source_record_npm_integrity_invalid",
                "npm SRI SHA-512 digest must be exactly 64 bytes",
            )
        })?;
        let canonical = base64::engine::general_purpose::STANDARD.encode(digest);
        if encoded != canonical {
            return Err(CatalogError::new(
                "source_record_npm_integrity_invalid",
                "npm SRI spelling is not canonical",
            ));
        }
        Ok(Self {
            algorithm: NpmIntegrityAlgorithm::Sha512,
            digest,
        })
    }

    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.digest
    }

    pub(crate) const fn algorithm(&self) -> NpmIntegrityAlgorithm {
        self.algorithm
    }

    pub fn sri(&self) -> String {
        format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(self.digest)
        )
    }
}

impl fmt::Debug for NpmIntegrity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NpmIntegrity")
            .field(&self.sri())
            .finish()
    }
}

impl Serialize for NpmIntegrity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.sri())
    }
}

impl<'de> Deserialize<'de> for NpmIntegrity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableRepository {
    pub(crate) url: RepositoryUrl,
    pub(crate) commit: GitCommit,
    pub(crate) tree: GitTree,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum NpmSignatureDecision {
    #[non_exhaustive]
    Verified {
        signatures: Vec<VerifiedNpmSignature>,
        registry_metadata_sha256: Hash256,
    },
    #[non_exhaustive]
    VerifiedAbsent { registry_metadata_sha256: Hash256 },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum NpmProvenanceDecision {
    #[non_exhaustive]
    Verified {
        statement_sha256: Hash256,
        source_commit: GitCommit,
    },
    #[non_exhaustive]
    VerifiedAbsent { registry_metadata_sha256: Hash256 },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum RepositoryOwnerDecision {
    #[non_exhaustive]
    Consistent {
        package_owner: NpmOwnerIdentity,
        repository_owner: RepositoryOwnerIdentity,
    },
    #[non_exhaustive]
    ReviewedMismatch { decision_id: ReviewDecisionId },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedNpmSignature {
    pub(crate) key_id: String,
    pub(crate) signature_sha256: Hash256,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Exact provider evidence admitted by the strict byte loader.
///
/// Provider identity and evidence members are read-only outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::ExactProviderArtifact;
///
/// fn forge(provider: &mut ExactProviderArtifact) {
///     provider.provider = "other-provider".to_owned();
/// }
/// ```
pub struct ExactProviderArtifact {
    pub(crate) provider: String,
    pub(crate) evidence: Vec<ProviderEvidenceArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEvidenceArtifact {
    pub(crate) source: ImmutableProviderEvidenceSource,
    pub(crate) accessed_on: Date,
    pub(crate) content_sha256: Hash256,
    pub(crate) terms: EvidenceTermsDisposition,
    pub(crate) facts: Vec<ProviderFact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum ImmutableProviderEvidenceSource {
    #[non_exhaustive]
    RepositoryFile {
        repository: RepositoryUrl,
        commit: GitCommit,
        path: SourcePath,
    },
    #[non_exhaustive]
    VersionedArtifact {
        url: ExactHttpsUrl,
        provider_revision: NonEmptyString,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum EvidenceTermsDisposition {
    #[non_exhaustive]
    Permissive {
        license: LicenseDecision,
        evidence_url: ExactHttpsUrl,
    },
    #[non_exhaustive]
    ReviewedUse {
        decision_id: ReviewDecisionId,
        evidence_url: ExactHttpsUrl,
    },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFact {
    pub(crate) fact_id: ProviderFactId,
    pub(crate) location: ExactFactLocation,
    #[serde(deserialize_with = "deserialize_typed_value_material")]
    pub(crate) normalized_value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum ExactFactLocation {
    #[non_exhaustive]
    JsonPointer { path: SourcePath, pointer: String },
    #[non_exhaustive]
    DocumentSection { path: SourcePath, section: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Closed typed-value projection material.
///
/// Use the checked constructors or projection builder; raw JSON
/// deserialization is intentionally unavailable.
///
/// ```compile_fail
/// use donat_connector_catalog::TypedValueMaterialV1;
/// let _: TypedValueMaterialV1 =
///     serde_json::from_str(r#"{"kind":"i64","value":"not-an-integer"}"#).unwrap();
/// ```
pub struct TypedValueMaterialV1(TypedValueMaterial);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
enum TypedValueMaterial {
    Null,
    Boolean(bool),
    String(String),
    I64(String),
    U64(String),
    Decimal(String),
    List(Vec<TypedValueMaterial>),
    Object(BTreeMap<String, TypedValueMaterial>),
    InlineBytes {
        #[serde(rename = "$binary")]
        binary: String,
        file_name: Option<String>,
        media_type: Option<String>,
    },
}

impl TypedValueMaterialV1 {
    pub fn string(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = Self(TypedValueMaterial::String(value.into()));
        value.validate()?;
        Ok(value)
    }

    pub fn i64(value: &str) -> Result<Self, CatalogError> {
        let value = Self(TypedValueMaterial::I64(value.to_owned()));
        value.validate()?;
        Ok(value)
    }

    pub fn u64(value: &str) -> Result<Self, CatalogError> {
        let value = Self(TypedValueMaterial::U64(value.to_owned()));
        value.validate()?;
        Ok(value)
    }

    pub fn decimal(value: &str) -> Result<Self, CatalogError> {
        let value = Self(TypedValueMaterial::Decimal(value.to_owned()));
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_typed_value(value: &TypedValue) -> Self {
        fn convert(value: &TypedValue) -> TypedValueMaterial {
            match value {
                TypedValue::Null => TypedValueMaterial::Null,
                TypedValue::Boolean(value) => TypedValueMaterial::Boolean(*value),
                TypedValue::String(value) => TypedValueMaterial::String(value.clone()),
                TypedValue::Number(CanonicalNumber::I64(value)) => {
                    TypedValueMaterial::I64(value.to_string())
                }
                TypedValue::Number(CanonicalNumber::U64(value)) => {
                    TypedValueMaterial::U64(value.to_string())
                }
                TypedValue::Number(CanonicalNumber::Decimal(value)) => {
                    TypedValueMaterial::Decimal(value.as_str().to_owned())
                }
                TypedValue::List(values) => {
                    TypedValueMaterial::List(values.iter().map(convert).collect())
                }
                TypedValue::Object(values) => TypedValueMaterial::Object(
                    values
                        .iter()
                        .map(|(name, value)| (name.clone(), convert(value)))
                        .collect(),
                ),
                TypedValue::InlineBytes(value) => TypedValueMaterial::InlineBytes {
                    binary: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(value.as_slice()),
                    file_name: value.file_name().map(str::to_owned),
                    media_type: Some(value.media_type().to_owned()),
                },
            }
        }
        Self(convert(value))
    }

    pub(crate) fn to_typed_value(&self) -> Result<TypedValue, CatalogError> {
        fn convert(value: &TypedValueMaterial) -> Result<TypedValue, CatalogError> {
            Ok(match value {
                TypedValueMaterial::Null => TypedValue::Null,
                TypedValueMaterial::Boolean(value) => TypedValue::Boolean(*value),
                TypedValueMaterial::String(value) => TypedValue::String(value.clone()),
                TypedValueMaterial::I64(value) => TypedValue::Number(CanonicalNumber::I64(
                    value.parse().map_err(|_| material_schema_error())?,
                )),
                TypedValueMaterial::U64(value) => TypedValue::Number(CanonicalNumber::U64(
                    value.parse().map_err(|_| material_schema_error())?,
                )),
                TypedValueMaterial::Decimal(value) => TypedValue::Number(CanonicalNumber::Decimal(
                    donat_value_contract::CanonicalDecimal::try_new(value)
                        .map_err(|_| material_schema_error())?,
                )),
                TypedValueMaterial::List(values) => {
                    TypedValue::List(values.iter().map(convert).collect::<Result<_, _>>()?)
                }
                TypedValueMaterial::Object(values) => TypedValue::Object(
                    values
                        .iter()
                        .map(|(name, value)| Ok((name.clone(), convert(value)?)))
                        .collect::<Result<_, CatalogError>>()?,
                ),
                TypedValueMaterial::InlineBytes {
                    binary,
                    file_name,
                    media_type,
                } => {
                    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(binary)
                        .map_err(|_| material_schema_error())?;
                    let media_type = media_type.as_deref().ok_or_else(material_schema_error)?;
                    let maximum_decoded_bytes = decoded.len();
                    TypedValue::InlineBytes(
                        BoundedInlineBytes::try_new(
                            decoded,
                            media_type,
                            file_name.as_deref(),
                            maximum_decoded_bytes,
                        )
                        .map_err(|_| material_schema_error())?,
                    )
                }
            })
        }
        self.validate()?;
        convert(&self.0)
    }

    fn validate(&self) -> Result<(), CatalogError> {
        let mut pending = vec![&self.0];
        while let Some(value) = pending.pop() {
            match value {
                TypedValueMaterial::Null | TypedValueMaterial::Boolean(_) => {}
                TypedValueMaterial::String(value) => validate_unicode_scalar_string(value)?,
                TypedValueMaterial::I64(value) => {
                    let parsed = value.parse::<i64>().map_err(|_| material_schema_error())?;
                    if parsed.to_string() != *value {
                        return Err(material_schema_error());
                    }
                }
                TypedValueMaterial::U64(value) => {
                    let parsed = value.parse::<u64>().map_err(|_| material_schema_error())?;
                    if parsed.to_string() != *value {
                        return Err(material_schema_error());
                    }
                }
                TypedValueMaterial::Decimal(value) => {
                    donat_value_contract::CanonicalDecimal::try_new(value)
                        .map_err(|_| material_schema_error())?;
                }
                TypedValueMaterial::List(values) => pending.extend(values),
                TypedValueMaterial::Object(values) => {
                    for (name, value) in values {
                        validate_unicode_scalar_string(name)?;
                        pending.push(value);
                    }
                }
                TypedValueMaterial::InlineBytes {
                    binary,
                    file_name,
                    media_type,
                } => {
                    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(binary)
                        .map_err(|_| material_schema_error())?;
                    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&decoded) != *binary
                    {
                        return Err(material_schema_error());
                    }
                    let media_type = media_type.as_deref().ok_or_else(material_schema_error)?;
                    BoundedInlineBytes::try_new(decoded, media_type, file_name.as_deref(), 131_072)
                        .map_err(|_| material_schema_error())?;
                }
            }
        }
        Ok(())
    }
}

impl Serialize for TypedValueMaterialV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

pub(crate) fn deserialize_typed_value_material<'de, D>(
    deserializer: D,
) -> Result<TypedValueMaterialV1, D::Error>
where
    D: Deserializer<'de>,
{
    let value = TypedValueMaterialV1(TypedValueMaterial::deserialize(deserializer)?);
    value.validate().map_err(serde::de::Error::custom)?;
    Ok(value)
}

pub(crate) fn deserialize_optional_typed_value_material<'de, D>(
    deserializer: D,
) -> Result<Option<TypedValueMaterialV1>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<TypedValueMaterial>::deserialize(deserializer)?
        .map(|value| {
            let value = TypedValueMaterialV1(value);
            value.validate().map_err(serde::de::Error::custom)?;
            Ok(value)
        })
        .transpose()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum ContractFact {
    ProviderEvidence {
        source_record_id: SourceRecordId,
        fact_id: ProviderFactId,
    },
    DonatPolicy {
        policy_id: DonatPolicyId,
        #[serde(deserialize_with = "deserialize_typed_value_material")]
        value: TypedValueMaterialV1,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContractReference {
    pub(crate) contract_id: ProviderContractId,
    pub(crate) facts: Vec<ContractFact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DonatOwnedSource {
    pub(crate) repository_commit: GitCommit,
    pub(crate) files: Vec<RepoFileHash>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFileHash {
    pub(crate) path: RepoPath,
    pub(crate) sha256: Hash256,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum CompatibilityDecision {
    TierA,
    TierB,
    TierC,
    Rejected,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
/// Read-only admission decision produced by strict source validation.
///
/// Variant payloads cannot be forged outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::AdmissionState;
///
/// let _ = AdmissionState::InventoryOnly {
///     findings: Vec::new(),
/// };
/// ```
pub enum AdmissionState {
    #[non_exhaustive]
    InventoryOnly { findings: Vec<FindingId> },
    #[non_exhaustive]
    ApprovedForPort {
        #[serde(with = "operation_ids")]
        operations: Vec<OperationId>,
    },
    #[non_exhaustive]
    EvidenceAccepted { contracts: Vec<ProviderContractId> },
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
/// Exact artifact identity admitted by the strict byte loader or checked builder.
///
/// Digest and path members are read-only outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::ArtifactHash;
///
/// fn forge(artifact: &mut ArtifactHash) {
///     artifact.digest = "not-a-hash".to_owned();
/// }
/// ```
pub struct ArtifactHash {
    pub(crate) artifact_id: ArtifactId,
    pub(crate) algorithm: HashAlgorithm,
    pub(crate) digest: String,
    pub(crate) path: Option<SourcePath>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum HashAlgorithm {
    Sha256,
    Sha512,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
/// Read-only legal disposition produced by strict source validation.
///
/// Variant payloads cannot be forged outside this crate.
///
/// ```compile_fail
/// use donat_connector_catalog::{FindingId, LicenseDecision};
///
/// let _ = LicenseDecision::Rejected {
///     finding: FindingId::literal("finding.unreviewed"),
/// };
/// ```
pub enum LicenseDecision {
    #[non_exhaustive]
    Permissive {
        spdx_id: String,
        selected_dual_license_branch: Option<String>,
        license_file_path: SourcePath,
        license_file_sha256: Hash256,
    },
    #[non_exhaustive]
    WrittenGrant {
        decision_id: ReviewDecisionId,
        grant_sha256: Hash256,
    },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeIdentity {
    pub(crate) id: NoticeId,
    pub(crate) license_file_path: SourcePath,
    pub(crate) license_file_sha256: Hash256,
    pub(crate) required_copyright_lines: Vec<String>,
    pub(crate) notice_bundle_destination: RepoPath,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyDecision {
    pub(crate) dependency: String,
    pub(crate) disposition: DependencyDisposition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum DependencyDisposition {
    #[non_exhaustive]
    Shipped { license: LicenseDecision },
    #[non_exhaustive]
    BuildOnly { license: LicenseDecision },
    #[non_exhaustive]
    TypeOnlyReplaced { replacement: String },
    #[non_exhaustive]
    BehaviorOnly { reason: FindingId },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedMaterialDecision {
    pub(crate) material_id: String,
    pub(crate) path: SourcePath,
    pub(crate) sha256: Hash256,
    pub(crate) disposition: EmbeddedMaterialDisposition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
pub enum EmbeddedMaterialDisposition {
    #[non_exhaustive]
    Shipped { license: LicenseDecision },
    #[non_exhaustive]
    BehaviorOnly { reason: FindingId },
    #[non_exhaustive]
    Rejected { finding: FindingId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFindings {
    pub(crate) findings: Vec<SafetyFinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFinding {
    pub(crate) finding_id: FindingId,
    pub(crate) kind: String,
    pub(crate) location: Option<SourcePath>,
    pub(crate) message: String,
}

impl ConnectorSourceRecord {
    pub const fn record_version(&self) -> u32 {
        self.record_version
    }

    pub const fn record_id(&self) -> SourceRecordId {
        self.record_id
    }

    pub const fn subject(&self) -> &SourceSubject {
        &self.subject
    }

    pub const fn reacquisition(&self) -> ReacquisitionPlan {
        self.reacquisition
    }

    pub fn artifact_hashes(&self) -> &[ArtifactHash] {
        &self.artifact_hashes
    }

    pub const fn license(&self) -> &LicenseDecision {
        &self.license
    }

    pub const fn notice(&self) -> &NoticeIdentity {
        &self.notice
    }

    pub fn entrypoints(&self) -> &[SourcePath] {
        &self.entrypoints
    }

    pub fn dependencies(&self) -> &[DependencyDecision] {
        &self.dependencies
    }

    pub fn embedded_material(&self) -> &[EmbeddedMaterialDecision] {
        &self.embedded_material
    }

    pub fn provider_contracts(&self) -> &[ProviderContractReference] {
        &self.provider_contracts
    }

    pub const fn compatibility(&self) -> CompatibilityDecision {
        self.compatibility
    }

    pub const fn admission(&self) -> &AdmissionState {
        &self.admission
    }

    pub const fn safety_findings(&self) -> &SafetyFindings {
        &self.safety_findings
    }

    pub const fn reviewer(&self) -> &ReviewIdentity {
        &self.reviewer
    }

    pub const fn approval_date(&self) -> &Date {
        &self.approval_date
    }

    pub const fn proposed_manifest(&self) -> Option<&RepoPath> {
        self.proposed_manifest.as_ref()
    }

    pub fn proposed_destinations(&self) -> &[RepoPath] {
        &self.proposed_destinations
    }

    pub fn red_tests(&self) -> &[TestId] {
        &self.red_tests
    }
}

impl ExactNpmPackage {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn version(&self) -> &ExactSemver {
        &self.version
    }

    pub const fn tarball_url(&self) -> &ExactHttpsUrl {
        &self.tarball_url
    }

    pub const fn integrity(&self) -> &NpmIntegrity {
        &self.integrity
    }

    pub const fn repository(&self) -> &ImmutableRepository {
        &self.repository
    }

    pub const fn npm_git_head(&self) -> &GitCommit {
        &self.npm_git_head
    }

    pub const fn package_repository(&self) -> &RepositoryUrl {
        &self.package_repository
    }

    pub const fn signature(&self) -> &NpmSignatureDecision {
        &self.signature
    }

    pub const fn provenance(&self) -> &NpmProvenanceDecision {
        &self.provenance
    }

    pub const fn tag_commit(&self) -> Option<&GitCommit> {
        self.tag_commit.as_ref()
    }

    pub const fn provenance_commit(&self) -> Option<&GitCommit> {
        self.provenance_commit.as_ref()
    }

    pub fn maintainers(&self) -> &[NpmMaintainerIdentity] {
        &self.maintainers
    }

    pub const fn repository_owner(&self) -> &RepositoryOwnerDecision {
        &self.repository_owner
    }
}

impl ImmutableRepository {
    pub const fn url(&self) -> &RepositoryUrl {
        &self.url
    }

    pub const fn commit(&self) -> &GitCommit {
        &self.commit
    }

    pub const fn tree(&self) -> &GitTree {
        &self.tree
    }
}

impl VerifiedNpmSignature {
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub const fn signature_sha256(&self) -> &Hash256 {
        &self.signature_sha256
    }
}

impl ExactProviderArtifact {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn evidence(&self) -> &[ProviderEvidenceArtifact] {
        &self.evidence
    }
}

impl ProviderEvidenceArtifact {
    pub const fn source(&self) -> &ImmutableProviderEvidenceSource {
        &self.source
    }

    pub const fn accessed_on(&self) -> &Date {
        &self.accessed_on
    }

    pub const fn content_sha256(&self) -> &Hash256 {
        &self.content_sha256
    }

    pub const fn terms(&self) -> &EvidenceTermsDisposition {
        &self.terms
    }

    pub fn facts(&self) -> &[ProviderFact] {
        &self.facts
    }
}

impl ProviderFact {
    pub const fn fact_id(&self) -> ProviderFactId {
        self.fact_id
    }

    pub const fn location(&self) -> &ExactFactLocation {
        &self.location
    }

    pub const fn normalized_value(&self) -> &TypedValueMaterialV1 {
        &self.normalized_value
    }
}

impl ProviderContractReference {
    pub const fn contract_id(&self) -> ProviderContractId {
        self.contract_id
    }

    pub fn facts(&self) -> &[ContractFact] {
        &self.facts
    }
}

impl DonatOwnedSource {
    pub const fn repository_commit(&self) -> &GitCommit {
        &self.repository_commit
    }

    pub fn files(&self) -> &[RepoFileHash] {
        &self.files
    }
}

impl RepoFileHash {
    pub const fn path(&self) -> &RepoPath {
        &self.path
    }

    pub const fn sha256(&self) -> &Hash256 {
        &self.sha256
    }
}

impl ArtifactHash {
    pub fn try_new(
        artifact_id: &str,
        algorithm: HashAlgorithm,
        digest: &str,
        path: Option<&str>,
    ) -> Result<Self, CatalogError> {
        let width = match algorithm {
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::Sha512 => 128,
        };
        if !valid_hash(digest, width) {
            return invalid_primitive("artifact digest");
        }
        Ok(Self {
            artifact_id: ArtifactId::parse(artifact_id)?,
            algorithm,
            digest: digest.to_owned(),
            path: path.map(SourcePath::parse).transpose()?,
        })
    }

    pub const fn artifact_id(&self) -> &ArtifactId {
        &self.artifact_id
    }

    pub const fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub const fn path(&self) -> Option<&SourcePath> {
        self.path.as_ref()
    }
}

impl NoticeIdentity {
    pub const fn id(&self) -> NoticeId {
        self.id
    }

    pub const fn license_file_path(&self) -> &SourcePath {
        &self.license_file_path
    }

    pub const fn license_file_sha256(&self) -> &Hash256 {
        &self.license_file_sha256
    }

    pub fn required_copyright_lines(&self) -> &[String] {
        &self.required_copyright_lines
    }

    pub const fn notice_bundle_destination(&self) -> &RepoPath {
        &self.notice_bundle_destination
    }
}

impl DependencyDecision {
    pub fn dependency(&self) -> &str {
        &self.dependency
    }

    pub const fn disposition(&self) -> &DependencyDisposition {
        &self.disposition
    }
}

impl EmbeddedMaterialDecision {
    pub fn material_id(&self) -> &str {
        &self.material_id
    }

    pub const fn path(&self) -> &SourcePath {
        &self.path
    }

    pub const fn sha256(&self) -> &Hash256 {
        &self.sha256
    }

    pub const fn disposition(&self) -> &EmbeddedMaterialDisposition {
        &self.disposition
    }
}

impl SafetyFindings {
    pub fn findings(&self) -> &[SafetyFinding] {
        &self.findings
    }
}

impl SafetyFinding {
    pub const fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub const fn location(&self) -> Option<&SourcePath> {
        self.location.as_ref()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceReviewRegistry {
    written_grants: BTreeSet<String>,
    reviewed_uses: BTreeSet<String>,
}

impl SourceReviewRegistry {
    pub fn approve_written_grant(&mut self, decision_id: &str) -> Result<(), CatalogError> {
        if !valid_id(decision_id) {
            return invalid_primitive("reviewed written-grant decision ID");
        }
        self.written_grants.insert(decision_id.to_owned());
        Ok(())
    }

    pub fn approve_reviewed_use(&mut self, decision_id: &str) -> Result<(), CatalogError> {
        if !valid_id(decision_id) {
            return invalid_primitive("reviewed evidence-use decision ID");
        }
        self.reviewed_uses.insert(decision_id.to_owned());
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct AcceptedRecordCatalog {
    records: BTreeMap<SourceRecordId, ConnectorSourceRecord>,
}

#[derive(Clone, Copy)]
pub struct PortApprovedRecord<'catalog> {
    record: &'catalog ConnectorSourceRecord,
    operations: &'catalog [OperationId],
}

impl fmt::Debug for PortApprovedRecord<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PortApprovedRecord")
            .field("record_id", &self.record.record_id)
            .field(
                "operations",
                &self
                    .operations
                    .iter()
                    .map(OperationId::as_str)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl PortApprovedRecord<'_> {
    pub const fn record(&self) -> &ConnectorSourceRecord {
        self.record
    }

    pub const fn operations(&self) -> &[OperationId] {
        self.operations
    }

    pub fn authorizes(&self, operation: OperationId) -> bool {
        self.operations.contains(&operation)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EvidenceAcceptedRecord<'catalog> {
    record: &'catalog ConnectorSourceRecord,
    contracts: &'catalog [ProviderContractId],
}

impl EvidenceAcceptedRecord<'_> {
    pub const fn record(&self) -> &ConnectorSourceRecord {
        self.record
    }

    pub const fn contracts(&self) -> &[ProviderContractId] {
        self.contracts
    }
}

impl AcceptedRecordCatalog {
    pub fn build(
        records: Vec<ConnectorSourceRecord>,
        operation_closures: &BTreeMap<SourceRecordId, BTreeSet<OperationId>>,
        reviews: &SourceReviewRegistry,
    ) -> Result<Self, CatalogError> {
        let mut indexed = BTreeMap::new();
        for record in records {
            validate_record(&record)?;
            validate_reviewed_decisions(&record, reviews)?;
            if indexed.insert(record.record_id, record).is_some() {
                return duplicate("source record ID");
            }
        }

        for (record_id, operations) in operation_closures {
            let record = indexed.get(record_id).ok_or_else(|| {
                CatalogError::new(
                    "catalog_fact_origin_unresolved",
                    "operation closure names an unknown source record",
                )
            })?;
            let AdmissionState::ApprovedForPort {
                operations: admitted,
            } = &record.admission
            else {
                if !operations.is_empty() {
                    return Err(CatalogError::new(
                        "catalog_source_not_executable",
                        "non-port admission cannot authorize an operation",
                    ));
                }
                continue;
            };
            let admitted = admitted.iter().copied().collect::<BTreeSet<_>>();
            if admitted != *operations {
                return admission_mismatch("approved operation closure differs from compilation");
            }
        }
        for record in indexed.values() {
            if let AdmissionState::ApprovedForPort { operations } = &record.admission {
                let compiled = operation_closures.get(&record.record_id).ok_or_else(|| {
                    CatalogError::new(
                        "source_record_admission_mismatch",
                        "approved source has no compiled operation closure",
                    )
                })?;
                if operations.iter().copied().collect::<BTreeSet<_>>() != *compiled {
                    return admission_mismatch(
                        "compiled operation closure omits an approved operation",
                    );
                }
            }
        }
        Ok(Self { records: indexed })
    }

    pub fn port_approved(
        &self,
        record_id: SourceRecordId,
    ) -> Result<PortApprovedRecord<'_>, CatalogError> {
        let record = self.records.get(&record_id).ok_or_else(|| {
            CatalogError::new(
                "catalog_fact_origin_unresolved",
                "source record does not resolve",
            )
        })?;
        let AdmissionState::ApprovedForPort { operations } = &record.admission else {
            return Err(CatalogError::new(
                "catalog_source_not_executable",
                "source record is not approved for operation porting",
            ));
        };
        Ok(PortApprovedRecord { record, operations })
    }

    pub fn evidence_accepted(
        &self,
        record_id: SourceRecordId,
    ) -> Result<EvidenceAcceptedRecord<'_>, CatalogError> {
        let record = self.records.get(&record_id).ok_or_else(|| {
            CatalogError::new(
                "catalog_fact_origin_unresolved",
                "source record does not resolve",
            )
        })?;
        let AdmissionState::EvidenceAccepted { contracts } = &record.admission else {
            return Err(CatalogError::new(
                "catalog_fact_origin_unresolved",
                "source record is not accepted provider evidence",
            ));
        };
        Ok(EvidenceAcceptedRecord { record, contracts })
    }

    pub(crate) fn capability_record(
        &self,
        record_id: SourceRecordId,
    ) -> Option<&ConnectorSourceRecord> {
        let record = self.records.get(&record_id)?;
        matches!(
            record.admission,
            AdmissionState::ApprovedForPort { .. } | AdmissionState::EvidenceAccepted { .. }
        )
        .then_some(record)
    }
}

pub fn load_record(path: impl AsRef<Path>) -> Result<ConnectorSourceRecord, CatalogError> {
    let bytes = std::fs::read(path)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))?;
    load_record_bytes(&bytes)
}

pub fn load_record_bytes(bytes: &[u8]) -> Result<ConnectorSourceRecord, CatalogError> {
    let parsed_value =
        serde_yaml::from_slice::<serde_yaml::Value>(bytes).map_err(map_source_decode_error)?;
    let record: ConnectorSourceRecord =
        serde_yaml::from_slice(bytes).map_err(map_source_decode_error)?;
    let rebuilt = serde_yaml::to_string(&record).map_err(map_source_decode_error)?;
    let mut rebuilt_value =
        serde_yaml::from_str::<serde_yaml::Value>(&rebuilt).map_err(map_source_decode_error)?;
    insert_tagged_unit_values(&mut rebuilt_value);
    if !same_yaml_shape(&parsed_value, &rebuilt_value) {
        return Err(CatalogError::new(
            "source_record_incomplete",
            "source record omitted or changed a required member",
        ));
    }
    validate_record(&record)?;
    Ok(record)
}

fn same_yaml_shape(left: &serde_yaml::Value, right: &serde_yaml::Value) -> bool {
    match (left, right) {
        (serde_yaml::Value::Null, serde_yaml::Value::Null)
        | (serde_yaml::Value::Bool(_), serde_yaml::Value::Bool(_))
        | (serde_yaml::Value::Number(_), serde_yaml::Value::Number(_))
        | (serde_yaml::Value::String(_), serde_yaml::Value::String(_)) => true,
        (serde_yaml::Value::Mapping(left), serde_yaml::Value::Mapping(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left_value)| {
                    right
                        .get(key)
                        .is_some_and(|right_value| same_yaml_shape(left_value, right_value))
                })
        }
        (serde_yaml::Value::Sequence(left), serde_yaml::Value::Sequence(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_yaml_shape(left, right))
        }
        (serde_yaml::Value::Tagged(left), serde_yaml::Value::Tagged(right)) => {
            left.tag == right.tag && same_yaml_shape(&left.value, &right.value)
        }
        (serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_), _)
        | (_, serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_)) => false,
        (serde_yaml::Value::Tagged(_), _) | (_, serde_yaml::Value::Tagged(_)) => false,
        _ => false,
    }
}

fn insert_tagged_unit_values(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::Mapping(values) => {
            let kind = serde_yaml::Value::String("kind".to_owned());
            let payload = serde_yaml::Value::String("value".to_owned());
            if values.len() == 1 && values.contains_key(&kind) {
                values.insert(payload, serde_yaml::Value::Null);
            }
            for value in values.values_mut() {
                insert_tagged_unit_values(value);
            }
        }
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                insert_tagged_unit_values(value);
            }
        }
        _ => {}
    }
}

pub fn canonical_yaml(record: &ConnectorSourceRecord) -> Result<Vec<u8>, CatalogError> {
    let mut value = serde_yaml::to_value(record)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))?;
    insert_tagged_unit_values(&mut value);
    serde_yaml::to_string(&value)
        .map(String::into_bytes)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))
}

fn validate_record(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    validate_required_structure(record)?;
    validate_record_primitives(record)?;
    validate_record_duplicates(record)?;
    validate_record_legal_state(record)?;
    validate_record_evidence(record)?;
    validate_record_admission(record)
}

fn map_source_decode_error(error: serde_yaml::Error) -> CatalogError {
    let detail = error.to_string();
    for code in [
        "source_record_duplicate",
        "source_record_invalid_primitive",
        "source_record_npm_integrity_invalid",
        "source_record_evidence_mismatch",
        "source_record_incomplete",
    ] {
        if detail.contains(code) {
            return CatalogError::new(code, detail);
        }
    }
    if detail.contains("duplicate field") || detail.contains("duplicate entry with key") {
        return CatalogError::new("source_record_duplicate", detail);
    }
    CatalogError::new("source_record_incomplete", detail)
}

fn validate_required_structure(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    if record.record_version == 0
        || record.entrypoints.is_empty()
        || record.proposed_destinations.is_empty()
        || record.red_tests.is_empty()
        || record.reviewer.is_empty()
    {
        return source_error(
            "source_record_incomplete",
            "required source-record collection or review identity is empty",
        );
    }
    match &record.subject {
        SourceSubject::ExactNpm(package) => {
            if package.name.is_empty() || package.maintainers.is_empty() {
                return source_error(
                    "source_record_incomplete",
                    "exact npm identity and maintainer inventory must be nonempty",
                );
            }
        }
        SourceSubject::ProviderArtifact(provider) => {
            if provider.provider.is_empty() {
                return source_error(
                    "source_record_incomplete",
                    "provider identity must be nonempty",
                );
            }
        }
        SourceSubject::DonatOwned(source) if source.files.is_empty() => {
            return source_error(
                "source_record_incomplete",
                "Donat-owned file inventory must be nonempty",
            );
        }
        SourceSubject::DonatOwned(_) => {}
    }
    Ok(())
}

fn validate_record_primitives(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    if !valid_id(record.reviewer.as_str()) || !valid_date(&record.approval_date) {
        return invalid_primitive("review identity or Gregorian approval date");
    }
    for path in &record.entrypoints {
        if !valid_path(path) {
            return invalid_primitive("repository-relative path");
        }
    }
    for path in record
        .proposed_destinations
        .iter()
        .chain(record.proposed_manifest.iter())
    {
        if !valid_path(path) {
            return invalid_primitive("repository-relative path");
        }
    }
    if !valid_path(&record.notice.license_file_path)
        || !valid_hash(&record.notice.license_file_sha256, 64)
        || !valid_path(&record.notice.notice_bundle_destination)
    {
        return invalid_primitive("notice identity");
    }
    for artifact in &record.artifact_hashes {
        let width = match artifact.algorithm {
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::Sha512 => 128,
        };
        if !valid_id(&artifact.artifact_id)
            || !valid_hash(&artifact.digest, width)
            || artifact.path.as_ref().is_some_and(|path| !valid_path(path))
        {
            return invalid_primitive("artifact identity");
        }
    }
    for dependency in &record.dependencies {
        if !valid_id(&dependency.dependency) {
            return invalid_primitive("dependency identity");
        }
        match &dependency.disposition {
            DependencyDisposition::Shipped { license }
            | DependencyDisposition::BuildOnly { license } => {
                validate_license_primitives(license)?;
            }
            DependencyDisposition::TypeOnlyReplaced { replacement } if !valid_id(replacement) => {
                return invalid_primitive("dependency replacement identity");
            }
            DependencyDisposition::BehaviorOnly { reason }
            | DependencyDisposition::Rejected { finding: reason }
                if !valid_id(reason) =>
            {
                return invalid_primitive("dependency finding identity");
            }
            _ => {}
        }
    }
    for embedded in &record.embedded_material {
        if !valid_id(&embedded.material_id)
            || !valid_path(&embedded.path)
            || !valid_hash(&embedded.sha256, 64)
        {
            return invalid_primitive("embedded material identity");
        }
        match &embedded.disposition {
            EmbeddedMaterialDisposition::Shipped { license } => {
                validate_license_primitives(license)?;
            }
            EmbeddedMaterialDisposition::BehaviorOnly { reason }
            | EmbeddedMaterialDisposition::Rejected { finding: reason }
                if !valid_id(reason) =>
            {
                return invalid_primitive("embedded material finding identity");
            }
            _ => {}
        }
    }
    for finding in &record.safety_findings.findings {
        if !valid_id(&finding.finding_id)
            || !valid_id(&finding.kind)
            || finding
                .location
                .as_ref()
                .is_some_and(|path| !valid_path(path))
            || finding.message.is_empty()
        {
            return invalid_primitive("safety finding");
        }
    }
    validate_license_primitives(&record.license)?;
    match &record.subject {
        SourceSubject::ExactNpm(package) => validate_npm_primitives(package),
        SourceSubject::ProviderArtifact(provider) => validate_provider_primitives(provider),
        SourceSubject::DonatOwned(source) => {
            if !valid_git(&source.repository_commit) {
                return invalid_primitive("Donat-owned repository commit");
            }
            for file in &source.files {
                if !valid_path(&file.path) || !valid_hash(&file.sha256, 64) {
                    return invalid_primitive("Donat-owned file");
                }
            }
            Ok(())
        }
    }
}

fn validate_npm_primitives(package: &ExactNpmPackage) -> Result<(), CatalogError> {
    if !valid_npm_name(&package.name)
        || !valid_https(&package.tarball_url)
        || !valid_https(&package.repository.url)
        || !valid_https(&package.package_repository)
        || !valid_git(&package.npm_git_head)
        || !valid_git(&package.repository.commit)
        || !valid_git(&package.repository.tree)
    {
        return invalid_primitive("exact npm identity");
    }
    match &package.signature {
        NpmSignatureDecision::Verified {
            signatures,
            registry_metadata_sha256,
        } => {
            if signatures.is_empty() || !valid_hash(registry_metadata_sha256, 64) {
                return invalid_primitive("npm signature decision");
            }
            for signature in signatures {
                if !valid_id(&signature.key_id) || !valid_hash(&signature.signature_sha256, 64) {
                    return invalid_primitive("npm signature");
                }
            }
        }
        NpmSignatureDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } if !valid_hash(registry_metadata_sha256, 64) => {
            return invalid_primitive("npm absent-signature evidence");
        }
        NpmSignatureDecision::Rejected { finding } if !valid_id(finding) => {
            return invalid_primitive("npm rejected signature");
        }
        _ => {}
    }
    match &package.provenance {
        NpmProvenanceDecision::Verified {
            statement_sha256,
            source_commit,
        } => {
            if !valid_hash(statement_sha256, 64) || !valid_git(source_commit) {
                return invalid_primitive("npm provenance");
            }
        }
        NpmProvenanceDecision::VerifiedAbsent {
            registry_metadata_sha256,
        } => {
            if !valid_hash(registry_metadata_sha256, 64) {
                return invalid_primitive("npm absent-provenance evidence");
            }
        }
        NpmProvenanceDecision::Rejected { finding } if !valid_id(finding) => {
            return invalid_primitive("npm rejected provenance");
        }
        _ => {}
    }
    if package
        .tag_commit
        .iter()
        .chain(package.provenance_commit.iter())
        .any(|commit| !valid_git(commit))
        || package
            .maintainers
            .iter()
            .any(|identity| !valid_id(identity))
    {
        return invalid_primitive("npm reviewed identity");
    }
    match &package.repository_owner {
        RepositoryOwnerDecision::Consistent {
            package_owner,
            repository_owner,
        } if !valid_id(package_owner) || !valid_id(repository_owner) => {
            return invalid_primitive("npm repository owner");
        }
        RepositoryOwnerDecision::ReviewedMismatch { decision_id } if !valid_id(decision_id) => {
            return invalid_primitive("npm repository owner decision");
        }
        RepositoryOwnerDecision::Rejected { finding } if !valid_id(finding) => {
            return invalid_primitive("npm repository owner finding");
        }
        _ => {}
    }
    Ok(())
}

fn validate_provider_primitives(provider: &ExactProviderArtifact) -> Result<(), CatalogError> {
    if !valid_id(&provider.provider) {
        return invalid_primitive("provider identity");
    }
    for evidence in &provider.evidence {
        if !valid_date(&evidence.accessed_on) || !valid_hash(&evidence.content_sha256, 64) {
            return invalid_primitive("provider evidence");
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
            _ => return invalid_primitive("immutable provider source"),
        }
        for fact in &evidence.facts {
            match &fact.location {
                ExactFactLocation::JsonPointer { path, pointer }
                    if valid_path(path) && valid_json_pointer(pointer) => {}
                ExactFactLocation::DocumentSection { path, section }
                    if valid_path(path) && !section.is_empty() => {}
                _ => return invalid_primitive("provider fact location"),
            }
        }
    }
    Ok(())
}

fn validate_record_duplicates(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    unique(record.entrypoints.iter().map(SourcePath::as_str))?;
    unique(
        record
            .artifact_hashes
            .iter()
            .map(|value| &value.artifact_id),
    )?;
    unique(record.dependencies.iter().map(|value| &value.dependency))?;
    unique(
        record
            .embedded_material
            .iter()
            .map(|value| &value.material_id),
    )?;
    unique(
        record
            .provider_contracts
            .iter()
            .map(|value| value.contract_id.as_str()),
    )?;
    unique(record.proposed_destinations.iter().map(RepoPath::as_str))?;
    unique(record.red_tests.iter().map(TestId::as_str))?;
    unique(
        record
            .safety_findings
            .findings
            .iter()
            .map(|finding| finding.finding_id.as_str()),
    )?;
    match &record.admission {
        AdmissionState::InventoryOnly { findings } => {
            unique(findings.iter().map(FindingId::as_str))?;
        }
        AdmissionState::ApprovedForPort { operations } => {
            unique(operations.iter().map(OperationId::as_str))?;
        }
        AdmissionState::EvidenceAccepted { contracts } => {
            unique(contracts.iter().map(ProviderContractId::as_str))?;
        }
    }

    match &record.subject {
        SourceSubject::ExactNpm(package) => {
            unique(
                package
                    .maintainers
                    .iter()
                    .map(NpmMaintainerIdentity::as_str),
            )?;
            if let NpmSignatureDecision::Verified { signatures, .. } = &package.signature {
                unique(signatures.iter().map(|signature| signature.key_id.as_str()))?;
            }
        }
        SourceSubject::ProviderArtifact(provider) => {
            let mut evidence_keys = BTreeSet::new();
            let mut fact_ids = BTreeSet::new();
            for evidence in &provider.evidence {
                let key = provider_source_key(&evidence.source);
                if !evidence_keys.insert((key, evidence.content_sha256.as_str())) {
                    return duplicate("provider evidence");
                }
                for fact in &evidence.facts {
                    if !fact_ids.insert(fact.fact_id.as_str()) {
                        return duplicate("provider fact");
                    }
                }
            }
            for contract in &record.provider_contracts {
                unique(contract.facts.iter().map(contract_fact_key))?;
            }
        }
        SourceSubject::DonatOwned(source) => {
            unique(source.files.iter().map(|file| file.path.as_str()))?;
        }
    }
    Ok(())
}

fn validate_record_legal_state(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    validate_license_legal(&record.license)?;
    if let LicenseDecision::Permissive {
        license_file_path,
        license_file_sha256,
        ..
    } = &record.license
        && (record.notice.license_file_path != *license_file_path
            || record.notice.license_file_sha256 != *license_file_sha256)
    {
        return legal_mismatch("license and notice identities disagree");
    }
    for dependency in &record.dependencies {
        match &dependency.disposition {
            DependencyDisposition::Shipped { license }
            | DependencyDisposition::BuildOnly { license } => validate_license_legal(license)?,
            _ => {}
        }
    }
    for embedded in &record.embedded_material {
        if let EmbeddedMaterialDisposition::Shipped { license } = &embedded.disposition {
            validate_license_legal(license)?;
        }
    }
    if let SourceSubject::ProviderArtifact(provider) = &record.subject {
        for evidence in &provider.evidence {
            match &evidence.terms {
                EvidenceTermsDisposition::Permissive { license, .. } => {
                    validate_license_legal(license)?;
                }
                EvidenceTermsDisposition::ReviewedUse { .. } => {}
                EvidenceTermsDisposition::Rejected { .. } => {
                    return legal_mismatch("provider evidence terms are rejected");
                }
            }
        }
    }
    Ok(())
}

fn validate_reviewed_decisions(
    record: &ConnectorSourceRecord,
    reviews: &SourceReviewRegistry,
) -> Result<(), CatalogError> {
    fn license(
        license: &LicenseDecision,
        reviews: &SourceReviewRegistry,
    ) -> Result<(), CatalogError> {
        if let LicenseDecision::WrittenGrant { decision_id, .. } = license
            && !reviews.written_grants.contains(decision_id.as_str())
        {
            return legal_mismatch("written grant decision is not in the reviewed registry");
        }
        Ok(())
    }

    license(&record.license, reviews)?;
    for dependency in &record.dependencies {
        match &dependency.disposition {
            DependencyDisposition::Shipped { license: value }
            | DependencyDisposition::BuildOnly { license: value } => license(value, reviews)?,
            _ => {}
        }
    }
    for embedded in &record.embedded_material {
        if let EmbeddedMaterialDisposition::Shipped { license: value } = &embedded.disposition {
            license(value, reviews)?;
        }
    }
    match &record.subject {
        SourceSubject::ProviderArtifact(provider) => {
            for evidence in &provider.evidence {
                match &evidence.terms {
                    EvidenceTermsDisposition::Permissive { license: value, .. } => {
                        license(value, reviews)?;
                    }
                    EvidenceTermsDisposition::ReviewedUse { decision_id, .. }
                        if !reviews.reviewed_uses.contains(decision_id.as_str()) =>
                    {
                        return legal_mismatch(
                            "evidence-use decision is not in the reviewed registry",
                        );
                    }
                    _ => {}
                }
            }
        }
        SourceSubject::ExactNpm(package) => {
            if let RepositoryOwnerDecision::ReviewedMismatch { decision_id } =
                &package.repository_owner
                && !reviews.reviewed_uses.contains(decision_id.as_str())
            {
                return legal_mismatch("repository-owner mismatch is not reviewed");
            }
        }
        SourceSubject::DonatOwned(_) => {}
    }
    Ok(())
}

fn validate_record_evidence(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    match &record.subject {
        SourceSubject::ExactNpm(package) => {
            if record.reacquisition != ReacquisitionPlan::ExactNpmReview {
                return evidence_mismatch("npm reacquisition plan");
            }
            validate_npm_identity(record, package)
        }
        SourceSubject::ProviderArtifact(provider) => validate_provider_joins(record, provider),
        SourceSubject::DonatOwned(_) => {
            if record.reacquisition != ReacquisitionPlan::DonatOwnedNoNetwork {
                return evidence_mismatch("Donat-owned reacquisition plan");
            }
            Ok(())
        }
    }
}

fn validate_npm_identity(
    record: &ConnectorSourceRecord,
    package: &ExactNpmPackage,
) -> Result<(), CatalogError> {
    if record.artifact_hashes.is_empty() {
        return evidence_mismatch("npm artifact inventory is empty");
    }
    let package_file_name = package.name.rsplit('/').next().ok_or_else(|| {
        CatalogError::new(
            "source_record_npm_identity_mismatch",
            "npm package name has no canonical tarball basename",
        )
    })?;
    let tarball_file_name = format!("{package_file_name}-{}.tgz", package.version.as_str());
    let expected_tarball_url = format!(
        "https://registry.npmjs.org/{}/-/{}",
        package.name, tarball_file_name
    );
    if package.package_repository != package.repository.url
        || package.npm_git_head != package.repository.commit
        || package.tarball_url.as_str() != expected_tarball_url
    {
        return npm_identity_mismatch("npm repository/package mapping");
    }
    match &package.provenance {
        NpmProvenanceDecision::Verified { source_commit, .. }
            if package.provenance_commit.as_ref() != Some(source_commit) =>
        {
            return npm_identity_mismatch("verified npm provenance commit");
        }
        NpmProvenanceDecision::VerifiedAbsent { .. } | NpmProvenanceDecision::Rejected { .. }
            if package.provenance_commit.is_some() =>
        {
            return npm_identity_mismatch("absent/rejected npm provenance commit");
        }
        _ => {}
    }
    if let (
        NpmSignatureDecision::Verified {
            registry_metadata_sha256: signature_metadata,
            ..
        }
        | NpmSignatureDecision::VerifiedAbsent {
            registry_metadata_sha256: signature_metadata,
        },
        NpmProvenanceDecision::VerifiedAbsent {
            registry_metadata_sha256: provenance_metadata,
        },
    ) = (&package.signature, &package.provenance)
        && signature_metadata != provenance_metadata
    {
        return npm_identity_mismatch("npm registry metadata decisions disagree");
    }
    let [artifact] = record.artifact_hashes.as_slice() else {
        return npm_identity_mismatch("npm tarball artifact inventory must be exact");
    };
    let matches_integrity = artifact.algorithm == HashAlgorithm::Sha512
        && artifact.path.as_deref() == Some(tarball_file_name.as_str())
        && decode_hex_64(&artifact.digest)
            .is_some_and(|digest| &digest == package.integrity.as_bytes());
    if !matches_integrity {
        return npm_identity_mismatch("npm SRI must match the canonical tarball artifact");
    }
    Ok(())
}

fn validate_provider_joins(
    record: &ConnectorSourceRecord,
    provider: &ExactProviderArtifact,
) -> Result<(), CatalogError> {
    if provider.evidence.is_empty()
        || record.artifact_hashes.is_empty()
        || record.provider_contracts.is_empty()
        || provider
            .evidence
            .iter()
            .any(|evidence| evidence.facts.is_empty())
        || record
            .provider_contracts
            .iter()
            .any(|contract| contract.facts.is_empty())
    {
        return evidence_mismatch(
            "provider evidence, artifacts, facts, and contracts must be nonempty",
        );
    }
    let repository_sources = provider
        .evidence
        .iter()
        .filter(|evidence| {
            matches!(
                evidence.source,
                ImmutableProviderEvidenceSource::RepositoryFile { .. }
            )
        })
        .count();
    let expected_plan = if repository_sources == provider.evidence.len() {
        ReacquisitionPlan::ProviderRepositoryReview
    } else if repository_sources == 0 {
        ReacquisitionPlan::ProviderVersionedArtifactReview
    } else {
        return evidence_mismatch("mixed provider evidence source variants");
    };
    if record.reacquisition != expected_plan {
        return evidence_mismatch("provider evidence/reacquisition variant");
    }

    let mut inventory_facts = BTreeSet::new();
    let mut joined_artifacts = BTreeSet::new();
    for evidence in &provider.evidence {
        let source_path = match &evidence.source {
            ImmutableProviderEvidenceSource::RepositoryFile { path, .. } => {
                path.as_str().to_owned()
            }
            ImmutableProviderEvidenceSource::VersionedArtifact { url, .. } => {
                versioned_artifact_path(url)?
            }
        };
        let artifact_matches = record
            .artifact_hashes
            .iter()
            .enumerate()
            .filter(|artifact| {
                artifact.1.algorithm == HashAlgorithm::Sha256
                    && artifact.1.digest == evidence.content_sha256.as_str()
                    && artifact.1.path.as_deref() == Some(source_path.as_str())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [artifact_index] = artifact_matches.as_slice() else {
            return evidence_mismatch("provider evidence artifact/content/path join");
        };
        if !joined_artifacts.insert(*artifact_index) {
            return evidence_mismatch("provider evidence reuses an artifact");
        }
        for fact in &evidence.facts {
            let fact_path = match &fact.location {
                ExactFactLocation::JsonPointer { path, .. }
                | ExactFactLocation::DocumentSection { path, .. } => path,
            };
            if fact_path.as_str() != source_path {
                return evidence_mismatch("provider fact path does not match evidence");
            }
            if !inventory_facts.insert(fact.fact_id) {
                return duplicate("provider fact");
            }
        }
    }
    if joined_artifacts.len() != record.artifact_hashes.len() {
        return evidence_mismatch("provider artifact inventory contains unrelated entries");
    }

    let mut referenced_facts = BTreeSet::new();
    for contract in &record.provider_contracts {
        for fact in &contract.facts {
            let ContractFact::ProviderEvidence {
                source_record_id,
                fact_id,
            } = fact
            else {
                return evidence_mismatch("provider contracts require provider evidence facts");
            };
            if source_record_id != &record.record_id || !inventory_facts.contains(fact_id) {
                return evidence_mismatch("provider contract fact reference");
            }
            referenced_facts.insert(*fact_id);
        }
    }
    if referenced_facts != inventory_facts {
        return evidence_mismatch("provider fact contract closure");
    }
    Ok(())
}

fn validate_record_admission(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    match &record.admission {
        AdmissionState::InventoryOnly { findings } => {
            let admitted = findings
                .iter()
                .map(FindingId::as_str)
                .collect::<BTreeSet<_>>();
            let actual = record
                .safety_findings
                .findings
                .iter()
                .map(|finding| finding.finding_id.as_str())
                .collect::<BTreeSet<_>>();
            if findings.is_empty() || admitted.len() != findings.len() || admitted != actual {
                return admission_mismatch("inventory-only safety finding closure");
            }
            Ok(())
        }
        AdmissionState::ApprovedForPort { operations } => {
            if operations.is_empty()
                || matches!(record.subject, SourceSubject::ProviderArtifact(_))
                || operations
                    .iter()
                    .map(OperationId::as_str)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != operations.len()
            {
                return admission_mismatch("approved operation closure");
            }
            validate_executable_source_state(record)?;
            Ok(())
        }
        AdmissionState::EvidenceAccepted { contracts } => {
            let SourceSubject::ProviderArtifact(_) = &record.subject else {
                return admission_mismatch("evidence admission subject");
            };
            let admitted = contracts.iter().copied().collect::<BTreeSet<_>>();
            let declared = record
                .provider_contracts
                .iter()
                .map(|contract| contract.contract_id)
                .collect::<BTreeSet<_>>();
            if contracts.is_empty() || admitted.len() != contracts.len() || admitted != declared {
                return admission_mismatch("accepted provider contract closure");
            }
            validate_executable_source_state(record)?;
            Ok(())
        }
    }
}

fn validate_executable_source_state(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    if record.compatibility == CompatibilityDecision::Rejected
        || !record.safety_findings.findings.is_empty()
        || record.dependencies.iter().any(|dependency| {
            matches!(
                dependency.disposition,
                DependencyDisposition::Rejected { .. }
            )
        })
        || record.embedded_material.iter().any(|material| {
            matches!(
                material.disposition,
                EmbeddedMaterialDisposition::Rejected { .. }
            )
        })
    {
        return admission_mismatch("rejected or unresolved executable source state");
    }
    if let SourceSubject::ExactNpm(package) = &record.subject
        && (matches!(package.signature, NpmSignatureDecision::Rejected { .. })
            || matches!(package.provenance, NpmProvenanceDecision::Rejected { .. })
            || matches!(
                package.repository_owner,
                RepositoryOwnerDecision::Rejected { .. }
            ))
    {
        return admission_mismatch("rejected npm executable source state");
    }
    Ok(())
}

fn versioned_artifact_path(url: &ExactHttpsUrl) -> Result<String, CatalogError> {
    let parsed = url::Url::parse(url.as_str()).map_err(|_| {
        CatalogError::new("source_record_evidence_mismatch", "invalid evidence URL")
    })?;
    let path = parsed
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .ok_or_else(|| {
            CatalogError::new(
                "source_record_evidence_mismatch",
                "versioned evidence URL has no immutable artifact path",
            )
        })?;
    if !valid_path(path) {
        return evidence_mismatch("versioned evidence artifact path");
    }
    Ok(path.to_owned())
}

fn validate_license_primitives(license: &LicenseDecision) -> Result<(), CatalogError> {
    match license {
        LicenseDecision::Permissive {
            license_file_path,
            license_file_sha256,
            ..
        } => {
            if !valid_path(license_file_path) || !valid_hash(license_file_sha256, 64) {
                return invalid_primitive("permissive license identity");
            }
        }
        LicenseDecision::WrittenGrant {
            decision_id,
            grant_sha256,
        } if !valid_id(decision_id) || !valid_hash(grant_sha256, 64) => {
            return invalid_primitive("written grant");
        }
        LicenseDecision::Rejected { finding } if !valid_id(finding) => {
            return invalid_primitive("rejected license");
        }
        _ => {}
    }
    Ok(())
}

fn validate_license_legal(license: &LicenseDecision) -> Result<(), CatalogError> {
    const ALLOWED: &[&str] = &[
        "MIT",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "ISC",
        "0BSD",
    ];
    match license {
        LicenseDecision::Permissive {
            spdx_id,
            selected_dual_license_branch,
            ..
        } => {
            if ALLOWED.contains(&spdx_id.as_str()) {
                if selected_dual_license_branch.is_some() {
                    return legal_mismatch("single license cannot select a dual branch");
                }
                return Ok(());
            }
            let branches = spdx_id.split(" OR ").collect::<Vec<_>>();
            if branches.len() < 2
                || branches.iter().any(|branch| !ALLOWED.contains(branch))
                || selected_dual_license_branch
                    .as_ref()
                    .is_none_or(|selected| !branches.contains(&selected.as_str()))
            {
                return legal_mismatch("Phase-1 permissive license decision");
            }
            Ok(())
        }
        LicenseDecision::WrittenGrant { .. } => Ok(()),
        LicenseDecision::Rejected { .. } => legal_mismatch("license is rejected"),
    }
}

fn valid_date(value: &str) -> bool {
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let Ok(year) = value[..4].parse::<u32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..].parse::<u32>() else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year != 0 && (1..=maximum_day).contains(&day)
}

fn valid_hash(value: &str, width: usize) -> bool {
    value.len() == width
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_hash256(value: &str) -> bool {
    valid_hash(value, 64)
}

fn valid_nonempty_string(value: &str) -> bool {
    !value.is_empty() && validate_unicode_scalar_string(value).is_ok()
}

fn valid_git(value: &str) -> bool {
    valid_hash(value, 40)
}

fn valid_https(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.as_str() == value
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

fn valid_id(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value.is_ascii()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_npm_name(value: &str) -> bool {
    fn valid_segment(value: &str) -> bool {
        !value.is_empty()
            && !value.starts_with(['.', '_'])
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'~')
            })
    }

    if value.is_empty() || value.len() > 214 || !value.is_ascii() {
        return false;
    }
    if let Some(scoped) = value.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        !name.contains('/') && valid_segment(scope) && valid_segment(name)
    } else {
        !value.contains('/') && valid_segment(value)
    }
}

fn valid_json_pointer(value: &str) -> bool {
    value.is_empty()
        || (value.starts_with('/')
            && !value
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
            && !value.ends_with('~'))
}

fn validate_unicode_scalar_string(value: &str) -> Result<(), CatalogError> {
    if value.chars().any(|character| {
        let scalar = u32::from(character);
        (0xfdd0..=0xfdef).contains(&scalar)
            || scalar & 0xffff == 0xfffe
            || scalar & 0xffff == 0xffff
    }) {
        return Err(material_schema_error());
    }
    Ok(())
}

fn material_schema_error() -> CatalogError {
    CatalogError::new(
        "catalog_jcs_schema_mismatch",
        "typed value material is not a closed canonical value",
    )
}

fn provider_source_key(source: &ImmutableProviderEvidenceSource) -> String {
    match source {
        ImmutableProviderEvidenceSource::RepositoryFile {
            repository,
            commit,
            path,
        } => format!("repository:{repository}\0{commit}\0{path}"),
        ImmutableProviderEvidenceSource::VersionedArtifact {
            url,
            provider_revision,
        } => format!("artifact:{url}\0{provider_revision}"),
    }
}

fn contract_fact_key(fact: &ContractFact) -> String {
    match fact {
        ContractFact::ProviderEvidence {
            source_record_id,
            fact_id,
        } => format!(
            "provider:{}:{}",
            source_record_id.as_str(),
            fact_id.as_str()
        ),
        ContractFact::DonatPolicy { policy_id, .. } => {
            format!("policy:{}", policy_id.as_str())
        }
    }
}

fn unique<T>(values: impl IntoIterator<Item = T>) -> Result<(), CatalogError>
where
    T: AsRef<str>,
{
    let mut seen = BTreeSet::new();
    for value in values {
        let value = value.as_ref();
        if !seen.insert(value.to_owned()) {
            return duplicate(value.to_owned());
        }
    }
    Ok(())
}

fn decode_hex_64(value: &str) -> Option<[u8; 64]> {
    if !valid_hash(value, 128) {
        return None;
    }
    let mut bytes = [0_u8; 64];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        bytes[index] = (high << 4) | low;
    }
    Some(bytes)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn source_error<T>(code: &'static str, detail: impl Into<String>) -> Result<T, CatalogError> {
    Err(CatalogError::new(code, detail))
}

fn invalid_primitive<T>(detail: &'static str) -> Result<T, CatalogError> {
    source_error("source_record_invalid_primitive", detail)
}

fn duplicate<T>(detail: impl Into<String>) -> Result<T, CatalogError> {
    source_error("source_record_duplicate", detail)
}

fn legal_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    source_error("source_record_legal_mismatch", detail)
}

fn npm_identity_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    source_error("source_record_npm_identity_mismatch", detail)
}

fn evidence_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    source_error("source_record_evidence_mismatch", detail)
}

fn admission_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    source_error("source_record_admission_mismatch", detail)
}
