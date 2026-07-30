use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use base64::Engine;
use donat_connector_abi::{InlineId, OperationId};
use donat_value_contract::{BoundedInlineBytes, CanonicalNumber, TypedValue};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::CatalogError;
use crate::canonical::{TypedValueMaterial, TypedValueMaterialV1};

pub(crate) trait SourcePrimitive: Sized {
    fn parse_source_primitive(value: &str) -> Result<Self, CatalogError>;
}

pub(crate) fn deserialize_source_primitive<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: SourcePrimitive,
{
    let value = String::deserialize(deserializer)?;
    T::parse_source_primitive(&value).map_err(serde::de::Error::custom)
}

fn deserialize_source_primitives<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: SourcePrimitive,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|value| T::parse_source_primitive(&value).map_err(serde::de::Error::custom))
        .collect()
}

fn deserialize_optional_source_primitive<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: SourcePrimitive,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| T::parse_source_primitive(&value).map_err(serde::de::Error::custom))
        .transpose()
}

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

        impl SourcePrimitive for $name {
            fn parse_source_primitive(value: &str) -> Result<Self, CatalogError> {
                Self::parse(value)
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

        impl SourcePrimitive for $name {
            fn parse_source_primitive(value: &str) -> Result<Self, CatalogError> {
                Self::parse(value)
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

impl SourcePrimitive for ExactSemver {
    fn parse_source_primitive(value: &str) -> Result<Self, CatalogError> {
        Self::try_new(value)
    }
}

impl SourcePrimitive for OperationId {
    fn parse_source_primitive(value: &str) -> Result<Self, CatalogError> {
        OperationId::parse(value)
            .map_err(|_| CatalogError::new("source_record_invalid_primitive", value))
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
///
/// ```compile_fail
/// use donat_connector_catalog::ConnectorSourceRecord;
///
/// let _: ConnectorSourceRecord = serde_yaml::from_str("record_version: 1").unwrap();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
///
/// ```compile_fail
/// use donat_connector_catalog::ExactNpmPackage;
///
/// let _: ExactNpmPackage = serde_yaml::from_str("{}").unwrap();
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

impl SourcePrimitive for NpmIntegrity {
    fn parse_source_primitive(value: &str) -> Result<Self, CatalogError> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableRepository {
    pub(crate) url: RepositoryUrl,
    pub(crate) commit: GitCommit,
    pub(crate) tree: GitTree,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedNpmSignature {
    pub(crate) key_id: String,
    pub(crate) signature_sha256: Hash256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
///
/// ```compile_fail
/// use donat_connector_catalog::ProviderEvidenceArtifact;
///
/// let _: ProviderEvidenceArtifact = serde_yaml::from_str("{}").unwrap();
/// ```
pub struct ExactProviderArtifact {
    pub(crate) provider: String,
    pub(crate) evidence: Vec<ProviderEvidenceArtifact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEvidenceArtifact {
    pub(crate) source: ImmutableProviderEvidenceSource,
    pub(crate) accessed_on: Date,
    pub(crate) content_sha256: Hash256,
    pub(crate) terms: EvidenceTermsDisposition,
    pub(crate) facts: Vec<ProviderFact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFact {
    pub(crate) fact_id: ProviderFactId,
    pub(crate) location: ExactFactLocation,
    #[serde(deserialize_with = "deserialize_typed_value_material")]
    pub(crate) normalized_value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContractReference {
    pub(crate) contract_id: ProviderContractId,
    pub(crate) facts: Vec<ContractFact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DonatOwnedSource {
    pub(crate) repository_commit: GitCommit,
    pub(crate) files: Vec<RepoFileHash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFileHash {
    pub(crate) path: RepoPath,
    pub(crate) sha256: Hash256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Eq, PartialEq, Serialize)]
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
        #[serde(serialize_with = "operation_ids::serialize")]
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
    use serde::{Serialize, Serializer};

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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeIdentity {
    pub(crate) id: NoticeId,
    pub(crate) license_file_path: SourcePath,
    pub(crate) license_file_sha256: Hash256,
    pub(crate) required_copyright_lines: Vec<String>,
    pub(crate) notice_bundle_destination: RepoPath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyDecision {
    pub(crate) dependency: String,
    pub(crate) disposition: DependencyDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedMaterialDecision {
    pub(crate) material_id: String,
    pub(crate) path: SourcePath,
    pub(crate) sha256: Hash256,
    pub(crate) disposition: EmbeddedMaterialDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFindings {
    pub(crate) findings: Vec<SafetyFinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyFinding {
    pub(crate) finding_id: FindingId,
    pub(crate) kind: String,
    pub(crate) location: Option<SourcePath>,
    pub(crate) message: String,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RawStructDeclaration {
    name: &'static str,
    fields: Vec<&'static str>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RawVariantDeclaration {
    tag: &'static str,
    fields: Vec<&'static str>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RawTaggedDeclaration {
    name: &'static str,
    variants: Vec<RawVariantDeclaration>,
}

mod source_record_input {
    use super::*;

    macro_rules! remote_vec {
        ($module:ident, $value:ty, $remote:literal) => {
            mod $module {
                use super::*;

                #[derive(Deserialize)]
                struct Item(#[serde(with = $remote)] $value);

                pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<$value>, D::Error>
                where
                    D: Deserializer<'de>,
                {
                    Vec::<Item>::deserialize(deserializer)
                        .map(|values| values.into_iter().map(|value| value.0).collect())
                }
            }
        };
    }

    #[derive(Deserialize)]
    #[serde(remote = "ConnectorSourceRecord", deny_unknown_fields)]
    struct ConnectorSourceRecordDef {
        record_version: u32,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        record_id: SourceRecordId,
        #[serde(deserialize_with = "deserialize_source_subject")]
        subject: SourceSubject,
        #[serde(with = "ReacquisitionPlanDef")]
        reacquisition: ReacquisitionPlan,
        #[serde(deserialize_with = "artifact_hashes::deserialize")]
        artifact_hashes: Vec<ArtifactHash>,
        #[serde(with = "LicenseDecisionDef")]
        license: LicenseDecision,
        #[serde(with = "NoticeIdentityDef")]
        notice: NoticeIdentity,
        #[serde(deserialize_with = "deserialize_source_primitives")]
        entrypoints: Vec<SourcePath>,
        #[serde(deserialize_with = "dependencies::deserialize")]
        dependencies: Vec<DependencyDecision>,
        #[serde(deserialize_with = "embedded_material::deserialize")]
        embedded_material: Vec<EmbeddedMaterialDecision>,
        #[serde(deserialize_with = "provider_contracts::deserialize")]
        provider_contracts: Vec<ProviderContractReference>,
        #[serde(with = "CompatibilityDecisionDef")]
        compatibility: CompatibilityDecision,
        #[serde(with = "AdmissionStateDef")]
        admission: AdmissionState,
        #[serde(with = "SafetyFindingsDef")]
        safety_findings: SafetyFindings,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        reviewer: ReviewIdentity,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        approval_date: Date,
        #[serde(deserialize_with = "deserialize_optional_source_primitive")]
        proposed_manifest: Option<RepoPath>,
        #[serde(deserialize_with = "deserialize_source_primitives")]
        proposed_destinations: Vec<RepoPath>,
        #[serde(deserialize_with = "deserialize_source_primitives")]
        red_tests: Vec<TestId>,
    }

    #[derive(Deserialize)]
    struct ExactNpmInput(#[serde(with = "ExactNpmPackageDef")] ExactNpmPackage);

    #[derive(Deserialize)]
    struct ProviderArtifactInput(#[serde(with = "ExactProviderArtifactDef")] ExactProviderArtifact);

    #[derive(Deserialize)]
    struct DonatOwnedInput(#[serde(with = "DonatOwnedSourceDef")] DonatOwnedSource);

    #[derive(Deserialize)]
    #[serde(
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum RawSourceSubject {
        ExactNpm(Box<ExactNpmInput>),
        ProviderArtifact(ProviderArtifactInput),
        DonatOwned(DonatOwnedInput),
    }

    fn deserialize_source_subject<'de, D>(deserializer: D) -> Result<SourceSubject, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match RawSourceSubject::deserialize(deserializer)? {
            RawSourceSubject::ExactNpm(value) => {
                let ExactNpmInput(value) = *value;
                SourceSubject::ExactNpm(value)
            }
            RawSourceSubject::ProviderArtifact(value) => SourceSubject::ProviderArtifact(value.0),
            RawSourceSubject::DonatOwned(value) => SourceSubject::DonatOwned(value.0),
        })
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "ReacquisitionPlan",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum ReacquisitionPlanDef {
        ExactNpmReview,
        ProviderRepositoryReview,
        ProviderVersionedArtifactReview,
        DonatOwnedNoNetwork,
    }

    #[derive(Deserialize)]
    #[serde(remote = "ExactNpmPackage", deny_unknown_fields)]
    struct ExactNpmPackageDef {
        name: String,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        version: ExactSemver,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        tarball_url: ExactHttpsUrl,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        integrity: NpmIntegrity,
        #[serde(with = "ImmutableRepositoryDef")]
        repository: ImmutableRepository,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        npm_git_head: GitCommit,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        package_repository: RepositoryUrl,
        #[serde(with = "NpmSignatureDecisionDef")]
        signature: NpmSignatureDecision,
        #[serde(with = "NpmProvenanceDecisionDef")]
        provenance: NpmProvenanceDecision,
        #[serde(deserialize_with = "deserialize_optional_source_primitive")]
        tag_commit: Option<GitCommit>,
        #[serde(deserialize_with = "deserialize_optional_source_primitive")]
        provenance_commit: Option<GitCommit>,
        #[serde(deserialize_with = "deserialize_source_primitives")]
        maintainers: Vec<NpmMaintainerIdentity>,
        #[serde(with = "RepositoryOwnerDecisionDef")]
        repository_owner: RepositoryOwnerDecision,
    }

    #[derive(Deserialize)]
    #[serde(remote = "ImmutableRepository", deny_unknown_fields)]
    struct ImmutableRepositoryDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        url: RepositoryUrl,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        commit: GitCommit,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        tree: GitTree,
    }

    #[derive(Deserialize)]
    #[serde(remote = "VerifiedNpmSignature", deny_unknown_fields)]
    struct VerifiedNpmSignatureDef {
        key_id: String,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        signature_sha256: Hash256,
    }
    remote_vec!(
        verified_signatures,
        VerifiedNpmSignature,
        "VerifiedNpmSignatureDef"
    );

    #[derive(Deserialize)]
    #[serde(
        remote = "NpmSignatureDecision",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum NpmSignatureDecisionDef {
        Verified {
            #[serde(deserialize_with = "verified_signatures::deserialize")]
            signatures: Vec<VerifiedNpmSignature>,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            registry_metadata_sha256: Hash256,
        },
        VerifiedAbsent {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            registry_metadata_sha256: Hash256,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "NpmProvenanceDecision",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum NpmProvenanceDecisionDef {
        Verified {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            statement_sha256: Hash256,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            source_commit: GitCommit,
        },
        VerifiedAbsent {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            registry_metadata_sha256: Hash256,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "RepositoryOwnerDecision",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum RepositoryOwnerDecisionDef {
        Consistent {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            package_owner: NpmOwnerIdentity,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            repository_owner: RepositoryOwnerIdentity,
        },
        ReviewedMismatch {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            decision_id: ReviewDecisionId,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "ExactProviderArtifact", deny_unknown_fields)]
    struct ExactProviderArtifactDef {
        provider: String,
        #[serde(deserialize_with = "provider_evidence::deserialize")]
        evidence: Vec<ProviderEvidenceArtifact>,
    }

    #[derive(Deserialize)]
    #[serde(remote = "ProviderEvidenceArtifact", deny_unknown_fields)]
    struct ProviderEvidenceArtifactDef {
        #[serde(with = "ImmutableProviderEvidenceSourceDef")]
        source: ImmutableProviderEvidenceSource,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        accessed_on: Date,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        content_sha256: Hash256,
        #[serde(with = "EvidenceTermsDispositionDef")]
        terms: EvidenceTermsDisposition,
        #[serde(deserialize_with = "provider_facts::deserialize")]
        facts: Vec<ProviderFact>,
    }
    remote_vec!(
        provider_evidence,
        ProviderEvidenceArtifact,
        "ProviderEvidenceArtifactDef"
    );

    #[derive(Deserialize)]
    #[serde(
        remote = "ImmutableProviderEvidenceSource",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum ImmutableProviderEvidenceSourceDef {
        RepositoryFile {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            repository: RepositoryUrl,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            commit: GitCommit,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            path: SourcePath,
        },
        VersionedArtifact {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            url: ExactHttpsUrl,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            provider_revision: NonEmptyString,
        },
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "EvidenceTermsDisposition",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum EvidenceTermsDispositionDef {
        Permissive {
            #[serde(with = "LicenseDecisionDef")]
            license: LicenseDecision,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            evidence_url: ExactHttpsUrl,
        },
        ReviewedUse {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            decision_id: ReviewDecisionId,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            evidence_url: ExactHttpsUrl,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "ProviderFact", deny_unknown_fields)]
    struct ProviderFactDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        fact_id: ProviderFactId,
        #[serde(with = "ExactFactLocationDef")]
        location: ExactFactLocation,
        #[serde(deserialize_with = "deserialize_typed_value_material")]
        normalized_value: TypedValueMaterialV1,
    }
    remote_vec!(provider_facts, ProviderFact, "ProviderFactDef");

    #[derive(Deserialize)]
    #[serde(
        remote = "ExactFactLocation",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum ExactFactLocationDef {
        JsonPointer {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            path: SourcePath,
            pointer: String,
        },
        DocumentSection {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            path: SourcePath,
            section: String,
        },
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "ContractFact",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum ContractFactDef {
        ProviderEvidence {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            source_record_id: SourceRecordId,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            fact_id: ProviderFactId,
        },
        DonatPolicy {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            policy_id: DonatPolicyId,
            #[serde(deserialize_with = "deserialize_typed_value_material")]
            value: TypedValueMaterialV1,
        },
    }
    remote_vec!(contract_facts, ContractFact, "ContractFactDef");

    #[derive(Deserialize)]
    #[serde(remote = "ProviderContractReference", deny_unknown_fields)]
    struct ProviderContractReferenceDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        contract_id: ProviderContractId,
        #[serde(deserialize_with = "contract_facts::deserialize")]
        facts: Vec<ContractFact>,
    }
    remote_vec!(
        provider_contracts,
        ProviderContractReference,
        "ProviderContractReferenceDef"
    );

    #[derive(Deserialize)]
    #[serde(remote = "DonatOwnedSource", deny_unknown_fields)]
    struct DonatOwnedSourceDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        repository_commit: GitCommit,
        #[serde(deserialize_with = "repo_files::deserialize")]
        files: Vec<RepoFileHash>,
    }

    #[derive(Deserialize)]
    #[serde(remote = "RepoFileHash", deny_unknown_fields)]
    struct RepoFileHashDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        path: RepoPath,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        sha256: Hash256,
    }
    remote_vec!(repo_files, RepoFileHash, "RepoFileHashDef");

    #[derive(Deserialize)]
    #[serde(
        remote = "CompatibilityDecision",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum CompatibilityDecisionDef {
        TierA,
        TierB,
        TierC,
        Rejected,
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "AdmissionState",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum AdmissionStateDef {
        InventoryOnly {
            #[serde(deserialize_with = "deserialize_source_primitives")]
            findings: Vec<FindingId>,
        },
        ApprovedForPort {
            #[serde(deserialize_with = "deserialize_source_primitives")]
            operations: Vec<OperationId>,
        },
        EvidenceAccepted {
            #[serde(deserialize_with = "deserialize_source_primitives")]
            contracts: Vec<ProviderContractId>,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "ArtifactHash", deny_unknown_fields)]
    struct ArtifactHashDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        artifact_id: ArtifactId,
        #[serde(with = "HashAlgorithmDef")]
        algorithm: HashAlgorithm,
        digest: String,
        #[serde(deserialize_with = "deserialize_optional_source_primitive")]
        path: Option<SourcePath>,
    }
    remote_vec!(artifact_hashes, ArtifactHash, "ArtifactHashDef");

    #[derive(Deserialize)]
    struct ArtifactHashesInput(
        #[serde(deserialize_with = "artifact_hashes::deserialize")] Vec<ArtifactHash>,
    );

    pub(super) fn deserialize_artifact_hashes<'de, D>(
        deserializer: D,
    ) -> Result<Vec<ArtifactHash>, D::Error>
    where
        D: Deserializer<'de>,
    {
        ArtifactHashesInput::deserialize(deserializer).map(|value| value.0)
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "HashAlgorithm",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum HashAlgorithmDef {
        Sha256,
        Sha512,
    }

    #[derive(Deserialize)]
    #[serde(
        remote = "LicenseDecision",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum LicenseDecisionDef {
        Permissive {
            spdx_id: String,
            selected_dual_license_branch: Option<String>,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            license_file_path: SourcePath,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            license_file_sha256: Hash256,
        },
        WrittenGrant {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            decision_id: ReviewDecisionId,
            #[serde(deserialize_with = "deserialize_source_primitive")]
            grant_sha256: Hash256,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "NoticeIdentity", deny_unknown_fields)]
    struct NoticeIdentityDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        id: NoticeId,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        license_file_path: SourcePath,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        license_file_sha256: Hash256,
        required_copyright_lines: Vec<String>,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        notice_bundle_destination: RepoPath,
    }

    #[derive(Deserialize)]
    #[serde(remote = "DependencyDecision", deny_unknown_fields)]
    struct DependencyDecisionDef {
        dependency: String,
        #[serde(with = "DependencyDispositionDef")]
        disposition: DependencyDisposition,
    }
    remote_vec!(dependencies, DependencyDecision, "DependencyDecisionDef");

    #[derive(Deserialize)]
    #[serde(
        remote = "DependencyDisposition",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum DependencyDispositionDef {
        Shipped {
            #[serde(with = "LicenseDecisionDef")]
            license: LicenseDecision,
        },
        BuildOnly {
            #[serde(with = "LicenseDecisionDef")]
            license: LicenseDecision,
        },
        TypeOnlyReplaced {
            replacement: String,
        },
        BehaviorOnly {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            reason: FindingId,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "EmbeddedMaterialDecision", deny_unknown_fields)]
    struct EmbeddedMaterialDecisionDef {
        material_id: String,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        path: SourcePath,
        #[serde(deserialize_with = "deserialize_source_primitive")]
        sha256: Hash256,
        #[serde(with = "EmbeddedMaterialDispositionDef")]
        disposition: EmbeddedMaterialDisposition,
    }
    remote_vec!(
        embedded_material,
        EmbeddedMaterialDecision,
        "EmbeddedMaterialDecisionDef"
    );

    #[derive(Deserialize)]
    #[serde(
        remote = "EmbeddedMaterialDisposition",
        deny_unknown_fields,
        tag = "kind",
        content = "value",
        rename_all = "snake_case"
    )]
    enum EmbeddedMaterialDispositionDef {
        Shipped {
            #[serde(with = "LicenseDecisionDef")]
            license: LicenseDecision,
        },
        BehaviorOnly {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            reason: FindingId,
        },
        Rejected {
            #[serde(deserialize_with = "deserialize_source_primitive")]
            finding: FindingId,
        },
    }

    #[derive(Deserialize)]
    #[serde(remote = "SafetyFindings", deny_unknown_fields)]
    struct SafetyFindingsDef {
        #[serde(deserialize_with = "safety_findings::deserialize")]
        findings: Vec<SafetyFinding>,
    }

    #[derive(Deserialize)]
    #[serde(remote = "SafetyFinding", deny_unknown_fields)]
    struct SafetyFindingDef {
        #[serde(deserialize_with = "deserialize_source_primitive")]
        finding_id: FindingId,
        kind: String,
        #[serde(deserialize_with = "deserialize_optional_source_primitive")]
        location: Option<SourcePath>,
        message: String,
    }
    remote_vec!(safety_findings, SafetyFinding, "SafetyFindingDef");

    #[derive(Deserialize)]
    struct ContractFactInput(#[serde(with = "ContractFactDef")] ContractFact);

    pub(super) fn deserialize_contract_fact<'de, D>(
        deserializer: D,
    ) -> Result<ContractFact, D::Error>
    where
        D: Deserializer<'de>,
    {
        ContractFactInput::deserialize(deserializer).map(|value| value.0)
    }

    #[cfg(test)]
    pub(super) fn raw_declaration_inventory()
    -> (Vec<RawStructDeclaration>, Vec<RawTaggedDeclaration>) {
        macro_rules! raw_struct {
            ($output:ident, $type:ident { $($field:ident),+ $(,)? }) => {{
                #[allow(dead_code)]
                fn exhaustive(value: $type) {
                    let $type { $($field: _),+ } = value;
                }
                $output.push(RawStructDeclaration {
                    name: stringify!($type),
                    fields: vec![$(stringify!($field)),+],
                });
            }};
        }
        macro_rules! raw_unit_enum {
            ($output:ident, $type:ident { $($variant:ident => $tag:literal),+ $(,)? }) => {{
                #[allow(dead_code)]
                fn exhaustive(value: $type) {
                    match value {
                        $($type::$variant => {}),+
                    }
                }
                $output.push(RawTaggedDeclaration {
                    name: stringify!($type),
                    variants: vec![
                        $(RawVariantDeclaration {
                            tag: $tag,
                            fields: Vec::new(),
                        }),+
                    ],
                });
            }};
        }
        macro_rules! raw_tuple_enum {
            ($output:ident, $type:ident { $($variant:ident => $tag:literal),+ $(,)? }) => {{
                #[allow(dead_code)]
                fn exhaustive(value: $type) {
                    match value {
                        $($type::$variant(_) => {}),+
                    }
                }
                $output.push(RawTaggedDeclaration {
                    name: stringify!($type),
                    variants: vec![
                        $(RawVariantDeclaration {
                            tag: $tag,
                            fields: Vec::new(),
                        }),+
                    ],
                });
            }};
        }
        macro_rules! raw_struct_enum {
            (
                $output:ident,
                $type:ident {
                    $(
                        $variant:ident { $($field:ident),+ $(,)? } => $tag:literal
                    ),+ $(,)?
                }
            ) => {{
                #[allow(dead_code)]
                fn exhaustive(value: $type) {
                    match value {
                        $(
                            $type::$variant { $($field: _),+ } => {}
                        ),+
                    }
                }
                $output.push(RawTaggedDeclaration {
                    name: stringify!($type),
                    variants: vec![
                        $(
                            RawVariantDeclaration {
                                tag: $tag,
                                fields: vec![$(stringify!($field)),+],
                            }
                        ),+
                    ],
                });
            }};
        }

        let mut structs = Vec::new();
        raw_struct!(
            structs,
            ConnectorSourceRecordDef {
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
            }
        );
        raw_struct!(
            structs,
            ExactNpmPackageDef {
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
            }
        );
        raw_struct!(structs, ImmutableRepositoryDef { url, commit, tree });
        raw_struct!(
            structs,
            VerifiedNpmSignatureDef {
                key_id,
                signature_sha256,
            }
        );
        raw_struct!(structs, ExactProviderArtifactDef { provider, evidence });
        raw_struct!(
            structs,
            ProviderEvidenceArtifactDef {
                source,
                accessed_on,
                content_sha256,
                terms,
                facts,
            }
        );
        raw_struct!(
            structs,
            ProviderFactDef {
                fact_id,
                location,
                normalized_value,
            }
        );
        raw_struct!(structs, ProviderContractReferenceDef { contract_id, facts });
        raw_struct!(
            structs,
            DonatOwnedSourceDef {
                repository_commit,
                files,
            }
        );
        raw_struct!(structs, RepoFileHashDef { path, sha256 });
        raw_struct!(
            structs,
            ArtifactHashDef {
                artifact_id,
                algorithm,
                digest,
                path,
            }
        );
        raw_struct!(
            structs,
            NoticeIdentityDef {
                id,
                license_file_path,
                license_file_sha256,
                required_copyright_lines,
                notice_bundle_destination,
            }
        );
        raw_struct!(
            structs,
            DependencyDecisionDef {
                dependency,
                disposition,
            }
        );
        raw_struct!(
            structs,
            EmbeddedMaterialDecisionDef {
                material_id,
                path,
                sha256,
                disposition,
            }
        );
        raw_struct!(structs, SafetyFindingsDef { findings });
        raw_struct!(
            structs,
            SafetyFindingDef {
                finding_id,
                kind,
                location,
                message,
            }
        );

        let mut tagged = Vec::new();
        raw_tuple_enum!(tagged, RawSourceSubject {
            ExactNpm => "exact_npm",
            ProviderArtifact => "provider_artifact",
            DonatOwned => "donat_owned",
        });
        raw_unit_enum!(tagged, ReacquisitionPlanDef {
            ExactNpmReview => "exact_npm_review",
            ProviderRepositoryReview => "provider_repository_review",
            ProviderVersionedArtifactReview => "provider_versioned_artifact_review",
            DonatOwnedNoNetwork => "donat_owned_no_network",
        });
        raw_struct_enum!(tagged, NpmSignatureDecisionDef {
            Verified {
                signatures,
                registry_metadata_sha256,
            } => "verified",
            VerifiedAbsent {
                registry_metadata_sha256,
            } => "verified_absent",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, NpmProvenanceDecisionDef {
            Verified {
                statement_sha256,
                source_commit,
            } => "verified",
            VerifiedAbsent {
                registry_metadata_sha256,
            } => "verified_absent",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, RepositoryOwnerDecisionDef {
            Consistent {
                package_owner,
                repository_owner,
            } => "consistent",
            ReviewedMismatch { decision_id } => "reviewed_mismatch",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, ImmutableProviderEvidenceSourceDef {
            RepositoryFile {
                repository,
                commit,
                path,
            } => "repository_file",
            VersionedArtifact {
                url,
                provider_revision,
            } => "versioned_artifact",
        });
        raw_struct_enum!(tagged, EvidenceTermsDispositionDef {
            Permissive {
                license,
                evidence_url,
            } => "permissive",
            ReviewedUse {
                decision_id,
                evidence_url,
            } => "reviewed_use",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, ExactFactLocationDef {
            JsonPointer { path, pointer } => "json_pointer",
            DocumentSection { path, section } => "document_section",
        });
        raw_struct_enum!(tagged, ContractFactDef {
            ProviderEvidence {
                source_record_id,
                fact_id,
            } => "provider_evidence",
            DonatPolicy { policy_id, value } => "donat_policy",
        });
        raw_unit_enum!(tagged, CompatibilityDecisionDef {
            TierA => "tier_a",
            TierB => "tier_b",
            TierC => "tier_c",
            Rejected => "rejected",
        });
        raw_struct_enum!(tagged, AdmissionStateDef {
            InventoryOnly { findings } => "inventory_only",
            ApprovedForPort { operations } => "approved_for_port",
            EvidenceAccepted { contracts } => "evidence_accepted",
        });
        raw_unit_enum!(tagged, HashAlgorithmDef {
            Sha256 => "sha256",
            Sha512 => "sha512",
        });
        raw_struct_enum!(tagged, LicenseDecisionDef {
            Permissive {
                spdx_id,
                selected_dual_license_branch,
                license_file_path,
                license_file_sha256,
            } => "permissive",
            WrittenGrant {
                decision_id,
                grant_sha256,
            } => "written_grant",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, DependencyDispositionDef {
            Shipped { license } => "shipped",
            BuildOnly { license } => "build_only",
            TypeOnlyReplaced { replacement } => "type_only_replaced",
            BehaviorOnly { reason } => "behavior_only",
            Rejected { finding } => "rejected",
        });
        raw_struct_enum!(tagged, EmbeddedMaterialDispositionDef {
            Shipped { license } => "shipped",
            BehaviorOnly { reason } => "behavior_only",
            Rejected { finding } => "rejected",
        });
        (structs, tagged)
    }

    #[derive(Deserialize)]
    #[serde(transparent)]
    pub(super) struct Input(
        #[serde(with = "ConnectorSourceRecordDef")] pub(super) ConnectorSourceRecord,
    );
}

pub(crate) fn deserialize_artifact_hashes<'de, D>(
    deserializer: D,
) -> Result<Vec<ArtifactHash>, D::Error>
where
    D: Deserializer<'de>,
{
    source_record_input::deserialize_artifact_hashes(deserializer)
}

pub(crate) fn deserialize_contract_fact<'de, D>(deserializer: D) -> Result<ContractFact, D::Error>
where
    D: Deserializer<'de>,
{
    source_record_input::deserialize_contract_fact(deserializer)
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

#[derive(Clone, Debug)]
enum LosslessYamlNumber {
    I64(i64),
    U64(u64),
    F64(f64),
}

#[derive(Clone, Debug)]
enum LosslessYamlNode {
    Null,
    Bool(bool),
    Number(LosslessYamlNumber),
    String(String),
    Sequence(Vec<Self>),
    Mapping(Vec<(Self, Self)>),
}

impl LosslessYamlNode {
    fn to_yaml_value(&self) -> serde_yaml::Value {
        match self {
            Self::Null => serde_yaml::Value::Null,
            Self::Bool(value) => serde_yaml::Value::Bool(*value),
            Self::Number(LosslessYamlNumber::I64(value)) => {
                serde_yaml::to_value(value).expect("i64 is representable as YAML")
            }
            Self::Number(LosslessYamlNumber::U64(value)) => {
                serde_yaml::to_value(value).expect("u64 is representable as YAML")
            }
            Self::Number(LosslessYamlNumber::F64(value)) => {
                serde_yaml::to_value(value).expect("f64 is representable as YAML")
            }
            Self::String(value) => serde_yaml::Value::String(value.clone()),
            Self::Sequence(values) => {
                serde_yaml::Value::Sequence(values.iter().map(Self::to_yaml_value).collect())
            }
            Self::Mapping(entries) => {
                let mut mapping = serde_yaml::Mapping::new();
                for (key, value) in entries {
                    mapping.insert(key.to_yaml_value(), value.to_yaml_value());
                }
                serde_yaml::Value::Mapping(mapping)
            }
        }
    }

    fn has_duplicate_mapping_key(&self) -> bool {
        match self {
            Self::Mapping(entries) => {
                let mut keys = BTreeSet::new();
                entries.iter().any(|(key, value)| {
                    let duplicate = match key {
                        Self::String(value) => !keys.insert(value),
                        _ => false,
                    };
                    duplicate
                        || key.has_duplicate_mapping_key()
                        || value.has_duplicate_mapping_key()
                })
            }
            Self::Sequence(values) => values.iter().any(Self::has_duplicate_mapping_key),
            _ => false,
        }
    }
}

impl<'de> Deserialize<'de> for LosslessYamlNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LosslessYamlVisitor;

        impl<'de> Visitor<'de> for LosslessYamlVisitor {
            type Value = LosslessYamlNode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one YAML node")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Null)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Null)
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Number(LosslessYamlNumber::I64(value)))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Number(LosslessYamlNumber::U64(value)))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::Number(LosslessYamlNumber::F64(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(LosslessYamlNode::String(value))
            }

            fn visit_seq<A>(self, mut values: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut output = Vec::new();
                while let Some(value) = values.next_element()? {
                    output.push(value);
                }
                Ok(LosslessYamlNode::Sequence(output))
            }

            fn visit_map<A>(self, mut values: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut output = Vec::new();
                while let Some(entry) = values.next_entry()? {
                    output.push(entry);
                }
                Ok(LosslessYamlNode::Mapping(output))
            }
        }

        deserializer.deserialize_any(LosslessYamlVisitor)
    }
}

#[derive(Clone, Copy)]
struct SourceFieldShape {
    name: &'static str,
    value: &'static SourceShape,
}

#[derive(Clone, Copy)]
struct SourceVariantShape {
    kind: &'static str,
    value: &'static SourceShape,
}

enum SourceShape {
    Null,
    Bool,
    U32,
    String(SourceStringShape),
    Sequence(&'static SourceShape),
    Struct {
        declaration: Option<&'static str>,
        fields: &'static [SourceFieldShape],
    },
    Tagged {
        declaration: &'static str,
        variants: &'static [SourceVariantShape],
    },
    Nullable(&'static SourceShape),
    TypedValue,
}

#[derive(Clone, Copy)]
enum SourceStringShape {
    Any,
    Id,
    IdOrEmpty,
    Date,
    ExactHttpsUrl,
    ExactSemver,
    Git,
    Hash256,
    HashDigest,
    NonEmpty,
    NpmName,
    Path,
    TypedString,
    TypedI64,
    TypedU64,
    TypedDecimal,
    InlineBinary,
    InlineFileName,
    InlineMediaType,
    JsonPointer,
}

static NULL_SHAPE: SourceShape = SourceShape::Null;
static BOOL_SHAPE: SourceShape = SourceShape::Bool;
static U32_SHAPE: SourceShape = SourceShape::U32;
static STRING_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Any);
static ID_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Id);
static ID_OR_EMPTY_SHAPE: SourceShape = SourceShape::String(SourceStringShape::IdOrEmpty);
static DATE_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Date);
static HTTPS_SHAPE: SourceShape = SourceShape::String(SourceStringShape::ExactHttpsUrl);
static EXACT_SEMVER_SHAPE: SourceShape = SourceShape::String(SourceStringShape::ExactSemver);
static GIT_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Git);
static HASH256_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Hash256);
static HASH_DIGEST_SHAPE: SourceShape = SourceShape::String(SourceStringShape::HashDigest);
static NONEMPTY_STRING_SHAPE: SourceShape = SourceShape::String(SourceStringShape::NonEmpty);
static NPM_NAME_SHAPE: SourceShape = SourceShape::String(SourceStringShape::NpmName);
static PATH_SHAPE: SourceShape = SourceShape::String(SourceStringShape::Path);
static JSON_POINTER_SHAPE: SourceShape = SourceShape::String(SourceStringShape::JsonPointer);
static STRING_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&STRING_SHAPE);
static ID_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&ID_SHAPE);
static PATH_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&PATH_SHAPE);

macro_rules! source_struct_shape {
    ($name:ident as $declaration:literal { $($field:literal => $shape:ident),+ $(,)? }) => {
        static $name: SourceShape = SourceShape::Struct {
            declaration: Some($declaration),
            fields: &[
                $(SourceFieldShape { name: $field, value: &$shape }),+
            ],
        };
    };
    ($name:ident { $($field:literal => $shape:ident),+ $(,)? }) => {
        static $name: SourceShape = SourceShape::Struct {
            declaration: None,
            fields: &[
                $(SourceFieldShape { name: $field, value: &$shape }),+
            ],
        };
    };
}

macro_rules! source_tagged_shape {
    ($name:ident as $declaration:literal { $($kind:literal => $shape:ident),+ $(,)? }) => {
        static $name: SourceShape = SourceShape::Tagged {
            declaration: $declaration,
            variants: &[
                $(SourceVariantShape { kind: $kind, value: &$shape }),+
            ],
        };
    };
}

source_struct_shape!(IMMUTABLE_REPOSITORY_SHAPE as "ImmutableRepositoryDef" {
    "url" => HTTPS_SHAPE,
    "commit" => GIT_SHAPE,
    "tree" => GIT_SHAPE,
});
source_struct_shape!(VERIFIED_SIGNATURE_SHAPE as "VerifiedNpmSignatureDef" {
    "key_id" => ID_SHAPE,
    "signature_sha256" => HASH256_SHAPE,
});
static VERIFIED_SIGNATURE_SEQUENCE_SHAPE: SourceShape =
    SourceShape::Sequence(&VERIFIED_SIGNATURE_SHAPE);
source_struct_shape!(NPM_SIGNATURE_VERIFIED_SHAPE {
    "signatures" => VERIFIED_SIGNATURE_SEQUENCE_SHAPE,
    "registry_metadata_sha256" => HASH256_SHAPE,
});
source_struct_shape!(REGISTRY_METADATA_SHAPE {
    "registry_metadata_sha256" => HASH256_SHAPE,
});
source_struct_shape!(FINDING_SHAPE {
    "finding" => ID_SHAPE,
});
source_tagged_shape!(NPM_SIGNATURE_SHAPE as "NpmSignatureDecisionDef" {
    "verified" => NPM_SIGNATURE_VERIFIED_SHAPE,
    "verified_absent" => REGISTRY_METADATA_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(NPM_PROVENANCE_VERIFIED_SHAPE {
    "statement_sha256" => HASH256_SHAPE,
    "source_commit" => GIT_SHAPE,
});
source_tagged_shape!(NPM_PROVENANCE_SHAPE as "NpmProvenanceDecisionDef" {
    "verified" => NPM_PROVENANCE_VERIFIED_SHAPE,
    "verified_absent" => REGISTRY_METADATA_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(REPOSITORY_OWNER_CONSISTENT_SHAPE {
    "package_owner" => ID_SHAPE,
    "repository_owner" => ID_SHAPE,
});
source_struct_shape!(DECISION_ID_SHAPE {
    "decision_id" => ID_SHAPE,
});
source_tagged_shape!(REPOSITORY_OWNER_SHAPE as "RepositoryOwnerDecisionDef" {
    "consistent" => REPOSITORY_OWNER_CONSISTENT_SHAPE,
    "reviewed_mismatch" => DECISION_ID_SHAPE,
    "rejected" => FINDING_SHAPE,
});
static NULLABLE_STRING_SHAPE: SourceShape = SourceShape::Nullable(&STRING_SHAPE);
static NULLABLE_GIT_SHAPE: SourceShape = SourceShape::Nullable(&GIT_SHAPE);
source_struct_shape!(EXACT_NPM_SHAPE as "ExactNpmPackageDef" {
    "name" => NPM_NAME_SHAPE,
    "version" => EXACT_SEMVER_SHAPE,
    "tarball_url" => HTTPS_SHAPE,
    "integrity" => STRING_SHAPE,
    "repository" => IMMUTABLE_REPOSITORY_SHAPE,
    "npm_git_head" => GIT_SHAPE,
    "package_repository" => HTTPS_SHAPE,
    "signature" => NPM_SIGNATURE_SHAPE,
    "provenance" => NPM_PROVENANCE_SHAPE,
    "tag_commit" => NULLABLE_GIT_SHAPE,
    "provenance_commit" => NULLABLE_GIT_SHAPE,
    "maintainers" => ID_SEQUENCE_SHAPE,
    "repository_owner" => REPOSITORY_OWNER_SHAPE,
});

source_struct_shape!(REPOSITORY_FILE_SOURCE_SHAPE {
    "repository" => HTTPS_SHAPE,
    "commit" => GIT_SHAPE,
    "path" => PATH_SHAPE,
});
source_struct_shape!(VERSIONED_ARTIFACT_SOURCE_SHAPE {
    "url" => HTTPS_SHAPE,
    "provider_revision" => NONEMPTY_STRING_SHAPE,
});
source_tagged_shape!(PROVIDER_EVIDENCE_SOURCE_SHAPE as "ImmutableProviderEvidenceSourceDef" {
    "repository_file" => REPOSITORY_FILE_SOURCE_SHAPE,
    "versioned_artifact" => VERSIONED_ARTIFACT_SOURCE_SHAPE,
});
source_struct_shape!(PERMISSIVE_LICENSE_SHAPE {
    "spdx_id" => STRING_SHAPE,
    "selected_dual_license_branch" => NULLABLE_STRING_SHAPE,
    "license_file_path" => PATH_SHAPE,
    "license_file_sha256" => HASH256_SHAPE,
});
source_struct_shape!(WRITTEN_GRANT_SHAPE {
    "decision_id" => ID_SHAPE,
    "grant_sha256" => HASH256_SHAPE,
});
source_tagged_shape!(LICENSE_SHAPE as "LicenseDecisionDef" {
    "permissive" => PERMISSIVE_LICENSE_SHAPE,
    "written_grant" => WRITTEN_GRANT_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(PERMISSIVE_TERMS_SHAPE {
    "license" => LICENSE_SHAPE,
    "evidence_url" => HTTPS_SHAPE,
});
source_struct_shape!(REVIEWED_USE_SHAPE {
    "decision_id" => ID_SHAPE,
    "evidence_url" => HTTPS_SHAPE,
});
source_tagged_shape!(EVIDENCE_TERMS_SHAPE as "EvidenceTermsDispositionDef" {
    "permissive" => PERMISSIVE_TERMS_SHAPE,
    "reviewed_use" => REVIEWED_USE_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(JSON_POINTER_LOCATION_SHAPE {
    "path" => PATH_SHAPE,
    "pointer" => JSON_POINTER_SHAPE,
});
source_struct_shape!(DOCUMENT_SECTION_LOCATION_SHAPE {
    "path" => PATH_SHAPE,
    "section" => NONEMPTY_STRING_SHAPE,
});
source_tagged_shape!(FACT_LOCATION_SHAPE as "ExactFactLocationDef" {
    "json_pointer" => JSON_POINTER_LOCATION_SHAPE,
    "document_section" => DOCUMENT_SECTION_LOCATION_SHAPE,
});
static TYPED_VALUE_SHAPE: SourceShape = SourceShape::TypedValue;
source_struct_shape!(PROVIDER_FACT_SHAPE as "ProviderFactDef" {
    "fact_id" => ID_SHAPE,
    "location" => FACT_LOCATION_SHAPE,
    "normalized_value" => TYPED_VALUE_SHAPE,
});
static PROVIDER_FACT_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&PROVIDER_FACT_SHAPE);
source_struct_shape!(PROVIDER_EVIDENCE_SHAPE as "ProviderEvidenceArtifactDef" {
    "source" => PROVIDER_EVIDENCE_SOURCE_SHAPE,
    "accessed_on" => DATE_SHAPE,
    "content_sha256" => HASH256_SHAPE,
    "terms" => EVIDENCE_TERMS_SHAPE,
    "facts" => PROVIDER_FACT_SEQUENCE_SHAPE,
});
static PROVIDER_EVIDENCE_SEQUENCE_SHAPE: SourceShape =
    SourceShape::Sequence(&PROVIDER_EVIDENCE_SHAPE);
source_struct_shape!(PROVIDER_ARTIFACT_SHAPE as "ExactProviderArtifactDef" {
    "provider" => ID_OR_EMPTY_SHAPE,
    "evidence" => PROVIDER_EVIDENCE_SEQUENCE_SHAPE,
});

source_struct_shape!(REPO_FILE_SHAPE as "RepoFileHashDef" {
    "path" => PATH_SHAPE,
    "sha256" => HASH256_SHAPE,
});
static REPO_FILE_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&REPO_FILE_SHAPE);
source_struct_shape!(DONAT_OWNED_SHAPE as "DonatOwnedSourceDef" {
    "repository_commit" => GIT_SHAPE,
    "files" => REPO_FILE_SEQUENCE_SHAPE,
});
source_tagged_shape!(SOURCE_SUBJECT_SHAPE as "RawSourceSubject" {
    "exact_npm" => EXACT_NPM_SHAPE,
    "provider_artifact" => PROVIDER_ARTIFACT_SHAPE,
    "donat_owned" => DONAT_OWNED_SHAPE,
});
source_tagged_shape!(REACQUISITION_SHAPE as "ReacquisitionPlanDef" {
    "exact_npm_review" => NULL_SHAPE,
    "provider_repository_review" => NULL_SHAPE,
    "provider_versioned_artifact_review" => NULL_SHAPE,
    "donat_owned_no_network" => NULL_SHAPE,
});
source_tagged_shape!(HASH_ALGORITHM_SHAPE as "HashAlgorithmDef" {
    "sha256" => NULL_SHAPE,
    "sha512" => NULL_SHAPE,
});
source_struct_shape!(ARTIFACT_HASH_SHAPE as "ArtifactHashDef" {
    "artifact_id" => ID_SHAPE,
    "algorithm" => HASH_ALGORITHM_SHAPE,
    "digest" => HASH_DIGEST_SHAPE,
    "path" => NULLABLE_PATH_SHAPE,
});
static ARTIFACT_HASH_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&ARTIFACT_HASH_SHAPE);
source_struct_shape!(NOTICE_SHAPE as "NoticeIdentityDef" {
    "id" => ID_SHAPE,
    "license_file_path" => PATH_SHAPE,
    "license_file_sha256" => HASH256_SHAPE,
    "required_copyright_lines" => STRING_SEQUENCE_SHAPE,
    "notice_bundle_destination" => PATH_SHAPE,
});
source_tagged_shape!(DEPENDENCY_DISPOSITION_SHAPE as "DependencyDispositionDef" {
    "shipped" => SHIPPED_LICENSE_SHAPE,
    "build_only" => SHIPPED_LICENSE_SHAPE,
    "type_only_replaced" => REPLACEMENT_SHAPE,
    "behavior_only" => REASON_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(SHIPPED_LICENSE_SHAPE {
    "license" => LICENSE_SHAPE,
});
source_struct_shape!(REPLACEMENT_SHAPE {
    "replacement" => ID_SHAPE,
});
source_struct_shape!(REASON_SHAPE {
    "reason" => ID_SHAPE,
});
source_struct_shape!(DEPENDENCY_SHAPE as "DependencyDecisionDef" {
    "dependency" => ID_SHAPE,
    "disposition" => DEPENDENCY_DISPOSITION_SHAPE,
});
static DEPENDENCY_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&DEPENDENCY_SHAPE);
source_tagged_shape!(EMBEDDED_DISPOSITION_SHAPE as "EmbeddedMaterialDispositionDef" {
    "shipped" => SHIPPED_LICENSE_SHAPE,
    "behavior_only" => REASON_SHAPE,
    "rejected" => FINDING_SHAPE,
});
source_struct_shape!(EMBEDDED_SHAPE as "EmbeddedMaterialDecisionDef" {
    "material_id" => ID_SHAPE,
    "path" => PATH_SHAPE,
    "sha256" => HASH256_SHAPE,
    "disposition" => EMBEDDED_DISPOSITION_SHAPE,
});
static EMBEDDED_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&EMBEDDED_SHAPE);
source_struct_shape!(PROVIDER_CONTRACT_FACT_SHAPE {
    "source_record_id" => ID_SHAPE,
    "fact_id" => ID_SHAPE,
});
source_struct_shape!(POLICY_CONTRACT_FACT_SHAPE {
    "policy_id" => ID_SHAPE,
    "value" => TYPED_VALUE_SHAPE,
});
source_tagged_shape!(CONTRACT_FACT_SHAPE as "ContractFactDef" {
    "provider_evidence" => PROVIDER_CONTRACT_FACT_SHAPE,
    "donat_policy" => POLICY_CONTRACT_FACT_SHAPE,
});
static CONTRACT_FACT_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&CONTRACT_FACT_SHAPE);
source_struct_shape!(PROVIDER_CONTRACT_SHAPE as "ProviderContractReferenceDef" {
    "contract_id" => ID_SHAPE,
    "facts" => CONTRACT_FACT_SEQUENCE_SHAPE,
});
static PROVIDER_CONTRACT_SEQUENCE_SHAPE: SourceShape =
    SourceShape::Sequence(&PROVIDER_CONTRACT_SHAPE);
source_tagged_shape!(COMPATIBILITY_SHAPE as "CompatibilityDecisionDef" {
    "tier_a" => NULL_SHAPE,
    "tier_b" => NULL_SHAPE,
    "tier_c" => NULL_SHAPE,
    "rejected" => NULL_SHAPE,
});
source_struct_shape!(INVENTORY_ADMISSION_SHAPE {
    "findings" => ID_SEQUENCE_SHAPE,
});
source_struct_shape!(PORT_ADMISSION_SHAPE {
    "operations" => ID_SEQUENCE_SHAPE,
});
source_struct_shape!(EVIDENCE_ADMISSION_SHAPE {
    "contracts" => ID_SEQUENCE_SHAPE,
});
source_tagged_shape!(ADMISSION_SHAPE as "AdmissionStateDef" {
    "inventory_only" => INVENTORY_ADMISSION_SHAPE,
    "approved_for_port" => PORT_ADMISSION_SHAPE,
    "evidence_accepted" => EVIDENCE_ADMISSION_SHAPE,
});
source_struct_shape!(SAFETY_FINDING_SHAPE as "SafetyFindingDef" {
    "finding_id" => ID_SHAPE,
    "kind" => ID_SHAPE,
    "location" => NULLABLE_PATH_SHAPE,
    "message" => NONEMPTY_STRING_SHAPE,
});
static SAFETY_FINDING_SEQUENCE_SHAPE: SourceShape = SourceShape::Sequence(&SAFETY_FINDING_SHAPE);
source_struct_shape!(SAFETY_FINDINGS_SHAPE as "SafetyFindingsDef" {
    "findings" => SAFETY_FINDING_SEQUENCE_SHAPE,
});
static NULLABLE_PATH_SHAPE: SourceShape = SourceShape::Nullable(&PATH_SHAPE);
static NULLABLE_PROPOSED_MANIFEST_SHAPE: SourceShape = SourceShape::Nullable(&PATH_SHAPE);
source_struct_shape!(SOURCE_RECORD_SHAPE as "ConnectorSourceRecordDef" {
    "record_version" => U32_SHAPE,
    "record_id" => ID_SHAPE,
    "subject" => SOURCE_SUBJECT_SHAPE,
    "reacquisition" => REACQUISITION_SHAPE,
    "artifact_hashes" => ARTIFACT_HASH_SEQUENCE_SHAPE,
    "license" => LICENSE_SHAPE,
    "notice" => NOTICE_SHAPE,
    "entrypoints" => PATH_SEQUENCE_SHAPE,
    "dependencies" => DEPENDENCY_SEQUENCE_SHAPE,
    "embedded_material" => EMBEDDED_SEQUENCE_SHAPE,
    "provider_contracts" => PROVIDER_CONTRACT_SEQUENCE_SHAPE,
    "compatibility" => COMPATIBILITY_SHAPE,
    "admission" => ADMISSION_SHAPE,
    "safety_findings" => SAFETY_FINDINGS_SHAPE,
    "reviewer" => ID_SHAPE,
    "approval_date" => DATE_SHAPE,
    "proposed_manifest" => NULLABLE_PROPOSED_MANIFEST_SHAPE,
    "proposed_destinations" => PATH_SEQUENCE_SHAPE,
    "red_tests" => ID_SEQUENCE_SHAPE,
});

#[derive(Default)]
struct SourceShapeIssues {
    structure: bool,
    invalid_primitive: bool,
    material_schema: bool,
}

fn inspect_source_string(value: &str, shape: SourceStringShape, issues: &mut SourceShapeIssues) {
    let valid = match shape {
        SourceStringShape::Any => true,
        SourceStringShape::Id => valid_id(value),
        SourceStringShape::IdOrEmpty => value.is_empty() || valid_id(value),
        SourceStringShape::Date => valid_date(value),
        SourceStringShape::ExactHttpsUrl => valid_https(value),
        SourceStringShape::ExactSemver => valid_exact_semver(value),
        SourceStringShape::Git => valid_git(value),
        SourceStringShape::Hash256 => valid_hash256(value),
        SourceStringShape::HashDigest => valid_hash(value, 64) || valid_hash(value, 128),
        SourceStringShape::NonEmpty => valid_nonempty_string(value),
        SourceStringShape::NpmName => value.is_empty() || valid_npm_name(value),
        SourceStringShape::Path => valid_path(value),
        SourceStringShape::JsonPointer => valid_json_pointer(value),
        SourceStringShape::TypedString => validate_unicode_scalar_string(value).is_ok(),
        SourceStringShape::TypedI64 => value
            .parse::<i64>()
            .is_ok_and(|parsed| parsed.to_string() == value),
        SourceStringShape::TypedU64 => value
            .parse::<u64>()
            .is_ok_and(|parsed| parsed.to_string() == value),
        SourceStringShape::TypedDecimal => {
            donat_value_contract::CanonicalDecimal::try_new(value).is_ok()
        }
        SourceStringShape::InlineBinary => base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| {
                decoded.len() <= 131_072
                    && base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(decoded) == value
            }),
        SourceStringShape::InlineFileName => {
            BoundedInlineBytes::try_new(Vec::new(), "application/octet-stream", Some(value), 0)
                .is_ok()
        }
        SourceStringShape::InlineMediaType => {
            BoundedInlineBytes::try_new(Vec::new(), value, None, 0).is_ok()
        }
    };
    if !valid {
        if matches!(
            shape,
            SourceStringShape::TypedString
                | SourceStringShape::TypedI64
                | SourceStringShape::TypedU64
                | SourceStringShape::TypedDecimal
                | SourceStringShape::InlineBinary
                | SourceStringShape::InlineFileName
                | SourceStringShape::InlineMediaType
        ) {
            issues.material_schema = true;
        } else {
            issues.invalid_primitive = true;
        }
    }
}

fn inspect_lossless_u32(node: &LosslessYamlNode, issues: &mut SourceShapeIssues) {
    let valid = match node {
        LosslessYamlNode::Number(LosslessYamlNumber::I64(value)) => u32::try_from(*value).is_ok(),
        LosslessYamlNode::Number(LosslessYamlNumber::U64(value)) => u32::try_from(*value).is_ok(),
        _ => false,
    };
    if !valid {
        issues.invalid_primitive = true;
    }
}

fn mapping_entries<'node>(
    node: &'node LosslessYamlNode,
    issues: &mut SourceShapeIssues,
) -> Option<&'node [(LosslessYamlNode, LosslessYamlNode)]> {
    let LosslessYamlNode::Mapping(entries) = node else {
        issues.structure = true;
        return None;
    };
    Some(entries)
}

fn inspect_struct_shape(
    node: &LosslessYamlNode,
    fields: &[SourceFieldShape],
    issues: &mut SourceShapeIssues,
) {
    let Some(entries) = mapping_entries(node, issues) else {
        return;
    };
    for field in fields {
        if !entries
            .iter()
            .any(|(key, _)| matches!(key, LosslessYamlNode::String(name) if name == field.name))
        {
            issues.structure = true;
        }
    }
    for (key, value) in entries {
        let LosslessYamlNode::String(name) = key else {
            issues.structure = true;
            continue;
        };
        let Some(field) = fields.iter().find(|field| field.name == name) else {
            issues.structure = true;
            continue;
        };
        inspect_source_shape(value, field.value, issues);
    }
}

fn inspect_tagged_shape(
    node: &LosslessYamlNode,
    variants: &[SourceVariantShape],
    issues: &mut SourceShapeIssues,
) {
    let Some(entries) = mapping_entries(node, issues) else {
        return;
    };
    let kinds = entries
        .iter()
        .filter_map(|(key, value)| {
            matches!(key, LosslessYamlNode::String(name) if name == "kind").then_some(value)
        })
        .collect::<Vec<_>>();
    let values = entries
        .iter()
        .filter_map(|(key, value)| {
            matches!(key, LosslessYamlNode::String(name) if name == "value").then_some(value)
        })
        .collect::<Vec<_>>();
    if kinds.is_empty() || values.is_empty() {
        issues.structure = true;
    }
    if entries.iter().any(|(key, _)| {
        !matches!(key, LosslessYamlNode::String(name) if name == "kind" || name == "value")
    }) {
        issues.structure = true;
    }
    let mut selected_variant = None;
    for kind in kinds {
        let LosslessYamlNode::String(kind) = kind else {
            issues.structure = true;
            continue;
        };
        let Some(variant) = variants.iter().find(|variant| variant.kind == kind) else {
            issues.structure = true;
            continue;
        };
        selected_variant.get_or_insert(variant);
    }
    let Some(variant) = selected_variant else {
        return;
    };
    for value in values {
        inspect_source_shape(value, variant.value, issues);
    }
}

fn inspect_typed_value(node: &LosslessYamlNode, issues: &mut SourceShapeIssues) {
    static TYPED_STRING_SHAPE: SourceShape = SourceShape::String(SourceStringShape::TypedString);
    static TYPED_I64_SHAPE: SourceShape = SourceShape::String(SourceStringShape::TypedI64);
    static TYPED_U64_SHAPE: SourceShape = SourceShape::String(SourceStringShape::TypedU64);
    static TYPED_DECIMAL_SHAPE: SourceShape = SourceShape::String(SourceStringShape::TypedDecimal);
    static INLINE_BINARY_SHAPE: SourceShape = SourceShape::String(SourceStringShape::InlineBinary);
    static INLINE_FILE_NAME_SHAPE: SourceShape =
        SourceShape::String(SourceStringShape::InlineFileName);
    static INLINE_MEDIA_TYPE_SHAPE: SourceShape =
        SourceShape::String(SourceStringShape::InlineMediaType);
    static NULLABLE_INLINE_FILE_NAME_SHAPE: SourceShape =
        SourceShape::Nullable(&INLINE_FILE_NAME_SHAPE);
    static NULLABLE_INLINE_MEDIA_TYPE_SHAPE: SourceShape =
        SourceShape::Nullable(&INLINE_MEDIA_TYPE_SHAPE);
    static INLINE_BYTES_SHAPE: SourceShape = SourceShape::Struct {
        declaration: None,
        fields: &[
            SourceFieldShape {
                name: "$binary",
                value: &INLINE_BINARY_SHAPE,
            },
            SourceFieldShape {
                name: "file_name",
                value: &NULLABLE_INLINE_FILE_NAME_SHAPE,
            },
            SourceFieldShape {
                name: "media_type",
                value: &NULLABLE_INLINE_MEDIA_TYPE_SHAPE,
            },
        ],
    };
    let Some(entries) = mapping_entries(node, issues) else {
        return;
    };
    let kinds = entries
        .iter()
        .filter_map(|(key, value)| {
            matches!(key, LosslessYamlNode::String(name) if name == "kind").then_some(value)
        })
        .collect::<Vec<_>>();
    let values = entries
        .iter()
        .filter_map(|(key, value)| {
            matches!(key, LosslessYamlNode::String(name) if name == "value").then_some(value)
        })
        .collect::<Vec<_>>();
    if kinds.is_empty()
        || values.is_empty()
        || entries.iter().any(|(key, _)| {
            !matches!(key, LosslessYamlNode::String(name) if name == "kind" || name == "value")
        })
    {
        issues.structure = true;
    }
    let mut selected_kind = None;
    for kind in kinds {
        let LosslessYamlNode::String(kind) = kind else {
            issues.structure = true;
            continue;
        };
        if !matches!(
            kind.as_str(),
            "null"
                | "boolean"
                | "string"
                | "i64"
                | "u64"
                | "decimal"
                | "inline_bytes"
                | "list"
                | "object"
        ) {
            issues.structure = true;
            continue;
        }
        selected_kind.get_or_insert(kind.as_str());
    }
    let Some(kind) = selected_kind else {
        return;
    };
    let scalar_shape = match kind {
        "null" => Some(&NULL_SHAPE),
        "boolean" => Some(&BOOL_SHAPE),
        "string" => Some(&TYPED_STRING_SHAPE),
        "i64" => Some(&TYPED_I64_SHAPE),
        "u64" => Some(&TYPED_U64_SHAPE),
        "decimal" => Some(&TYPED_DECIMAL_SHAPE),
        "inline_bytes" => Some(&INLINE_BYTES_SHAPE),
        "list" | "object" => None,
        _ => {
            issues.structure = true;
            return;
        }
    };
    for value in values {
        match kind {
            "list" => match value {
                LosslessYamlNode::Sequence(values) => {
                    for value in values {
                        inspect_typed_value(value, issues);
                    }
                }
                _ => issues.structure = true,
            },
            "object" => match value {
                LosslessYamlNode::Mapping(entries) => {
                    for (key, value) in entries {
                        match key {
                            LosslessYamlNode::String(name) => {
                                if validate_unicode_scalar_string(name).is_err() {
                                    issues.material_schema = true;
                                }
                            }
                            _ => issues.structure = true,
                        }
                        inspect_typed_value(value, issues);
                    }
                }
                _ => issues.structure = true,
            },
            _ => inspect_source_shape(
                value,
                scalar_shape.expect("all scalar branches select a shape"),
                issues,
            ),
        }
    }
}

fn inspect_source_shape(
    node: &LosslessYamlNode,
    shape: &SourceShape,
    issues: &mut SourceShapeIssues,
) {
    match shape {
        SourceShape::Null if matches!(node, LosslessYamlNode::Null) => {}
        SourceShape::Bool if matches!(node, LosslessYamlNode::Bool(_)) => {}
        SourceShape::U32 => inspect_lossless_u32(node, issues),
        SourceShape::String(shape) => match node {
            LosslessYamlNode::String(value) => inspect_source_string(value, *shape, issues),
            _ => issues.invalid_primitive = true,
        },
        SourceShape::Null | SourceShape::Bool => issues.invalid_primitive = true,
        SourceShape::Sequence(value_shape) => match node {
            LosslessYamlNode::Sequence(values) => {
                for value in values {
                    inspect_source_shape(value, value_shape, issues);
                }
            }
            _ => issues.structure = true,
        },
        SourceShape::Struct {
            declaration,
            fields,
        } => {
            let _ = declaration;
            inspect_struct_shape(node, fields, issues);
        }
        SourceShape::Tagged {
            declaration,
            variants,
        } => {
            let _ = declaration;
            inspect_tagged_shape(node, variants, issues);
        }
        SourceShape::Nullable(value_shape) => {
            if !matches!(node, LosslessYamlNode::Null) {
                inspect_source_shape(node, value_shape, issues);
            }
        }
        SourceShape::TypedValue => inspect_typed_value(node, issues),
    }
}

fn lossless_mapping(node: &LosslessYamlNode) -> &[(LosslessYamlNode, LosslessYamlNode)] {
    let LosslessYamlNode::Mapping(entries) = node else {
        unreachable!("lossless source structure was validated before staged access");
    };
    entries
}

fn lossless_field<'node>(node: &'node LosslessYamlNode, name: &str) -> &'node LosslessYamlNode {
    lossless_mapping(node)
        .iter()
        .find_map(|(key, value)| {
            matches!(key, LosslessYamlNode::String(candidate) if candidate == name).then_some(value)
        })
        .unwrap_or_else(|| {
            unreachable!("lossless source member was validated before staged access: {name}")
        })
}

fn lossless_string(node: &LosslessYamlNode) -> &str {
    let LosslessYamlNode::String(value) = node else {
        unreachable!("lossless source scalar kind was validated before staged access");
    };
    value
}

fn lossless_sequence(node: &LosslessYamlNode) -> &[LosslessYamlNode] {
    let LosslessYamlNode::Sequence(values) = node else {
        unreachable!("lossless source sequence was validated before staged access");
    };
    values
}

fn lossless_tagged(node: &LosslessYamlNode) -> (&str, &LosslessYamlNode) {
    (
        lossless_string(lossless_field(node, "kind")),
        lossless_field(node, "value"),
    )
}

fn lossless_fields<'node>(
    node: &'node LosslessYamlNode,
    name: &'node str,
) -> impl Iterator<Item = &'node LosslessYamlNode> {
    lossless_mapping(node)
        .iter()
        .filter_map(move |(key, value)| {
            matches!(key, LosslessYamlNode::String(candidate) if candidate == name).then_some(value)
        })
}

fn lossless_is_zero_u32(node: &LosslessYamlNode) -> bool {
    matches!(
        node,
        LosslessYamlNode::Number(LosslessYamlNumber::I64(0))
            | LosslessYamlNode::Number(LosslessYamlNumber::U64(0))
    )
}

fn validate_lossless_required_structure(node: &LosslessYamlNode) -> Result<(), CatalogError> {
    if lossless_is_zero_u32(lossless_field(node, "record_version"))
        || lossless_sequence(lossless_field(node, "entrypoints")).is_empty()
        || lossless_sequence(lossless_field(node, "proposed_destinations")).is_empty()
        || lossless_sequence(lossless_field(node, "red_tests")).is_empty()
        || matches!(
            lossless_field(node, "reviewer"),
            LosslessYamlNode::String(value) if value.is_empty()
        )
    {
        return source_error(
            "source_record_incomplete",
            "required source-record collection or review identity is empty",
        );
    }

    let (subject_kind, subject) = lossless_tagged(lossless_field(node, "subject"));
    match subject_kind {
        "exact_npm"
            if matches!(
                lossless_field(subject, "name"),
                LosslessYamlNode::String(value) if value.is_empty()
            ) || lossless_sequence(lossless_field(subject, "maintainers")).is_empty() =>
        {
            source_error(
                "source_record_incomplete",
                "exact npm identity and maintainer inventory must be nonempty",
            )
        }
        "provider_artifact"
            if matches!(
                lossless_field(subject, "provider"),
                LosslessYamlNode::String(value) if value.is_empty()
            ) =>
        {
            source_error(
                "source_record_incomplete",
                "provider identity must be nonempty",
            )
        }
        "donat_owned" if lossless_sequence(lossless_field(subject, "files")).is_empty() => {
            source_error(
                "source_record_incomplete",
                "Donat-owned file inventory must be nonempty",
            )
        }
        _ => Ok(()),
    }
}

fn validate_lossless_contextual_primitives(node: &LosslessYamlNode) -> Result<(), CatalogError> {
    for artifact in lossless_sequence(lossless_field(node, "artifact_hashes")) {
        let algorithms = lossless_fields(artifact, "algorithm")
            .map(lossless_tagged)
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>();
        for digest in lossless_fields(artifact, "digest").map(lossless_string) {
            let matches_an_algorithm = algorithms.iter().any(|algorithm| match *algorithm {
                "sha256" => valid_hash(digest, 64),
                "sha512" => valid_hash(digest, 128),
                _ => unreachable!("lossless hash algorithm was structurally validated"),
            });
            if !matches_an_algorithm {
                return invalid_primitive("artifact digest does not match its hash algorithm");
            }
        }
    }
    Ok(())
}

fn validate_lossless_license_legal(node: &LosslessYamlNode) -> Result<(), CatalogError> {
    let (kind, value) = lossless_tagged(node);
    match kind {
        "permissive" => validate_permissive_license_legal(
            lossless_string(lossless_field(value, "spdx_id")),
            match lossless_field(value, "selected_dual_license_branch") {
                LosslessYamlNode::Null => None,
                value => Some(lossless_string(value)),
            },
        ),
        "written_grant" => Ok(()),
        "rejected" => legal_mismatch("license is rejected"),
        _ => unreachable!("lossless license branch was structurally validated"),
    }
}

fn validate_lossless_legal_state(node: &LosslessYamlNode) -> Result<(), CatalogError> {
    let license = lossless_field(node, "license");
    validate_lossless_license_legal(license)?;
    let (license_kind, license_value) = lossless_tagged(license);
    if license_kind == "permissive" {
        let notice = lossless_field(node, "notice");
        if lossless_string(lossless_field(license_value, "license_file_path"))
            != lossless_string(lossless_field(notice, "license_file_path"))
            || lossless_string(lossless_field(license_value, "license_file_sha256"))
                != lossless_string(lossless_field(notice, "license_file_sha256"))
        {
            return legal_mismatch("license and notice identities disagree");
        }
    }

    for dependency in lossless_sequence(lossless_field(node, "dependencies")) {
        let (kind, value) = lossless_tagged(lossless_field(dependency, "disposition"));
        if matches!(kind, "shipped" | "build_only") {
            validate_lossless_license_legal(lossless_field(value, "license"))?;
        }
    }
    for embedded in lossless_sequence(lossless_field(node, "embedded_material")) {
        let (kind, value) = lossless_tagged(lossless_field(embedded, "disposition"));
        if kind == "shipped" {
            validate_lossless_license_legal(lossless_field(value, "license"))?;
        }
    }

    let (subject_kind, subject) = lossless_tagged(lossless_field(node, "subject"));
    if subject_kind == "provider_artifact" {
        for evidence in lossless_sequence(lossless_field(subject, "evidence")) {
            let (kind, value) = lossless_tagged(lossless_field(evidence, "terms"));
            match kind {
                "permissive" => {
                    validate_lossless_license_legal(lossless_field(value, "license"))?;
                }
                "reviewed_use" => {}
                "rejected" => {
                    return legal_mismatch("provider evidence terms are rejected");
                }
                _ => unreachable!("lossless evidence-terms branch was structurally validated"),
            }
        }
    }
    Ok(())
}

fn lossless_unique<'node>(
    values: impl IntoIterator<Item = &'node str>,
) -> Result<(), CatalogError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return duplicate(value.to_owned());
        }
    }
    Ok(())
}

fn lossless_member_string<'node>(node: &'node LosslessYamlNode, name: &str) -> &'node str {
    lossless_string(lossless_field(node, name))
}

fn lossless_contract_fact_key(fact: &LosslessYamlNode) -> String {
    let (kind, value) = lossless_tagged(fact);
    match kind {
        "provider_evidence" => format!(
            "provider:{}:{}",
            lossless_member_string(value, "source_record_id"),
            lossless_member_string(value, "fact_id")
        ),
        "donat_policy" => format!("policy:{}", lossless_member_string(value, "policy_id")),
        _ => unreachable!("lossless contract-fact branch was structurally validated"),
    }
}

fn lossless_provider_source_key(source: &LosslessYamlNode) -> String {
    let (kind, value) = lossless_tagged(source);
    match kind {
        "repository_file" => format!(
            "repository:{}\0{}\0{}",
            lossless_member_string(value, "repository"),
            lossless_member_string(value, "commit"),
            lossless_member_string(value, "path")
        ),
        "versioned_artifact" => format!(
            "artifact:{}\0{}",
            lossless_member_string(value, "url"),
            lossless_member_string(value, "provider_revision")
        ),
        _ => unreachable!("lossless provider-source branch was structurally validated"),
    }
}

fn validate_lossless_duplicates(node: &LosslessYamlNode) -> Result<(), CatalogError> {
    lossless_unique(
        lossless_sequence(lossless_field(node, "entrypoints"))
            .iter()
            .map(lossless_string),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "artifact_hashes"))
            .iter()
            .map(|artifact| lossless_member_string(artifact, "artifact_id")),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "dependencies"))
            .iter()
            .map(|dependency| lossless_member_string(dependency, "dependency")),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "embedded_material"))
            .iter()
            .map(|embedded| lossless_member_string(embedded, "material_id")),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "provider_contracts"))
            .iter()
            .map(|contract| lossless_member_string(contract, "contract_id")),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "proposed_destinations"))
            .iter()
            .map(lossless_string),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(node, "red_tests"))
            .iter()
            .map(lossless_string),
    )?;
    lossless_unique(
        lossless_sequence(lossless_field(
            lossless_field(node, "safety_findings"),
            "findings",
        ))
        .iter()
        .map(|finding| lossless_member_string(finding, "finding_id")),
    )?;

    let (admission_kind, admission) = lossless_tagged(lossless_field(node, "admission"));
    let admission_member = match admission_kind {
        "inventory_only" => "findings",
        "approved_for_port" => "operations",
        "evidence_accepted" => "contracts",
        _ => unreachable!("lossless admission branch was structurally validated"),
    };
    lossless_unique(
        lossless_sequence(lossless_field(admission, admission_member))
            .iter()
            .map(lossless_string),
    )?;

    let (subject_kind, subject) = lossless_tagged(lossless_field(node, "subject"));
    match subject_kind {
        "exact_npm" => {
            lossless_unique(
                lossless_sequence(lossless_field(subject, "maintainers"))
                    .iter()
                    .map(lossless_string),
            )?;
            let (signature_kind, signature) = lossless_tagged(lossless_field(subject, "signature"));
            if signature_kind == "verified" {
                lossless_unique(
                    lossless_sequence(lossless_field(signature, "signatures"))
                        .iter()
                        .map(|value| lossless_member_string(value, "key_id")),
                )?;
            }
        }
        "provider_artifact" => {
            let mut evidence_keys = BTreeSet::new();
            let mut fact_ids = BTreeSet::new();
            for evidence in lossless_sequence(lossless_field(subject, "evidence")) {
                let key = (
                    lossless_provider_source_key(lossless_field(evidence, "source")),
                    lossless_member_string(evidence, "content_sha256"),
                );
                if !evidence_keys.insert(key) {
                    return duplicate("provider evidence");
                }
                for fact in lossless_sequence(lossless_field(evidence, "facts")) {
                    if !fact_ids.insert(lossless_member_string(fact, "fact_id")) {
                        return duplicate("provider fact");
                    }
                }
            }
            for contract in lossless_sequence(lossless_field(node, "provider_contracts")) {
                let keys = lossless_sequence(lossless_field(contract, "facts"))
                    .iter()
                    .map(lossless_contract_fact_key)
                    .collect::<Vec<_>>();
                lossless_unique(keys.iter().map(String::as_str))?;
            }
        }
        "donat_owned" => {
            lossless_unique(
                lossless_sequence(lossless_field(subject, "files"))
                    .iter()
                    .map(|file| lossless_member_string(file, "path")),
            )?;
        }
        _ => unreachable!("lossless source-subject branch was structurally validated"),
    }
    Ok(())
}

