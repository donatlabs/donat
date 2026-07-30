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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CanonicalOwnerPathDescriptor {
    pub normalized_owner: &'static str,
    pub normalized_member: &'static str,
    pub normalized_source: CanonicalDeclarationSource,
    pub domain: &'static str,
    pub canonical_path: &'static str,
    pub owner_class: &'static str,
    pub order: &'static str,
    pub null_empty: &'static str,
    pub branch_type: &'static str,
    pub material_member: &'static str,
    pub material_source: CanonicalDeclarationSource,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalDeclarationSource {
    Source,
    Model,
    ValueContract,
    ConnectorAbi,
    ProjectionSchema,
    BuilderDerived,
    Constant,
    NamedDerived,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CanonicalMutationDescriptor {
    pub case: CanonicalMutationCase,
    pub disposition: CanonicalMutationDisposition,
    pub material_source: CanonicalDeclarationSource,
    pub domain: &'static str,
    pub canonical_path: &'static str,
    pub material_member: &'static str,
    pub branch_type: &'static str,
    pub null_empty: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalMutationDisposition {
    Mutable,
    Singleton,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalMutationCase {
    SourceRecord,
    Semantic,
    Provenance,
    ValueContract,
    TypedValue,
}

macro_rules! projection_schema {
    (
        owner_paths {
            $(
                (
                    $owner:literal,
                    $normalized_member:literal,
                    $normalized_source:ident,
                    $domain:literal,
                    $path:literal,
                    $class:literal,
                    $order:literal,
                    $null_empty:literal,
                    $branch:literal,
                    $member:literal,
                    $material_source:ident,
                    $case:ident,
                    $disposition:ident $(,)?
                );
            )*
        }
        $($declaration:item)*
    ) => {
        $($declaration)*

        /// The exact declaration descriptor that generated every closed
        /// canonical material type.
        pub const CANONICAL_PROJECTION_SCHEMA_DECLARATIONS: &str =
            stringify!($($declaration)*);

        pub const CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS:
            &[CanonicalOwnerPathDescriptor] = &[
                $(
                    CanonicalOwnerPathDescriptor {
                        normalized_owner: $owner,
                        normalized_member: $normalized_member,
                        normalized_source:
                            CanonicalDeclarationSource::$normalized_source,
                        domain: $domain,
                        canonical_path: $path,
                        owner_class: $class,
                        order: $order,
                        null_empty: $null_empty,
                        branch_type: $branch,
                        material_member: $member,
                        material_source:
                            CanonicalDeclarationSource::$material_source,
                    },
                )*
            ];

        pub const CANONICAL_PROJECTION_MUTATION_DESCRIPTORS:
            &[CanonicalMutationDescriptor] = &[
                $(
                    CanonicalMutationDescriptor {
                        case: CanonicalMutationCase::$case,
                        disposition: CanonicalMutationDisposition::$disposition,
                        material_source:
                            CanonicalDeclarationSource::$material_source,
                        domain: $domain,
                        canonical_path: $path,
                        material_member: $member,
                        branch_type: $branch,
                        null_empty: $null_empty,
                    },
                )*
            ];
    };
}

projection_schema! {
owner_paths {
    (
        "StableSemver.major",
        "StableSemver.major",
        Model,
        "semantic",
        "StableSemver.major",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.major",
        Model,
        Semantic,
        Mutable,
    );

    (
        "StableSemver.minor",
        "StableSemver.minor",
        Model,
        "semantic",
        "StableSemver.minor",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.minor",
        Model,
        Semantic,
        Mutable,
    );

    (
        "StableSemver.patch",
        "StableSemver.patch",
        Model,
        "semantic",
        "StableSemver.patch",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.patch",
        Model,
        Semantic,
        Mutable,
    );

    (
        "StableSemver.major",
        "StableSemver.major",
        Model,
        "provenance",
        "StableSemver.major",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.major",
        Model,
        Provenance,
        Mutable,
    );

    (
        "StableSemver.minor",
        "StableSemver.minor",
        Model,
        "provenance",
        "StableSemver.minor",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.minor",
        Model,
        Provenance,
        Mutable,
    );

    (
        "StableSemver.patch",
        "StableSemver.patch",
        Model,
        "provenance",
        "StableSemver.patch",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.patch",
        Model,
        Provenance,
        Mutable,
    );

    (
        "ConnectorSourceRecord.record_version",
        "ConnectorSourceRecord.record_version",
        Source,
        "source-record",
        "SourceRecordMaterialV1.record_version",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SourceRecordMaterialV1.record_version",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.record_id",
        "ConnectorSourceRecord.record_id",
        Source,
        "source-record",
        "SourceRecordMaterialV1.record_id",
        "normalized",
        "scalar",
        "required",
        "SourceRecordId",
        "SourceRecordMaterialV1.record_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.subject",
        "ConnectorSourceRecord.subject",
        Source,
        "source-record",
        "SourceRecordMaterialV1.subject",
        "normalized",
        "scalar",
        "required",
        "SourceSubjectMaterialV1",
        "SourceRecordMaterialV1.subject",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.reacquisition",
        "ConnectorSourceRecord.reacquisition",
        Source,
        "source-record",
        "SourceRecordMaterialV1.reacquisition",
        "normalized",
        "scalar",
        "required",
        "ReacquisitionMaterialV1",
        "SourceRecordMaterialV1.reacquisition",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.artifact_hashes",
        "ConnectorSourceRecord.artifact_hashes",
        Source,
        "source-record",
        "SourceRecordMaterialV1.artifact_hashes",
        "normalized",
        "artifact_id",
        "empty_array",
        "Vec<ArtifactHashMaterialV1>",
        "SourceRecordMaterialV1.artifact_hashes",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.license",
        "ConnectorSourceRecord.license",
        Source,
        "source-record",
        "SourceRecordMaterialV1.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "SourceRecordMaterialV1.license",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.notice",
        "ConnectorSourceRecord.notice",
        Source,
        "source-record",
        "SourceRecordMaterialV1.notice",
        "normalized",
        "scalar",
        "required",
        "NoticeMaterialV1",
        "SourceRecordMaterialV1.notice",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.entrypoints",
        "ConnectorSourceRecord.entrypoints",
        Source,
        "source-record",
        "SourceRecordMaterialV1.entrypoints",
        "normalized",
        "declared",
        "empty_array",
        "Vec<SourcePath>",
        "SourceRecordMaterialV1.entrypoints",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.dependencies",
        "ConnectorSourceRecord.dependencies",
        Source,
        "source-record",
        "SourceRecordMaterialV1.dependencies",
        "normalized",
        "dependency",
        "empty_array",
        "Vec<DependencyDecisionMaterialV1>",
        "SourceRecordMaterialV1.dependencies",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.embedded_material",
        "ConnectorSourceRecord.embedded_material",
        Source,
        "source-record",
        "SourceRecordMaterialV1.embedded_material",
        "normalized",
        "material_id",
        "empty_array",
        "Vec<EmbeddedDecisionMaterialV1>",
        "SourceRecordMaterialV1.embedded_material",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.provider_contracts",
        "ConnectorSourceRecord.provider_contracts",
        Source,
        "source-record",
        "SourceRecordMaterialV1.provider_contracts",
        "normalized",
        "contract_id",
        "empty_array",
        "Vec<ProviderContractMaterialV1>",
        "SourceRecordMaterialV1.provider_contracts",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.compatibility",
        "ConnectorSourceRecord.compatibility",
        Source,
        "source-record",
        "SourceRecordMaterialV1.compatibility",
        "normalized",
        "scalar",
        "required",
        "CompatibilityMaterialV1",
        "SourceRecordMaterialV1.compatibility",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.admission",
        "ConnectorSourceRecord.admission",
        Source,
        "source-record",
        "SourceRecordMaterialV1.admission",
        "normalized",
        "scalar",
        "required",
        "AdmissionMaterialV1",
        "SourceRecordMaterialV1.admission",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.safety_findings",
        "ConnectorSourceRecord.safety_findings",
        Source,
        "source-record",
        "SourceRecordMaterialV1.safety_findings",
        "normalized",
        "scalar",
        "required",
        "SafetyFindingsMaterialV1",
        "SourceRecordMaterialV1.safety_findings",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.reviewer",
        "ConnectorSourceRecord.reviewer",
        Source,
        "source-record",
        "SourceRecordMaterialV1.reviewer",
        "normalized",
        "scalar",
        "required",
        "ReviewIdentity",
        "SourceRecordMaterialV1.reviewer",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.approval_date",
        "ConnectorSourceRecord.approval_date",
        Source,
        "source-record",
        "SourceRecordMaterialV1.approval_date",
        "normalized",
        "scalar",
        "required",
        "Date",
        "SourceRecordMaterialV1.approval_date",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.proposed_manifest",
        "ConnectorSourceRecord.proposed_manifest",
        Source,
        "source-record",
        "SourceRecordMaterialV1.proposed_manifest",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<RepoPath>",
        "SourceRecordMaterialV1.proposed_manifest",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.proposed_destinations",
        "ConnectorSourceRecord.proposed_destinations",
        Source,
        "source-record",
        "SourceRecordMaterialV1.proposed_destinations",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<RepoPath>",
        "SourceRecordMaterialV1.proposed_destinations",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorSourceRecord.red_tests",
        "ConnectorSourceRecord.red_tests",
        Source,
        "source-record",
        "SourceRecordMaterialV1.red_tests",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<TestId>",
        "SourceRecordMaterialV1.red_tests",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SourceSubject::ExactNpm",
        "SourceSubject::ExactNpm",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.kind",
        "normalized",
        "scalar",
        "required",
        "exact_npm",
        "SourceSubjectMaterialV1::ExactNpm",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SourceSubject::ProviderArtifact",
        "SourceSubject::ProviderArtifact",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=provider_artifact}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_artifact",
        "SourceSubjectMaterialV1::ProviderArtifact",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SourceSubject::DonatOwned",
        "SourceSubject::DonatOwned",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=donat_owned}.kind",
        "normalized",
        "scalar",
        "required",
        "donat_owned",
        "SourceSubjectMaterialV1::DonatOwned",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.name",
        "ExactNpmPackage.name",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.name",
        "normalized",
        "scalar",
        "required",
        "string",
        "ExactNpmMaterialV1.name",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.version",
        "ExactNpmPackage.version",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.version",
        "normalized",
        "scalar",
        "required",
        "ExactSemver",
        "ExactNpmMaterialV1.version",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.tarball_url",
        "ExactNpmPackage.tarball_url",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.tarball_url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "ExactNpmMaterialV1.tarball_url",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.integrity",
        "ExactNpmPackage.integrity",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.integrity",
        "normalized",
        "scalar",
        "required",
        "NpmIntegrity",
        "ExactNpmMaterialV1.integrity",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.repository",
        "ExactNpmPackage.repository",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.repository",
        "normalized",
        "scalar",
        "required",
        "ImmutableRepository",
        "ExactNpmMaterialV1.repository",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.npm_git_head",
        "ExactNpmPackage.npm_git_head",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.npm_git_head",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "ExactNpmMaterialV1.npm_git_head",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.package_repository",
        "ExactNpmPackage.package_repository",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.package_repository",
        "normalized",
        "scalar",
        "required",
        "RepositoryUrl",
        "ExactNpmMaterialV1.package_repository",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.signature",
        "ExactNpmPackage.signature",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.signature",
        "normalized",
        "scalar",
        "required",
        "NpmSignatureMaterialV1",
        "ExactNpmMaterialV1.signature",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.provenance",
        "ExactNpmPackage.provenance",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.provenance",
        "normalized",
        "scalar",
        "required",
        "NpmProvenanceMaterialV1",
        "ExactNpmMaterialV1.provenance",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.tag_commit",
        "ExactNpmPackage.tag_commit",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.tag_commit",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<GitCommit>",
        "ExactNpmMaterialV1.tag_commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.provenance_commit",
        "ExactNpmPackage.provenance_commit",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.provenance_commit",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<GitCommit>",
        "ExactNpmMaterialV1.provenance_commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.maintainers",
        "ExactNpmPackage.maintainers",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.maintainers",
        "normalized",
        "identity",
        "empty_array",
        "Vec<NpmMaintainerIdentity>",
        "ExactNpmMaterialV1.maintainers",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactNpmPackage.repository_owner",
        "ExactNpmPackage.repository_owner",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=exact_npm}.value.repository_owner",
        "normalized",
        "scalar",
        "required",
        "RepositoryOwnerMaterialV1",
        "ExactNpmMaterialV1.repository_owner",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmIntegrity.algorithm",
        "NpmIntegrity.algorithm",
        Source,
        "source-record",
        "NpmIntegrity.algorithm",
        "normalized",
        "scalar",
        "required",
        "sha512",
        "NpmIntegrity.algorithm",
        ProjectionSchema,
        SourceRecord,
        Singleton,
    );

    (
        "NpmIntegrity.digest",
        "NpmIntegrity.digest",
        Source,
        "source-record",
        "NpmIntegrity.digest",
        "normalized",
        "scalar",
        "required",
        "bytes64",
        "NpmIntegrity.digest",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableRepository.url",
        "ImmutableRepository.url",
        Source,
        "source-record",
        "ImmutableRepository.url",
        "normalized",
        "scalar",
        "required",
        "RepositoryUrl",
        "ImmutableRepository.url",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableRepository.commit",
        "ImmutableRepository.commit",
        Source,
        "source-record",
        "ImmutableRepository.commit",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "ImmutableRepository.commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableRepository.tree",
        "ImmutableRepository.tree",
        Source,
        "source-record",
        "ImmutableRepository.tree",
        "normalized",
        "scalar",
        "required",
        "GitTree",
        "ImmutableRepository.tree",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::Verified",
        "NpmSignatureDecision::Verified",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=verified}.kind",
        "normalized",
        "scalar",
        "required",
        "verified",
        "NpmSignatureMaterialV1::Verified",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::VerifiedAbsent",
        "NpmSignatureDecision::VerifiedAbsent",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=verified_absent}.kind",
        "normalized",
        "scalar",
        "required",
        "verified_absent",
        "NpmSignatureMaterialV1::VerifiedAbsent",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::Rejected",
        "NpmSignatureDecision::Rejected",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "NpmSignatureMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::Verified.signatures",
        "NpmSignatureDecision::Verified.signatures",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=verified}.value.signatures",
        "normalized",
        "key_id",
        "nonempty_array",
        "NonEmptyVec<VerifiedNpmSignature>",
        "NpmSignatureMaterialV1::Verified.signatures",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::Verified.registry_metadata_sha256",
        "NpmSignatureDecision::Verified.registry_metadata_sha256",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=verified}.value.registry_metadata_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "NpmSignatureMaterialV1::Verified.registry_metadata_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::VerifiedAbsent.registry_metadata_sha256",
        "NpmSignatureDecision::VerifiedAbsent.registry_metadata_sha256",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=verified_absent}.value.registry_metadata_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "NpmSignatureMaterialV1::VerifiedAbsent.registry_metadata_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmSignatureDecision::Rejected.finding",
        "NpmSignatureDecision::Rejected.finding",
        Source,
        "source-record",
        "NpmSignatureMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "NpmSignatureMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "VerifiedNpmSignature.key_id",
        "VerifiedNpmSignature.key_id",
        Source,
        "source-record",
        "VerifiedNpmSignatureMaterialV1.key_id",
        "normalized",
        "scalar",
        "required",
        "Id",
        "VerifiedNpmSignatureMaterialV1.key_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "VerifiedNpmSignature.signature_sha256",
        "VerifiedNpmSignature.signature_sha256",
        Source,
        "source-record",
        "VerifiedNpmSignatureMaterialV1.signature_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "VerifiedNpmSignatureMaterialV1.signature_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::Verified",
        "NpmProvenanceDecision::Verified",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=verified}.kind",
        "normalized",
        "scalar",
        "required",
        "verified",
        "NpmProvenanceMaterialV1::Verified",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::VerifiedAbsent",
        "NpmProvenanceDecision::VerifiedAbsent",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=verified_absent}.kind",
        "normalized",
        "scalar",
        "required",
        "verified_absent",
        "NpmProvenanceMaterialV1::VerifiedAbsent",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::Rejected",
        "NpmProvenanceDecision::Rejected",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "NpmProvenanceMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::Verified.statement_sha256",
        "NpmProvenanceDecision::Verified.statement_sha256",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=verified}.value.statement_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "NpmProvenanceMaterialV1::Verified.statement_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::Verified.source_commit",
        "NpmProvenanceDecision::Verified.source_commit",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=verified}.value.source_commit",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "NpmProvenanceMaterialV1::Verified.source_commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::VerifiedAbsent.registry_metadata_sha256",
        "NpmProvenanceDecision::VerifiedAbsent.registry_metadata_sha256",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=verified_absent}.value.registry_metadata_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "NpmProvenanceMaterialV1::VerifiedAbsent.registry_metadata_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NpmProvenanceDecision::Rejected.finding",
        "NpmProvenanceDecision::Rejected.finding",
        Source,
        "source-record",
        "NpmProvenanceMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "NpmProvenanceMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::Consistent",
        "RepositoryOwnerDecision::Consistent",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=consistent}.kind",
        "normalized",
        "scalar",
        "required",
        "consistent",
        "RepositoryOwnerMaterialV1::Consistent",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::ReviewedMismatch",
        "RepositoryOwnerDecision::ReviewedMismatch",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.kind",
        "normalized",
        "scalar",
        "required",
        "reviewed_mismatch",
        "RepositoryOwnerMaterialV1::ReviewedMismatch",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::Rejected",
        "RepositoryOwnerDecision::Rejected",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "RepositoryOwnerMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::Consistent.package_owner",
        "RepositoryOwnerDecision::Consistent.package_owner",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=consistent}.value.package_owner",
        "normalized",
        "scalar",
        "required",
        "NpmOwnerIdentity",
        "RepositoryOwnerMaterialV1::Consistent.package_owner",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::Consistent.repository_owner",
        "RepositoryOwnerDecision::Consistent.repository_owner",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=consistent}.value.repository_owner",
        "normalized",
        "scalar",
        "required",
        "RepositoryOwnerIdentity",
        "RepositoryOwnerMaterialV1::Consistent.repository_owner",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::ReviewedMismatch.decision_id",
        "RepositoryOwnerDecision::ReviewedMismatch.decision_id",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.value.decision_id",
        "normalized",
        "scalar",
        "required",
        "ReviewDecisionId",
        "RepositoryOwnerMaterialV1::ReviewedMismatch.decision_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepositoryOwnerDecision::Rejected.finding",
        "RepositoryOwnerDecision::Rejected.finding",
        Source,
        "source-record",
        "RepositoryOwnerMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "RepositoryOwnerMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactProviderArtifact.provider",
        "ExactProviderArtifact.provider",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=provider_artifact}.value.provider",
        "normalized",
        "scalar",
        "required",
        "string",
        "ProviderArtifactMaterialV1.provider",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactProviderArtifact.evidence",
        "ExactProviderArtifact.evidence",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=provider_artifact}.value.evidence",
        "normalized",
        "canonical_source_identity_content_sha256",
        "nonempty_array",
        "NonEmptyVec<ProviderEvidenceMaterialV1>",
        "ProviderArtifactMaterialV1.evidence",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.source",
        "ProviderEvidenceArtifact.source",
        Source,
        "source-record",
        "ProviderEvidenceMaterialV1.source",
        "normalized",
        "scalar",
        "required",
        "ImmutableProviderEvidenceSource",
        "ProviderEvidenceMaterialV1.source",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.accessed_on",
        "ProviderEvidenceArtifact.accessed_on",
        Source,
        "source-record",
        "ProviderEvidenceMaterialV1.accessed_on",
        "normalized",
        "scalar",
        "required",
        "Date",
        "ProviderEvidenceMaterialV1.accessed_on",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.content_sha256",
        "ProviderEvidenceArtifact.content_sha256",
        Source,
        "source-record",
        "ProviderEvidenceMaterialV1.content_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "ProviderEvidenceMaterialV1.content_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.terms",
        "ProviderEvidenceArtifact.terms",
        Source,
        "source-record",
        "ProviderEvidenceMaterialV1.terms",
        "normalized",
        "scalar",
        "required",
        "EvidenceTermsMaterialV1",
        "ProviderEvidenceMaterialV1.terms",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.facts",
        "ProviderEvidenceArtifact.facts",
        Source,
        "source-record",
        "ProviderEvidenceMaterialV1.facts",
        "normalized",
        "fact_id",
        "nonempty_array",
        "NonEmptyVec<ProviderFactMaterialV1>",
        "ProviderEvidenceMaterialV1.facts",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile",
        "ImmutableProviderEvidenceSource::RepositoryFile",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=repository_file}.kind",
        "normalized",
        "scalar",
        "required",
        "repository_file",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact",
        "ImmutableProviderEvidenceSource::VersionedArtifact",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.kind",
        "normalized",
        "scalar",
        "required",
        "versioned_artifact",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.repository",
        "ImmutableProviderEvidenceSource::RepositoryFile.repository",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.repository",
        "normalized",
        "scalar",
        "required",
        "RepositoryUrl",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.repository",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.commit",
        "ImmutableProviderEvidenceSource::RepositoryFile.commit",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.commit",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.path",
        "ImmutableProviderEvidenceSource::RepositoryFile.path",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact.url",
        "ImmutableProviderEvidenceSource::VersionedArtifact.url",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact.url",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
        "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
        Source,
        "source-record",
        "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.provider_revision",
        "normalized",
        "scalar",
        "required",
        "NonEmptyString",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact.provider_revision",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive",
        "EvidenceTermsDisposition::Permissive",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=permissive}.kind",
        "normalized",
        "scalar",
        "required",
        "permissive",
        "EvidenceTermsMaterialV1::Permissive",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse",
        "EvidenceTermsDisposition::ReviewedUse",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=reviewed_use}.kind",
        "normalized",
        "scalar",
        "required",
        "reviewed_use",
        "EvidenceTermsMaterialV1::ReviewedUse",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Rejected",
        "EvidenceTermsDisposition::Rejected",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "EvidenceTermsMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive.license",
        "EvidenceTermsDisposition::Permissive.license",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=permissive}.value.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "EvidenceTermsMaterialV1::Permissive.license",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive.evidence_url",
        "EvidenceTermsDisposition::Permissive.evidence_url",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=permissive}.value.evidence_url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "EvidenceTermsMaterialV1::Permissive.evidence_url",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse.decision_id",
        "EvidenceTermsDisposition::ReviewedUse.decision_id",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=reviewed_use}.value.decision_id",
        "normalized",
        "scalar",
        "required",
        "ReviewDecisionId",
        "EvidenceTermsMaterialV1::ReviewedUse.decision_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse.evidence_url",
        "EvidenceTermsDisposition::ReviewedUse.evidence_url",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=reviewed_use}.value.evidence_url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "EvidenceTermsMaterialV1::ReviewedUse.evidence_url",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Rejected.finding",
        "EvidenceTermsDisposition::Rejected.finding",
        Source,
        "source-record",
        "EvidenceTermsMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "EvidenceTermsMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderFact.fact_id",
        "ProviderFact.fact_id",
        Source,
        "source-record",
        "ProviderFactMaterialV1.fact_id",
        "normalized",
        "scalar",
        "required",
        "ProviderFactId",
        "ProviderFactMaterialV1.fact_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderFact.location",
        "ProviderFact.location",
        Source,
        "source-record",
        "ProviderFactMaterialV1.location",
        "normalized",
        "scalar",
        "required",
        "ExactFactLocationMaterialV1",
        "ProviderFactMaterialV1.location",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderFact.normalized_value",
        "ProviderFact.normalized_value",
        Source,
        "source-record",
        "ProviderFactMaterialV1.normalized_value",
        "normalized",
        "scalar",
        "required",
        "TypedValueMaterialV1",
        "ProviderFactMaterialV1.normalized_value",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer",
        "ExactFactLocation::JsonPointer",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=json_pointer}.kind",
        "normalized",
        "scalar",
        "required",
        "json_pointer",
        "ExactFactLocationMaterialV1::JsonPointer",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection",
        "ExactFactLocation::DocumentSection",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=document_section}.kind",
        "normalized",
        "scalar",
        "required",
        "document_section",
        "ExactFactLocationMaterialV1::DocumentSection",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer.path",
        "ExactFactLocation::JsonPointer.path",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=json_pointer}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ExactFactLocationMaterialV1::JsonPointer.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer.pointer",
        "ExactFactLocation::JsonPointer.pointer",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=json_pointer}.value.pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "ExactFactLocationMaterialV1::JsonPointer.pointer",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection.path",
        "ExactFactLocation::DocumentSection.path",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=document_section}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ExactFactLocationMaterialV1::DocumentSection.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection.section",
        "ExactFactLocation::DocumentSection.section",
        Source,
        "source-record",
        "ExactFactLocationMaterialV1{kind=document_section}.value.section",
        "normalized",
        "scalar",
        "required",
        "string",
        "ExactFactLocationMaterialV1::DocumentSection.section",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DonatOwnedSource.repository_commit",
        "DonatOwnedSource.repository_commit",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=donat_owned}.value.repository_commit",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "DonatOwnedMaterialV1.repository_commit",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DonatOwnedSource.files",
        "DonatOwnedSource.files",
        Source,
        "source-record",
        "SourceSubjectMaterialV1{kind=donat_owned}.value.files",
        "normalized",
        "path",
        "nonempty_array",
        "NonEmptyVec<RepoFileHash>",
        "DonatOwnedMaterialV1.files",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepoFileHash.path",
        "RepoFileHash.path",
        Source,
        "source-record",
        "RepoFileHashMaterialV1.path",
        "normalized",
        "scalar",
        "required",
        "RepoPath",
        "RepoFileHashMaterialV1.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "RepoFileHash.sha256",
        "RepoFileHash.sha256",
        Source,
        "source-record",
        "RepoFileHashMaterialV1.sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "RepoFileHashMaterialV1.sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ReacquisitionPlan::ExactNpmReview",
        "ReacquisitionPlan::ExactNpmReview",
        Source,
        "source-record",
        "ReacquisitionMaterialV1{kind=exact_npm_review}.kind",
        "normalized",
        "scalar",
        "required",
        "exact_npm_review",
        "ReacquisitionMaterialV1::ExactNpmReview",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ReacquisitionPlan::ProviderRepositoryReview",
        "ReacquisitionPlan::ProviderRepositoryReview",
        Source,
        "source-record",
        "ReacquisitionMaterialV1{kind=provider_repository_review}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_repository_review",
        "ReacquisitionMaterialV1::ProviderRepositoryReview",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ReacquisitionPlan::ProviderVersionedArtifactReview",
        "ReacquisitionPlan::ProviderVersionedArtifactReview",
        Source,
        "source-record",
        "ReacquisitionMaterialV1{kind=provider_versioned_artifact_review}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_versioned_artifact_review",
        "ReacquisitionMaterialV1::ProviderVersionedArtifactReview",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ReacquisitionPlan::DonatOwnedNoNetwork",
        "ReacquisitionPlan::DonatOwnedNoNetwork",
        Source,
        "source-record",
        "ReacquisitionMaterialV1{kind=donat_owned_no_network}.kind",
        "normalized",
        "scalar",
        "required",
        "donat_owned_no_network",
        "ReacquisitionMaterialV1::DonatOwnedNoNetwork",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ArtifactHash.artifact_id",
        "ArtifactHash.artifact_id",
        Source,
        "source-record",
        "ArtifactHashMaterialV1.artifact_id",
        "normalized",
        "scalar",
        "required",
        "ArtifactId",
        "ArtifactHashMaterialV1.artifact_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ArtifactHash.algorithm",
        "ArtifactHash.algorithm",
        Source,
        "source-record",
        "ArtifactHashMaterialV1.algorithm",
        "normalized",
        "scalar",
        "required",
        "HashAlgorithm",
        "ArtifactHashMaterialV1.algorithm",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ArtifactHash.digest",
        "ArtifactHash.digest",
        Source,
        "source-record",
        "ArtifactHashMaterialV1.digest",
        "normalized",
        "scalar",
        "required",
        "Hash256_or_Hash512",
        "ArtifactHashMaterialV1.digest",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ArtifactHash.path",
        "ArtifactHash.path",
        Source,
        "source-record",
        "ArtifactHashMaterialV1.path",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<SourcePath>",
        "ArtifactHashMaterialV1.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "HashAlgorithm::Sha256",
        "HashAlgorithm::Sha256",
        Source,
        "source-record",
        "HashAlgorithmMaterialV1{kind=sha256}.kind",
        "normalized",
        "scalar",
        "required",
        "sha256",
        "HashAlgorithmMaterialV1::Sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "HashAlgorithm::Sha512",
        "HashAlgorithm::Sha512",
        Source,
        "source-record",
        "HashAlgorithmMaterialV1{kind=sha512}.kind",
        "normalized",
        "scalar",
        "required",
        "sha512",
        "HashAlgorithmMaterialV1::Sha512",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Permissive",
        "LicenseDecision::Permissive",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=permissive}.kind",
        "normalized",
        "scalar",
        "required",
        "permissive",
        "LicenseDecisionMaterialV1::Permissive",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant",
        "LicenseDecision::WrittenGrant",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=written_grant}.kind",
        "normalized",
        "scalar",
        "required",
        "written_grant",
        "LicenseDecisionMaterialV1::WrittenGrant",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Rejected",
        "LicenseDecision::Rejected",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "LicenseDecisionMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.spdx_id",
        "LicenseDecision::Permissive.spdx_id",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=permissive}.value.spdx_id",
        "normalized",
        "scalar",
        "required",
        "string",
        "LicenseDecisionMaterialV1::Permissive.spdx_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.selected_dual_license_branch",
        "LicenseDecision::Permissive.selected_dual_license_branch",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=permissive}.value.selected_dual_license_branch",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<string>",
        "LicenseDecisionMaterialV1::Permissive.selected_dual_license_branch",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.license_file_path",
        "LicenseDecision::Permissive.license_file_path",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=permissive}.value.license_file_path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "LicenseDecisionMaterialV1::Permissive.license_file_path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.license_file_sha256",
        "LicenseDecision::Permissive.license_file_sha256",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=permissive}.value.license_file_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "LicenseDecisionMaterialV1::Permissive.license_file_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant.decision_id",
        "LicenseDecision::WrittenGrant.decision_id",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=written_grant}.value.decision_id",
        "normalized",
        "scalar",
        "required",
        "ReviewDecisionId",
        "LicenseDecisionMaterialV1::WrittenGrant.decision_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant.grant_sha256",
        "LicenseDecision::WrittenGrant.grant_sha256",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=written_grant}.value.grant_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "LicenseDecisionMaterialV1::WrittenGrant.grant_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "LicenseDecision::Rejected.finding",
        "LicenseDecision::Rejected.finding",
        Source,
        "source-record",
        "LicenseDecisionMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "LicenseDecisionMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NoticeIdentity.id",
        "NoticeIdentity.id",
        Source,
        "source-record",
        "NoticeMaterialV1.id",
        "normalized",
        "scalar",
        "required",
        "NoticeId",
        "NoticeMaterialV1.id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NoticeIdentity.license_file_path",
        "NoticeIdentity.license_file_path",
        Source,
        "source-record",
        "NoticeMaterialV1.license_file_path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "NoticeMaterialV1.license_file_path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NoticeIdentity.license_file_sha256",
        "NoticeIdentity.license_file_sha256",
        Source,
        "source-record",
        "NoticeMaterialV1.license_file_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "NoticeMaterialV1.license_file_sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NoticeIdentity.required_copyright_lines",
        "NoticeIdentity.required_copyright_lines",
        Source,
        "source-record",
        "NoticeMaterialV1.required_copyright_lines",
        "normalized",
        "declared",
        "empty_array",
        "Vec<string>",
        "NoticeMaterialV1.required_copyright_lines",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "NoticeIdentity.notice_bundle_destination",
        "NoticeIdentity.notice_bundle_destination",
        Source,
        "source-record",
        "NoticeMaterialV1.notice_bundle_destination",
        "normalized",
        "scalar",
        "required",
        "RepoPath",
        "NoticeMaterialV1.notice_bundle_destination",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDecision.dependency",
        "DependencyDecision.dependency",
        Source,
        "source-record",
        "DependencyDecisionMaterialV1.dependency",
        "normalized",
        "scalar",
        "required",
        "Id",
        "DependencyDecisionMaterialV1.dependency",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDecision.disposition",
        "DependencyDecision.disposition",
        Source,
        "source-record",
        "DependencyDecisionMaterialV1.disposition",
        "normalized",
        "scalar",
        "required",
        "DependencyDispositionMaterialV1",
        "DependencyDecisionMaterialV1.disposition",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::Shipped",
        "DependencyDisposition::Shipped",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=shipped}.kind",
        "normalized",
        "scalar",
        "required",
        "shipped",
        "DependencyDispositionMaterialV1::Shipped",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::BuildOnly",
        "DependencyDisposition::BuildOnly",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=build_only}.kind",
        "normalized",
        "scalar",
        "required",
        "build_only",
        "DependencyDispositionMaterialV1::BuildOnly",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::TypeOnlyReplaced",
        "DependencyDisposition::TypeOnlyReplaced",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=type_only_replaced}.kind",
        "normalized",
        "scalar",
        "required",
        "type_only_replaced",
        "DependencyDispositionMaterialV1::TypeOnlyReplaced",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::BehaviorOnly",
        "DependencyDisposition::BehaviorOnly",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=behavior_only}.kind",
        "normalized",
        "scalar",
        "required",
        "behavior_only",
        "DependencyDispositionMaterialV1::BehaviorOnly",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::Rejected",
        "DependencyDisposition::Rejected",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "DependencyDispositionMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::Shipped.license",
        "DependencyDisposition::Shipped.license",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=shipped}.value.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "DependencyDispositionMaterialV1::Shipped.license",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::BuildOnly.license",
        "DependencyDisposition::BuildOnly.license",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=build_only}.value.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "DependencyDispositionMaterialV1::BuildOnly.license",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::TypeOnlyReplaced.replacement",
        "DependencyDisposition::TypeOnlyReplaced.replacement",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=type_only_replaced}.value.replacement",
        "normalized",
        "scalar",
        "required",
        "Id",
        "DependencyDispositionMaterialV1::TypeOnlyReplaced.replacement",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::BehaviorOnly.reason",
        "DependencyDisposition::BehaviorOnly.reason",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=behavior_only}.value.reason",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "DependencyDispositionMaterialV1::BehaviorOnly.reason",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "DependencyDisposition::Rejected.finding",
        "DependencyDisposition::Rejected.finding",
        Source,
        "source-record",
        "DependencyDispositionMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "DependencyDispositionMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.material_id",
        "EmbeddedMaterialDecision.material_id",
        Source,
        "source-record",
        "EmbeddedDecisionMaterialV1.material_id",
        "normalized",
        "scalar",
        "required",
        "Id",
        "EmbeddedDecisionMaterialV1.material_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.path",
        "EmbeddedMaterialDecision.path",
        Source,
        "source-record",
        "EmbeddedDecisionMaterialV1.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "EmbeddedDecisionMaterialV1.path",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.sha256",
        "EmbeddedMaterialDecision.sha256",
        Source,
        "source-record",
        "EmbeddedDecisionMaterialV1.sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "EmbeddedDecisionMaterialV1.sha256",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.disposition",
        "EmbeddedMaterialDecision.disposition",
        Source,
        "source-record",
        "EmbeddedDecisionMaterialV1.disposition",
        "normalized",
        "scalar",
        "required",
        "EmbeddedMaterialDispositionMaterialV1",
        "EmbeddedDecisionMaterialV1.disposition",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Shipped",
        "EmbeddedMaterialDisposition::Shipped",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=shipped}.kind",
        "normalized",
        "scalar",
        "required",
        "shipped",
        "EmbeddedMaterialDispositionMaterialV1::Shipped",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::BehaviorOnly",
        "EmbeddedMaterialDisposition::BehaviorOnly",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.kind",
        "normalized",
        "scalar",
        "required",
        "behavior_only",
        "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Rejected",
        "EmbeddedMaterialDisposition::Rejected",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "EmbeddedMaterialDispositionMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Shipped.license",
        "EmbeddedMaterialDisposition::Shipped.license",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=shipped}.value.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "EmbeddedMaterialDispositionMaterialV1::Shipped.license",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::BehaviorOnly.reason",
        "EmbeddedMaterialDisposition::BehaviorOnly.reason",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.value.reason",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly.reason",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Rejected.finding",
        "EmbeddedMaterialDisposition::Rejected.finding",
        Source,
        "source-record",
        "EmbeddedMaterialDispositionMaterialV1{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "EmbeddedMaterialDispositionMaterialV1::Rejected.finding",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderContractReference.contract_id",
        "ProviderContractReference.contract_id",
        Source,
        "source-record",
        "ProviderContractMaterialV1.contract_id",
        "normalized",
        "scalar",
        "required",
        "ProviderContractId",
        "ProviderContractMaterialV1.contract_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ProviderContractReference.facts",
        "ProviderContractReference.facts",
        Source,
        "source-record",
        "ProviderContractMaterialV1.facts",
        "normalized",
        "kind_then_fact_or_policy_id",
        "nonempty_array",
        "NonEmptyVec<ContractFactMaterialV1>",
        "ProviderContractMaterialV1.facts",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence",
        "ContractFact::ProviderEvidence",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=provider_evidence}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_evidence",
        "ContractFactMaterialV1::ProviderEvidence",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::DonatPolicy",
        "ContractFact::DonatPolicy",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=donat_policy}.kind",
        "normalized",
        "scalar",
        "required",
        "donat_policy",
        "ContractFactMaterialV1::DonatPolicy",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence.source_record_id",
        "ContractFact::ProviderEvidence.source_record_id",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=provider_evidence}.value.source_record_id",
        "normalized",
        "scalar",
        "required",
        "SourceRecordId",
        "ContractFactMaterialV1::ProviderEvidence.source_record_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence.fact_id",
        "ContractFact::ProviderEvidence.fact_id",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=provider_evidence}.value.fact_id",
        "normalized",
        "scalar",
        "required",
        "ProviderFactId",
        "ContractFactMaterialV1::ProviderEvidence.fact_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::DonatPolicy.policy_id",
        "ContractFact::DonatPolicy.policy_id",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=donat_policy}.value.policy_id",
        "normalized",
        "scalar",
        "required",
        "DonatPolicyId",
        "ContractFactMaterialV1::DonatPolicy.policy_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ContractFact::DonatPolicy.value",
        "ContractFact::DonatPolicy.value",
        Source,
        "source-record",
        "ContractFactMaterialV1{kind=donat_policy}.value.value",
        "normalized",
        "scalar",
        "required",
        "TypedValueMaterialV1",
        "ContractFactMaterialV1::DonatPolicy.value",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "CompatibilityDecision::TierA",
        "CompatibilityDecision::TierA",
        Source,
        "source-record",
        "CompatibilityMaterialV1{kind=tier_a}.kind",
        "normalized",
        "scalar",
        "required",
        "tier_a",
        "CompatibilityMaterialV1::TierA",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "CompatibilityDecision::TierB",
        "CompatibilityDecision::TierB",
        Source,
        "source-record",
        "CompatibilityMaterialV1{kind=tier_b}.kind",
        "normalized",
        "scalar",
        "required",
        "tier_b",
        "CompatibilityMaterialV1::TierB",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "CompatibilityDecision::TierC",
        "CompatibilityDecision::TierC",
        Source,
        "source-record",
        "CompatibilityMaterialV1{kind=tier_c}.kind",
        "normalized",
        "scalar",
        "required",
        "tier_c",
        "CompatibilityMaterialV1::TierC",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "CompatibilityDecision::Rejected",
        "CompatibilityDecision::Rejected",
        Source,
        "source-record",
        "CompatibilityMaterialV1{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "CompatibilityMaterialV1::Rejected",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::InventoryOnly",
        "AdmissionState::InventoryOnly",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=inventory_only}.kind",
        "normalized",
        "scalar",
        "required",
        "inventory_only",
        "AdmissionMaterialV1::InventoryOnly",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::ApprovedForPort",
        "AdmissionState::ApprovedForPort",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=approved_for_port}.kind",
        "normalized",
        "scalar",
        "required",
        "approved_for_port",
        "AdmissionMaterialV1::ApprovedForPort",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::EvidenceAccepted",
        "AdmissionState::EvidenceAccepted",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=evidence_accepted}.kind",
        "normalized",
        "scalar",
        "required",
        "evidence_accepted",
        "AdmissionMaterialV1::EvidenceAccepted",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::InventoryOnly.findings",
        "AdmissionState::InventoryOnly.findings",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=inventory_only}.value.findings",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<FindingId>",
        "AdmissionMaterialV1::InventoryOnly.findings",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::ApprovedForPort.operations",
        "AdmissionState::ApprovedForPort.operations",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=approved_for_port}.value.operations",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<OperationId>",
        "AdmissionMaterialV1::ApprovedForPort.operations",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "AdmissionState::EvidenceAccepted.contracts",
        "AdmissionState::EvidenceAccepted.contracts",
        Source,
        "source-record",
        "AdmissionMaterialV1{kind=evidence_accepted}.value.contracts",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<ProviderContractId>",
        "AdmissionMaterialV1::EvidenceAccepted.contracts",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SafetyFindings.findings",
        "SafetyFindings.findings",
        Source,
        "source-record",
        "SafetyFindingsMaterialV1.findings",
        "normalized",
        "finding_id",
        "empty_array",
        "Vec<SafetyFindingMaterialV1>",
        "SafetyFindingsMaterialV1.findings",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SafetyFinding.finding_id",
        "SafetyFinding.finding_id",
        Source,
        "source-record",
        "SafetyFindingMaterialV1.finding_id",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "SafetyFindingMaterialV1.finding_id",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SafetyFinding.kind",
        "SafetyFinding.kind",
        Source,
        "source-record",
        "SafetyFindingMaterialV1.kind",
        "normalized",
        "scalar",
        "required",
        "Id",
        "SafetyFindingMaterialV1.kind",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SafetyFinding.location",
        "SafetyFinding.location",
        Source,
        "source-record",
        "SafetyFindingMaterialV1.location",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<SourcePath>",
        "SafetyFindingMaterialV1.location",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "SafetyFinding.message",
        "SafetyFinding.message",
        Source,
        "source-record",
        "SafetyFindingMaterialV1.message",
        "normalized",
        "scalar",
        "required",
        "string",
        "SafetyFindingMaterialV1.message",
        ProjectionSchema,
        SourceRecord,
        Mutable,
    );

    (
        "ConnectorManifest.connector",
        "ConnectorManifest.connector",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.id",
        "normalized",
        "scalar",
        "required",
        "ConnectorId",
        "SemanticConnectorMaterialV1.id",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.connector_version",
        "ConnectorManifest.connector_version",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticConnectorMaterialV1.version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.connector",
        "ConnectorManifest.connector",
        Model,
        "provenance",
        "ProvenanceMaterialV1.connector.id",
        "normalized",
        "scalar",
        "required",
        "ConnectorId",
        "ProvenanceConnectorIdentity.id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ConnectorManifest.connector_version",
        "ConnectorManifest.connector_version",
        Model,
        "provenance",
        "ProvenanceMaterialV1.connector.version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "ProvenanceConnectorIdentity.version",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ConnectorManifest.manifest_version",
        "ConnectorManifest.manifest_version",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.manifest_version",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticConnectorMaterialV1.manifest_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.runtime_abi_epoch",
        "ConnectorManifest.runtime_abi_epoch",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.runtime_abi_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticConnectorMaterialV1.runtime_abi_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.value_language_epoch",
        "ConnectorManifest.value_language_epoch",
        Model,
        "semantic",
        "SemanticMaterialV1.value_language_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticMaterialV1.value_language_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.provider",
        "ConnectorManifest.provider",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.provider",
        "normalized",
        "scalar",
        "required",
        "ProviderId",
        "SemanticConnectorMaterialV1.provider",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.api_identity",
        "ConnectorManifest.api_identity",
        Model,
        "semantic",
        "SemanticMaterialV1.connector.api_identity",
        "normalized",
        "scalar",
        "required",
        "ApiIdentity",
        "SemanticConnectorMaterialV1.api_identity",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.credentials",
        "ConnectorManifest.credentials",
        Model,
        "semantic",
        "SemanticMaterialV1.credentials",
        "normalized",
        "credential",
        "empty_array",
        "Vec<SemanticCredentialMaterialV1>",
        "SemanticMaterialV1.credentials",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.origins",
        "ConnectorManifest.origins",
        Model,
        "semantic",
        "SemanticMaterialV1.origins",
        "normalized",
        "origin",
        "nonempty_array",
        "NonEmptyVec<SemanticOriginMaterialV1>",
        "SemanticMaterialV1.origins",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.operations",
        "ConnectorManifest.operations",
        Model,
        "semantic",
        "SemanticMaterialV1.operations",
        "normalized",
        "operation",
        "nonempty_array",
        "NonEmptyVec<SemanticOperationMaterialV1>",
        "SemanticMaterialV1.operations",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.triggers",
        "ConnectorManifest.triggers",
        Model,
        "semantic",
        "SemanticMaterialV1.triggers",
        "normalized",
        "kind_then_trigger",
        "empty_array",
        "Vec<SemanticTriggerMaterialV1>",
        "SemanticMaterialV1.triggers",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorManifest.provenance",
        "ConnectorManifest.provenance",
        Model,
        "provenance",
        "ProvenanceMaterialV1.manifest_references",
        "normalized",
        "source_record_id",
        "nonempty_array",
        "NonEmptyVec<ManifestProvenanceMaterialV1>",
        "ProvenanceMaterialV1.manifest_references",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "CredentialSpec.credential",
        "CredentialSpec.credential",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.credential",
        "normalized",
        "scalar",
        "required",
        "CredentialSpecId",
        "SemanticCredentialMaterialV1.credential",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.version",
        "CredentialSpec.version",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticCredentialMaterialV1.version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.fields",
        "CredentialSpec.fields",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.fields",
        "normalized",
        "field",
        "nonempty_array",
        "NonEmptyVec<CredentialFieldMaterialV1>",
        "SemanticCredentialMaterialV1.fields",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.auth_plan",
        "CredentialSpec.auth_plan",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.auth_plan",
        "normalized",
        "scalar",
        "required",
        "CredentialAuthMaterialV1",
        "SemanticCredentialMaterialV1.auth_plan",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.allowed_origins",
        "CredentialSpec.allowed_origins",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.allowed_origins",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<OriginId>",
        "SemanticCredentialMaterialV1.allowed_origins",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.scopes",
        "CredentialSpec.scopes",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.scopes",
        "normalized",
        "lexical",
        "empty_array",
        "Vec<StaticScope>",
        "SemanticCredentialMaterialV1.scopes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.auth_processor",
        "CredentialSpec.auth_processor",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.auth_processor",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<VersionedProcessorRef>",
        "SemanticCredentialMaterialV1.auth_processor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.credential_test_operation",
        "CredentialSpec.credential_test_operation",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.credential_test_operation",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<VersionedOperationReference>",
        "SemanticCredentialMaterialV1.credential_test_operation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialSpec.bounds",
        "CredentialSpec.bounds",
        Model,
        "semantic",
        "SemanticCredentialMaterialV1.bounds",
        "normalized",
        "scalar",
        "required",
        "CredentialBoundsMaterialV1",
        "SemanticCredentialMaterialV1.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialFieldSpec.field",
        "CredentialFieldSpec.field",
        Model,
        "semantic",
        "CredentialFieldMaterialV1.field",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialFieldMaterialV1.field",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialFieldSpec.required",
        "CredentialFieldSpec.required",
        Model,
        "semantic",
        "CredentialFieldMaterialV1.required",
        "normalized",
        "scalar",
        "required",
        "bool",
        "CredentialFieldMaterialV1.required",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialFieldSpec.secret",
        "CredentialFieldSpec.secret",
        Model,
        "semantic",
        "CredentialFieldMaterialV1.secret",
        "normalized",
        "scalar",
        "required",
        "SecretClassificationMaterialV1",
        "CredentialFieldMaterialV1.secret",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialFieldSpec.maximum_bytes",
        "CredentialFieldSpec.maximum_bytes",
        Model,
        "semantic",
        "CredentialFieldMaterialV1.maximum_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "CredentialFieldMaterialV1.maximum_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialFieldSpec.redaction",
        "CredentialFieldSpec.redaction",
        Model,
        "semantic",
        "CredentialFieldMaterialV1.redaction",
        "normalized",
        "scalar",
        "required",
        "RedactionMaterialV1",
        "CredentialFieldMaterialV1.redaction",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialBounds.maximum_field_bytes",
        "CredentialBounds.maximum_field_bytes",
        Model,
        "semantic",
        "CredentialBoundsMaterialV1.maximum_field_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "CredentialBoundsMaterialV1.maximum_field_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialBounds.maximum_aggregate_bytes",
        "CredentialBounds.maximum_aggregate_bytes",
        Model,
        "semantic",
        "CredentialBoundsMaterialV1.maximum_aggregate_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "CredentialBoundsMaterialV1.maximum_aggregate_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CredentialBounds.maximum_token_bytes",
        "CredentialBounds.maximum_token_bytes",
        Model,
        "semantic",
        "CredentialBoundsMaterialV1.maximum_token_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "CredentialBoundsMaterialV1.maximum_token_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SecretClassification::Secret",
        "SecretClassification::Secret",
        Model,
        "semantic",
        "SecretClassificationMaterialV1{kind=secret}.kind",
        "normalized",
        "scalar",
        "required",
        "secret",
        "SecretClassificationMaterialV1::Secret",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SecretClassification::Sensitive",
        "SecretClassification::Sensitive",
        Model,
        "semantic",
        "SecretClassificationMaterialV1{kind=sensitive}.kind",
        "normalized",
        "scalar",
        "required",
        "sensitive",
        "SecretClassificationMaterialV1::Sensitive",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SecretClassification::NonSecret",
        "SecretClassification::NonSecret",
        Model,
        "semantic",
        "SecretClassificationMaterialV1{kind=non_secret}.kind",
        "normalized",
        "scalar",
        "required",
        "non_secret",
        "SecretClassificationMaterialV1::NonSecret",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RedactionPlan::Omit",
        "RedactionPlan::Omit",
        Model,
        "semantic",
        "RedactionMaterialV1{kind=omit}.kind",
        "normalized",
        "scalar",
        "required",
        "omit",
        "RedactionMaterialV1::Omit",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RedactionPlan::Fixed",
        "RedactionPlan::Fixed",
        Model,
        "semantic",
        "RedactionMaterialV1{kind=fixed}.kind",
        "normalized",
        "scalar",
        "required",
        "fixed",
        "RedactionMaterialV1::Fixed",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RedactionPlan::PreserveLast",
        "RedactionPlan::PreserveLast",
        Model,
        "semantic",
        "RedactionMaterialV1{kind=preserve_last}.kind",
        "normalized",
        "scalar",
        "required",
        "preserve_last",
        "RedactionMaterialV1::PreserveLast",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RedactionPlan::Fixed.replacement",
        "RedactionPlan::Fixed.replacement",
        Model,
        "semantic",
        "RedactionMaterialV1{kind=fixed}.value.replacement",
        "normalized",
        "scalar",
        "required",
        "string",
        "RedactionMaterialV1::Fixed.replacement",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RedactionPlan::PreserveLast.characters",
        "RedactionPlan::PreserveLast.characters",
        Model,
        "semantic",
        "RedactionMaterialV1{kind=preserve_last}.value.characters",
        "normalized",
        "scalar",
        "required",
        "u8",
        "RedactionMaterialV1::PreserveLast.characters",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedHeaderApiKey",
        "AuthPlan::FixedHeaderApiKey",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_header_api_key}.kind",
        "normalized",
        "scalar",
        "required",
        "fixed_header_api_key",
        "CredentialAuthMaterialV1::FixedHeaderApiKey",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedQueryApiKey",
        "AuthPlan::FixedQueryApiKey",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_query_api_key}.kind",
        "normalized",
        "scalar",
        "required",
        "fixed_query_api_key",
        "CredentialAuthMaterialV1::FixedQueryApiKey",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::Bearer",
        "AuthPlan::Bearer",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=bearer}.kind",
        "normalized",
        "scalar",
        "required",
        "bearer",
        "CredentialAuthMaterialV1::Bearer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::HttpBasic",
        "AuthPlan::HttpBasic",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=http_basic}.kind",
        "normalized",
        "scalar",
        "required",
        "http_basic",
        "CredentialAuthMaterialV1::HttpBasic",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials",
        "AuthPlan::OAuth2ClientCredentials",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.kind",
        "normalized",
        "scalar",
        "required",
        "oauth2_client_credentials",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::PreprovisionedOAuthAccessToken",
        "AuthPlan::PreprovisionedOAuthAccessToken",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.kind",
        "normalized",
        "scalar",
        "required",
        "preprovisioned_oauth_access_token",
        "CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedHeaderApiKey.field",
        "AuthPlan::FixedHeaderApiKey.field",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.field",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::FixedHeaderApiKey.field",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedHeaderApiKey.header",
        "AuthPlan::FixedHeaderApiKey.header",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.header",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "CredentialAuthMaterialV1::FixedHeaderApiKey.header",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedQueryApiKey.field",
        "AuthPlan::FixedQueryApiKey.field",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.field",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::FixedQueryApiKey.field",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::FixedQueryApiKey.query",
        "AuthPlan::FixedQueryApiKey.query",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.query",
        "normalized",
        "scalar",
        "required",
        "StaticQueryKey",
        "CredentialAuthMaterialV1::FixedQueryApiKey.query",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::Bearer.token",
        "AuthPlan::Bearer.token",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=bearer}.value.token",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::Bearer.token",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::HttpBasic.username",
        "AuthPlan::HttpBasic.username",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=http_basic}.value.username",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::HttpBasic.username",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::HttpBasic.password",
        "AuthPlan::HttpBasic.password",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=http_basic}.value.password",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::HttpBasic.password",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.client_id",
        "AuthPlan::OAuth2ClientCredentials.client_id",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_id",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.client_id",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.client_secret",
        "AuthPlan::OAuth2ClientCredentials.client_secret",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_secret",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.client_secret",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.token_origin",
        "AuthPlan::OAuth2ClientCredentials.token_origin",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_origin",
        "normalized",
        "scalar",
        "required",
        "OriginId",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_origin",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.token_step",
        "AuthPlan::OAuth2ClientCredentials.token_step",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_step",
        "normalized",
        "scalar",
        "required",
        "CompiledStepId",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_step",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.scopes",
        "AuthPlan::OAuth2ClientCredentials.scopes",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.scopes",
        "normalized",
        "lexical",
        "empty_array",
        "Vec<StaticScope>",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.scopes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::OAuth2ClientCredentials.token_pointer",
        "AuthPlan::OAuth2ClientCredentials.token_pointer",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_pointer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "AuthPlan::PreprovisionedOAuthAccessToken.token",
        "AuthPlan::PreprovisionedOAuthAccessToken.token",
        Model,
        "semantic",
        "CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.value.token",
        "normalized",
        "scalar",
        "required",
        "CredentialFieldId",
        "CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken.token",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedOrigin.origin",
        "FixedOrigin.origin",
        Model,
        "semantic",
        "SemanticOriginMaterialV1.origin",
        "normalized",
        "scalar",
        "required",
        "OriginId",
        "SemanticOriginMaterialV1.origin",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedOrigin.scheme",
        "FixedOrigin.scheme",
        Model,
        "semantic",
        "SemanticOriginMaterialV1.scheme",
        "normalized",
        "scalar",
        "required",
        "HttpsOnly",
        "SemanticOriginMaterialV1.scheme",
        ProjectionSchema,
        Semantic,
        Singleton,
    );

    (
        "FixedOrigin.host",
        "FixedOrigin.host",
        Model,
        "semantic",
        "SemanticOriginMaterialV1.host",
        "normalized",
        "scalar",
        "required",
        "StaticDnsName",
        "SemanticOriginMaterialV1.host",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedOrigin.port",
        "FixedOrigin.port",
        Model,
        "semantic",
        "SemanticOriginMaterialV1.port",
        "normalized",
        "scalar",
        "required",
        "u16",
        "SemanticOriginMaterialV1.port",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedOrigin.network_policy",
        "FixedOrigin.network_policy",
        Model,
        "semantic",
        "SemanticOriginMaterialV1.network_policy",
        "normalized",
        "scalar",
        "required",
        "NetworkPolicyMaterialV1",
        "SemanticOriginMaterialV1.network_policy",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "NetworkPolicy::PublicOnly",
        "NetworkPolicy::PublicOnly",
        Model,
        "semantic",
        "NetworkPolicyMaterialV1{kind=public_only}.kind",
        "normalized",
        "scalar",
        "required",
        "public_only",
        "NetworkPolicyMaterialV1::PublicOnly",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "NetworkPolicy::PrivateAllowed",
        "NetworkPolicy::PrivateAllowed",
        Model,
        "semantic",
        "NetworkPolicyMaterialV1{kind=private_allowed}.kind",
        "normalized",
        "scalar",
        "required",
        "private_allowed",
        "NetworkPolicyMaterialV1::PrivateAllowed",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "NetworkPolicy::PrivateAllowed.policy",
        "NetworkPolicy::PrivateAllowed.policy",
        Model,
        "semantic",
        "NetworkPolicyMaterialV1{kind=private_allowed}.value.policy",
        "normalized",
        "scalar",
        "required",
        "Id",
        "NetworkPolicyMaterialV1::PrivateAllowed.policy",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.connector",
        "OperationSpec.connector",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.connector",
        "normalized",
        "scalar",
        "required",
        "ConnectorId",
        "SemanticOperationMaterialV1.connector",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.connector_version",
        "OperationSpec.connector_version",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.connector_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticOperationMaterialV1.connector_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.operation",
        "OperationSpec.operation",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.operation",
        "normalized",
        "scalar",
        "required",
        "OperationId",
        "SemanticOperationMaterialV1.operation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.operation_version",
        "OperationSpec.operation_version",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.operation_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticOperationMaterialV1.operation_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.runtime_abi_epoch",
        "OperationSpec.runtime_abi_epoch",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.runtime_abi_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticOperationMaterialV1.runtime_abi_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.value_language_epoch",
        "OperationSpec.value_language_epoch",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.value_language_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticOperationMaterialV1.value_language_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.input",
        "OperationSpec.input",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.input",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticOperationMaterialV1.input",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.input_contract_sha256",
        "OperationSpec.input_contract_sha256",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.input_contract_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "SemanticOperationMaterialV1.input_contract_sha256",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.output",
        "OperationSpec.output",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.output",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticOperationMaterialV1.output",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.output_contract_sha256",
        "OperationSpec.output_contract_sha256",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.output_contract_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "SemanticOperationMaterialV1.output_contract_sha256",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.credential",
        "OperationSpec.credential",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.credential",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<VersionedCredentialMaterialV1>",
        "SemanticOperationMaterialV1.credential",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.origins",
        "OperationSpec.origins",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.origins",
        "normalized",
        "origin",
        "nonempty_array",
        "NonEmptyVec<SemanticOriginMaterialV1>",
        "SemanticOperationMaterialV1.origins",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.steps",
        "OperationSpec.steps",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.steps",
        "normalized",
        "declared",
        "nonempty_array",
        "NonEmptyVec<SemanticStepMaterialV1>",
        "SemanticOperationMaterialV1.steps",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.pre_request_transforms",
        "OperationSpec.pre_request_transforms",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.pre_request_transforms",
        "normalized",
        "declared",
        "empty_array",
        "Vec<VersionedProcessorMaterialV1>",
        "SemanticOperationMaterialV1.pre_request_transforms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.post_response_transforms",
        "OperationSpec.post_response_transforms",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.post_response_transforms",
        "normalized",
        "declared",
        "empty_array",
        "Vec<VersionedProcessorMaterialV1>",
        "SemanticOperationMaterialV1.post_response_transforms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.operation_processor",
        "OperationSpec.operation_processor",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.operation_processor",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<VersionedProcessorMaterialV1>",
        "SemanticOperationMaterialV1.operation_processor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.effect",
        "OperationSpec.effect",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.effect",
        "normalized",
        "scalar",
        "required",
        "OperationEffectMaterialV1",
        "SemanticOperationMaterialV1.effect",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.pagination",
        "OperationSpec.pagination",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.pagination",
        "normalized",
        "scalar",
        "required",
        "PaginationMaterialV1",
        "SemanticOperationMaterialV1.pagination",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.error_map",
        "OperationSpec.error_map",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.error_map",
        "normalized",
        "scalar",
        "required",
        "ErrorMapMaterialV1",
        "SemanticOperationMaterialV1.error_map",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.capacity",
        "OperationSpec.capacity",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.capacity",
        "normalized",
        "scalar",
        "required",
        "CapacityDefaultsMaterialV1",
        "SemanticOperationMaterialV1.capacity",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.rate",
        "OperationSpec.rate",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.rate",
        "normalized",
        "scalar",
        "required",
        "RateDefaultsMaterialV1",
        "SemanticOperationMaterialV1.rate",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.serialization_key_default",
        "OperationSpec.serialization_key_default",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.serialization_key_default",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<TypedSerializationKeyDefaultMaterialV1>",
        "SemanticOperationMaterialV1.serialization_key_default",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.bounds",
        "OperationSpec.bounds",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.bounds",
        "normalized",
        "scalar",
        "required",
        "OperationBoundsMaterialV1",
        "SemanticOperationMaterialV1.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationSpec.resolved_fact_values",
        "OperationSpec.resolved_fact_values",
        Model,
        "semantic",
        "SemanticOperationMaterialV1.resolved_fact_values",
        "normalized",
        "use_site",
        "empty_array",
        "Vec<ResolvedFactValueMaterialV1>",
        "SemanticOperationMaterialV1.resolved_fact_values",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedCredentialReference.credential",
        "VersionedCredentialReference.credential",
        Model,
        "semantic",
        "VersionedCredentialMaterialV1.credential",
        "normalized",
        "scalar",
        "required",
        "CredentialSpecId",
        "VersionedCredentialMaterialV1.credential",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedCredentialReference.version",
        "VersionedCredentialReference.version",
        Model,
        "semantic",
        "VersionedCredentialMaterialV1.version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "VersionedCredentialMaterialV1.version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedProcessorRef.id",
        "VersionedProcessorRef.id",
        Model,
        "semantic",
        "VersionedProcessorMaterialV1.id",
        "normalized",
        "scalar",
        "required",
        "typed_processor_id",
        "VersionedProcessorMaterialV1.id",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedProcessorRef.implementation_revision",
        "VersionedProcessorRef.implementation_revision",
        Model,
        "semantic",
        "VersionedProcessorMaterialV1.implementation_revision",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "VersionedProcessorMaterialV1.implementation_revision",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedOperationReference.operation",
        "VersionedOperationReference.operation",
        Model,
        "semantic",
        "VersionedOperationReferenceMaterialV1.operation",
        "normalized",
        "scalar",
        "required",
        "OperationId",
        "VersionedOperationReferenceMaterialV1.operation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "VersionedOperationReference.version",
        "VersionedOperationReference.version",
        Model,
        "semantic",
        "VersionedOperationReferenceMaterialV1.version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "VersionedOperationReferenceMaterialV1.version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.step",
        "CompiledStepSpec.step",
        Model,
        "semantic",
        "SemanticStepMaterialV1.step",
        "normalized",
        "scalar",
        "required",
        "CompiledStepId",
        "SemanticStepMaterialV1.step",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.method",
        "CompiledStepSpec.method",
        Model,
        "semantic",
        "SemanticStepMaterialV1.method",
        "normalized",
        "scalar",
        "required",
        "StaticHttpMethod",
        "SemanticStepMaterialV1.method",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.origin",
        "CompiledStepSpec.origin",
        Model,
        "semantic",
        "SemanticStepMaterialV1.origin",
        "normalized",
        "scalar",
        "required",
        "OriginId",
        "SemanticStepMaterialV1.origin",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.path",
        "CompiledStepSpec.path",
        Model,
        "semantic",
        "SemanticStepMaterialV1.path",
        "normalized",
        "scalar",
        "required",
        "StaticPathTemplate",
        "SemanticStepMaterialV1.path",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.query",
        "CompiledStepSpec.query",
        Model,
        "semantic",
        "SemanticStepMaterialV1.query",
        "normalized",
        "name",
        "empty_array",
        "Vec<CompiledQueryBindingMaterialV1>",
        "SemanticStepMaterialV1.query",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.headers",
        "CompiledStepSpec.headers",
        Model,
        "semantic",
        "SemanticStepMaterialV1.headers",
        "normalized",
        "name",
        "empty_array",
        "Vec<CompiledHeaderBindingMaterialV1>",
        "SemanticStepMaterialV1.headers",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.credential_action",
        "CompiledStepSpec.credential_action",
        Model,
        "semantic",
        "SemanticStepMaterialV1.credential_action",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<CompiledCredentialActionMaterialV1>",
        "SemanticStepMaterialV1.credential_action",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.request",
        "CompiledStepSpec.request",
        Model,
        "semantic",
        "SemanticStepMaterialV1.request",
        "normalized",
        "scalar",
        "required",
        "CompiledRequestMaterialV1",
        "SemanticStepMaterialV1.request",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.success_statuses",
        "CompiledStepSpec.success_statuses",
        Model,
        "semantic",
        "SemanticStepMaterialV1.success_statuses",
        "normalized",
        "minimum_then_maximum",
        "nonempty_array",
        "NonEmptyVec<StatusRangeMaterialV1>",
        "SemanticStepMaterialV1.success_statuses",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.response",
        "CompiledStepSpec.response",
        Model,
        "semantic",
        "SemanticStepMaterialV1.response",
        "normalized",
        "scalar",
        "required",
        "CompiledResponseMaterialV1",
        "SemanticStepMaterialV1.response",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.selected_response_headers",
        "CompiledStepSpec.selected_response_headers",
        Model,
        "semantic",
        "SemanticStepMaterialV1.selected_response_headers",
        "normalized",
        "canonical_lowercase_header_name",
        "empty_array",
        "Vec<SelectedResponseHeaderMaterialV1>",
        "SemanticStepMaterialV1.selected_response_headers",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledStepSpec.bounds",
        "CompiledStepSpec.bounds",
        Model,
        "semantic",
        "SemanticStepMaterialV1.bounds",
        "normalized",
        "scalar",
        "required",
        "StepBoundsMaterialV1",
        "SemanticStepMaterialV1.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledQueryBinding.name",
        "CompiledQueryBinding.name",
        Model,
        "semantic",
        "CompiledQueryBindingMaterialV1.name",
        "normalized",
        "scalar",
        "required",
        "StaticQueryKey",
        "CompiledQueryBindingMaterialV1.name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledQueryBinding.binding",
        "CompiledQueryBinding.binding",
        Model,
        "semantic",
        "CompiledQueryBindingMaterialV1.binding",
        "normalized",
        "scalar",
        "required",
        "BindingMaterialV1",
        "CompiledQueryBindingMaterialV1.binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledHeaderBinding.name",
        "CompiledHeaderBinding.name",
        Model,
        "semantic",
        "CompiledHeaderBindingMaterialV1.name",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "CompiledHeaderBindingMaterialV1.name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledHeaderBinding.binding",
        "CompiledHeaderBinding.binding",
        Model,
        "semantic",
        "CompiledHeaderBindingMaterialV1.binding",
        "normalized",
        "scalar",
        "required",
        "BindingMaterialV1",
        "CompiledHeaderBindingMaterialV1.binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBinding.field",
        "CompiledBinding.field",
        Model,
        "semantic",
        "BindingMaterialV1.field",
        "normalized",
        "scalar",
        "required",
        "Id",
        "BindingMaterialV1.field",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBinding.source",
        "CompiledBinding.source",
        Model,
        "semantic",
        "BindingMaterialV1.source",
        "normalized",
        "scalar",
        "required",
        "CompiledBindingSourceMaterialV1",
        "BindingMaterialV1.source",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBinding.required",
        "CompiledBinding.required",
        Model,
        "semantic",
        "BindingMaterialV1.required",
        "normalized",
        "scalar",
        "required",
        "bool",
        "BindingMaterialV1.required",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBinding.default",
        "CompiledBinding.default",
        Model,
        "semantic",
        "BindingMaterialV1.default",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<TypedValueMaterialV1>",
        "BindingMaterialV1.default",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBinding.mapping",
        "CompiledBinding.mapping",
        Model,
        "semantic",
        "BindingMaterialV1.mapping",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<Id>",
        "BindingMaterialV1.mapping",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBindingSource::Input",
        "CompiledBindingSource::Input",
        Model,
        "semantic",
        "CompiledBindingSourceMaterialV1{kind=input}.kind",
        "normalized",
        "scalar",
        "required",
        "input",
        "CompiledBindingSourceMaterialV1::Input",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBindingSource::Constant",
        "CompiledBindingSource::Constant",
        Model,
        "semantic",
        "CompiledBindingSourceMaterialV1{kind=constant}.kind",
        "normalized",
        "scalar",
        "required",
        "constant",
        "CompiledBindingSourceMaterialV1::Constant",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledBindingSource::Constant.value",
        "CompiledBindingSource::Constant.value",
        Model,
        "semantic",
        "CompiledBindingSourceMaterialV1{kind=constant}.value.value",
        "normalized",
        "scalar",
        "required",
        "TypedValueMaterialV1",
        "CompiledBindingSourceMaterialV1::Constant.value",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledCredentialAction.credential",
        "CompiledCredentialAction.credential",
        Model,
        "semantic",
        "CompiledCredentialActionMaterialV1.credential",
        "normalized",
        "scalar",
        "required",
        "CredentialSpecId",
        "CompiledCredentialActionMaterialV1.credential",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::None",
        "CompiledRequestShape::None",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=none}.kind",
        "normalized",
        "scalar",
        "required",
        "none",
        "CompiledRequestMaterialV1::None",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::Json",
        "CompiledRequestShape::Json",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=json}.kind",
        "normalized",
        "scalar",
        "required",
        "json",
        "CompiledRequestMaterialV1::Json",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::FormUrlencoded",
        "CompiledRequestShape::FormUrlencoded",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=form_urlencoded}.kind",
        "normalized",
        "scalar",
        "required",
        "form_urlencoded",
        "CompiledRequestMaterialV1::FormUrlencoded",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::Multipart",
        "CompiledRequestShape::Multipart",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=multipart}.kind",
        "normalized",
        "scalar",
        "required",
        "multipart",
        "CompiledRequestMaterialV1::Multipart",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::RawBytes",
        "CompiledRequestShape::RawBytes",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=raw_bytes}.kind",
        "normalized",
        "scalar",
        "required",
        "raw_bytes",
        "CompiledRequestMaterialV1::RawBytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::Json.bindings",
        "CompiledRequestShape::Json.bindings",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=json}.value.bindings",
        "normalized",
        "declared",
        "empty_array",
        "Vec<Id>",
        "CompiledRequestMaterialV1::Json.bindings",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::FormUrlencoded.bindings",
        "CompiledRequestShape::FormUrlencoded.bindings",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=form_urlencoded}.value.bindings",
        "normalized",
        "declared",
        "empty_array",
        "Vec<Id>",
        "CompiledRequestMaterialV1::FormUrlencoded.bindings",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::Multipart.bindings",
        "CompiledRequestShape::Multipart.bindings",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=multipart}.value.bindings",
        "normalized",
        "declared",
        "empty_array",
        "Vec<Id>",
        "CompiledRequestMaterialV1::Multipart.bindings",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledRequestShape::RawBytes.binding",
        "CompiledRequestShape::RawBytes.binding",
        Model,
        "semantic",
        "CompiledRequestMaterialV1{kind=raw_bytes}.value.binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "CompiledRequestMaterialV1::RawBytes.binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledResponseShape::Json",
        "CompiledResponseShape::Json",
        Model,
        "semantic",
        "CompiledResponseMaterialV1{kind=json}.kind",
        "normalized",
        "scalar",
        "required",
        "json",
        "CompiledResponseMaterialV1::Json",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledResponseShape::RawBytes",
        "CompiledResponseShape::RawBytes",
        Model,
        "semantic",
        "CompiledResponseMaterialV1{kind=raw_bytes}.kind",
        "normalized",
        "scalar",
        "required",
        "raw_bytes",
        "CompiledResponseMaterialV1::RawBytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledResponseShape::Json.mappings",
        "CompiledResponseShape::Json.mappings",
        Model,
        "semantic",
        "CompiledResponseMaterialV1{kind=json}.value.mappings",
        "normalized",
        "declared",
        "empty_array",
        "Vec<ResponseMappingMaterialV1>",
        "CompiledResponseMaterialV1::Json.mappings",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompiledResponseShape::RawBytes.target",
        "CompiledResponseShape::RawBytes.target",
        Model,
        "semantic",
        "CompiledResponseMaterialV1{kind=raw_bytes}.value.target",
        "normalized",
        "scalar",
        "required",
        "Id",
        "CompiledResponseMaterialV1::RawBytes.target",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ResponseMapping.pointer",
        "ResponseMapping.pointer",
        Model,
        "semantic",
        "ResponseMappingMaterialV1.pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "ResponseMappingMaterialV1.pointer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ResponseMapping.target",
        "ResponseMapping.target",
        Model,
        "semantic",
        "ResponseMappingMaterialV1.target",
        "normalized",
        "scalar",
        "required",
        "Id",
        "ResponseMappingMaterialV1.target",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StatusRange.minimum",
        "StatusRange.minimum",
        Model,
        "semantic",
        "StatusRangeMaterialV1.minimum",
        "normalized",
        "scalar",
        "required",
        "u16",
        "StatusRangeMaterialV1.minimum",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StatusRange.maximum",
        "StatusRange.maximum",
        Model,
        "semantic",
        "StatusRangeMaterialV1.maximum",
        "normalized",
        "scalar",
        "required",
        "u16",
        "StatusRangeMaterialV1.maximum",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SelectedResponseHeader.canonical_lowercase_header_name",
        "SelectedResponseHeader.canonical_lowercase_header_name",
        Model,
        "semantic",
        "SelectedResponseHeaderMaterialV1.canonical_lowercase_header_name",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "SelectedResponseHeaderMaterialV1.canonical_lowercase_header_name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SelectedResponseHeader.capability",
        "SelectedResponseHeader.capability",
        Model,
        "semantic",
        "SelectedResponseHeaderMaterialV1.capability",
        "normalized",
        "scalar",
        "required",
        "CapabilityId",
        "SelectedResponseHeaderMaterialV1.capability",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_headers",
        "StepBounds.maximum_headers",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_headers",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_headers",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_header_bytes",
        "StepBounds.maximum_header_bytes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_header_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_header_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_url_bytes",
        "StepBounds.maximum_url_bytes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_url_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_url_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_request_bytes",
        "StepBounds.maximum_request_bytes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_request_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_request_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_response_bytes",
        "StepBounds.maximum_response_bytes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_response_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_response_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_json_depth",
        "StepBounds.maximum_json_depth",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_json_depth",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_json_depth",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_json_nodes",
        "StepBounds.maximum_json_nodes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_json_nodes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_json_nodes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.maximum_inline_binary_bytes",
        "StepBounds.maximum_inline_binary_bytes",
        Model,
        "semantic",
        "StepBoundsMaterialV1.maximum_inline_binary_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StepBoundsMaterialV1.maximum_inline_binary_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "StepBounds.deadline_ms",
        "StepBounds.deadline_ms",
        Model,
        "semantic",
        "StepBoundsMaterialV1.deadline_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "StepBoundsMaterialV1.deadline_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_calls",
        "OperationBounds.maximum_calls",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_calls",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_calls",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_pages",
        "OperationBounds.maximum_pages",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_pages",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_pages",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_items",
        "OperationBounds.maximum_items",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_items",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_items",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_aggregate_request_bytes",
        "OperationBounds.maximum_aggregate_request_bytes",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_aggregate_request_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_aggregate_request_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_aggregate_response_bytes",
        "OperationBounds.maximum_aggregate_response_bytes",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_aggregate_response_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_aggregate_response_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_output_canonical_bytes",
        "OperationBounds.maximum_output_canonical_bytes",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_output_canonical_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "OperationBoundsMaterialV1.maximum_output_canonical_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.maximum_redirects",
        "OperationBounds.maximum_redirects",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.maximum_redirects",
        "normalized",
        "scalar",
        "required",
        "u8",
        "OperationBoundsMaterialV1.maximum_redirects",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationBounds.deadline_ms",
        "OperationBounds.deadline_ms",
        Model,
        "semantic",
        "OperationBoundsMaterialV1.deadline_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "OperationBoundsMaterialV1.deadline_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationEffect::ReadOnly",
        "OperationEffect::ReadOnly",
        Model,
        "semantic",
        "OperationEffectMaterialV1{kind=read_only}.kind",
        "normalized",
        "scalar",
        "required",
        "read_only",
        "OperationEffectMaterialV1::ReadOnly",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationEffect::ProviderIdempotent",
        "OperationEffect::ProviderIdempotent",
        Model,
        "semantic",
        "OperationEffectMaterialV1{kind=provider_idempotent}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_idempotent",
        "OperationEffectMaterialV1::ProviderIdempotent",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "OperationEffect::ProviderIdempotent.side_effect_steps",
        "OperationEffect::ProviderIdempotent.side_effect_steps",
        Model,
        "semantic",
        "OperationEffectMaterialV1{kind=provider_idempotent}.value.side_effect_steps",
        "normalized",
        "step",
        "nonempty_array",
        "NonEmptyVec<ProviderIdempotentStepMaterialV1>",
        "OperationEffectMaterialV1::ProviderIdempotent.side_effect_steps",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ProviderIdempotentStep.step",
        "ProviderIdempotentStep.step",
        Model,
        "semantic",
        "ProviderIdempotentStepMaterialV1.step",
        "normalized",
        "scalar",
        "required",
        "CompiledStepId",
        "ProviderIdempotentStepMaterialV1.step",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ProviderIdempotentStep.fixed_binding",
        "ProviderIdempotentStep.fixed_binding",
        Model,
        "semantic",
        "ProviderIdempotentStepMaterialV1.fixed_binding",
        "normalized",
        "scalar",
        "required",
        "FixedIdempotencyBindingMaterialV1",
        "ProviderIdempotentStepMaterialV1.fixed_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ProviderIdempotentStep.scope",
        "ProviderIdempotentStep.scope",
        Model,
        "semantic",
        "ProviderIdempotentStepMaterialV1.scope",
        "normalized",
        "scalar",
        "required",
        "ProviderIdempotencyScope",
        "ProviderIdempotentStepMaterialV1.scope",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ProviderIdempotentStep.minimum_retention_ms",
        "ProviderIdempotentStep.minimum_retention_ms",
        Model,
        "semantic",
        "ProviderIdempotentStepMaterialV1.minimum_retention_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "ProviderIdempotentStepMaterialV1.minimum_retention_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ProviderIdempotentStep.clock_safety_margin_ms",
        "ProviderIdempotentStep.clock_safety_margin_ms",
        Model,
        "semantic",
        "ProviderIdempotentStepMaterialV1.clock_safety_margin_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "ProviderIdempotentStepMaterialV1.clock_safety_margin_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedIdempotencyBinding::Header",
        "FixedIdempotencyBinding::Header",
        Model,
        "semantic",
        "FixedIdempotencyBindingMaterialV1{kind=header}.kind",
        "normalized",
        "scalar",
        "required",
        "header",
        "FixedIdempotencyBindingMaterialV1::Header",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedIdempotencyBinding::BodyField",
        "FixedIdempotencyBinding::BodyField",
        Model,
        "semantic",
        "FixedIdempotencyBindingMaterialV1{kind=body_field}.kind",
        "normalized",
        "scalar",
        "required",
        "body_field",
        "FixedIdempotencyBindingMaterialV1::BodyField",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedIdempotencyBinding::Header.name",
        "FixedIdempotencyBinding::Header.name",
        Model,
        "semantic",
        "FixedIdempotencyBindingMaterialV1{kind=header}.value.name",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "FixedIdempotencyBindingMaterialV1::Header.name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "FixedIdempotencyBinding::BodyField.pointer",
        "FixedIdempotencyBinding::BodyField.pointer",
        Model,
        "semantic",
        "FixedIdempotencyBindingMaterialV1{kind=body_field}.value.pointer",
        "normalized",
        "scalar",
        "required",
        "StaticBodyPointer",
        "FixedIdempotencyBindingMaterialV1::BodyField.pointer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::None",
        "PaginationPlan::None",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=none}.kind",
        "normalized",
        "scalar",
        "required",
        "none",
        "PaginationMaterialV1::None",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Cursor",
        "PaginationPlan::Cursor",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=cursor}.kind",
        "normalized",
        "scalar",
        "required",
        "cursor",
        "PaginationMaterialV1::Cursor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit",
        "PaginationPlan::OffsetLimit",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.kind",
        "normalized",
        "scalar",
        "required",
        "offset_limit",
        "PaginationMaterialV1::OffsetLimit",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber",
        "PaginationPlan::PageNumber",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.kind",
        "normalized",
        "scalar",
        "required",
        "page_number",
        "PaginationMaterialV1::PageNumber",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::LinkRelation",
        "PaginationPlan::LinkRelation",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=link_relation}.kind",
        "normalized",
        "scalar",
        "required",
        "link_relation",
        "PaginationMaterialV1::LinkRelation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Processor",
        "PaginationPlan::Processor",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=processor}.kind",
        "normalized",
        "scalar",
        "required",
        "processor",
        "PaginationMaterialV1::Processor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Cursor.request_binding",
        "PaginationPlan::Cursor.request_binding",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=cursor}.value.request_binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "PaginationMaterialV1::Cursor.request_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Cursor.response_pointer",
        "PaginationPlan::Cursor.response_pointer",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=cursor}.value.response_pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "PaginationMaterialV1::Cursor.response_pointer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Cursor.bounds",
        "PaginationPlan::Cursor.bounds",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=cursor}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "PaginationBoundsMaterialV1",
        "PaginationMaterialV1::Cursor.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit.offset_binding",
        "PaginationPlan::OffsetLimit.offset_binding",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.value.offset_binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "PaginationMaterialV1::OffsetLimit.offset_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit.limit_binding",
        "PaginationPlan::OffsetLimit.limit_binding",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.value.limit_binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "PaginationMaterialV1::OffsetLimit.limit_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit.initial_offset",
        "PaginationPlan::OffsetLimit.initial_offset",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.value.initial_offset",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "PaginationMaterialV1::OffsetLimit.initial_offset",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit.page_size",
        "PaginationPlan::OffsetLimit.page_size",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.value.page_size",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationMaterialV1::OffsetLimit.page_size",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::OffsetLimit.bounds",
        "PaginationPlan::OffsetLimit.bounds",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=offset_limit}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "PaginationBoundsMaterialV1",
        "PaginationMaterialV1::OffsetLimit.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber.page_binding",
        "PaginationPlan::PageNumber.page_binding",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.value.page_binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "PaginationMaterialV1::PageNumber.page_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber.page_size_binding",
        "PaginationPlan::PageNumber.page_size_binding",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.value.page_size_binding",
        "normalized",
        "scalar",
        "required",
        "Id",
        "PaginationMaterialV1::PageNumber.page_size_binding",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber.initial_page",
        "PaginationPlan::PageNumber.initial_page",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.value.initial_page",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "PaginationMaterialV1::PageNumber.initial_page",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber.page_size",
        "PaginationPlan::PageNumber.page_size",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.value.page_size",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationMaterialV1::PageNumber.page_size",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::PageNumber.bounds",
        "PaginationPlan::PageNumber.bounds",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=page_number}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "PaginationBoundsMaterialV1",
        "PaginationMaterialV1::PageNumber.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::LinkRelation.relation",
        "PaginationPlan::LinkRelation.relation",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=link_relation}.value.relation",
        "normalized",
        "scalar",
        "required",
        "string",
        "PaginationMaterialV1::LinkRelation.relation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::LinkRelation.selected_header",
        "PaginationPlan::LinkRelation.selected_header",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=link_relation}.value.selected_header",
        "normalized",
        "scalar",
        "required",
        "SelectedResponseHeaderMaterialV1",
        "PaginationMaterialV1::LinkRelation.selected_header",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::LinkRelation.bounds",
        "PaginationPlan::LinkRelation.bounds",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=link_relation}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "PaginationBoundsMaterialV1",
        "PaginationMaterialV1::LinkRelation.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Processor.processor",
        "PaginationPlan::Processor.processor",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=processor}.value.processor",
        "normalized",
        "scalar",
        "required",
        "VersionedProcessorMaterialV1",
        "PaginationMaterialV1::Processor.processor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationPlan::Processor.bounds",
        "PaginationPlan::Processor.bounds",
        Model,
        "semantic",
        "PaginationMaterialV1{kind=processor}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "PaginationBoundsMaterialV1",
        "PaginationMaterialV1::Processor.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_calls",
        "PaginationBounds.maximum_calls",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_calls",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_calls",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_pages",
        "PaginationBounds.maximum_pages",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_pages",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_pages",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_items",
        "PaginationBounds.maximum_items",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_items",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_items",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_response_bytes",
        "PaginationBounds.maximum_response_bytes",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_response_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_response_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_aggregate_response_bytes",
        "PaginationBounds.maximum_aggregate_response_bytes",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_aggregate_response_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_aggregate_response_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "PaginationBounds.maximum_output_canonical_bytes",
        "PaginationBounds.maximum_output_canonical_bytes",
        Model,
        "semantic",
        "PaginationBoundsMaterialV1.maximum_output_canonical_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "PaginationBoundsMaterialV1.maximum_output_canonical_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CapacityDefaults.maximum_in_flight",
        "CapacityDefaults.maximum_in_flight",
        Model,
        "semantic",
        "CapacityDefaultsMaterialV1.maximum_in_flight",
        "normalized",
        "scalar",
        "required",
        "u32",
        "CapacityDefaultsMaterialV1.maximum_in_flight",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RateDefaults.burst",
        "RateDefaults.burst",
        Model,
        "semantic",
        "RateDefaultsMaterialV1.burst",
        "normalized",
        "scalar",
        "required",
        "u32",
        "RateDefaultsMaterialV1.burst",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RateDefaults.refill_interval_ms",
        "RateDefaults.refill_interval_ms",
        Model,
        "semantic",
        "RateDefaultsMaterialV1.refill_interval_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "RateDefaultsMaterialV1.refill_interval_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TypedSerializationKeyDefault.field",
        "TypedSerializationKeyDefault.field",
        Model,
        "semantic",
        "TypedSerializationKeyDefaultMaterialV1.field",
        "normalized",
        "scalar",
        "required",
        "Id",
        "TypedSerializationKeyDefaultMaterialV1.field",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TypedSerializationKeyDefault.value",
        "TypedSerializationKeyDefault.value",
        Model,
        "semantic",
        "TypedSerializationKeyDefaultMaterialV1.value",
        "normalized",
        "scalar",
        "required",
        "TypedValueMaterialV1",
        "TypedSerializationKeyDefaultMaterialV1.value",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ResolvedFactValue.use_site",
        "ResolvedFactValue.use_site",
        Model,
        "semantic",
        "ResolvedFactValueMaterialV1.use_site",
        "normalized",
        "scalar",
        "required",
        "Id",
        "ResolvedFactValueMaterialV1.use_site",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ResolvedFactValue.value",
        "ResolvedFactValue.value",
        Model,
        "semantic",
        "ResolvedFactValueMaterialV1.value",
        "normalized",
        "scalar",
        "required",
        "TypedValueMaterialV1",
        "ResolvedFactValueMaterialV1.value",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMap.rules",
        "ErrorMap.rules",
        Model,
        "semantic",
        "ErrorMapMaterialV1.rules",
        "normalized",
        "declared",
        "empty_array",
        "Vec<ErrorRuleMaterialV1>",
        "ErrorMapMaterialV1.rules",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMap.fallback",
        "ErrorMap.fallback",
        Model,
        "semantic",
        "ErrorMapMaterialV1.fallback",
        "normalized",
        "scalar",
        "required",
        "CompleteErrorFallbackMaterialV1",
        "ErrorMapMaterialV1.fallback",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorRule.matcher",
        "ErrorRule.matcher",
        Model,
        "semantic",
        "ErrorRuleMaterialV1.matcher",
        "normalized",
        "scalar",
        "required",
        "ErrorMatcherMaterialV1",
        "ErrorRuleMaterialV1.matcher",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorRule.action",
        "ErrorRule.action",
        Model,
        "semantic",
        "ErrorRuleMaterialV1.action",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "ErrorRuleMaterialV1.action",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.transport",
        "CompleteErrorFallback.transport",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.transport",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.transport",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.timeout",
        "CompleteErrorFallback.timeout",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.timeout",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.timeout",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.http_429",
        "CompleteErrorFallback.http_429",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.http_429",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.http_429",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.http_5xx",
        "CompleteErrorFallback.http_5xx",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.http_5xx",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.http_5xx",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.authentication",
        "CompleteErrorFallback.authentication",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.authentication",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.authentication",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.validation",
        "CompleteErrorFallback.validation",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.validation",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.validation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.permanent",
        "CompleteErrorFallback.permanent",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.permanent",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.permanent",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "CompleteErrorFallback.invariant",
        "CompleteErrorFallback.invariant",
        Model,
        "semantic",
        "CompleteErrorFallbackMaterialV1.invariant",
        "normalized",
        "scalar",
        "required",
        "ErrorActionMaterialV1",
        "CompleteErrorFallbackMaterialV1.invariant",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorAction.class",
        "ErrorAction.class",
        Model,
        "semantic",
        "ErrorActionMaterialV1.class",
        "normalized",
        "scalar",
        "required",
        "ConnectorErrorClassMaterialV1",
        "ErrorActionMaterialV1.class",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorAction.code",
        "ErrorAction.code",
        Model,
        "semantic",
        "ErrorActionMaterialV1.code",
        "normalized",
        "scalar",
        "required",
        "StaticErrorCode",
        "ErrorActionMaterialV1.code",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorAction.safe_message",
        "ErrorAction.safe_message",
        Model,
        "semantic",
        "ErrorActionMaterialV1.safe_message",
        "normalized",
        "scalar",
        "required",
        "StaticSafeMessage",
        "ErrorActionMaterialV1.safe_message",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorAction.retry_after",
        "ErrorAction.retry_after",
        Model,
        "semantic",
        "ErrorActionMaterialV1.retry_after",
        "normalized",
        "scalar",
        "required",
        "RetryAfterMaterialV1",
        "ErrorActionMaterialV1.retry_after",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorAction.correlations",
        "ErrorAction.correlations",
        Model,
        "semantic",
        "ErrorActionMaterialV1.correlations",
        "normalized",
        "step_then_header",
        "empty_array",
        "Vec<ErrorCorrelationMaterialV1>",
        "ErrorActionMaterialV1.correlations",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorCorrelationBinding.canonical_lowercase_header_name",
        "ErrorCorrelationBinding.canonical_lowercase_header_name",
        Model,
        "semantic",
        "ErrorCorrelationMaterialV1.canonical_lowercase_header_name",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "ErrorCorrelationMaterialV1.canonical_lowercase_header_name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorCorrelationBinding.capability",
        "ErrorCorrelationBinding.capability",
        Model,
        "semantic",
        "ErrorCorrelationMaterialV1.capability",
        "normalized",
        "scalar",
        "required",
        "CapabilityId",
        "ErrorCorrelationMaterialV1.capability",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorCorrelationBinding.step",
        "ErrorCorrelationBinding.step",
        Model,
        "semantic",
        "ErrorCorrelationMaterialV1.step",
        "normalized",
        "scalar",
        "required",
        "CompiledStepId",
        "ErrorCorrelationMaterialV1.step",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Transport",
        "ConnectorErrorClass::Transport",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=transport}.kind",
        "normalized",
        "scalar",
        "required",
        "transport",
        "ConnectorErrorClassMaterialV1::Transport",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Timeout",
        "ConnectorErrorClass::Timeout",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=timeout}.kind",
        "normalized",
        "scalar",
        "required",
        "timeout",
        "ConnectorErrorClassMaterialV1::Timeout",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Http429",
        "ConnectorErrorClass::Http429",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=http_429}.kind",
        "normalized",
        "scalar",
        "required",
        "http_429",
        "ConnectorErrorClassMaterialV1::Http429",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Http5xx",
        "ConnectorErrorClass::Http5xx",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=http_5xx}.kind",
        "normalized",
        "scalar",
        "required",
        "http_5xx",
        "ConnectorErrorClassMaterialV1::Http5xx",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Authentication",
        "ConnectorErrorClass::Authentication",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=authentication}.kind",
        "normalized",
        "scalar",
        "required",
        "authentication",
        "ConnectorErrorClassMaterialV1::Authentication",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Validation",
        "ConnectorErrorClass::Validation",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=validation}.kind",
        "normalized",
        "scalar",
        "required",
        "validation",
        "ConnectorErrorClassMaterialV1::Validation",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Permanent",
        "ConnectorErrorClass::Permanent",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=permanent}.kind",
        "normalized",
        "scalar",
        "required",
        "permanent",
        "ConnectorErrorClassMaterialV1::Permanent",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ConnectorErrorClass::Invariant",
        "ConnectorErrorClass::Invariant",
        ConnectorAbi,
        "semantic",
        "ConnectorErrorClassMaterialV1{kind=invariant}.kind",
        "normalized",
        "scalar",
        "required",
        "invariant",
        "ConnectorErrorClassMaterialV1::Invariant",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RetryAfterPolicy::Never",
        "RetryAfterPolicy::Never",
        Model,
        "semantic",
        "RetryAfterMaterialV1{kind=never}.kind",
        "normalized",
        "scalar",
        "required",
        "never",
        "RetryAfterMaterialV1::Never",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RetryAfterPolicy::RetryAfterHeader",
        "RetryAfterPolicy::RetryAfterHeader",
        Model,
        "semantic",
        "RetryAfterMaterialV1{kind=retry_after_header}.kind",
        "normalized",
        "scalar",
        "required",
        "retry_after_header",
        "RetryAfterMaterialV1::RetryAfterHeader",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RetryAfterPolicy::RetryAfterHeader.step",
        "RetryAfterPolicy::RetryAfterHeader.step",
        Model,
        "semantic",
        "RetryAfterMaterialV1{kind=retry_after_header}.value.step",
        "normalized",
        "scalar",
        "required",
        "CompiledStepId",
        "RetryAfterMaterialV1::RetryAfterHeader.step",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RetryAfterPolicy::RetryAfterHeader.capability",
        "RetryAfterPolicy::RetryAfterHeader.capability",
        Model,
        "semantic",
        "RetryAfterMaterialV1{kind=retry_after_header}.value.capability",
        "normalized",
        "scalar",
        "required",
        "CapabilityId",
        "RetryAfterMaterialV1::RetryAfterHeader.capability",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "RetryAfterPolicy::RetryAfterHeader.maximum_seconds",
        "RetryAfterPolicy::RetryAfterHeader.maximum_seconds",
        Model,
        "semantic",
        "RetryAfterMaterialV1{kind=retry_after_header}.value.maximum_seconds",
        "normalized",
        "scalar",
        "required",
        "u32",
        "RetryAfterMaterialV1::RetryAfterHeader.maximum_seconds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Status",
        "ErrorMatcher::Status",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=status}.kind",
        "normalized",
        "scalar",
        "required",
        "status",
        "ErrorMatcherMaterialV1::Status",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::ProviderCode",
        "ErrorMatcher::ProviderCode",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=provider_code}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_code",
        "ErrorMatcherMaterialV1::ProviderCode",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Header",
        "ErrorMatcher::Header",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=header}.kind",
        "normalized",
        "scalar",
        "required",
        "header",
        "ErrorMatcherMaterialV1::Header",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::MalformedDeclaredSuccess",
        "ErrorMatcher::MalformedDeclaredSuccess",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=malformed_declared_success}.kind",
        "normalized",
        "scalar",
        "required",
        "malformed_declared_success",
        "ErrorMatcherMaterialV1::MalformedDeclaredSuccess",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Status.minimum",
        "StatusRange.minimum",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=status}.value.minimum",
        "normalized",
        "scalar",
        "required",
        "u16",
        "StatusRangeMaterialV1.minimum",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Status.maximum",
        "StatusRange.maximum",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=status}.value.maximum",
        "normalized",
        "scalar",
        "required",
        "u16",
        "StatusRangeMaterialV1.maximum",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::ProviderCode.pointer",
        "ErrorMatcher::ProviderCode.pointer",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=provider_code}.value.pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "ErrorMatcherMaterialV1::ProviderCode.pointer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::ProviderCode.codes",
        "ErrorMatcher::ProviderCode.codes",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=provider_code}.value.codes",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<StaticProviderCode>",
        "ErrorMatcherMaterialV1::ProviderCode.codes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Header.name",
        "ErrorMatcher::Header.name",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=header}.value.name",
        "normalized",
        "scalar",
        "required",
        "StaticHeaderName",
        "ErrorMatcherMaterialV1::Header.name",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ErrorMatcher::Header.values",
        "ErrorMatcher::Header.values",
        Model,
        "semantic",
        "ErrorMatcherMaterialV1{kind=header}.value.values",
        "normalized",
        "lexical",
        "nonempty_array",
        "NonEmptyVec<StaticHeaderValue>",
        "ErrorMatcherMaterialV1::Header.values",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook",
        "TriggerSpec::Webhook",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.kind",
        "normalized",
        "scalar",
        "required",
        "webhook",
        "SemanticTriggerMaterialV1::Webhook",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.connector",
        "TriggerSpec::Webhook.connector",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.connector",
        "normalized",
        "scalar",
        "required",
        "ConnectorId",
        "SemanticTriggerMaterialV1::Webhook.connector",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.connector_version",
        "TriggerSpec::Webhook.connector_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.connector_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Webhook.connector_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.trigger",
        "TriggerSpec::Webhook.trigger",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.trigger",
        "normalized",
        "scalar",
        "required",
        "TriggerId",
        "SemanticTriggerMaterialV1::Webhook.trigger",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.trigger_version",
        "TriggerSpec::Webhook.trigger_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.trigger_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Webhook.trigger_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.event_version",
        "TriggerSpec::Webhook.event_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.event_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Webhook.event_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.runtime_abi_epoch",
        "TriggerSpec::Webhook.runtime_abi_epoch",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.runtime_abi_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticTriggerMaterialV1::Webhook.runtime_abi_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.authenticator",
        "TriggerSpec::Webhook.authenticator",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.authenticator",
        "normalized",
        "scalar",
        "required",
        "VersionedProcessorMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.authenticator",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.codec",
        "TriggerSpec::Webhook.codec",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.codec",
        "normalized",
        "scalar",
        "required",
        "VersionedProcessorMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.codec",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.normalizer",
        "TriggerSpec::Webhook.normalizer",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.normalizer",
        "normalized",
        "scalar",
        "required",
        "VersionedProcessorMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.normalizer",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.selected_headers",
        "TriggerSpec::Webhook.selected_headers",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.selected_headers",
        "normalized",
        "lexical",
        "empty_array",
        "Vec<StaticHeaderName>",
        "SemanticTriggerMaterialV1::Webhook.selected_headers",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.raw_body_max_bytes",
        "TriggerSpec::Webhook.raw_body_max_bytes",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.raw_body_max_bytes",
        "normalized",
        "scalar",
        "required",
        "u32",
        "SemanticTriggerMaterialV1::Webhook.raw_body_max_bytes",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.timestamp_window_ms",
        "TriggerSpec::Webhook.timestamp_window_ms",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.timestamp_window_ms",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "SemanticTriggerMaterialV1::Webhook.timestamp_window_ms",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.event_id",
        "TriggerSpec::Webhook.event_id",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.event_id",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.event_id",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.event_type",
        "TriggerSpec::Webhook.event_type",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.event_type",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.event_type",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.output",
        "TriggerSpec::Webhook.output",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.output",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.output",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.redaction",
        "TriggerSpec::Webhook.redaction",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.redaction",
        "normalized",
        "scalar",
        "required",
        "RedactionMaterialV1",
        "SemanticTriggerMaterialV1::Webhook.redaction",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Webhook.subscription_operations",
        "TriggerSpec::Webhook.subscription_operations",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=webhook}.value.subscription_operations",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<SubscriptionOperationIdsMaterialV1>",
        "SemanticTriggerMaterialV1::Webhook.subscription_operations",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll",
        "TriggerSpec::Poll",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.kind",
        "normalized",
        "scalar",
        "required",
        "poll",
        "SemanticTriggerMaterialV1::Poll",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.connector",
        "TriggerSpec::Poll.connector",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.connector",
        "normalized",
        "scalar",
        "required",
        "ConnectorId",
        "SemanticTriggerMaterialV1::Poll.connector",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.connector_version",
        "TriggerSpec::Poll.connector_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.connector_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Poll.connector_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.trigger",
        "TriggerSpec::Poll.trigger",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.trigger",
        "normalized",
        "scalar",
        "required",
        "TriggerId",
        "SemanticTriggerMaterialV1::Poll.trigger",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.trigger_version",
        "TriggerSpec::Poll.trigger_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.trigger_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Poll.trigger_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.event_version",
        "TriggerSpec::Poll.event_version",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.event_version",
        "normalized",
        "scalar",
        "required",
        "StableSemver",
        "SemanticTriggerMaterialV1::Poll.event_version",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.runtime_abi_epoch",
        "TriggerSpec::Poll.runtime_abi_epoch",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.runtime_abi_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "SemanticTriggerMaterialV1::Poll.runtime_abi_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.checkpoint",
        "TriggerSpec::Poll.checkpoint",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.checkpoint",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticTriggerMaterialV1::Poll.checkpoint",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.processor",
        "TriggerSpec::Poll.processor",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.processor",
        "normalized",
        "scalar",
        "required",
        "VersionedProcessorMaterialV1",
        "SemanticTriggerMaterialV1::Poll.processor",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.event_type",
        "TriggerSpec::Poll.event_type",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.event_type",
        "normalized",
        "scalar",
        "required",
        "ValueContractMaterialV1",
        "SemanticTriggerMaterialV1::Poll.event_type",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.per_poll_event_limit",
        "TriggerSpec::Poll.per_poll_event_limit",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.per_poll_event_limit",
        "normalized",
        "scalar",
        "required",
        "u32",
        "SemanticTriggerMaterialV1::Poll.per_poll_event_limit",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "TriggerSpec::Poll.bounds",
        "TriggerSpec::Poll.bounds",
        Model,
        "semantic",
        "SemanticTriggerMaterialV1{kind=poll}.value.bounds",
        "normalized",
        "scalar",
        "required",
        "OperationBoundsMaterialV1",
        "SemanticTriggerMaterialV1::Poll.bounds",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SubscriptionOperationIds.create",
        "SubscriptionOperationIds.create",
        Model,
        "semantic",
        "SubscriptionOperationIdsMaterialV1.create",
        "normalized",
        "scalar",
        "required",
        "OperationId",
        "SubscriptionOperationIdsMaterialV1.create",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SubscriptionOperationIds.delete",
        "SubscriptionOperationIds.delete",
        Model,
        "semantic",
        "SubscriptionOperationIdsMaterialV1.delete",
        "normalized",
        "scalar",
        "required",
        "OperationId",
        "SubscriptionOperationIdsMaterialV1.delete",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "SubscriptionOperationIds.check",
        "SubscriptionOperationIds.check",
        Model,
        "semantic",
        "SubscriptionOperationIdsMaterialV1.check",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<OperationId>",
        "SubscriptionOperationIdsMaterialV1.check",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "ValueContractCatalog.value_language_epoch",
        "value_contract_material::value_language_epoch",
        BuilderDerived,
        "value-contract",
        "ValueContractMaterialV1.value_language_epoch",
        "normalized",
        "scalar",
        "required",
        "Epoch",
        "ValueContractMaterialV1.value_language_epoch",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueContractCatalog.roots",
        "ValueContractCatalog.roots",
        ValueContract,
        "value-contract",
        "ValueContractMaterialV1.roots",
        "normalized",
        "utf16_member_name",
        "empty_object",
        "Map<string,FieldMaterialV1>",
        "ValueContractMaterialV1.roots",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueContractCatalog.named_objects",
        "ValueContractCatalog.named_objects",
        ValueContract,
        "value-contract",
        "ValueContractMaterialV1.named_objects",
        "normalized",
        "utf16_member_name",
        "empty_object",
        "Map<string,NamedObjectMaterialV1>",
        "ValueContractMaterialV1.named_objects",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "NamedObject.fields",
        "ValueObjectContract.fields",
        ValueContract,
        "value-contract",
        "NamedObjectMaterialV1.fields",
        "normalized",
        "utf16_member_name",
        "empty_object",
        "Map<string,FieldMaterialV1>",
        "NamedObjectMaterialV1.fields",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "Field.required",
        "ValueContractField.required",
        ValueContract,
        "value-contract",
        "FieldMaterialV1.required",
        "normalized",
        "scalar",
        "required",
        "bool",
        "FieldMaterialV1.required",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "Field.type_ref",
        "ValueContractField.type_ref",
        ValueContract,
        "value-contract",
        "FieldMaterialV1.type_ref",
        "normalized",
        "scalar",
        "required",
        "TypeRefMaterialV1",
        "FieldMaterialV1.type_ref",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "TypeRef.nullable",
        "TypeRef.nullable",
        ValueContract,
        "value-contract",
        "TypeRefMaterialV1.nullable",
        "normalized",
        "scalar",
        "required",
        "bool",
        "TypeRefMaterialV1.nullable",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "TypeRef.value_type",
        "TypeRef.value_type",
        ValueContract,
        "value-contract",
        "TypeRefMaterialV1.value_type",
        "normalized",
        "scalar",
        "required",
        "ValueTypeMaterialV1",
        "TypeRefMaterialV1.value_type",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Scalar",
        "ValueType::Scalar",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=scalar}.kind",
        "normalized",
        "scalar",
        "required",
        "scalar",
        "ValueTypeMaterial::Scalar",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Enum",
        "ValueType::Enum",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=enum}.kind",
        "normalized",
        "scalar",
        "required",
        "enum",
        "ValueTypeMaterial::Enum",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Object",
        "ValueType::Object",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=object}.kind",
        "normalized",
        "scalar",
        "required",
        "object",
        "ValueTypeMaterial::Object",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::List",
        "ValueType::List",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=list}.kind",
        "normalized",
        "scalar",
        "required",
        "list",
        "ValueTypeMaterial::List",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Ref",
        "ValueType::Ref",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=ref}.kind",
        "normalized",
        "scalar",
        "required",
        "ref",
        "ValueTypeMaterial::Ref",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Scalar.scalar",
        "ValueType::Scalar.scalar",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=scalar}.value",
        "normalized",
        "scalar",
        "required",
        "ValueScalarMaterialV1",
        "ValueTypeMaterial::Scalar.value",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Enum.name",
        "ValueType::Enum.name",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=enum}.value.name",
        "normalized",
        "scalar",
        "required",
        "string",
        "ValueTypeMaterial::Enum.name",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Enum.values",
        "ValueType::Enum.values",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=enum}.value.values",
        "normalized",
        "declared",
        "empty_array",
        "Vec<string>",
        "ValueTypeMaterial::Enum.values",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Object.fields",
        "ValueType::Object.fields",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=object}.value.fields",
        "normalized",
        "utf16_member_name",
        "empty_object",
        "Map<string,FieldMaterialV1>",
        "ValueTypeMaterial::Object.fields",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::List.element",
        "ValueType::List.element",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=list}.value.element",
        "normalized",
        "scalar",
        "required",
        "TypeRefMaterialV1",
        "ValueTypeMaterial::List.element",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueType::Ref.name",
        "ValueType::Ref.name",
        ValueContract,
        "value-contract",
        "ValueTypeMaterialV1{kind=ref}.value.name",
        "normalized",
        "scalar",
        "required",
        "string",
        "ValueTypeMaterial::Ref.name",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Boolean",
        "ValueScalar::Boolean",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=boolean}.kind",
        "normalized",
        "scalar",
        "required",
        "boolean",
        "ValueScalarMaterial::Boolean",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::String",
        "ValueScalar::String",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=string}.kind",
        "normalized",
        "scalar",
        "required",
        "string",
        "ValueScalarMaterial::String",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Int32",
        "ValueScalar::Int32",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=int32}.kind",
        "normalized",
        "scalar",
        "required",
        "int32",
        "ValueScalarMaterial::Int32",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Int64",
        "ValueScalar::Int64",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=int64}.kind",
        "normalized",
        "scalar",
        "required",
        "int64",
        "ValueScalarMaterial::Int64",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::UInt64",
        "ValueScalar::UInt64",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=uint64}.kind",
        "normalized",
        "scalar",
        "required",
        "uint64",
        "ValueScalarMaterial::UInt64",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Decimal",
        "ValueScalar::Decimal",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=decimal}.kind",
        "normalized",
        "scalar",
        "required",
        "decimal",
        "ValueScalarMaterial::Decimal",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Uuid",
        "ValueScalar::Uuid",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=uuid}.kind",
        "normalized",
        "scalar",
        "required",
        "uuid",
        "ValueScalarMaterial::Uuid",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Date",
        "ValueScalar::Date",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=date}.kind",
        "normalized",
        "scalar",
        "required",
        "date",
        "ValueScalarMaterial::Date",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Timestamp",
        "ValueScalar::Timestamp",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=timestamp}.kind",
        "normalized",
        "scalar",
        "required",
        "timestamp",
        "ValueScalarMaterial::Timestamp",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::TimestampTz",
        "ValueScalar::TimestampTz",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=timestamptz}.kind",
        "normalized",
        "scalar",
        "required",
        "timestamptz",
        "ValueScalarMaterial::TimestampTz",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Json",
        "ValueScalar::Json",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=json}.kind",
        "normalized",
        "scalar",
        "required",
        "json",
        "ValueScalarMaterial::Json",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Custom",
        "ValueScalar::Custom",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=custom}.kind",
        "normalized",
        "scalar",
        "required",
        "custom",
        "ValueScalarMaterial::Custom",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "ValueScalar::Custom.name",
        "ValueScalar::Custom.name",
        ValueContract,
        "value-contract",
        "ValueScalarMaterialV1{kind=custom}.value.name",
        "normalized",
        "scalar",
        "required",
        "string",
        "ValueScalarMaterial::Custom.name",
        ProjectionSchema,
        ValueContract,
        Mutable,
    );
    (
        "TypedValue::Null",
        "TypedValue::Null",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=null}.kind",
        "normalized",
        "scalar",
        "required",
        "null",
        "TypedValueMaterial::Null",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Boolean",
        "TypedValue::Boolean",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=boolean}.kind",
        "normalized",
        "scalar",
        "required",
        "boolean",
        "TypedValueMaterial::Boolean",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::String",
        "TypedValue::String",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=string}.kind",
        "normalized",
        "scalar",
        "required",
        "string",
        "TypedValueMaterial::String",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::I64",
        "CanonicalNumber::I64",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=i64}.kind",
        "normalized",
        "scalar",
        "required",
        "i64",
        "TypedValueMaterial::I64",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::U64",
        "CanonicalNumber::U64",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=u64}.kind",
        "normalized",
        "scalar",
        "required",
        "u64",
        "TypedValueMaterial::U64",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Decimal",
        "CanonicalNumber::Decimal",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=decimal}.kind",
        "normalized",
        "scalar",
        "required",
        "decimal",
        "TypedValueMaterial::Decimal",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::List",
        "TypedValue::List",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=list}.kind",
        "normalized",
        "scalar",
        "required",
        "list",
        "TypedValueMaterial::List",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Object",
        "TypedValue::Object",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=object}.kind",
        "normalized",
        "scalar",
        "required",
        "object",
        "TypedValueMaterial::Object",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::InlineBytes",
        "TypedValue::InlineBytes",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=inline_bytes}.kind",
        "normalized",
        "scalar",
        "required",
        "inline_bytes",
        "TypedValueMaterial::InlineBytes",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Boolean.value",
        "TypedValue::Boolean.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=boolean}.value",
        "normalized",
        "scalar",
        "required",
        "bool",
        "TypedValueMaterial::Boolean.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::String.value",
        "TypedValue::String.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=string}.value",
        "normalized",
        "scalar",
        "required",
        "string",
        "TypedValueMaterial::String.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::I64.value",
        "CanonicalNumber::I64.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=i64}.value",
        "normalized",
        "scalar",
        "required",
        "i64-string",
        "TypedValueMaterial::I64.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::U64.value",
        "CanonicalNumber::U64.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=u64}.value",
        "normalized",
        "scalar",
        "required",
        "u64-string",
        "TypedValueMaterial::U64.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Decimal.value",
        "CanonicalNumber::Decimal.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=decimal}.value",
        "normalized",
        "scalar",
        "required",
        "decimal-string",
        "TypedValueMaterial::Decimal.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::List.value",
        "TypedValue::List.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=list}.value",
        "normalized",
        "declared",
        "empty_array",
        "Vec<TypedValueMaterialV1>",
        "TypedValueMaterial::List.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::Object.value",
        "TypedValue::Object.value",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=object}.value",
        "normalized",
        "utf16_member_name",
        "empty_object",
        "Map<string,TypedValueMaterialV1>",
        "TypedValueMaterial::Object.value",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::InlineBytes.bytes",
        "BoundedInlineBytes.bytes",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=inline_bytes}.value.$binary",
        "normalized",
        "scalar",
        "required",
        "base64url",
        "TypedValueMaterial::InlineBytes.binary",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::InlineBytes.media_type",
        "BoundedInlineBytes.media_type",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=inline_bytes}.value.media_type",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<string>",
        "TypedValueMaterial::InlineBytes.media_type",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "TypedValue::InlineBytes.file_name",
        "BoundedInlineBytes.file_name",
        ValueContract,
        "value-contract",
        "TypedValueMaterialV1{kind=inline_bytes}.value.file_name",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<string>",
        "TypedValueMaterial::InlineBytes.file_name",
        Source,
        TypedValue,
        Mutable,
    );

    (
        "ManifestProvenanceReference.source_record_id",
        "ManifestProvenanceReference.source_record_id",
        Model,
        "provenance",
        "ManifestProvenanceMaterialV1.source_record_id",
        "normalized",
        "scalar",
        "required",
        "SourceRecordId",
        "ManifestProvenanceMaterialV1.source_record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes",
        "ManifestProvenanceReference.artifact_hashes",
        Model,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes",
        "normalized",
        "artifact_id",
        "nonempty_array",
        "NonEmptyVec<ArtifactHashMaterialV1>",
        "ManifestProvenanceMaterialV1.artifact_hashes",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.license_id",
        "ManifestProvenanceReference.license_id",
        Model,
        "provenance",
        "ManifestProvenanceMaterialV1.license_id",
        "normalized",
        "scalar",
        "required",
        "LicenseIdentity",
        "ManifestProvenanceMaterialV1.license_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.notice_id",
        "ManifestProvenanceReference.notice_id",
        Model,
        "provenance",
        "ManifestProvenanceMaterialV1.notice_id",
        "normalized",
        "scalar",
        "required",
        "NoticeId",
        "ManifestProvenanceMaterialV1.notice_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.contract_facts",
        "ManifestProvenanceReference.contract_facts",
        Model,
        "provenance",
        "ManifestProvenanceMaterialV1.contract_fact_origins",
        "normalized",
        "use_site",
        "empty_array",
        "Vec<ResolvedFactOriginMaterialV1>",
        "ManifestProvenanceMaterialV1.contract_fact_origins",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ResolvedContractFactBinding.use_site",
        "ResolvedContractFactBinding.use_site",
        Model,
        "provenance",
        "ResolvedFactOriginMaterialV1.use_site",
        "normalized",
        "scalar",
        "required",
        "Id",
        "ResolvedFactOriginMaterialV1.use_site",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ResolvedContractFactBinding.fact",
        "ResolvedContractFactBinding.fact",
        Model,
        "provenance",
        "ResolvedFactOriginMaterialV1.origin",
        "normalized",
        "scalar",
        "required",
        "ResolvedFactOrigin",
        "ResolvedFactOriginMaterialV1.origin",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence.source_record_id",
        "ContractFact::ProviderEvidence.source_record_id",
        Source,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.source_record_id",
        "normalized",
        "scalar",
        "required",
        "SourceRecordId",
        "ResolvedFactOriginV1::ProviderEvidence.source_record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence.fact_id",
        "ContractFact::ProviderEvidence.fact_id",
        Source,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.fact_id",
        "normalized",
        "scalar",
        "required",
        "ProviderFactId",
        "ResolvedFactOriginV1::ProviderEvidence.fact_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ContractFact::DonatPolicy.policy_id",
        "ContractFact::DonatPolicy.policy_id",
        Source,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=donat_policy}.value.policy_id",
        "normalized",
        "scalar",
        "required",
        "DonatPolicyId",
        "ResolvedFactOriginV1::DonatPolicy.policy_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ContractFact::ProviderEvidence",
        "ContractFact::ProviderEvidence",
        Source,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=provider_evidence}.kind",
        "normalized",
        "scalar",
        "required",
        "provider_evidence",
        "ResolvedFactOriginV1::ProviderEvidence",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ContractFact::DonatPolicy",
        "ContractFact::DonatPolicy",
        Source,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=donat_policy}.kind",
        "normalized",
        "scalar",
        "required",
        "donat_policy",
        "ResolvedFactOriginV1::DonatPolicy",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].artifact_id",
        "ArtifactHash.artifact_id",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].artifact_id",
        "normalized",
        "scalar",
        "required",
        "ArtifactId",
        "ArtifactHashMaterialV1.artifact_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].algorithm",
        "ArtifactHash.algorithm",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm",
        "normalized",
        "scalar",
        "required",
        "HashAlgorithm",
        "ArtifactHashMaterialV1.algorithm",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].digest",
        "ArtifactHash.digest",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].digest",
        "normalized",
        "scalar",
        "required",
        "Hash256_or_Hash512",
        "ArtifactHashMaterialV1.digest",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].path",
        "ArtifactHash.path",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].path",
        "normalized",
        "scalar",
        "explicit_null",
        "Option<SourcePath>",
        "ArtifactHashMaterialV1.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].algorithm::Sha256",
        "HashAlgorithm::Sha256",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha256}.kind",
        "normalized",
        "scalar",
        "required",
        "sha256",
        "HashAlgorithmMaterialV1::Sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ManifestProvenanceReference.artifact_hashes[].algorithm::Sha512",
        "HashAlgorithm::Sha512",
        Source,
        "provenance",
        "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha512}.kind",
        "normalized",
        "scalar",
        "required",
        "sha512",
        "HashAlgorithmMaterialV1::Sha512",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactProviderArtifact.provider",
        "ExactProviderArtifact.provider",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].provider",
        "normalized",
        "scalar",
        "required",
        "string",
        "ProviderEvidenceOriginMaterialV1.provider",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactProviderArtifact.evidence",
        "ExactProviderArtifact.evidence",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence",
        "normalized",
        "canonical_source_identity",
        "nonempty_array",
        "NonEmptyVec<ProviderEvidenceOriginEntryMaterialV1>",
        "ProviderEvidenceOriginMaterialV1.evidence",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.accessed_on",
        "ProviderEvidenceArtifact.accessed_on",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].accessed_on",
        "normalized",
        "scalar",
        "required",
        "Date",
        "ProviderEvidenceOriginEntryMaterialV1.accessed_on",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.content_sha256",
        "ProviderEvidenceArtifact.content_sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].content_sha256",
        "normalized",
        "scalar",
        "required",
        "Hash256",
        "ProviderEvidenceOriginEntryMaterialV1.content_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.source",
        "ProviderEvidenceArtifact.source",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source",
        "normalized",
        "scalar",
        "required",
        "ImmutableProviderEvidenceSource",
        "ProviderEvidenceOriginEntryMaterialV1.source",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.terms",
        "ProviderEvidenceArtifact.terms",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms",
        "normalized",
        "scalar",
        "required",
        "EvidenceTermsMaterialV1",
        "ProviderEvidenceOriginEntryMaterialV1.terms",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderEvidenceArtifact.facts",
        "ProviderEvidenceArtifact.facts",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts",
        "normalized",
        "fact_id",
        "nonempty_array",
        "ProviderEvidenceOriginFactMaterialV1",
        "ProviderEvidenceOriginEntryMaterialV1.facts",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile",
        "ImmutableProviderEvidenceSource::RepositoryFile",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.kind",
        "normalized",
        "scalar",
        "required",
        "repository_file",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact",
        "ImmutableProviderEvidenceSource::VersionedArtifact",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.kind",
        "normalized",
        "scalar",
        "required",
        "versioned_artifact",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.repository",
        "ImmutableProviderEvidenceSource::RepositoryFile.repository",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.repository",
        "normalized",
        "scalar",
        "required",
        "RepositoryUrl",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.repository",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.commit",
        "ImmutableProviderEvidenceSource::RepositoryFile.commit",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.commit",
        "normalized",
        "scalar",
        "required",
        "GitCommit",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.commit",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::RepositoryFile.path",
        "ImmutableProviderEvidenceSource::RepositoryFile.path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ProviderEvidenceSourceMaterialV1::RepositoryFile.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact.url",
        "ImmutableProviderEvidenceSource::VersionedArtifact.url",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact.url",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
        "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.provider_revision",
        "normalized",
        "scalar",
        "required",
        "NonEmptyString",
        "ProviderEvidenceSourceMaterialV1::VersionedArtifact.provider_revision",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderFact.fact_id",
        "ProviderFact.fact_id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].fact_id",
        "normalized",
        "scalar",
        "required",
        "ProviderFactId",
        "ProviderEvidenceOriginFactMaterialV1.fact_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ProviderFact.location",
        "ProviderFact.location",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location",
        "normalized",
        "scalar",
        "required",
        "ExactFactLocationMaterialV1",
        "ProviderEvidenceOriginFactMaterialV1.location",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer",
        "ExactFactLocation::JsonPointer",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.kind",
        "normalized",
        "scalar",
        "required",
        "json_pointer",
        "ExactFactLocationMaterialV1::JsonPointer",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection",
        "ExactFactLocation::DocumentSection",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.kind",
        "normalized",
        "scalar",
        "required",
        "document_section",
        "ExactFactLocationMaterialV1::DocumentSection",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer.path",
        "ExactFactLocation::JsonPointer.path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ExactFactLocationMaterialV1::JsonPointer.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::JsonPointer.pointer",
        "ExactFactLocation::JsonPointer.pointer",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.pointer",
        "normalized",
        "scalar",
        "required",
        "StaticJsonPointer",
        "ExactFactLocationMaterialV1::JsonPointer.pointer",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection.path",
        "ExactFactLocation::DocumentSection.path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.path",
        "normalized",
        "scalar",
        "required",
        "SourcePath",
        "ExactFactLocationMaterialV1::DocumentSection.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "ExactFactLocation::DocumentSection.section",
        "ExactFactLocation::DocumentSection.section",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.section",
        "normalized",
        "scalar",
        "required",
        "string",
        "ExactFactLocationMaterialV1::DocumentSection.section",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive",
        "EvidenceTermsDisposition::Permissive",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.kind",
        "normalized",
        "scalar",
        "required",
        "permissive",
        "EvidenceTermsMaterialV1::Permissive",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse",
        "EvidenceTermsDisposition::ReviewedUse",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.kind",
        "normalized",
        "scalar",
        "required",
        "reviewed_use",
        "EvidenceTermsMaterialV1::ReviewedUse",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Rejected",
        "EvidenceTermsDisposition::Rejected",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.kind",
        "normalized",
        "scalar",
        "required",
        "rejected",
        "EvidenceTermsMaterialV1::Rejected",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive.license",
        "EvidenceTermsDisposition::Permissive.license",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.license",
        "normalized",
        "scalar",
        "required",
        "LicenseDecisionMaterialV1",
        "EvidenceTermsMaterialV1::Permissive.license",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Permissive.evidence_url",
        "EvidenceTermsDisposition::Permissive.evidence_url",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.evidence_url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "EvidenceTermsMaterialV1::Permissive.evidence_url",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse.decision_id",
        "EvidenceTermsDisposition::ReviewedUse.decision_id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.decision_id",
        "normalized",
        "scalar",
        "required",
        "ReviewDecisionId",
        "EvidenceTermsMaterialV1::ReviewedUse.decision_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::ReviewedUse.evidence_url",
        "EvidenceTermsDisposition::ReviewedUse.evidence_url",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.evidence_url",
        "normalized",
        "scalar",
        "required",
        "ExactHttpsUrl",
        "EvidenceTermsMaterialV1::ReviewedUse.evidence_url",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EvidenceTermsDisposition::Rejected.finding",
        "EvidenceTermsDisposition::Rejected.finding",
        Source,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.value.finding",
        "normalized",
        "scalar",
        "required",
        "FindingId",
        "EvidenceTermsMaterialV1::Rejected.finding",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DonatOwnedSource.files[].path",
        "RepoFileHash.path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.files[].path",
        "normalized",
        "source_record_id_then_path",
        "required",
        "RepoPath",
        "FileDecisionMaterialV1.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DonatOwnedSource.files[].sha256",
        "RepoFileHash.sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.files[].sha256",
        "normalized",
        "source_record_id_then_path",
        "required",
        "Hash256",
        "FileDecisionMaterialV1.sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Permissive",
        "LicenseDecision::Permissive",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=permissive}.kind",
        "normalized",
        "canonical_bytes",
        "required",
        "permissive",
        "LicenseDecisionMaterialV1::Permissive",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant",
        "LicenseDecision::WrittenGrant",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=written_grant}.kind",
        "normalized",
        "canonical_bytes",
        "required",
        "written_grant",
        "LicenseDecisionMaterialV1::WrittenGrant",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Rejected",
        "LicenseDecision::Rejected",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=rejected}.kind",
        "normalized",
        "canonical_bytes",
        "required",
        "rejected",
        "LicenseDecisionMaterialV1::Rejected",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.spdx_id",
        "LicenseDecision::Permissive.spdx_id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.spdx_id",
        "normalized",
        "canonical_bytes",
        "required",
        "string",
        "LicenseDecisionMaterialV1::Permissive.spdx_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.selected_dual_license_branch",
        "LicenseDecision::Permissive.selected_dual_license_branch",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.selected_dual_license_branch",
        "normalized",
        "canonical_bytes",
        "explicit_null",
        "Option<string>",
        "LicenseDecisionMaterialV1::Permissive.selected_dual_license_branch",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.license_file_path",
        "LicenseDecision::Permissive.license_file_path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_path",
        "normalized",
        "canonical_bytes",
        "required",
        "SourcePath",
        "LicenseDecisionMaterialV1::Permissive.license_file_path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Permissive.license_file_sha256",
        "LicenseDecision::Permissive.license_file_sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_sha256",
        "normalized",
        "canonical_bytes",
        "required",
        "Hash256",
        "LicenseDecisionMaterialV1::Permissive.license_file_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant.decision_id",
        "LicenseDecision::WrittenGrant.decision_id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.decision_id",
        "normalized",
        "canonical_bytes",
        "required",
        "ReviewDecisionId",
        "LicenseDecisionMaterialV1::WrittenGrant.decision_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::WrittenGrant.grant_sha256",
        "LicenseDecision::WrittenGrant.grant_sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.grant_sha256",
        "normalized",
        "canonical_bytes",
        "required",
        "Hash256",
        "LicenseDecisionMaterialV1::WrittenGrant.grant_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "LicenseDecision::Rejected.finding",
        "LicenseDecision::Rejected.finding",
        Source,
        "provenance",
        "ProvenanceMaterialV1.licenses[]{kind=rejected}.value.finding",
        "normalized",
        "canonical_bytes",
        "required",
        "FindingId",
        "LicenseDecisionMaterialV1::Rejected.finding",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "NoticeIdentity.id",
        "NoticeIdentity.id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.notices[].id",
        "normalized",
        "id",
        "required",
        "NoticeId",
        "NoticeMaterialV1.id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "NoticeIdentity.license_file_path",
        "NoticeIdentity.license_file_path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.notices[].license_file_path",
        "normalized",
        "id",
        "required",
        "SourcePath",
        "NoticeMaterialV1.license_file_path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "NoticeIdentity.license_file_sha256",
        "NoticeIdentity.license_file_sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.notices[].license_file_sha256",
        "normalized",
        "id",
        "required",
        "Hash256",
        "NoticeMaterialV1.license_file_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "NoticeIdentity.required_copyright_lines",
        "NoticeIdentity.required_copyright_lines",
        Source,
        "provenance",
        "ProvenanceMaterialV1.notices[].required_copyright_lines",
        "normalized",
        "declared",
        "empty_array",
        "Vec<string>",
        "NoticeMaterialV1.required_copyright_lines",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "NoticeIdentity.notice_bundle_destination",
        "NoticeIdentity.notice_bundle_destination",
        Source,
        "provenance",
        "ProvenanceMaterialV1.notices[].notice_bundle_destination",
        "normalized",
        "id",
        "required",
        "RepoPath",
        "NoticeMaterialV1.notice_bundle_destination",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDecision.dependency",
        "DependencyDecision.dependency",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].dependency",
        "normalized",
        "dependency",
        "required",
        "Id",
        "DependencyDecisionMaterialV1.dependency",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDecision.disposition",
        "DependencyDecision.disposition",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition",
        "normalized",
        "dependency",
        "required",
        "DependencyDispositionMaterialV1",
        "DependencyDecisionMaterialV1.disposition",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::Shipped",
        "DependencyDisposition::Shipped",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.kind",
        "normalized",
        "dependency",
        "required",
        "shipped",
        "DependencyDispositionMaterialV1::Shipped",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::BuildOnly",
        "DependencyDisposition::BuildOnly",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.kind",
        "normalized",
        "dependency",
        "required",
        "build_only",
        "DependencyDispositionMaterialV1::BuildOnly",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::TypeOnlyReplaced",
        "DependencyDisposition::TypeOnlyReplaced",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.kind",
        "normalized",
        "dependency",
        "required",
        "type_only_replaced",
        "DependencyDispositionMaterialV1::TypeOnlyReplaced",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::BehaviorOnly",
        "DependencyDisposition::BehaviorOnly",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.kind",
        "normalized",
        "dependency",
        "required",
        "behavior_only",
        "DependencyDispositionMaterialV1::BehaviorOnly",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::Rejected",
        "DependencyDisposition::Rejected",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.kind",
        "normalized",
        "dependency",
        "required",
        "rejected",
        "DependencyDispositionMaterialV1::Rejected",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::Shipped.license",
        "DependencyDisposition::Shipped.license",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.value.license",
        "normalized",
        "dependency",
        "required",
        "LicenseDecisionMaterialV1",
        "DependencyDispositionMaterialV1::Shipped.license",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::BuildOnly.license",
        "DependencyDisposition::BuildOnly.license",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.value.license",
        "normalized",
        "dependency",
        "required",
        "LicenseDecisionMaterialV1",
        "DependencyDispositionMaterialV1::BuildOnly.license",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::TypeOnlyReplaced.replacement",
        "DependencyDisposition::TypeOnlyReplaced.replacement",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.value.replacement",
        "normalized",
        "dependency",
        "required",
        "Id",
        "DependencyDispositionMaterialV1::TypeOnlyReplaced.replacement",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::BehaviorOnly.reason",
        "DependencyDisposition::BehaviorOnly.reason",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.value.reason",
        "normalized",
        "dependency",
        "required",
        "FindingId",
        "DependencyDispositionMaterialV1::BehaviorOnly.reason",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "DependencyDisposition::Rejected.finding",
        "DependencyDisposition::Rejected.finding",
        Source,
        "provenance",
        "ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.value.finding",
        "normalized",
        "dependency",
        "required",
        "FindingId",
        "DependencyDispositionMaterialV1::Rejected.finding",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.material_id",
        "EmbeddedMaterialDecision.material_id",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].material_id",
        "normalized",
        "material_id",
        "required",
        "Id",
        "EmbeddedDecisionMaterialV1.material_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.path",
        "EmbeddedMaterialDecision.path",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].path",
        "normalized",
        "material_id",
        "required",
        "SourcePath",
        "EmbeddedDecisionMaterialV1.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.sha256",
        "EmbeddedMaterialDecision.sha256",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].sha256",
        "normalized",
        "material_id",
        "required",
        "Hash256",
        "EmbeddedDecisionMaterialV1.sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDecision.disposition",
        "EmbeddedMaterialDecision.disposition",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition",
        "normalized",
        "material_id",
        "required",
        "EmbeddedMaterialDispositionMaterialV1",
        "EmbeddedDecisionMaterialV1.disposition",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Shipped",
        "EmbeddedMaterialDisposition::Shipped",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.kind",
        "normalized",
        "material_id",
        "required",
        "shipped",
        "EmbeddedMaterialDispositionMaterialV1::Shipped",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::BehaviorOnly",
        "EmbeddedMaterialDisposition::BehaviorOnly",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.kind",
        "normalized",
        "material_id",
        "required",
        "behavior_only",
        "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Rejected",
        "EmbeddedMaterialDisposition::Rejected",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.kind",
        "normalized",
        "material_id",
        "required",
        "rejected",
        "EmbeddedMaterialDispositionMaterialV1::Rejected",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Shipped.license",
        "EmbeddedMaterialDisposition::Shipped.license",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.value.license",
        "normalized",
        "material_id",
        "required",
        "LicenseDecisionMaterialV1",
        "EmbeddedMaterialDispositionMaterialV1::Shipped.license",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::BehaviorOnly.reason",
        "EmbeddedMaterialDisposition::BehaviorOnly.reason",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.value.reason",
        "normalized",
        "material_id",
        "required",
        "FindingId",
        "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly.reason",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "EmbeddedMaterialDisposition::Rejected.finding",
        "EmbeddedMaterialDisposition::Rejected.finding",
        Source,
        "provenance",
        "ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.value.finding",
        "normalized",
        "material_id",
        "required",
        "FindingId",
        "EmbeddedMaterialDispositionMaterialV1::Rejected.finding",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::canonical_schema_epoch",
        "derived::canonical_schema_epoch",
        Constant,
        "semantic",
        "SemanticMaterialV1.canonical_schema_epoch",
        "constant",
        "scalar",
        "required",
        "CANONICAL_SCHEMA_EPOCH",
        "SemanticMaterialV1.canonical_schema_epoch",
        ProjectionSchema,
        Semantic,
        Mutable,
    );

    (
        "derived::canonical_schema_epoch",
        "derived::canonical_schema_epoch",
        Constant,
        "provenance",
        "ProvenanceMaterialV1.canonical_schema_epoch",
        "constant",
        "scalar",
        "required",
        "CANONICAL_SCHEMA_EPOCH",
        "ProvenanceMaterialV1.canonical_schema_epoch",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::classifier_epoch",
        "derived::classifier_epoch",
        Constant,
        "provenance",
        "ProvenanceMaterialV1.classifier_epoch",
        "constant",
        "scalar",
        "required",
        "CLASSIFIER_EPOCH",
        "ProvenanceMaterialV1.classifier_epoch",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::generator_epoch",
        "derived::generator_epoch",
        Constant,
        "provenance",
        "ProvenanceMaterialV1.generator_epoch",
        "constant",
        "scalar",
        "required",
        "GENERATOR_EPOCH",
        "ProvenanceMaterialV1.generator_epoch",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::semantic_sha256",
        "derived::semantic_sha256",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.connector.semantic_sha256",
        "derived:semantic_domain_hash",
        "scalar",
        "required",
        "Hash256",
        "ProvenanceConnectorIdentity.semantic_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::source_identity.record_id",
        "derived::source_identity.record_id",
        NamedDerived,
        "provenance",
        "SourceIdentityMaterialV1.record_id",
        "derived:accepted_record_join",
        "record_id",
        "required",
        "SourceRecordId",
        "SourceIdentityMaterialV1.record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::source_identity.record_sha256",
        "derived::source_identity.record_sha256",
        NamedDerived,
        "provenance",
        "SourceIdentityMaterialV1.record_sha256",
        "derived:source_record_domain_hash",
        "record_id",
        "required",
        "Hash256",
        "SourceIdentityMaterialV1.record_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact.source_record_id",
        "derived::artifact.source_record_id",
        NamedDerived,
        "provenance",
        "ArtifactDecisionMaterialV1.source_record_id",
        "derived:accepted_record_join",
        "source_record_id_then_artifact_id",
        "required",
        "SourceRecordId",
        "ArtifactDecisionMaterialV1.source_record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact.artifact_id",
        "derived::artifact.artifact_id",
        NamedDerived,
        "provenance",
        "ArtifactDecisionMaterialV1.artifact_id",
        "derived:accepted_record_artifact_inventory",
        "source_record_id_then_artifact_id",
        "required",
        "ArtifactId",
        "ArtifactDecisionMaterialV1.artifact_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact.algorithm",
        "derived::artifact.algorithm",
        NamedDerived,
        "provenance",
        "ArtifactDecisionMaterialV1.algorithm",
        "derived:accepted_record_artifact_inventory",
        "source_record_id_then_artifact_id",
        "required",
        "HashAlgorithm",
        "ArtifactDecisionMaterialV1.algorithm",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact.digest",
        "derived::artifact.digest",
        NamedDerived,
        "provenance",
        "ArtifactDecisionMaterialV1.digest",
        "derived:accepted_record_artifact_inventory",
        "source_record_id_then_artifact_id",
        "required",
        "Hash256_or_Hash512",
        "ArtifactDecisionMaterialV1.digest",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact.path",
        "derived::artifact.path",
        NamedDerived,
        "provenance",
        "ArtifactDecisionMaterialV1.path",
        "derived:accepted_record_artifact_inventory",
        "source_record_id_then_artifact_id",
        "explicit_null",
        "Option<SourcePath>",
        "ArtifactDecisionMaterialV1.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::file.source_record_id",
        "derived::file.source_record_id",
        NamedDerived,
        "provenance",
        "FileDecisionMaterialV1.source_record_id",
        "derived:accepted_donat_record_join",
        "source_record_id_then_path",
        "required",
        "SourceRecordId",
        "FileDecisionMaterialV1.source_record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::file.path",
        "derived::file.path",
        NamedDerived,
        "provenance",
        "FileDecisionMaterialV1.path",
        "derived:accepted_donat_file_inventory",
        "source_record_id_then_path",
        "required",
        "RepoPath",
        "FileDecisionMaterialV1.path",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::file.sha256",
        "derived::file.sha256",
        NamedDerived,
        "provenance",
        "FileDecisionMaterialV1.sha256",
        "derived:accepted_donat_file_inventory",
        "source_record_id_then_path",
        "required",
        "Hash256",
        "FileDecisionMaterialV1.sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::source_identity",
        "derived::source_identity",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.sources",
        "derived:accepted_record_join",
        "record_id",
        "empty_array",
        "Vec<SourceIdentityMaterialV1>",
        "ProvenanceMaterialV1.sources",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::artifact",
        "derived::artifact",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.artifacts",
        "derived:accepted_record_artifact_inventory",
        "source_record_id_then_artifact_id",
        "empty_array",
        "Vec<ArtifactDecisionMaterialV1>",
        "ProvenanceMaterialV1.artifacts",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::license",
        "derived::license",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.licenses",
        "derived:accepted_record_license_inventory",
        "canonical_bytes",
        "empty_array",
        "Vec<LicenseDecisionMaterialV1>",
        "ProvenanceMaterialV1.licenses",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::dependency",
        "derived::dependency",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.dependencies",
        "derived:accepted_record_dependency_inventory",
        "dependency",
        "empty_array",
        "Vec<DependencyDecisionMaterialV1>",
        "ProvenanceMaterialV1.dependencies",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::embedded_material",
        "derived::embedded_material",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.embedded_material",
        "derived:accepted_record_embedded_inventory",
        "material_id",
        "empty_array",
        "Vec<EmbeddedDecisionMaterialV1>",
        "ProvenanceMaterialV1.embedded_material",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::notice",
        "derived::notice",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.notices",
        "derived:accepted_record_notice_inventory",
        "id",
        "empty_array",
        "Vec<NoticeMaterialV1>",
        "ProvenanceMaterialV1.notices",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::provider_evidence",
        "derived::provider_evidence",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.provider_evidence",
        "derived:accepted_provider_record_inventory",
        "source_record_id",
        "empty_array",
        "Vec<ProviderEvidenceOriginMaterialV1>",
        "ProvenanceMaterialV1.provider_evidence",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::provider_evidence.source_record_id",
        "derived::provider_evidence.source_record_id",
        NamedDerived,
        "provenance",
        "ProviderEvidenceOriginMaterialV1.source_record_id",
        "derived:accepted_provider_record_join",
        "source_record_id",
        "required",
        "SourceRecordId",
        "ProviderEvidenceOriginMaterialV1.source_record_id",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::fact_origin.artifact_content_sha256",
        "derived::fact_origin.artifact_content_sha256",
        NamedDerived,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.artifact_content_sha256",
        "derived:provider_fact_content_join",
        "use_site",
        "required",
        "Hash256",
        "ResolvedFactOriginV1::ProviderEvidence.artifact_content_sha256",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::fact_origin.location",
        "derived::fact_origin.location",
        NamedDerived,
        "provenance",
        "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.location",
        "derived:provider_fact_location_join",
        "use_site",
        "required",
        "ExactFactLocationMaterialV1",
        "ResolvedFactOriginV1::ProviderEvidence.location",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

    (
        "derived::donat_policy_ids",
        "derived::donat_policy_ids",
        NamedDerived,
        "provenance",
        "ProvenanceMaterialV1.donat_policy_ids",
        "derived:contract_fact_policy_set",
        "lexical",
        "empty_array",
        "Vec<DonatPolicyId>",
        "ProvenanceMaterialV1.donat_policy_ids",
        ProjectionSchema,
        Provenance,
        Mutable,
    );

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
    integrity: NpmIntegrity,
    repository: ImmutableRepository,
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
struct NpmIntegrity {
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
struct ImmutableRepository {
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

#[cfg(test)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceRecordMaterialDto {
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
    #[serde(deserialize_with = "crate::source::deserialize_source_primitive")]
    source_record_id: SourceRecordId,
    #[serde(deserialize_with = "crate::source::deserialize_artifact_hashes")]
    artifact_hashes: Vec<ArtifactHash>,
    license_id: String,
    #[serde(deserialize_with = "crate::source::deserialize_source_primitive")]
    notice_id: crate::NoticeId,
    contract_facts: Vec<ResolvedContractFactBindingDto>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResolvedContractFactBindingDto {
    use_site: String,
    #[serde(deserialize_with = "crate::source::deserialize_contract_fact")]
    fact: ContractFact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[cfg(test)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticMaterialDto {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[cfg(test)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceMaterialDto {
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
            ResolvedFactOriginV1::DonatPolicy { policy_id: _ } => None,
        }
    }
}

pub fn resolve_fact_bindings(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    requirements: &crate::CheckedFactRequirements,
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
        let required_domain = requirements.required_domain(use_site).ok_or_else(|| {
            CatalogError::new(
                "catalog_fact_binding_mismatch",
                "fact use site has no checked normalized origin requirement",
            )
        })?;
        if !required_domain.accepts(fact) {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "fact origin domain differs from the normalized use-site requirement",
            ));
        }
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
                integrity: NpmIntegrity {
                    algorithm: match integrity.algorithm() {
                        crate::source::NpmIntegrityAlgorithm::Sha512 => {
                            NpmIntegrityAlgorithmMaterialV1::Sha512(())
                        }
                    },
                    digest: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(integrity.as_bytes()),
                },
                repository: ImmutableRepository {
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
                    .then_with(|| left.content_sha256.cmp(&right.content_sha256))
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
        ContractFactMaterialV1::DonatPolicy {
            policy_id,
            value: _,
        } => {
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
    let mut success_statuses = success_statuses
        .iter()
        .map(status_range_material)
        .collect::<Vec<_>>();
    success_statuses.sort_by_key(|status| (status.minimum, status.maximum));
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
        success_statuses,
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
            let mut side_effect_steps = side_effect_steps
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
                .collect::<Vec<_>>();
            side_effect_steps.sort_by(|left, right| left.step.cmp(&right.step));
            OperationEffectMaterialV1::ProviderIdempotent { side_effect_steps }
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
        SemanticTriggerMaterialV1::Poll {
            connector: _,
            connector_version: _,
            trigger,
            trigger_version: _,
            event_version: _,
            runtime_abi_epoch: _,
            checkpoint: _,
            processor: _,
            event_type: _,
            per_poll_event_limit: _,
            bounds: _,
        } => ("poll", trigger),
        SemanticTriggerMaterialV1::Webhook {
            connector: _,
            connector_version: _,
            trigger,
            trigger_version: _,
            event_version: _,
            runtime_abi_epoch: _,
            authenticator: _,
            codec: _,
            normalizer: _,
            selected_headers: _,
            raw_body_max_bytes: _,
            timestamp_window_ms: _,
            event_id: _,
            event_type: _,
            output: _,
            redaction: _,
            subscription_operations: _,
        } => ("webhook", trigger),
    }
}

/// Builds provenance from one checked compilation proof.
///
/// The checked proof already borrows the exact accepted-record and reviewed
/// policy contexts used by compilation. Neither contradictory contexts nor a
/// caller-selected semantic hash are accepted by this API.
///
/// ```compile_fail
/// use std::collections::BTreeMap;
/// use donat_connector_catalog::{
///     provenance_material, AcceptedRecordCatalog, CheckedConnectorManifest,
///     DonatPolicyId,
/// };
/// use donat_value_contract::TypedValue;
///
/// fn forge(
///     checked: &CheckedConnectorManifest<'_>,
///     other_catalog: &AcceptedRecordCatalog,
///     other_policies: &BTreeMap<DonatPolicyId, TypedValue>,
/// ) {
///     let claimed_semantic_hash = [0xff; 32];
///     let _ = provenance_material(
///         checked,
///         other_catalog,
///         other_policies,
///         claimed_semantic_hash,
///         1,
///         1,
///         1,
///     );
/// }
/// ```
pub fn provenance_material(
    checked: &crate::CheckedConnectorManifest<'_>,
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
    let accepted_records = checked.accepted_records();
    let reviewed_policies = checked.reviewed_policies();
    let semantic_hash = semantic_sha256(&semantic_material(checked, canonical_schema_epoch)?)?;
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
            accepted_records
                .capability_record(*record_id)
                .ok_or_else(|| {
                    CatalogError::new(
                        "catalog_projection_input_mismatch",
                        "provenance source record has no checked capability",
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
        checked.fact_requirements(),
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
            ContractFact::DonatPolicy {
                policy_id,
                value: _,
            } => Some(policy_id.as_str().to_owned()),
            ContractFact::ProviderEvidence {
                source_record_id: _,
                fact_id: _,
            } => None,
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
        provider_evidence_source_key(&left.source)
            .cmp(&provider_evidence_source_key(&right.source))
            .then_with(|| left.content_sha256.cmp(&right.content_sha256))
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

#[cfg(test)]
mod projection_schema_mutations {
    use std::{path::Path, sync::OnceLock};

    use donat_value_contract::{BoundedInlineBytes, CanonicalDecimal, CanonicalNumber};
    use serde::de::DeserializeOwned;
    use syn::{Fields, GenericArgument, Item, PathArguments, Type, TypePath};

    use super::*;

    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum PathSegment {
        Field(String),
        Element(usize),
    }

    fn full_vector(label: &str) -> &'static str {
        let document = include_str!(
            "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
        );
        let marker = format!("\n{label}:\n{{");
        let start = document.rfind(&marker).unwrap() + marker.len() - 1;
        let end = document[start..].find('\n').unwrap() + start;
        &document[start..end]
    }

    fn decode_exact<T>(bytes: &[u8]) -> Result<T, CatalogError>
    where
        T: DeserializeOwned + Serialize,
    {
        let canonical = canonicalize_raw(bytes)?;
        let decoded = serde_json::from_slice::<T>(&canonical)
            .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
        if canonical_material_bytes(&decoded)? != canonical {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "projection omitted or changed a declared member",
            ));
        }
        Ok(decoded)
    }

    fn source_material(value: SourceRecordMaterialDto) -> SourceRecordMaterialV1 {
        let SourceRecordMaterialDto {
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
        } = value;
        SourceRecordMaterialV1 {
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
    }

    fn semantic_material(value: SemanticMaterialDto) -> SemanticMaterialV1 {
        let SemanticMaterialDto {
            canonical_schema_epoch,
            connector,
            credentials,
            operations,
            origins,
            triggers,
            value_language_epoch,
        } = value;
        SemanticMaterialV1 {
            canonical_schema_epoch,
            connector,
            credentials,
            operations,
            origins,
            triggers,
            value_language_epoch,
        }
    }

    fn provenance_material(value: ProvenanceMaterialDto) -> ProvenanceMaterialV1 {
        let ProvenanceMaterialDto {
            artifacts,
            canonical_schema_epoch,
            classifier_epoch,
            connector,
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
        } = value;
        ProvenanceMaterialV1 {
            artifacts,
            canonical_schema_epoch,
            classifier_epoch,
            connector,
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
        }
    }

    fn value_at_mut<'value>(
        mut value: &'value mut serde_json::Value,
        path: &[PathSegment],
    ) -> &'value mut serde_json::Value {
        for segment in path {
            value = match segment {
                PathSegment::Field(name) => value
                    .as_object_mut()
                    .and_then(|object| object.get_mut(name))
                    .unwrap(),
                PathSegment::Element(index) => value
                    .as_array_mut()
                    .and_then(|values| values.get_mut(*index))
                    .unwrap(),
            };
        }
        value
    }

    #[derive(Clone, Debug)]
    enum RouteSegment {
        Field(String),
        Element,
        Branch(String),
    }

    fn route_segments(canonical_path: &str) -> Vec<RouteSegment> {
        let root_end = canonical_path
            .find(['.', '{'])
            .unwrap_or(canonical_path.len());
        let mut suffix = &canonical_path[root_end..];
        let mut segments = Vec::new();
        while !suffix.is_empty() {
            if let Some(rest) = suffix.strip_prefix("[]") {
                segments.push(RouteSegment::Element);
                suffix = rest;
                continue;
            }
            if let Some(rest) = suffix.strip_prefix("{kind=") {
                let end = rest.find('}').expect("generated branch route closes");
                segments.push(RouteSegment::Branch(rest[..end].to_owned()));
                suffix = &rest[end + 1..];
                continue;
            }
            if let Some(rest) = suffix.strip_prefix('.') {
                let end = rest.find(['.', '{', '[']).unwrap_or(rest.len());
                segments.push(RouteSegment::Field(rest[..end].to_owned()));
                suffix = &rest[end..];
                continue;
            }
            panic!("unparsed generated mutation route: {canonical_path}");
        }
        segments
    }

    fn route_matches(
        value: &serde_json::Value,
        segments: &[RouteSegment],
        path: &mut Vec<PathSegment>,
        matches: &mut Vec<Vec<PathSegment>>,
    ) {
        let Some((segment, rest)) = segments.split_first() else {
            matches.push(path.clone());
            return;
        };
        match segment {
            RouteSegment::Field(name) => {
                if let Some(child) = value.as_object().and_then(|object| object.get(name)) {
                    path.push(PathSegment::Field(name.clone()));
                    route_matches(child, rest, path, matches);
                    path.pop();
                }
            }
            RouteSegment::Element => {
                if let Some(values) = value.as_array() {
                    for (index, child) in values.iter().enumerate() {
                        path.push(PathSegment::Element(index));
                        route_matches(child, rest, path, matches);
                        path.pop();
                    }
                }
            }
            RouteSegment::Branch(kind) => {
                if value
                    .as_object()
                    .and_then(|object| object.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some(kind)
                {
                    route_matches(value, rest, path, matches);
                }
            }
        }
    }

    fn generated_route_matches(
        value: &serde_json::Value,
        segments: &[RouteSegment],
    ) -> Vec<Vec<PathSegment>> {
        fn visit_roots(
            value: &serde_json::Value,
            segments: &[RouteSegment],
            path: &mut Vec<PathSegment>,
            matches: &mut Vec<Vec<PathSegment>>,
        ) {
            route_matches(value, segments, path, matches);
            match value {
                serde_json::Value::Object(object) => {
                    for (name, child) in object {
                        path.push(PathSegment::Field(name.clone()));
                        visit_roots(child, segments, path, matches);
                        path.pop();
                    }
                }
                serde_json::Value::Array(values) => {
                    for (index, child) in values.iter().enumerate() {
                        path.push(PathSegment::Element(index));
                        visit_roots(child, segments, path, matches);
                        path.pop();
                    }
                }
                _ => {}
            }
        }

        let mut matches = Vec::new();
        visit_roots(value, segments, &mut Vec::new(), &mut matches);
        matches
    }

    fn accepted_replacement_candidates(value: &serde_json::Value) -> Vec<serde_json::Value> {
        match value {
            serde_json::Value::Null => vec![
                serde_json::Value::String("x".to_owned()),
                serde_json::Value::Bool(true),
                serde_json::json!(1),
                serde_json::json!({"id": "x", "implementation_revision": 1}),
                serde_json::json!({"major": 1, "minor": 0, "patch": 1}),
                serde_json::json!([]),
            ],
            serde_json::Value::Bool(value) => vec![serde_json::Value::Bool(!value)],
            serde_json::Value::Number(value) => {
                let replacement = if let Some(value) = value.as_u64() {
                    serde_json::json!(value.saturating_add(1))
                } else if let Some(value) = value.as_i64() {
                    serde_json::json!(value.saturating_add(1))
                } else {
                    serde_json::json!(1)
                };
                vec![replacement]
            }
            serde_json::Value::String(value) => {
                let mut candidates = Vec::new();
                if let Ok(number) = value.parse::<u64>() {
                    candidates.push(serde_json::Value::String(
                        number.saturating_add(1).to_string(),
                    ));
                }
                if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    let mut changed = value.clone().into_bytes();
                    changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
                    candidates.push(serde_json::Value::String(
                        String::from_utf8(changed).unwrap(),
                    ));
                }
                candidates.extend([
                    serde_json::Value::String(format!("{value}.mutation")),
                    serde_json::Value::String("x".to_owned()),
                ]);
                candidates
            }
            serde_json::Value::Array(values) => {
                let mut candidates = Vec::new();
                if let Some(first) = values.first() {
                    for replacement in accepted_replacement_candidates(first) {
                        let mut changed = values.clone();
                        changed[0] = replacement;
                        candidates.push(serde_json::Value::Array(changed));
                    }
                    let mut changed = values.clone();
                    changed.push(first.clone());
                    candidates.push(serde_json::Value::Array(changed));
                } else {
                    candidates.extend([
                        serde_json::json!(["x"]),
                        serde_json::json!([1]),
                        serde_json::json!([{"kind": "string", "value": "x"}]),
                    ]);
                }
                candidates
            }
            serde_json::Value::Object(object) => {
                let mut candidates = Vec::new();
                for (name, child) in object {
                    for replacement in accepted_replacement_candidates(child) {
                        let mut changed = object.clone();
                        changed.insert(name.clone(), replacement);
                        candidates.push(serde_json::Value::Object(changed));
                    }
                }
                candidates
            }
        }
    }

    fn mutation_route_segments(descriptor: &CanonicalMutationDescriptor) -> Vec<RouteSegment> {
        let mut segments = route_segments(descriptor.canonical_path);
        let Some((_, member)) = descriptor.material_member.split_once("::") else {
            return segments;
        };
        if !member.contains('.')
            && matches!(segments.last(), Some(RouteSegment::Field(name)) if name == "kind")
        {
            segments.pop();
        }
        segments
    }

    fn mutation_case_root(case: CanonicalMutationCase) -> &'static str {
        match case {
            CanonicalMutationCase::SourceRecord => "SourceRecordMaterialV1",
            CanonicalMutationCase::Semantic => "SemanticMaterialV1",
            CanonicalMutationCase::Provenance => "ProvenanceMaterialV1",
            CanonicalMutationCase::ValueContract => "ValueContractMaterialV1",
            CanonicalMutationCase::TypedValue => "TypedValueMaterialV1",
        }
    }

    #[derive(Clone)]
    struct DeclaredField {
        rust_name: Option<String>,
        type_name: String,
    }

    #[derive(Clone)]
    struct DeclaredVariant {
        rust_name: String,
        fields: Vec<DeclaredField>,
    }

    #[derive(Clone)]
    enum DeclaredShape {
        Struct(Vec<DeclaredField>),
        Enum(Vec<DeclaredVariant>),
    }

    fn declared_type_name(value: &Type) -> String {
        match value {
            Type::Path(TypePath { path, .. }) => {
                let segment = path.segments.last().unwrap();
                match &segment.arguments {
                    PathArguments::None => segment.ident.to_string(),
                    PathArguments::AngleBracketed(arguments) => {
                        let values = arguments
                            .args
                            .iter()
                            .filter_map(|argument| match argument {
                                GenericArgument::Type(value) => Some(declared_type_name(value)),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        format!("{}<{}>", segment.ident, values.join(","))
                    }
                    PathArguments::Parenthesized(_) => segment.ident.to_string(),
                }
            }
            Type::Array(value) => format!("Vec<{}>", declared_type_name(&value.elem)),
            Type::Group(value) => declared_type_name(&value.elem),
            Type::Paren(value) => declared_type_name(&value.elem),
            Type::Reference(value) => declared_type_name(&value.elem),
            Type::Slice(value) => format!("Vec<{}>", declared_type_name(&value.elem)),
            Type::Tuple(value) if value.elems.is_empty() => "()".to_owned(),
            _ => "()".to_owned(),
        }
    }

    fn declared_fields(fields: &Fields) -> Vec<DeclaredField> {
        match fields {
            Fields::Named(fields) => fields
                .named
                .iter()
                .map(|field| DeclaredField {
                    rust_name: field.ident.as_ref().map(ToString::to_string),
                    type_name: declared_type_name(&field.ty),
                })
                .collect(),
            Fields::Unnamed(fields) => fields
                .unnamed
                .iter()
                .map(|field| DeclaredField {
                    rust_name: None,
                    type_name: declared_type_name(&field.ty),
                })
                .collect(),
            Fields::Unit => Vec::new(),
        }
    }

    fn declaration_schema() -> &'static BTreeMap<String, DeclaredShape> {
        static SCHEMA: OnceLock<BTreeMap<String, DeclaredShape>> = OnceLock::new();
        SCHEMA.get_or_init(|| {
            let mut schema = BTreeMap::new();
            for source in [
                CANONICAL_PROJECTION_SCHEMA_DECLARATIONS,
                include_str!("source.rs"),
            ] {
                let file = syn::parse_file(source).unwrap();
                for item in file.items {
                    match item {
                        Item::Struct(value) => {
                            schema.insert(
                                value.ident.to_string(),
                                DeclaredShape::Struct(declared_fields(&value.fields)),
                            );
                        }
                        Item::Enum(value) => {
                            schema.insert(
                                value.ident.to_string(),
                                DeclaredShape::Enum(
                                    value
                                        .variants
                                        .iter()
                                        .map(|variant| DeclaredVariant {
                                            rust_name: variant.ident.to_string(),
                                            fields: declared_fields(&variant.fields),
                                        })
                                        .collect(),
                                ),
                            );
                        }
                        _ => {}
                    }
                }
            }
            schema
        })
    }

    fn declared_struct_field_type(owner: &str, field: &str) -> Option<&'static str> {
        let DeclaredShape::Struct(fields) = declaration_schema().get(owner)? else {
            return None;
        };
        fields
            .iter()
            .find(|candidate| candidate.rust_name.as_deref() == Some(field))
            .map(|field| field.type_name.as_str())
    }

    fn declared_variant(owner: &str, variant: &str) -> Option<&'static DeclaredVariant> {
        let DeclaredShape::Enum(variants) = declaration_schema().get(owner)? else {
            return None;
        };
        variants
            .iter()
            .find(|candidate| candidate.rust_name == variant)
    }

    fn declared_variant_field_type(
        owner: &str,
        variant: &str,
        field: &str,
    ) -> Option<&'static str> {
        declared_variant(owner, variant)?
            .fields
            .iter()
            .find(|candidate| {
                candidate.rust_name.as_deref() == Some(field)
                    || (candidate.rust_name.is_none() && field == "value")
            })
            .map(|field| field.type_name.as_str())
    }

    fn material_member_owner(member: &str) -> &str {
        member
            .split_once("::")
            .map_or_else(|| member.split('.').next().unwrap(), |(owner, _)| owner)
    }

    fn material_owner_exists(case: CanonicalMutationCase, owner: &str) -> bool {
        declaration_schema().contains_key(owner)
            || CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
                .iter()
                .any(|descriptor| {
                    descriptor.case == case
                        && material_member_owner(descriptor.material_member) == owner
                })
    }

    fn resolve_material_owner(case: CanonicalMutationCase, type_name: &str) -> Option<String> {
        let type_name = type_name.trim();
        let mut candidates = vec![type_name.to_owned()];
        if let Some(value) = type_name.strip_suffix("V1") {
            candidates.push(value.to_owned());
        }
        if let Some(value) = type_name.strip_suffix("MaterialV1") {
            candidates.push(format!("{value}Material"));
            candidates.push(format!("{value}V1"));
        } else {
            candidates.push(format!("{type_name}MaterialV1"));
        }
        candidates
            .into_iter()
            .find(|candidate| material_owner_exists(case, candidate))
    }

    fn type_reaches_material_owner(
        case: CanonicalMutationCase,
        type_name: &str,
        target_owner: &str,
        visiting: &mut BTreeSet<String>,
    ) -> bool {
        let type_name = type_name.trim();
        if let Some(inner) = generic_argument(type_name) {
            let inner = if type_name.starts_with("Map<") || type_name.starts_with("BTreeMap<") {
                inner.rsplit_once(',').map_or(inner, |(_, value)| value)
            } else {
                inner
            };
            return type_reaches_material_owner(case, inner.trim(), target_owner, visiting);
        }
        let Some(owner) = resolve_material_owner(case, type_name) else {
            return false;
        };
        if owner == target_owner {
            return true;
        }
        if !visiting.insert(owner.clone()) {
            return false;
        }
        let reaches = match declaration_schema().get(&owner) {
            Some(DeclaredShape::Struct(fields)) => fields.iter().any(|field| {
                type_reaches_material_owner(case, &field.type_name, target_owner, visiting)
            }),
            Some(DeclaredShape::Enum(variants)) => variants.iter().any(|variant| {
                variant.fields.iter().any(|field| {
                    type_reaches_material_owner(case, &field.type_name, target_owner, visiting)
                })
            }),
            None => false,
        };
        visiting.remove(&owner);
        reaches
    }

    fn populate_reachable_container(
        case: CanonicalMutationCase,
        domain: &str,
        value: &mut serde_json::Value,
        type_name: &str,
        target_owner: &str,
    ) -> bool {
        let type_name = type_name.trim();
        if let Some(inner) = generic_argument(type_name) {
            if type_name.starts_with("Option<") {
                if value.is_null()
                    && type_reaches_material_owner(case, inner, target_owner, &mut BTreeSet::new())
                {
                    *value = sample_for_type(domain, inner, &mut BTreeSet::new());
                    return true;
                }
                return !value.is_null()
                    && populate_reachable_container(case, domain, value, inner, target_owner);
            }
            if type_name.starts_with("Box<") {
                return populate_reachable_container(case, domain, value, inner, target_owner);
            }
            if type_name.starts_with("Vec<")
                || type_name.starts_with("NonEmptyVec<")
                || type_name.starts_with("BTreeSet<")
            {
                let Some(values) = value.as_array_mut() else {
                    return false;
                };
                if values.is_empty()
                    && type_reaches_material_owner(case, inner, target_owner, &mut BTreeSet::new())
                {
                    values.push(sample_for_type(domain, inner, &mut BTreeSet::new()));
                    return true;
                }
                return values.iter_mut().any(|child| {
                    populate_reachable_container(case, domain, child, inner, target_owner)
                });
            }
            if type_name.starts_with("Map<") || type_name.starts_with("BTreeMap<") {
                let inner = inner.rsplit_once(',').map_or(inner, |(_, value)| value);
                let Some(values) = value.as_object_mut() else {
                    return false;
                };
                if values.is_empty()
                    && type_reaches_material_owner(
                        case,
                        inner.trim(),
                        target_owner,
                        &mut BTreeSet::new(),
                    )
                {
                    values.insert(
                        "x".to_owned(),
                        sample_for_type(domain, inner.trim(), &mut BTreeSet::new()),
                    );
                    return true;
                }
                return values.values_mut().any(|child| {
                    populate_reachable_container(case, domain, child, inner.trim(), target_owner)
                });
            }
        }

        let Some(owner) = resolve_material_owner(case, type_name) else {
            return false;
        };
        let Some(shape) = declaration_schema().get(&owner) else {
            return false;
        };
        match shape {
            DeclaredShape::Struct(fields) => {
                if fields.len() == 1 && fields[0].rust_name.is_none() {
                    return populate_reachable_container(
                        case,
                        domain,
                        value,
                        &fields[0].type_name,
                        target_owner,
                    );
                }
                for field in fields {
                    if !type_reaches_material_owner(
                        case,
                        &field.type_name,
                        target_owner,
                        &mut BTreeSet::new(),
                    ) {
                        continue;
                    }
                    let Some(field_name) = field.rust_name.as_deref() else {
                        continue;
                    };
                    let member = format!("{owner}.{field_name}");
                    let wire = descriptor_for_member(case, &member)
                        .map_or(field_name, |descriptor| {
                            terminal_wire_name(descriptor.canonical_path)
                        });
                    let Some(child) = value
                        .as_object_mut()
                        .and_then(|object| object.get_mut(wire))
                    else {
                        continue;
                    };
                    if populate_reachable_container(
                        case,
                        domain,
                        child,
                        &field.type_name,
                        target_owner,
                    ) {
                        return true;
                    }
                }
                false
            }
            DeclaredShape::Enum(variants) => {
                let Some(kind) = value
                    .as_object()
                    .and_then(|object| object.get("kind"))
                    .and_then(serde_json::Value::as_str)
                else {
                    return false;
                };
                let Some(variant) = variants.iter().find(|variant| {
                    descriptor_for_member(case, &format!("{owner}::{}", variant.rust_name))
                        .and_then(|descriptor| variant_tag(descriptor.canonical_path))
                        == Some(kind)
                }) else {
                    return false;
                };
                let Some(payload) = value
                    .as_object_mut()
                    .and_then(|object| object.get_mut("value"))
                else {
                    return false;
                };
                if variant.fields.len() == 1 && variant.fields[0].rust_name.is_none() {
                    return populate_reachable_container(
                        case,
                        domain,
                        payload,
                        &variant.fields[0].type_name,
                        target_owner,
                    );
                }
                for field in &variant.fields {
                    if !type_reaches_material_owner(
                        case,
                        &field.type_name,
                        target_owner,
                        &mut BTreeSet::new(),
                    ) {
                        continue;
                    }
                    let Some(field_name) = field.rust_name.as_deref() else {
                        continue;
                    };
                    let Some(child) = payload
                        .as_object_mut()
                        .and_then(|object| object.get_mut(field_name))
                    else {
                        continue;
                    };
                    if populate_reachable_container(
                        case,
                        domain,
                        child,
                        &field.type_name,
                        target_owner,
                    ) {
                        return true;
                    }
                }
                false
            }
        }
    }

    fn terminal_wire_name(canonical_path: &str) -> &str {
        canonical_path
            .rsplit('.')
            .next()
            .unwrap()
            .split(['{', '['])
            .next()
            .unwrap()
    }

    fn direct_struct_fields(
        case: CanonicalMutationCase,
        owner: &str,
    ) -> Vec<&'static CanonicalMutationDescriptor> {
        let prefix = format!("{owner}.");
        let mut members = BTreeSet::new();
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.case == case)
            .filter(|descriptor| {
                descriptor
                    .material_member
                    .strip_prefix(&prefix)
                    .is_some_and(|field| !field.contains('.') && !field.contains("::"))
            })
            .filter(|descriptor| members.insert(descriptor.material_member))
            .collect()
    }

    fn descriptor_for_member(
        case: CanonicalMutationCase,
        member: &str,
    ) -> Option<&'static CanonicalMutationDescriptor> {
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.case == case && descriptor.material_member == member)
    }

    fn direct_enum_variants(
        case: CanonicalMutationCase,
        owner: &str,
    ) -> Vec<&'static CanonicalMutationDescriptor> {
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.case == case)
            .filter(|descriptor| {
                direct_variant(descriptor).is_some_and(|(candidate, _)| candidate == owner)
            })
            .collect()
    }

    fn direct_variant_fields(
        case: CanonicalMutationCase,
        variant: &str,
    ) -> Vec<&'static CanonicalMutationDescriptor> {
        let prefix = format!("{variant}.");
        let mut members = BTreeSet::new();
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.case == case)
            .filter(|descriptor| {
                descriptor
                    .material_member
                    .strip_prefix(&prefix)
                    .is_some_and(|field| !field.contains('.'))
            })
            .filter(|descriptor| members.insert(descriptor.material_member))
            .collect()
    }

    #[derive(Default)]
    struct TypedSelection {
        member_paths: Vec<Vec<PathSegment>>,
        owner_paths: Vec<Vec<PathSegment>>,
    }

    fn typed_selection(
        case: CanonicalMutationCase,
        value: &serde_json::Value,
        target_member: &str,
    ) -> TypedSelection {
        fn visit(
            case: CanonicalMutationCase,
            value: &serde_json::Value,
            type_name: &str,
            target_member: &str,
            target_owner: &str,
            path: &mut Vec<PathSegment>,
            selection: &mut TypedSelection,
        ) {
            let type_name = type_name.trim();
            if let Some(inner) = generic_argument(type_name) {
                if type_name.starts_with("Option<") || type_name.starts_with("Box<") {
                    if !value.is_null() {
                        visit(
                            case,
                            value,
                            inner,
                            target_member,
                            target_owner,
                            path,
                            selection,
                        );
                    }
                    return;
                }
                if type_name.starts_with("Vec<")
                    || type_name.starts_with("NonEmptyVec<")
                    || type_name.starts_with("BTreeSet<")
                {
                    if let Some(values) = value.as_array() {
                        for (index, child) in values.iter().enumerate() {
                            path.push(PathSegment::Element(index));
                            visit(
                                case,
                                child,
                                inner,
                                target_member,
                                target_owner,
                                path,
                                selection,
                            );
                            path.pop();
                        }
                    }
                    return;
                }
                if type_name.starts_with("Map<") || type_name.starts_with("BTreeMap<") {
                    let inner = inner.rsplit_once(',').map_or(inner, |(_, value)| value);
                    if let Some(values) = value.as_object() {
                        for (name, child) in values {
                            path.push(PathSegment::Field(name.clone()));
                            visit(
                                case,
                                child,
                                inner.trim(),
                                target_member,
                                target_owner,
                                path,
                                selection,
                            );
                            path.pop();
                        }
                    }
                    return;
                }
            }

            let Some(owner) = resolve_material_owner(case, type_name) else {
                return;
            };
            if owner == target_owner {
                selection.owner_paths.push(path.clone());
            }

            let variants = direct_enum_variants(case, &owner);
            if !variants.is_empty() {
                let Some(kind) = value
                    .as_object()
                    .and_then(|object| object.get("kind"))
                    .and_then(serde_json::Value::as_str)
                else {
                    return;
                };
                let Some(variant) = variants
                    .into_iter()
                    .find(|variant| variant_tag(variant.canonical_path) == Some(kind))
                else {
                    return;
                };
                if variant.material_member == target_member {
                    selection.member_paths.push(path.clone());
                }
                let Some(payload) = value.as_object().and_then(|object| object.get("value")) else {
                    return;
                };
                let (_, variant_name) = direct_variant(variant).unwrap();
                let fields = direct_variant_fields(case, variant.material_member);
                let mut visited_payload = false;
                for field in fields {
                    let field_name = field
                        .material_member
                        .strip_prefix(&format!("{}.", variant.material_member))
                        .unwrap();
                    let field_type = declared_variant_field_type(&owner, variant_name, field_name)
                        .unwrap_or(field.branch_type);
                    if field_name == "value" {
                        visited_payload = true;
                        path.push(PathSegment::Field("value".to_owned()));
                        if field.material_member == target_member {
                            selection.member_paths.push(path.clone());
                        }
                        visit(
                            case,
                            payload,
                            field_type,
                            target_member,
                            target_owner,
                            path,
                            selection,
                        );
                        path.pop();
                        continue;
                    }
                    let wire = terminal_wire_name(field.canonical_path);
                    let Some(child) = payload.as_object().and_then(|object| object.get(wire))
                    else {
                        continue;
                    };
                    path.push(PathSegment::Field("value".to_owned()));
                    path.push(PathSegment::Field(wire.to_owned()));
                    if field.material_member == target_member {
                        selection.member_paths.push(path.clone());
                    }
                    visit(
                        case,
                        child,
                        field_type,
                        target_member,
                        target_owner,
                        path,
                        selection,
                    );
                    path.pop();
                    path.pop();
                }
                if !visited_payload
                    && let Some(declared) = declared_variant(&owner, variant_name)
                    && declared.fields.len() == 1
                    && declared.fields[0].rust_name.is_none()
                    && declared.fields[0].type_name != "()"
                {
                    path.push(PathSegment::Field("value".to_owned()));
                    visit(
                        case,
                        payload,
                        &declared.fields[0].type_name,
                        target_member,
                        target_owner,
                        path,
                        selection,
                    );
                    path.pop();
                }
                return;
            }

            if let Some(DeclaredShape::Struct(fields)) = declaration_schema().get(&owner) {
                if fields.len() == 1 && fields[0].rust_name.is_none() {
                    visit(
                        case,
                        value,
                        &fields[0].type_name,
                        target_member,
                        target_owner,
                        path,
                        selection,
                    );
                    return;
                }
                for declared in fields {
                    let Some(field_name) = declared.rust_name.as_deref() else {
                        continue;
                    };
                    let member = format!("{owner}.{field_name}");
                    let descriptor = descriptor_for_member(case, &member);
                    let wire = descriptor.map_or(field_name, |descriptor| {
                        terminal_wire_name(descriptor.canonical_path)
                    });
                    let Some(child) = value.as_object().and_then(|object| object.get(wire)) else {
                        continue;
                    };
                    path.push(PathSegment::Field(wire.to_owned()));
                    if member == target_member {
                        selection.member_paths.push(path.clone());
                    }
                    visit(
                        case,
                        child,
                        &declared.type_name,
                        target_member,
                        target_owner,
                        path,
                        selection,
                    );
                    path.pop();
                }
                return;
            }

            for field in direct_struct_fields(case, &owner) {
                let wire = terminal_wire_name(field.canonical_path);
                let Some(child) = value.as_object().and_then(|object| object.get(wire)) else {
                    continue;
                };
                path.push(PathSegment::Field(wire.to_owned()));
                if field.material_member == target_member {
                    selection.member_paths.push(path.clone());
                }
                visit(
                    case,
                    child,
                    field.branch_type,
                    target_member,
                    target_owner,
                    path,
                    selection,
                );
                path.pop();
            }
        }

        let target_owner = material_member_owner(target_member);
        let mut selection = TypedSelection::default();
        visit(
            case,
            value,
            mutation_case_root(case),
            target_member,
            target_owner,
            &mut Vec::new(),
            &mut selection,
        );
        selection.member_paths.sort();
        selection.member_paths.dedup();
        selection.owner_paths.sort();
        selection.owner_paths.dedup();
        selection
    }

    fn typed_selection_progress(selection: &TypedSelection) -> usize {
        if !selection.member_paths.is_empty() {
            2
        } else if !selection.owner_paths.is_empty() {
            1
        } else {
            0
        }
    }

    fn generic_argument(value: &str) -> Option<&str> {
        let start = value.find('<')?;
        value
            .ends_with('>')
            .then_some(&value[start + 1..value.len() - 1])
    }

    fn variant_base(member: &str) -> Option<&str> {
        let separator = member.find("::")?;
        let field = member[separator + 2..]
            .find('.')
            .map(|offset| separator + 2 + offset);
        Some(field.map_or(member, |field| &member[..field]))
    }

    fn variant_tag(canonical_path: &str) -> Option<&str> {
        let start = canonical_path.find("{kind=")? + "{kind=".len();
        let end = canonical_path[start..].find('}')? + start;
        Some(&canonical_path[start..end])
    }

    fn sample_for_type(
        domain: &str,
        value: &str,
        visiting: &mut BTreeSet<String>,
    ) -> serde_json::Value {
        let value = value.trim();
        if let Some(inner) = generic_argument(value) {
            if value.starts_with("Option<") {
                return sample_for_type(domain, inner, visiting);
            }
            if value.starts_with("Vec<") || value.starts_with("NonEmptyVec<") {
                return serde_json::Value::Array(vec![sample_for_type(domain, inner, visiting)]);
            }
            if value.starts_with("Map<") || value.starts_with("BTreeMap<") {
                let inner = inner.rsplit_once(',').map_or(inner, |(_, value)| value);
                return serde_json::json!({
                    "x": sample_for_type(domain, inner.trim(), visiting)
                });
            }
            if value.starts_with("Box<") {
                return sample_for_type(domain, inner, visiting);
            }
        }
        if value.starts_with("Hash256")
            || value == "GitCommit"
            || value == "GitTree"
            || value == "artifact_content_sha256"
            || value == "semantic_sha256"
        {
            return serde_json::Value::String("1".repeat(
                if value == "GitCommit" || value == "GitTree" {
                    40
                } else {
                    64
                },
            ));
        }
        if value.starts_with("Hash512") || value == "bytes64" {
            return serde_json::Value::String("1".repeat(128));
        }
        if matches!(
            value,
            "Epoch" | "u8" | "u16" | "u32" | "NonZeroU16" | "NonZeroU32"
        ) {
            return serde_json::json!(1);
        }
        if matches!(
            value,
            "u64" | "i64" | "NonZeroU64" | "decimal" | "positive_i64_decimal"
        ) {
            return serde_json::Value::String("1".to_owned());
        }
        if value == "bool" {
            return serde_json::Value::Bool(true);
        }
        if value == "StableSemver" {
            return serde_json::json!({"major": 1, "minor": 0, "patch": 0});
        }
        if value == "TypedValueMaterialV1" || value == "TypedValue" {
            return serde_json::json!({"kind": "string", "value": "x"});
        }
        if value == "ValueContractMaterialV1" || value == "ValueContractCatalog" {
            return serde_json::json!({
                "named_objects": {},
                "roots": {},
                "value_language_epoch": 1
            });
        }
        if value == "ValueScalarMaterialV1" || value == "ValueScalar" {
            return serde_json::json!({"kind": "string", "value": null});
        }
        if value == "ValueTypeMaterialV1" || value == "ValueType" {
            return serde_json::json!({
                "kind": "scalar",
                "value": {"kind": "string", "value": null}
            });
        }
        if value == "()" {
            return serde_json::Value::Null;
        }

        let candidates = [value.to_owned(), format!("{value}MaterialV1")];
        for candidate in candidates {
            if !visiting.insert(candidate.clone()) {
                continue;
            }
            let variant = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
                .iter()
                .find(|descriptor| {
                    descriptor.domain == domain
                        && descriptor
                            .material_member
                            .strip_prefix(&format!("{candidate}::"))
                            .is_some_and(|suffix| !suffix.contains('.'))
                });
            if let Some(variant) = variant {
                let sample = sample_variant(variant, visiting);
                visiting.remove(&candidate);
                return sample;
            }

            let prefix = format!("{candidate}.");
            let mut fields = BTreeMap::new();
            for descriptor in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS {
                if descriptor.domain != domain
                    || descriptor.material_member.contains("::")
                    || !descriptor.material_member.starts_with(&prefix)
                {
                    continue;
                }
                let field = &descriptor.material_member[prefix.len()..];
                if field.contains('.') {
                    continue;
                }
                let wire = descriptor
                    .canonical_path
                    .rsplit('.')
                    .next()
                    .unwrap()
                    .trim_end_matches("[]")
                    .to_owned();
                fields
                    .entry(wire)
                    .or_insert_with(|| sample_for_type(domain, descriptor.branch_type, visiting));
            }
            if !fields.is_empty() {
                visiting.remove(&candidate);
                return serde_json::Value::Object(fields.into_iter().collect());
            }
            visiting.remove(&candidate);
        }
        serde_json::Value::String("x".to_owned())
    }

    fn sample_variant(
        descriptor: &CanonicalMutationDescriptor,
        visiting: &mut BTreeSet<String>,
    ) -> serde_json::Value {
        let base = variant_base(descriptor.material_member)
            .expect("variant descriptor has an exact enum member");
        let tag =
            variant_tag(descriptor.canonical_path).expect("variant descriptor has a canonical tag");
        let prefix = format!("{base}.");
        let mut fields = BTreeMap::new();
        for field in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS {
            if field.domain != descriptor.domain
                || field.case != descriptor.case
                || !field.material_member.starts_with(&prefix)
            {
                continue;
            }
            let member = &field.material_member[prefix.len()..];
            if member.contains('.') {
                continue;
            }
            let wire = field
                .canonical_path
                .rsplit('.')
                .next()
                .unwrap()
                .trim_end_matches("[]");
            fields
                .entry(wire.to_owned())
                .or_insert_with(|| sample_for_type(descriptor.domain, field.branch_type, visiting));
        }
        let (owner, variant) = base.split_once("::").unwrap();
        let declared = declared_variant(owner, variant);
        let newtype = declared.is_some_and(|variant| {
            variant.fields.len() == 1 && variant.fields[0].rust_name.is_none()
        });
        let unnamed_payload = declared
            .filter(|variant| {
                variant.fields.len() == 1
                    && variant.fields[0].rust_name.is_none()
                    && variant.fields[0].type_name != "()"
            })
            .map(|variant| variant.fields[0].type_name.as_str());
        let payload = if let Some(type_name) = unnamed_payload {
            sample_for_type(descriptor.domain, type_name, &mut BTreeSet::new())
        } else if fields.is_empty() {
            serde_json::Value::Null
        } else if newtype && fields.len() == 1 && fields.contains_key("value") {
            fields.remove("value").unwrap()
        } else {
            serde_json::Value::Object(fields.into_iter().collect())
        };
        serde_json::json!({"kind": tag, "value": payload})
    }

    fn variant_samples(descriptor: &CanonicalMutationDescriptor) -> Vec<serde_json::Value> {
        let sample = sample_variant(descriptor, &mut BTreeSet::new());
        let mut samples = vec![sample];
        if !CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .any(|field| {
                field.domain == descriptor.domain
                    && field.case == descriptor.case
                    && field
                        .material_member
                        .strip_prefix(&format!("{}.", descriptor.material_member))
                        .is_some_and(|suffix| !suffix.contains('.'))
            })
        {
            samples.push(serde_json::json!({
                "kind": variant_tag(descriptor.canonical_path).unwrap()
            }));
        }
        samples
    }

    fn enum_owner_candidates(value: &str) -> Vec<String> {
        let value = value.trim();
        let mut candidates = vec![value.to_owned()];
        if value.ends_with("MaterialV1") {
            candidates.push(value.trim_end_matches("V1").to_owned());
            candidates.push(value.replace("MaterialV1", "V1"));
        } else {
            candidates.push(format!("{value}MaterialV1"));
        }
        candidates
    }

    fn typed_replacement_candidates(
        descriptor: &CanonicalMutationDescriptor,
        value: &serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let mut replacements = Vec::new();
        match descriptor.branch_type {
            "i64-string" | "u64-string" => {
                replacements.push(serde_json::Value::String("2".to_owned()));
            }
            "decimal-string" => {
                replacements.push(serde_json::Value::String("2.5".to_owned()));
            }
            "base64url" => {
                replacements.push(serde_json::Value::String("AA".to_owned()));
            }
            _ => {}
        }
        let owners = if let Some((owner, _)) = direct_variant(descriptor) {
            vec![owner.to_owned()]
        } else {
            enum_owner_candidates(descriptor.branch_type)
        };
        for variant in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|candidate| {
                candidate.domain == descriptor.domain
                    && candidate.case == descriptor.case
                    && direct_variant(candidate)
                        .is_some_and(|(owner, _)| owners.iter().any(|candidate| candidate == owner))
            })
        {
            replacements.extend(variant_samples(variant));
        }
        if !replacements.is_empty() {
            return replacements;
        }
        replacements = accepted_replacement_candidates(value);
        replacements.push(sample_for_type(
            descriptor.domain,
            descriptor.branch_type,
            &mut BTreeSet::new(),
        ));
        replacements
    }

    #[derive(Clone)]
    struct RebuiltMaterial {
        bytes: Vec<u8>,
        hash: [u8; 32],
    }

    fn exact_rebuild<T: Serialize>(
        input: &[u8],
        material: &T,
        domain: CatalogHashDomain,
    ) -> Result<RebuiltMaterial, CatalogError> {
        let input = canonicalize_raw(input)?;
        let bytes = canonical_material_bytes(material)?;
        if bytes != input {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "projection omitted or changed a declared member",
            ));
        }
        Ok(RebuiltMaterial {
            hash: domain_hash_bytes(domain, &bytes),
            bytes,
        })
    }

    fn source_rebuild(bytes: &[u8]) -> Result<RebuiltMaterial, CatalogError> {
        let material = source_material(decode_exact(bytes)?);
        exact_rebuild(bytes, &material, CatalogHashDomain::SourceRecord)
    }

    fn semantic_rebuild(bytes: &[u8]) -> Result<RebuiltMaterial, CatalogError> {
        let material = semantic_material(decode_exact(bytes)?);
        exact_rebuild(bytes, &material, CatalogHashDomain::Semantic)
    }

    fn provenance_rebuild(bytes: &[u8]) -> Result<RebuiltMaterial, CatalogError> {
        let material = provenance_material(decode_exact(bytes)?);
        exact_rebuild(bytes, &material, CatalogHashDomain::Provenance)
    }

    fn contract_rebuild(bytes: &[u8]) -> Result<RebuiltMaterial, CatalogError> {
        let material = decode_value_contract_material(bytes)?;
        exact_rebuild(bytes, &material, CatalogHashDomain::ValueContract)
    }

    #[derive(Deserialize, Serialize)]
    #[serde(transparent)]
    struct TypedValueMaterialDto(
        #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
        TypedValueMaterialV1,
    );

    fn typed_value_rebuild(bytes: &[u8]) -> Result<RebuiltMaterial, CatalogError> {
        let material = decode_exact::<TypedValueMaterialDto>(bytes)?.0;
        exact_rebuild(bytes, &material, CatalogHashDomain::ValueContract)
    }

    #[derive(Clone)]
    struct BuiltCase {
        case: CanonicalMutationCase,
        domain: &'static str,
        bytes: Vec<u8>,
        rebuild: fn(&[u8]) -> Result<RebuiltMaterial, CatalogError>,
    }

    fn accepted_json_case(case: &BuiltCase, value: serde_json::Value) -> Option<BuiltCase> {
        let bytes = canonicalize_raw(&serde_json::to_vec(&value).unwrap()).ok()?;
        let rebuilt = (case.rebuild)(&bytes).ok()?;
        Some(BuiltCase {
            case: case.case,
            domain: case.domain,
            bytes: rebuilt.bytes,
            rebuild: case.rebuild,
        })
    }

    fn direct_variant(descriptor: &CanonicalMutationDescriptor) -> Option<(&str, &str)> {
        let (owner, member) = descriptor.material_member.split_once("::")?;
        (!member.contains('.')).then_some((owner, member))
    }

    fn owning_variant_descriptor(
        descriptor: &CanonicalMutationDescriptor,
    ) -> Option<&'static CanonicalMutationDescriptor> {
        let variant = variant_base(descriptor.material_member)?;
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .find(|candidate| {
                candidate.case == descriptor.case && candidate.material_member == variant
            })
    }

    fn activation_transition(
        case: &BuiltCase,
        target: &CanonicalMutationDescriptor,
        progress: usize,
    ) -> Option<BuiltCase> {
        let baseline = serde_json::from_slice::<serde_json::Value>(&case.bytes).unwrap();
        if progress == 1
            && let Some(variant) = owning_variant_descriptor(target)
        {
            let selection = typed_selection(case.case, &baseline, target.material_member);
            for path in selection.owner_paths {
                for replacement in variant_samples(variant) {
                    let mut changed = baseline.clone();
                    *value_at_mut(&mut changed, &path) = replacement;
                    let changed_selection =
                        typed_selection(case.case, &changed, target.material_member);
                    if typed_selection_progress(&changed_selection) != 2 {
                        continue;
                    }
                    if let Some(candidate) = accepted_json_case(case, changed) {
                        return Some(candidate);
                    }
                }
            }
        }

        if progress == 0 {
            let mut changed = baseline.clone();
            if populate_reachable_container(
                case.case,
                case.domain,
                &mut changed,
                mutation_case_root(case.case),
                material_member_owner(target.material_member),
            ) {
                let selection = typed_selection(case.case, &changed, target.material_member);
                if typed_selection_progress(&selection) > progress
                    && let Some(candidate) = accepted_json_case(case, changed)
                {
                    return Some(candidate);
                }
            }
        }

        let target_owner = material_member_owner(target.material_member);
        let mut containers = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.domain == case.domain && descriptor.case == case.case)
            .filter(|descriptor| {
                descriptor.branch_type.starts_with("Vec<")
                    || descriptor.branch_type.starts_with("NonEmptyVec<")
                    || descriptor.branch_type.starts_with("BTreeSet<")
                    || descriptor.branch_type.starts_with("Map<")
                    || descriptor.branch_type.starts_with("BTreeMap<")
                    || descriptor.branch_type.starts_with("Option<")
            })
            .collect::<Vec<_>>();
        containers.sort_by_key(|descriptor| !descriptor.branch_type.contains(target_owner));
        for descriptor in containers {
            if progress == 0
                && !descriptor.branch_type.contains(target_owner)
                && !type_reaches_material_owner(
                    case.case,
                    descriptor.branch_type,
                    target_owner,
                    &mut BTreeSet::new(),
                )
            {
                continue;
            }
            for path in generated_route_matches(&baseline, &mutation_route_segments(descriptor)) {
                let current = value_at_mut(&mut baseline.clone(), &path).clone();
                let empty = match &current {
                    serde_json::Value::Null => true,
                    serde_json::Value::Array(values) => values.is_empty(),
                    serde_json::Value::Object(values) => values.is_empty(),
                    _ => false,
                };
                if !empty {
                    continue;
                }
                let replacement =
                    sample_for_type(case.domain, descriptor.branch_type, &mut BTreeSet::new());
                if replacement == current {
                    continue;
                }
                let mut changed = baseline.clone();
                *value_at_mut(&mut changed, &path) = replacement;
                let selection = typed_selection(case.case, &changed, target.material_member);
                if typed_selection_progress(&selection) <= progress {
                    continue;
                }
                let Some(candidate) = accepted_json_case(case, changed) else {
                    continue;
                };
                return Some(candidate);
            }
        }

        let variants = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.domain == case.domain && descriptor.case == case.case)
            .filter_map(|descriptor| {
                direct_variant(descriptor).map(|(owner, variant)| (descriptor, owner, variant))
            })
            .collect::<Vec<_>>();
        for (active, owner, active_variant) in &variants {
            let active_segments = mutation_route_segments(active);
            for path in generated_route_matches(&baseline, &active_segments) {
                for (target, target_owner, target_variant) in &variants {
                    if owner != target_owner || active_variant == target_variant {
                        continue;
                    }
                    for replacement in variant_samples(target) {
                        let mut changed = baseline.clone();
                        *value_at_mut(&mut changed, &path) = replacement;
                        let selection =
                            typed_selection(case.case, &changed, target.material_member);
                        if typed_selection_progress(&selection) <= progress {
                            continue;
                        }
                        let Some(candidate) = accepted_json_case(case, changed) else {
                            continue;
                        };
                        return Some(candidate);
                    }
                }
            }
        }

        None
    }

    fn activate_case(
        cases: &[BuiltCase],
        descriptor: &CanonicalMutationDescriptor,
    ) -> Option<BuiltCase> {
        let bases = cases
            .iter()
            .filter(|case| case.domain == descriptor.domain && case.case == descriptor.case)
            .collect::<Vec<_>>();
        let mut scored = Vec::new();
        for base in &bases {
            let baseline = serde_json::from_slice::<serde_json::Value>(&base.bytes).unwrap();
            let selection = typed_selection(base.case, &baseline, descriptor.material_member);
            let progress = typed_selection_progress(&selection);
            if progress == 2 {
                return Some((*base).clone());
            }
            scored.push(((*base).clone(), progress));
        }
        scored.sort_by_key(|candidate| std::cmp::Reverse(candidate.1));
        for (base, _) in scored {
            let mut case = base;
            loop {
                let baseline = serde_json::from_slice::<serde_json::Value>(&case.bytes).unwrap();
                let selection = typed_selection(case.case, &baseline, descriptor.material_member);
                let progress = typed_selection_progress(&selection);
                if progress == 2 {
                    return Some(case);
                }
                let Some(candidate) = activation_transition(&case, descriptor, progress) else {
                    break;
                };
                case = candidate;
            }
        }
        None
    }

    fn built_cases() -> Vec<BuiltCase> {
        let mut cases = Vec::new();
        let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for entry in std::fs::read_dir(fixture_directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
                continue;
            }
            let Ok(record) = crate::load_record(&path) else {
                continue;
            };
            let material = source_record_material(&record).unwrap();
            cases.push(BuiltCase {
                case: CanonicalMutationCase::SourceRecord,
                domain: "source-record",
                bytes: canonical_material_bytes(&material).unwrap(),
                rebuild: source_rebuild,
            });
        }
        let source =
            source_material(decode_exact(full_vector("source-record").as_bytes()).unwrap());
        cases.push(BuiltCase {
            case: CanonicalMutationCase::SourceRecord,
            domain: "source-record",
            bytes: canonical_material_bytes(&source).unwrap(),
            rebuild: source_rebuild,
        });
        let semantic = semantic_material(decode_exact(full_vector("semantic").as_bytes()).unwrap());
        cases.push(BuiltCase {
            case: CanonicalMutationCase::Semantic,
            domain: "semantic",
            bytes: canonical_material_bytes(&semantic).unwrap(),
            rebuild: semantic_rebuild,
        });
        let provenance =
            provenance_material(decode_exact(full_vector("provenance").as_bytes()).unwrap());
        cases.push(BuiltCase {
            case: CanonicalMutationCase::Provenance,
            domain: "provenance",
            bytes: canonical_material_bytes(&provenance).unwrap(),
            rebuild: provenance_rebuild,
        });
        let contract = decode_value_contract_material(full_vector("value-contract").as_bytes())
            .expect("accepted full value-contract vector");
        cases.push(BuiltCase {
            case: CanonicalMutationCase::ValueContract,
            domain: "value-contract",
            bytes: canonical_material_bytes(&contract).unwrap(),
            rebuild: contract_rebuild,
        });
        let inline_bytes =
            BoundedInlineBytes::try_new(vec![0xff, 0x00], "application/octet-stream", None, 2)
                .unwrap();
        let typed_values = [
            TypedValue::Null,
            TypedValue::Boolean(true),
            TypedValue::String("value".to_owned()),
            TypedValue::Number(CanonicalNumber::I64(-1)),
            TypedValue::Number(CanonicalNumber::U64(1)),
            TypedValue::Number(CanonicalNumber::Decimal(
                CanonicalDecimal::try_new("-1.5").unwrap(),
            )),
            TypedValue::List(vec![TypedValue::String("item".to_owned())]),
            TypedValue::Object(
                [("field".to_owned(), TypedValue::String("value".to_owned()))]
                    .into_iter()
                    .collect(),
            ),
            TypedValue::InlineBytes(inline_bytes),
        ];
        cases.extend(typed_values.iter().map(|value| {
            let material = typed_value_material(value);
            BuiltCase {
                case: CanonicalMutationCase::TypedValue,
                domain: "value-contract",
                bytes: canonical_material_bytes(&material).unwrap(),
                rebuild: typed_value_rebuild,
            }
        }));
        cases
    }

    #[test]
    fn every_generated_mutation_route_hits_one_declared_path() {
        let cases = built_cases();
        let mut active_cases =
            BTreeMap::<(CanonicalMutationCase, &'static str), Option<BuiltCase>>::new();
        let mut missed = Vec::new();
        for descriptor in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS {
            if descriptor.disposition == CanonicalMutationDisposition::Singleton {
                continue;
            }
            let case = active_cases
                .entry((descriptor.case, descriptor.material_member))
                .or_insert_with(|| activate_case(&cases, descriptor))
                .clone();
            let Some(case) = case else {
                missed.push(format!(
                    "{}:{} ({}) has no accepted builder case",
                    descriptor.domain, descriptor.canonical_path, descriptor.material_member,
                ));
                continue;
            };

            let baseline_rebuilt = (case.rebuild)(&case.bytes).unwrap();
            assert_eq!(
                baseline_rebuilt.bytes, case.bytes,
                "accepted fixture did not round-trip through the real material builder"
            );
            let baseline =
                serde_json::from_slice::<serde_json::Value>(&baseline_rebuilt.bytes).unwrap();
            let targets =
                typed_selection(case.case, &baseline, descriptor.material_member).member_paths;
            if targets.is_empty() {
                missed.push(format!(
                    "{}:{} ({}) has no exact typed occurrence",
                    descriptor.domain, descriptor.canonical_path, descriptor.material_member,
                ));
                continue;
            }

            for target in targets {
                let before = value_at_mut(&mut baseline.clone(), &target).clone();
                let mut path_hit = false;
                for replacement in typed_replacement_candidates(descriptor, &before) {
                    let mut changed = baseline.clone();
                    *value_at_mut(&mut changed, &target) = replacement;
                    if changed == baseline {
                        continue;
                    }
                    let changed_bytes = serde_json::to_vec(&changed).unwrap();
                    let Ok(changed_rebuilt) = (case.rebuild)(&changed_bytes) else {
                        continue;
                    };
                    assert_ne!(
                        baseline_rebuilt.bytes, changed_rebuilt.bytes,
                        "valid route mutation was canonical-byte-inert: {}",
                        descriptor.canonical_path
                    );
                    assert_ne!(
                        baseline_rebuilt.hash, changed_rebuilt.hash,
                        "valid route mutation was hash-inert: {}",
                        descriptor.canonical_path
                    );

                    let mut rebuilt_json =
                        serde_json::from_slice::<serde_json::Value>(&changed_rebuilt.bytes)
                            .unwrap();
                    let rebuilt_member = value_at_mut(&mut rebuilt_json, &target).clone();
                    assert_ne!(
                        before, rebuilt_member,
                        "real builder discarded the exact declared-path mutation: {}",
                        descriptor.canonical_path
                    );
                    *value_at_mut(&mut rebuilt_json, &target) = before.clone();
                    assert_eq!(
                        baseline, rebuilt_json,
                        "real builder changed JSON outside the exact declared path: {}",
                        descriptor.canonical_path
                    );
                    path_hit = true;
                    break;
                }
                if !path_hit {
                    missed.push(format!(
                        "{}:{} ({}) rejected every mutation at {target:?}",
                        descriptor.domain, descriptor.canonical_path, descriptor.material_member,
                    ));
                }
            }
        }
        assert!(
            missed.is_empty(),
            "generated routes lacked a branch-complete builder case: {missed:#?}"
        );
    }

    #[test]
    fn generated_singleton_dispositions_are_exact_and_decoder_enforced() {
        fn declared_singleton(descriptor: &CanonicalMutationDescriptor) -> bool {
            let Some((owner, field)) = descriptor.material_member.split_once('.') else {
                return false;
            };
            if owner.contains("::") || field.contains('.') {
                return false;
            }
            let Some(type_name) = declared_struct_field_type(owner, field) else {
                return false;
            };
            let Some(owner) = resolve_material_owner(descriptor.case, type_name) else {
                return false;
            };
            let Some(DeclaredShape::Enum(variants)) = declaration_schema().get(&owner) else {
                return false;
            };
            variants.len() == 1
                && (variants[0].fields.is_empty()
                    || (variants[0].fields.len() == 1
                        && variants[0].fields[0].rust_name.is_none()
                        && variants[0].fields[0].type_name == "()"))
        }

        let declared = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| declared_singleton(descriptor))
            .map(|descriptor| (descriptor.case, descriptor.material_member))
            .collect::<BTreeSet<_>>();
        let generated = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.disposition == CanonicalMutationDisposition::Singleton)
            .map(|descriptor| (descriptor.case, descriptor.material_member))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            generated, declared,
            "generated singleton dispositions and exact unit-enum declarations diverged"
        );

        let cases = built_cases();
        for descriptor in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.disposition == CanonicalMutationDisposition::Singleton)
        {
            let case = activate_case(&cases, descriptor)
                .expect("singleton owner has a real accepted case");
            (case.rebuild)(&case.bytes).expect("singleton baseline rebuilds");
            let mut changed = serde_json::from_slice::<serde_json::Value>(&case.bytes).unwrap();
            let paths =
                typed_selection(case.case, &changed, descriptor.material_member).member_paths;
            assert!(
                !paths.is_empty(),
                "singleton descriptor has no exact typed member path"
            );
            for path in paths {
                let target = value_at_mut(&mut changed, &path);
                let kind = target
                    .as_object_mut()
                    .and_then(|object| object.get_mut("kind"))
                    .expect("singleton material is tagged");
                *kind = serde_json::Value::String("__donat_singleton_probe__".to_owned());
            }
            let changed_bytes = canonicalize_raw(&serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                (case.rebuild)(&changed_bytes).is_err(),
                "undeclared singleton alternative passed the real decoder: {}",
                descriptor.material_member
            );
        }
    }
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
    let mut value = JValueSeed {
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
    value.canonicalize_numbers()?;
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
    fn canonicalize_numbers(&mut self) -> Result<(), CatalogError> {
        match self {
            Self::Number(value) => *value = canonical_number(value)?,
            Self::Array(values) => {
                for value in values {
                    value.canonicalize_numbers()?;
                }
            }
            Self::Object(values) => {
                for (_, value) in values {
                    value.canonicalize_numbers()?;
                }
            }
            Self::Null | Self::Bool(_) | Self::String(_) => {}
        }
        Ok(())
    }

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
        Ok(raw.to_owned())
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