pub fn load_record(path: impl AsRef<Path>) -> Result<ConnectorSourceRecord, CatalogError> {
    let bytes = std::fs::read(path)
        .map_err(|error| CatalogError::new("source_record_incomplete", error.to_string()))?;
    load_record_bytes(&bytes)
}

pub fn load_record_bytes(bytes: &[u8]) -> Result<ConnectorSourceRecord, CatalogError> {
    let parsed =
        serde_yaml::from_slice::<LosslessYamlNode>(bytes).map_err(map_source_decode_error)?;
    let mut issues = SourceShapeIssues::default();
    inspect_source_shape(&parsed, &SOURCE_RECORD_SHAPE, &mut issues);
    if issues.structure {
        return Err(CatalogError::new(
            "source_record_incomplete",
            "source record has an unknown, missing, or malformed structural member",
        ));
    }
    validate_lossless_required_structure(&parsed)?;
    if issues.invalid_primitive {
        return invalid_primitive("source record scalar kind");
    }
    if issues.material_schema {
        return Err(material_schema_error());
    }
    validate_lossless_contextual_primitives(&parsed)?;
    if parsed.has_duplicate_mapping_key() {
        return duplicate("source mapping member");
    }
    validate_lossless_duplicates(&parsed)?;
    validate_lossless_legal_state(&parsed)?;

    let deduplicated = parsed.to_yaml_value();
    let record = serde_yaml::from_value::<source_record_input::Input>(deduplicated)
        .map_err(map_source_decode_error)?
        .0;
    validate_record_after_lossless_stages(&record)?;
    Ok(record)
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
    validate_record_after_lossless_stages(record)
}

fn validate_record_after_lossless_stages(
    record: &ConnectorSourceRecord,
) -> Result<(), CatalogError> {
    validate_record_evidence(record)?;
    validate_record_admission(record)
}

fn map_source_decode_error(error: serde_yaml::Error) -> CatalogError {
    let detail = error.to_string();
    for code in [
        "catalog_jcs_schema_mismatch",
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
            if !referenced_facts.insert(*fact_id) {
                return evidence_mismatch(
                    "provider fact belongs to more than one admitted contract",
                );
            }
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
    let path = parsed.path().strip_prefix('/').ok_or_else(|| {
        CatalogError::new(
            "source_record_evidence_mismatch",
            "versioned evidence URL has no canonical relative artifact path",
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
    match license {
        LicenseDecision::Permissive {
            spdx_id,
            selected_dual_license_branch,
            ..
        } => validate_permissive_license_legal(spdx_id, selected_dual_license_branch.as_deref()),
        LicenseDecision::WrittenGrant { .. } => Ok(()),
        LicenseDecision::Rejected { .. } => legal_mismatch("license is rejected"),
    }
}

fn validate_permissive_license_legal(
    spdx_id: &str,
    selected_dual_license_branch: Option<&str>,
) -> Result<(), CatalogError> {
    const ALLOWED: &[&str] = &[
        "MIT",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "ISC",
        "0BSD",
    ];
    if ALLOWED.contains(&spdx_id) {
        if selected_dual_license_branch.is_some() {
            return legal_mismatch("single license cannot select a dual branch");
        }
        return Ok(());
    }
    let branches = spdx_id.split(" OR ").collect::<Vec<_>>();
    if branches.len() < 2
        || branches.iter().any(|branch| !ALLOWED.contains(branch))
        || selected_dual_license_branch.is_none_or(|selected| !branches.contains(&selected))
    {
        return legal_mismatch("Phase-1 permissive license decision");
    }
    Ok(())
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

#[cfg(test)]
mod declaration_totality_tests {
    use super::*;

    type StructInventory = BTreeMap<&'static str, BTreeSet<&'static str>>;
    type TaggedInventory = BTreeMap<&'static str, BTreeMap<&'static str, BTreeSet<&'static str>>>;

    fn typed_value_raw_declaration() -> RawTaggedDeclaration {
        #[allow(dead_code)]
        fn exhaustive(value: TypedValueMaterial) {
            match value {
                TypedValueMaterial::Null => {}
                TypedValueMaterial::Boolean(_) => {}
                TypedValueMaterial::String(_) => {}
                TypedValueMaterial::I64(_) => {}
                TypedValueMaterial::U64(_) => {}
                TypedValueMaterial::Decimal(_) => {}
                TypedValueMaterial::List(_) => {}
                TypedValueMaterial::Object(_) => {}
                TypedValueMaterial::InlineBytes {
                    binary: _,
                    file_name: _,
                    media_type: _,
                } => {}
            }
        }
        RawTaggedDeclaration {
            name: "TypedValueMaterial",
            variants: vec![
                RawVariantDeclaration {
                    tag: "null",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "boolean",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "string",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "i64",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "u64",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "decimal",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "list",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "object",
                    fields: Vec::new(),
                },
                RawVariantDeclaration {
                    tag: "inline_bytes",
                    fields: vec!["$binary", "file_name", "media_type"],
                },
            ],
        }
    }

    fn declared_inventory() -> (StructInventory, TaggedInventory) {
        let (structs, mut tagged) = source_record_input::raw_declaration_inventory();
        tagged.push(typed_value_raw_declaration());
        let structs = structs
            .into_iter()
            .map(|declaration| (declaration.name, declaration.fields.into_iter().collect()))
            .collect();
        let tagged = tagged
            .into_iter()
            .map(|declaration| {
                (
                    declaration.name,
                    declaration
                        .variants
                        .into_iter()
                        .map(|variant| (variant.tag, variant.fields.into_iter().collect()))
                        .collect(),
                )
            })
            .collect();
        (structs, tagged)
    }

    fn shape_inventory() -> (StructInventory, TaggedInventory) {
        fn visit(
            shape: &'static SourceShape,
            structs: &mut StructInventory,
            tagged: &mut TaggedInventory,
            visited: &mut BTreeSet<usize>,
        ) {
            let identity = std::ptr::from_ref(shape) as usize;
            if !visited.insert(identity) {
                return;
            }
            match shape {
                SourceShape::Null
                | SourceShape::Bool
                | SourceShape::U32
                | SourceShape::String(_) => {}
                SourceShape::Sequence(value) | SourceShape::Nullable(value) => {
                    visit(value, structs, tagged, visited);
                }
                SourceShape::Struct {
                    declaration,
                    fields,
                } => {
                    if let Some(declaration) = declaration {
                        let prior = structs
                            .insert(declaration, fields.iter().map(|field| field.name).collect());
                        assert!(prior.is_none(), "duplicate struct shape {declaration}");
                    }
                    for field in *fields {
                        visit(field.value, structs, tagged, visited);
                    }
                }
                SourceShape::Tagged {
                    declaration,
                    variants,
                } => {
                    let branches = variants
                        .iter()
                        .map(|variant| {
                            let fields = match variant.value {
                                SourceShape::Struct {
                                    declaration: None,
                                    fields,
                                } => fields.iter().map(|field| field.name).collect(),
                                _ => BTreeSet::new(),
                            };
                            (variant.kind, fields)
                        })
                        .collect();
                    let prior = tagged.insert(declaration, branches);
                    assert!(prior.is_none(), "duplicate tagged shape {declaration}");
                    for variant in *variants {
                        visit(variant.value, structs, tagged, visited);
                    }
                }
                SourceShape::TypedValue => {
                    tagged.insert(
                        "TypedValueMaterial",
                        [
                            ("null", BTreeSet::new()),
                            ("boolean", BTreeSet::new()),
                            ("string", BTreeSet::new()),
                            ("i64", BTreeSet::new()),
                            ("u64", BTreeSet::new()),
                            ("decimal", BTreeSet::new()),
                            ("list", BTreeSet::new()),
                            ("object", BTreeSet::new()),
                            (
                                "inline_bytes",
                                ["$binary", "file_name", "media_type"].into_iter().collect(),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    );
                }
            }
        }

        let mut structs = BTreeMap::new();
        let mut tagged = BTreeMap::new();
        visit(
            &SOURCE_RECORD_SHAPE,
            &mut structs,
            &mut tagged,
            &mut BTreeSet::new(),
        );
        (structs, tagged)
    }

    fn case_coverage(
        node: &LosslessYamlNode,
        shape: &'static SourceShape,
        structs: &mut BTreeSet<&'static str>,
        tagged: &mut BTreeSet<(&'static str, String)>,
    ) {
        match shape {
            SourceShape::Null | SourceShape::Bool | SourceShape::U32 | SourceShape::String(_) => {}
            SourceShape::Sequence(value_shape) => {
                for value in lossless_sequence(node) {
                    case_coverage(value, value_shape, structs, tagged);
                }
            }
            SourceShape::Struct {
                declaration,
                fields,
            } => {
                if let Some(declaration) = declaration {
                    structs.insert(declaration);
                }
                for field in *fields {
                    case_coverage(
                        lossless_field(node, field.name),
                        field.value,
                        structs,
                        tagged,
                    );
                }
            }
            SourceShape::Tagged {
                declaration,
                variants,
            } => {
                let (kind, value) = lossless_tagged(node);
                tagged.insert((declaration, kind.to_owned()));
                let variant = variants
                    .iter()
                    .find(|variant| variant.kind == kind)
                    .expect("case branch is declared");
                case_coverage(value, variant.value, structs, tagged);
            }
            SourceShape::Nullable(value_shape) => {
                if !matches!(node, LosslessYamlNode::Null) {
                    case_coverage(node, value_shape, structs, tagged);
                }
            }
            SourceShape::TypedValue => {
                let (kind, value) = lossless_tagged(node);
                tagged.insert(("TypedValueMaterial", kind.to_owned()));
                match kind {
                    "list" => {
                        for value in lossless_sequence(value) {
                            case_coverage(value, &TYPED_VALUE_SHAPE, structs, tagged);
                        }
                    }
                    "object" => {
                        for (_, value) in lossless_mapping(value) {
                            case_coverage(value, &TYPED_VALUE_SHAPE, structs, tagged);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    struct BranchCase {
        name: &'static str,
        document: serde_json::Value,
        expected_error: Option<&'static str>,
    }

    fn fixture_value(source: &str) -> serde_json::Value {
        serde_yaml::from_str(source).unwrap()
    }

    fn branch_cases() -> Vec<BranchCase> {
        let npm = fixture_value(include_str!("../tests/fixtures/serpapi-npm-record.yaml"));
        let provider = fixture_value(include_str!(
            "../tests/fixtures/provider-contract-record.yaml"
        ));
        let donat = fixture_value(include_str!("../tests/fixtures/donat-owned-record.yaml"));
        let permissive_license = serde_json::json!({
            "kind": "permissive",
            "value": {
                "spdx_id": "MIT",
                "selected_dual_license_branch": null,
                "license_file_path": "LICENSE",
                "license_file_sha256":
                    "2222222222222222222222222222222222222222222222222222222222222222"
            }
        });
        let written_grant = serde_json::json!({
            "kind": "written_grant",
            "value": {
                "decision_id": "review.written.grant",
                "grant_sha256":
                    "3333333333333333333333333333333333333333333333333333333333333333"
            }
        });

        let mut npm_collections = npm.clone();
        npm_collections["dependencies"] = serde_json::json!([
            {
                "dependency": "dependency.shipped",
                "disposition": {
                    "kind": "shipped",
                    "value": {"license": permissive_license.clone()}
                }
            },
            {
                "dependency": "dependency.build",
                "disposition": {
                    "kind": "build_only",
                    "value": {"license": written_grant.clone()}
                }
            },
            {
                "dependency": "dependency.type",
                "disposition": {
                    "kind": "type_only_replaced",
                    "value": {"replacement": "donat.value.contract"}
                }
            },
            {
                "dependency": "dependency.behavior",
                "disposition": {
                    "kind": "behavior_only",
                    "value": {"reason": "finding.behavior.only"}
                }
            },
            {
                "dependency": "dependency.rejected",
                "disposition": {
                    "kind": "rejected",
                    "value": {"finding": "finding.dependency.rejected"}
                }
            }
        ]);
        npm_collections["embedded_material"] = serde_json::json!([
            {
                "material_id": "embedded.shipped",
                "path": "embedded/shipped.json",
                "sha256":
                    "4444444444444444444444444444444444444444444444444444444444444444",
                "disposition": {
                    "kind": "shipped",
                    "value": {"license": permissive_license.clone()}
                }
            },
            {
                "material_id": "embedded.behavior",
                "path": "embedded/behavior.json",
                "sha256":
                    "5555555555555555555555555555555555555555555555555555555555555555",
                "disposition": {
                    "kind": "behavior_only",
                    "value": {"reason": "finding.embedded.behavior"}
                }
            },
            {
                "material_id": "embedded.rejected",
                "path": "embedded/rejected.json",
                "sha256":
                    "6666666666666666666666666666666666666666666666666666666666666666",
                "disposition": {
                    "kind": "rejected",
                    "value": {"finding": "finding.embedded.rejected"}
                }
            }
        ]);
        npm_collections["provider_contracts"] = serde_json::json!([{
            "contract_id": "contract.policy.values",
            "facts": [
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.null",
                        "value": {"kind": "null", "value": null}
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.boolean",
                        "value": {"kind": "boolean", "value": true}
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.i64",
                        "value": {"kind": "i64", "value": "-1"}
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.u64",
                        "value": {"kind": "u64", "value": "1"}
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.decimal",
                        "value": {"kind": "decimal", "value": "1.5"}
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.list",
                        "value": {
                            "kind": "list",
                            "value": [{"kind": "string", "value": "list-value"}]
                        }
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.object",
                        "value": {
                            "kind": "object",
                            "value": {
                                "member": {"kind": "string", "value": "object-value"}
                            }
                        }
                    }
                },
                {
                    "kind": "donat_policy",
                    "value": {
                        "policy_id": "policy.inline",
                        "value": {
                            "kind": "inline_bytes",
                            "value": {
                                "$binary": "Wg",
                                "file_name": null,
                                "media_type": "application/octet-stream"
                            }
                        }
                    }
                }
            ]
        }]);
        npm_collections["compatibility"] = serde_json::json!({"kind": "tier_b", "value": null});

        let mut npm_absent_mismatch_written = npm.clone();
        npm_absent_mismatch_written["subject"]["value"]["signature"] = serde_json::json!({
            "kind": "verified_absent",
            "value": {
                "registry_metadata_sha256":
                    "8192a3b4c5d6e7f8091a2b3c4d5e6f8091a2b3c4d5e6f708192a3b4c5d6e7f90"
            }
        });
        npm_absent_mismatch_written["subject"]["value"]["repository_owner"] = serde_json::json!({
            "kind": "reviewed_mismatch",
            "value": {"decision_id": "review.owner.mismatch"}
        });
        npm_absent_mismatch_written["license"] = written_grant.clone();
        npm_absent_mismatch_written["compatibility"] =
            serde_json::json!({"kind": "tier_c", "value": null});

        let mut npm_rejected_verified = npm.clone();
        npm_rejected_verified["subject"]["value"]["signature"] = serde_json::json!({
            "kind": "rejected",
            "value": {"finding": "finding.signature.rejected"}
        });
        npm_rejected_verified["subject"]["value"]["provenance"] = serde_json::json!({
            "kind": "verified",
            "value": {
                "statement_sha256":
                    "7777777777777777777777777777777777777777777777777777777777777777",
                "source_commit": "0123456789abcdef0123456789abcdef01234567"
            }
        });
        npm_rejected_verified["subject"]["value"]["provenance_commit"] =
            serde_json::json!("0123456789abcdef0123456789abcdef01234567");
        npm_rejected_verified["subject"]["value"]["repository_owner"] = serde_json::json!({
            "kind": "rejected",
            "value": {"finding": "finding.owner.rejected"}
        });
        npm_rejected_verified["compatibility"] =
            serde_json::json!({"kind": "rejected", "value": null});

        let mut npm_provenance_rejected = npm.clone();
        npm_provenance_rejected["subject"]["value"]["provenance"] = serde_json::json!({
            "kind": "rejected",
            "value": {"finding": "finding.provenance.rejected"}
        });

        let mut provider_permissive = provider.clone();
        provider_permissive["subject"]["value"]["evidence"][0]["terms"] = serde_json::json!({
            "kind": "permissive",
            "value": {
                "license": permissive_license.clone(),
                "evidence_url": "https://example.test/terms/permissive"
            }
        });

        let mut provider_versioned = provider.clone();
        provider_versioned["subject"]["value"]["evidence"][0]["source"] = serde_json::json!({
            "kind": "versioned_artifact",
            "value": {
                "url": "https://example.test/releases/v1/openapi.json",
                "provider_revision": "v1"
            }
        });
        provider_versioned["subject"]["value"]["evidence"][0]["facts"][0]["location"] = serde_json::json!({
            "kind": "document_section",
            "value": {
                "path": "releases/v1/openapi.json",
                "section": "Idempotency"
            }
        });
        provider_versioned["reacquisition"] = serde_json::json!({
            "kind": "provider_versioned_artifact_review",
            "value": null
        });
        provider_versioned["artifact_hashes"][0]["path"] =
            serde_json::json!("releases/v1/openapi.json");

        let mut rejected_license = npm.clone();
        rejected_license["license"] = serde_json::json!({
            "kind": "rejected",
            "value": {"finding": "finding.license.rejected"}
        });

        let mut rejected_terms = provider.clone();
        rejected_terms["subject"]["value"]["evidence"][0]["terms"] = serde_json::json!({
            "kind": "rejected",
            "value": {"finding": "finding.terms.rejected"}
        });

        vec![
            BranchCase {
                name: "npm-verified",
                document: npm,
                expected_error: None,
            },
            BranchCase {
                name: "provider-repository-reviewed-use",
                document: provider,
                expected_error: None,
            },
            BranchCase {
                name: "donat-owned",
                document: donat,
                expected_error: None,
            },
            BranchCase {
                name: "npm-collections-and-typed-values",
                document: npm_collections,
                expected_error: None,
            },
            BranchCase {
                name: "npm-absent-mismatch-written-grant",
                document: npm_absent_mismatch_written,
                expected_error: None,
            },
            BranchCase {
                name: "npm-rejected-and-verified-provenance",
                document: npm_rejected_verified,
                expected_error: None,
            },
            BranchCase {
                name: "npm-rejected-provenance",
                document: npm_provenance_rejected,
                expected_error: None,
            },
            BranchCase {
                name: "provider-permissive-terms",
                document: provider_permissive,
                expected_error: None,
            },
            BranchCase {
                name: "provider-versioned-document-section",
                document: provider_versioned,
                expected_error: None,
            },
            BranchCase {
                name: "rejected-license",
                document: rejected_license,
                expected_error: Some("source_record_legal_mismatch"),
            },
            BranchCase {
                name: "rejected-evidence-terms",
                document: rejected_terms,
                expected_error: Some("source_record_legal_mismatch"),
            },
        ]
    }

    #[derive(Clone, Debug)]
    enum CasePathStep {
        Member(String),
        Index(usize),
    }

    #[derive(Clone, Debug)]
    enum StructuralMutation {
        UnknownMember(Vec<CasePathStep>),
        MissingMember {
            object: Vec<CasePathStep>,
            member: String,
        },
        InvalidTag(Vec<CasePathStep>),
    }

    fn collect_structural_mutations(
        value: &serde_json::Value,
        shape: &'static SourceShape,
        path: &mut Vec<CasePathStep>,
        mutations: &mut Vec<StructuralMutation>,
    ) {
        match shape {
            SourceShape::Null | SourceShape::Bool | SourceShape::U32 | SourceShape::String(_) => {}
            SourceShape::Sequence(value_shape) => {
                for (index, value) in value.as_array().unwrap().iter().enumerate() {
                    path.push(CasePathStep::Index(index));
                    collect_structural_mutations(value, value_shape, path, mutations);
                    path.pop();
                }
            }
            SourceShape::Struct { fields, .. } => {
                mutations.push(StructuralMutation::UnknownMember(path.clone()));
                for field in *fields {
                    mutations.push(StructuralMutation::MissingMember {
                        object: path.clone(),
                        member: field.name.to_owned(),
                    });
                    path.push(CasePathStep::Member(field.name.to_owned()));
                    collect_structural_mutations(&value[field.name], field.value, path, mutations);
                    path.pop();
                }
            }
            SourceShape::Tagged { variants, .. } => {
                mutations.push(StructuralMutation::UnknownMember(path.clone()));
                mutations.push(StructuralMutation::MissingMember {
                    object: path.clone(),
                    member: "kind".to_owned(),
                });
                mutations.push(StructuralMutation::MissingMember {
                    object: path.clone(),
                    member: "value".to_owned(),
                });
                mutations.push(StructuralMutation::InvalidTag(path.clone()));
                let kind = value["kind"].as_str().unwrap();
                let variant = variants
                    .iter()
                    .find(|variant| variant.kind == kind)
                    .unwrap();
                path.push(CasePathStep::Member("value".to_owned()));
                collect_structural_mutations(&value["value"], variant.value, path, mutations);
                path.pop();
            }
            SourceShape::Nullable(value_shape) => {
                if !value.is_null() {
                    collect_structural_mutations(value, value_shape, path, mutations);
                }
            }
            SourceShape::TypedValue => {
                mutations.push(StructuralMutation::UnknownMember(path.clone()));
                mutations.push(StructuralMutation::MissingMember {
                    object: path.clone(),
                    member: "kind".to_owned(),
                });
                mutations.push(StructuralMutation::MissingMember {
                    object: path.clone(),
                    member: "value".to_owned(),
                });
                mutations.push(StructuralMutation::InvalidTag(path.clone()));
                match value["kind"].as_str().unwrap() {
                    "list" => {
                        path.push(CasePathStep::Member("value".to_owned()));
                        for (index, value) in value["value"].as_array().unwrap().iter().enumerate()
                        {
                            path.push(CasePathStep::Index(index));
                            collect_structural_mutations(
                                value,
                                &TYPED_VALUE_SHAPE,
                                path,
                                mutations,
                            );
                            path.pop();
                        }
                        path.pop();
                    }
                    "object" => {
                        path.push(CasePathStep::Member("value".to_owned()));
                        for (name, value) in value["value"].as_object().unwrap() {
                            path.push(CasePathStep::Member(name.clone()));
                            collect_structural_mutations(
                                value,
                                &TYPED_VALUE_SHAPE,
                                path,
                                mutations,
                            );
                            path.pop();
                        }
                        path.pop();
                    }
                    "inline_bytes" => {
                        let payload_path = {
                            let mut payload = path.clone();
                            payload.push(CasePathStep::Member("value".to_owned()));
                            payload
                        };
                        mutations.push(StructuralMutation::UnknownMember(payload_path.clone()));
                        for member in ["$binary", "file_name", "media_type"] {
                            mutations.push(StructuralMutation::MissingMember {
                                object: payload_path.clone(),
                                member: member.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn value_at_path_mut<'value>(
        mut value: &'value mut serde_json::Value,
        path: &[CasePathStep],
    ) -> &'value mut serde_json::Value {
        for step in path {
            value = match step {
                CasePathStep::Member(member) => &mut value[member],
                CasePathStep::Index(index) => &mut value[*index],
            };
        }
        value
    }

    fn apply_structural_mutation(document: &mut serde_json::Value, mutation: &StructuralMutation) {
        match mutation {
            StructuralMutation::UnknownMember(path) => {
                value_at_path_mut(document, path)
                    .as_object_mut()
                    .unwrap()
                    .insert("__unknown_member".to_owned(), serde_json::json!(true));
            }
            StructuralMutation::MissingMember { object, member } => {
                value_at_path_mut(document, object)
                    .as_object_mut()
                    .unwrap()
                    .remove(member);
            }
            StructuralMutation::InvalidTag(path) => {
                value_at_path_mut(document, path)["kind"] = serde_json::json!("unknown_branch");
            }
        }
    }

    #[test]
    fn private_raw_declarations_and_public_loader_cases_are_branch_total() {
        let declared = declared_inventory();
        assert_eq!(shape_inventory(), declared);

        let mut covered_structs = BTreeSet::new();
        let mut covered_tagged = BTreeSet::new();
        for case in branch_cases() {
            let bytes = serde_yaml::to_string(&case.document).unwrap();
            match case.expected_error {
                None => {
                    load_record_bytes(bytes.as_bytes()).unwrap_or_else(|error| {
                        panic!("branch case {} must be accepted: {error}", case.name)
                    });
                }
                Some(expected) => {
                    let error = load_record_bytes(bytes.as_bytes()).unwrap_err();
                    assert_eq!(error.code(), expected, "branch case {}", case.name);
                }
            }
            let node = serde_yaml::from_str::<LosslessYamlNode>(&bytes).unwrap();
            case_coverage(
                &node,
                &SOURCE_RECORD_SHAPE,
                &mut covered_structs,
                &mut covered_tagged,
            );

            let mut mutations = Vec::new();
            collect_structural_mutations(
                &case.document,
                &SOURCE_RECORD_SHAPE,
                &mut Vec::new(),
                &mut mutations,
            );
            for mutation in mutations {
                let mut changed = case.document.clone();
                apply_structural_mutation(&mut changed, &mutation);
                let changed = serde_yaml::to_string(&changed).unwrap();
                let error = load_record_bytes(changed.as_bytes()).unwrap_err();
                assert_eq!(
                    error.code(),
                    "source_record_incomplete",
                    "branch case {} mutation {mutation:?}: {error}",
                    case.name,
                );
            }
        }
        assert_eq!(
            covered_structs,
            declared.0.keys().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(
            covered_tagged,
            declared
                .1
                .iter()
                .flat_map(|(name, variants)| {
                    variants
                        .keys()
                        .map(move |variant| (*name, (*variant).to_owned()))
                })
                .collect::<BTreeSet<_>>()
        );
    }
}
